// Persistence: turn an `Extracted` payload into files (CSV / TXT / downloaded images),
// and own directory-name reservation/collision handling.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use wreq::Client;

use crate::config::{Settings, Source};
use crate::crawler::{CrawlResult, Extracted};
use crate::state::Task;
use crate::util::{build_headers, expand_path, filename_from_url, log, now_secs, sanitize_filename, BoxError};

fn csv_cell(s: &str) -> String { format!("\"{}\"", s.replace('"', "\"\"")) }

pub async fn persist(title: String, extracted: Extracted, source: &Source, settings: &Settings, client: &Client, page_url: &str, task: &Task) -> Result<CrawlResult, BoxError> {
    let base = expand_path(&settings.general.download_dir);
    let dir_name = reserve_dir_name(&title, &settings.general.dir_naming, &settings.general.dir_collision, &base);
    let dest = base.join(&dir_name);
    tokio::fs::create_dir_all(&dest).await?;
    release_dir_lock(&dest);
    let safe = sanitize_filename(&title);
    let dir = dest.to_string_lossy().to_string();

    match extracted {
        Extracted::Data { headers, rows } => {
            let mut csv = String::from("\u{feff}");
            csv.push_str(&headers.iter().map(|h| csv_cell(h)).collect::<Vec<_>>().join(","));
            csv.push('\n');
            for row in &rows {
                csv.push_str(&row.iter().map(|c| csv_cell(c)).collect::<Vec<_>>().join(","));
                csv.push('\n');
            }
            if std::fs::write(dest.join(format!("{}.csv", safe)), csv.as_bytes()).is_err() {
                let _ = tokio::fs::remove_dir_all(&dest).await;
                return Err("write csv failed".into());
            }
            { let mut g = task.lock().unwrap(); g.total = rows.len(); g.done = rows.len(); }
            log("[csv]", &format!("{} rows", rows.len()));
            Ok(CrawlResult { title: safe, count: rows.len(), download_dir: dir })
        }
        Extracted::Text(body) => {
            if std::fs::write(dest.join(format!("{}.txt", safe)), body.as_bytes()).is_err() {
                let _ = tokio::fs::remove_dir_all(&dest).await;
                return Err("write text failed".into());
            }
            { let mut g = task.lock().unwrap(); g.total = g.total.max(1); g.done = g.total; }
            log("[txt]", &format!("{} chars", body.chars().count()));
            Ok(CrawlResult { title: safe, count: 1, download_dir: dir })
        }
        Extracted::Images(urls) => {
            if source.output() == "csv" {
                let mut csv = String::from("\u{feff}index,url\n");
                for (i, u) in urls.iter().enumerate() { csv.push_str(&format!("{},{}\n", i, csv_cell(u))); }
                if std::fs::write(dest.join(format!("{}.csv", safe)), csv.as_bytes()).is_err() {
                    let _ = tokio::fs::remove_dir_all(&dest).await;
                    return Err("write csv failed".into());
                }
                { let mut g = task.lock().unwrap(); g.total = urls.len(); g.done = urls.len(); }
                log("[csv]", &format!("{} links", urls.len()));
                return Ok(CrawlResult { title: safe, count: urls.len(), download_dir: dir });
            }
            { let mut g = task.lock().unwrap(); g.total = urls.len(); g.done = 0; }
            let max_c = if settings.download.max_concurrent == 0 { num_cpus::get().min(6).max(2) } else { settings.download.max_concurrent.max(1) };
            let sem = Arc::new(tokio::sync::Semaphore::new(max_c));
            let headers = Arc::new(build_headers(&source.headers));
            let mut handles = Vec::new();
            for (i, img_url) in urls.iter().enumerate() {
                let c = client.clone();
                let u = img_url.clone();
                let d = dest.join(filename_from_url(&u, i));
                let s = sem.clone();
                let ret = settings.download.retries;
                let to = settings.download.timeout;
                let referer = page_url.to_string();
                let tk = task.clone();
                let hdrs = headers.clone();
                handles.push(tokio::spawn(async move {
                    let _p = s.acquire().await.expect("semaphore");
                    let r = download_with_retry(&c, &u, &d, to, ret, &referer, &hdrs).await;
                    if r.is_ok() { tk.lock().unwrap().done += 1; }
                    r
                }));
            }
            let (mut ok, mut fail) = (0usize, 0usize);
            for h in handles {
                match h.await {
                    Ok(Ok(_)) => ok += 1,
                    Err(e) => { fail += 1; log("[spawn]", &e.to_string()); }
                    Ok(Err(e)) => { fail += 1; log("[dl]", &e.to_string()); }
                }
            }
            log("[done]", &format!("{} - ok={} fail={}", title, ok, fail));
            if ok == 0 {
                let _ = tokio::fs::remove_dir_all(&dest).await;
                return Err("all downloads failed".into());
            }
            Ok(CrawlResult { title, count: ok, download_dir: dir })
        }
    }
}

