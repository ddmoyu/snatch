// Persistence: download image files, export image links to CSV, or write extracted text.
// Also owns directory-name reservation/collision handling.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use wreq::Client;
use zhconv::zhconv;

use crate::config::Settings;
use crate::crawler::CrawlResult;
use crate::state::Task;
use crate::util::{expand_path, filename_from_url, log, now_secs, sanitize_filename, BoxError};

pub async fn download_all(title: String, images: &[String], page_url: &str, settings: &Settings, client: &Client, mode: &str, output: &str, convert: Option<&str>, task: &Task) -> Result<CrawlResult, BoxError> {
    let base = expand_path(&settings.general.download_dir);
    let dir_name = reserve_dir_name(&title, &settings.general.dir_naming, &settings.general.dir_collision, &base);
    let dest = base.join(&dir_name); tokio::fs::create_dir_all(&dest).await?; release_dir_lock(&dest);
    let is_text = mode == "text";
    if is_text {
        let mut all_text = images.join("\n\n==========\n\n");
        if let Some("simplify") = convert { all_text = zhconv(&all_text, "zh-Hans".parse().unwrap()); }
        let text_path = dest.join(format!("{}.txt", sanitize_filename(&title)));
        if std::fs::write(&text_path, &all_text).is_err() { let _ = tokio::fs::remove_dir_all(&dest).await; return Err("write text failed".into()); }
        { let mut g = task.lock().unwrap(); g.total = images.len(); g.done = images.len(); }
        log("[txt]", &format!("saved {} chars", all_text.len()));
        return Ok(CrawlResult { title: sanitize_filename(&title), image_count: 1, download_dir: dest.to_string_lossy().to_string() });
    }
    // Image-link export: write the URL list to CSV instead of downloading the files.
    if output == "csv" {
        let mut csv = String::from("\u{feff}index,url\n");
        for (i, u) in images.iter().enumerate() { csv.push_str(&format!("{},\"{}\"\n", i, u.replace('"', "\"\""))); }
        let path = dest.join(format!("{}.csv", sanitize_filename(&title)));
        if std::fs::write(&path, csv.as_bytes()).is_err() { let _ = tokio::fs::remove_dir_all(&dest).await; return Err("write csv failed".into()); }
        { let mut g = task.lock().unwrap(); g.total = images.len(); g.done = images.len(); }
        log("[csv]", &format!("exported {} links", images.len()));
        return Ok(CrawlResult { title: sanitize_filename(&title), image_count: images.len(), download_dir: dest.to_string_lossy().to_string() });
    }
    { let mut g = task.lock().unwrap(); g.total = images.len(); g.done = 0; }
    let max_c = if settings.download.max_concurrent == 0 { num_cpus::get().min(6).max(2) } else { settings.download.max_concurrent.max(1) };
    let sem = Arc::new(tokio::sync::Semaphore::new(max_c)); let mut handles = Vec::new();
    for (i, img_url) in images.iter().enumerate() {
        let c = client.clone(); let u = img_url.clone(); let d = dest.join(filename_from_url(&u, i));
        let s = sem.clone(); let ret = settings.download.retries; let to = settings.download.timeout; let ref_url = page_url.to_string(); let tk = task.clone();
        handles.push(tokio::spawn(async move { let _p = s.acquire().await.expect("semaphore"); let r = download_with_retry(&c, &u, &d, to, ret, &ref_url).await; if r.is_ok() { tk.lock().unwrap().done += 1; } r }));
    }
    let (mut ok, mut fail) = (0usize, 0usize);
    for h in handles { match h.await { Ok(Ok(_)) => ok += 1, Err(e) => { fail += 1; log("[spawn]", &e.to_string()); } Ok(Err(e)) => { fail += 1; log("[dl]", &e.to_string()); } } }
    log("[done]", &format!("{} - ok={} fail={}", title, ok, fail));
    if ok == 0 { let _ = tokio::fs::remove_dir_all(&dest).await; return Err("all downloads failed".into()); }
    Ok(CrawlResult { title, image_count: ok, download_dir: dest.to_string_lossy().to_string() })
}

async fn download_with_retry(client: &Client, url: &str, dest: &Path, to: u64, retries: u32, referer: &str) -> Result<(), BoxError> {
    let mut last = String::new();
    for attempt in 0..=retries { if attempt > 0 { tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await; } match download_one(client, url, dest, to, referer).await { Ok(_) => return Ok(()), Err(e) => last = e.to_string() } }
    Err(format!("failed after {} retries: {}", retries, last).into())
}

async fn download_one(client: &Client, url: &str, dest: &Path, to: u64, referer: &str) -> Result<(), BoxError> {
    let resp = client.get(url).header("Referer", referer).header("Accept", "image/avif,image/webp,image/*,*/*;q=0.8").header("Accept-Language", "en-US,en;q=0.9").timeout(Duration::from_secs(to)).send().await?;
    if !resp.status().is_success() { return Err(format!("HTTP {}", resp.status()).into()); }
    let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !ct.starts_with("image/") && !ct.is_empty() { log("[skip]", &format!("not image: {}", ct)); return Err(format!("not image: {}", ct).into()); }
    let bytes = resp.bytes().await?; if bytes.is_empty() { return Err("empty".into()); }
    tokio::fs::write(dest, &bytes).await?; Ok(())
}

static DIR_LOCK: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
// Once the directory exists on disk, its own presence guards the name, so drop the in-memory reservation.
fn release_dir_lock(path: &Path) { if let Ok(mut l) = DIR_LOCK.lock() { l.remove(path); } }
fn reserve_dir_name(title: &str, naming: &str, collision: &str, base: &Path) -> String { let mut locked = DIR_LOCK.lock().unwrap(); let bn = match naming { "title_timestamp" => format!("{}_{:010}", title, now_secs()), _ => title.to_string() }; let candidate = |n: Option<usize>| -> String { match collision { "timestamp" => match n { Some(n) => format!("{}_{:010}_{}", bn, now_secs(), n), None => format!("{}_{:010}", bn, now_secs()) }, "merge" => bn.clone(), _ => match n { Some(n) => format!("{}_{}", bn, n), None => bn.clone() } } }; let mut name = candidate(None); let mut path = base.join(&name); if !path.exists() && !locked.contains(&path) { locked.insert(path.clone()); return name; } if collision == "merge" { return name; } for n in 2..10000 { name = candidate(Some(n)); path = base.join(&name); if !path.exists() && !locked.contains(&path) { locked.insert(path.clone()); return name; } } name = format!("{}_{:010}", bn, now_secs()); locked.insert(base.join(&name)); name }