async fn download_with_retry(client: &Client, url: &str, dest: &Path, to: u64, retries: u32, referer: &str, headers: &[(String, String)]) -> Result<(), BoxError> {
    let mut last = String::new();
    for attempt in 0..=retries {
        if attempt > 0 { tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await; }
        match download_one(client, url, dest, to, referer, headers).await { Ok(_) => return Ok(()), Err(e) => last = e.to_string() }
    }
    Err(format!("failed after {} retries: {}", retries, last).into())
}

async fn download_one(client: &Client, url: &str, dest: &Path, to: u64, referer: &str, headers: &[(String, String)]) -> Result<(), BoxError> {
    let mut req = client.get(url).header("Accept", "image/avif,image/webp,image/*,*/*;q=0.8").header("Accept-Language", "en-US,en;q=0.9").timeout(Duration::from_secs(to));
    // The embedding page is the natural Referer for hotlink-protected images; a source-set Referer wins.
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("referer")) { req = req.header("Referer", referer); }
    for (k, v) in headers { req = req.header(k.as_str(), v.as_str()); }
    let resp = req.send().await?;
    if !resp.status().is_success() { return Err(format!("HTTP {}", resp.status()).into()); }
    let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !ct.starts_with("image/") && !ct.is_empty() { log("[skip]", &format!("not image: {}", ct)); return Err(format!("not image: {}", ct).into()); }
    let bytes = resp.bytes().await?;
    if bytes.is_empty() { return Err("empty".into()); }
    tokio::fs::write(dest, &bytes).await?;
    Ok(())
}

static DIR_LOCK: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
// Once the directory exists on disk, its own presence guards the name, so drop the in-memory reservation.
fn release_dir_lock(path: &Path) { if let Ok(mut l) = DIR_LOCK.lock() { l.remove(path); } }
fn reserve_dir_name(title: &str, naming: &str, collision: &str, base: &Path) -> String { let mut locked = DIR_LOCK.lock().unwrap(); let bn = match naming { "title_timestamp" => format!("{}_{:010}", title, now_secs()), _ => title.to_string() }; let candidate = |n: Option<usize>| -> String { match collision { "timestamp" => match n { Some(n) => format!("{}_{:010}_{}", bn, now_secs(), n), None => format!("{}_{:010}", bn, now_secs()) }, "merge" => bn.clone(), _ => match n { Some(n) => format!("{}_{}", bn, n), None => bn.clone() } } }; let mut name = candidate(None); let mut path = base.join(&name); if !path.exists() && !locked.contains(&path) { locked.insert(path.clone()); return name; } if collision == "merge" { return name; } for n in 2..10000 { name = candidate(Some(n)); path = base.join(&name); if !path.exists() && !locked.contains(&path) { locked.insert(path.clone()); return name; } } name = format!("{}_{:010}", bn, now_secs()); locked.insert(base.join(&name)); name }
