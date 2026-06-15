// Snatch v0.3.0 - Clipboard image crawler with zhconv
#![allow(unused_must_use)]
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use dirs; use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem}; use num_cpus;
use rusqlite::{params, Connection}; use scraper::{Html, Selector}; use serde::Deserialize;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tokio_util::sync::CancellationToken;
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent}; use url::Url;
use zhconv::zhconv; use wreq::Client; use wreq_util::Emulation;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
struct AppState { settings: Settings, rules: Vec<ScraperRule>, db: Mutex<Connection>, client: Client, processing: Mutex<HashSet<String>>, cancel: CancellationToken }
#[derive(Deserialize)] struct Settings { general: GeneralSettings, download: DownloadSettings, notification: NotificationSettings, advanced: AdvancedSettings }
#[derive(Deserialize)] struct GeneralSettings { download_dir: String, dir_naming: String, dir_collision: String }
#[derive(Deserialize)] struct DownloadSettings { max_concurrent: usize, timeout: u64, retries: u32 }
#[derive(Deserialize)] struct NotificationSettings { on_complete: bool, on_error: bool }
#[derive(Deserialize)] struct AdvancedSettings { clipboard_poll_ms: u64 }
#[derive(Deserialize, Clone)] struct ScraperConfig { rules: Vec<ScraperRule> }
#[derive(Deserialize, Clone)] struct ScraperRule { name: String, domain: String, container: Option<String>, #[serde(default)] selectors: Vec<SelectorDef>, #[serde(default)] pagination: Option<PaginationConfig>, #[serde(default)] follow_detail: Option<FollowDetailConfig>, #[serde(default)] exclude: Vec<String>, #[serde(default)] path_contains: Option<String>, #[serde(default)] delay_ms: Option<u64>, #[serde(default)] strip: Vec<String>, #[serde(default = "default_mode")] mode: String, #[serde(default)] content_selector: Option<String>, #[serde(default)] convert: Option<String> }
fn default_mode() -> String { "image".to_string() }
#[derive(Deserialize, Clone)] struct PaginationConfig { #[serde(rename = "type")] pagination_type: String, param: Option<String>, start: Option<usize>, end: Option<usize>, next_selector: Option<String>, max_pages: Option<usize> }
#[derive(Deserialize, Clone)] struct FollowDetailConfig { link_selector: String, container: Option<String>, #[serde(default)] selectors: Vec<SelectorDef> }
#[derive(Deserialize, Clone)] struct SelectorDef { expression: String, attribute: String }
struct CrawlResult { title: String, image_count: usize, download_dir: String }

const DEFAULT_SETTINGS: &str = r##"# Snatch Settings
[general]
download_dir = "~/Desktop/Snatch"
dir_naming = "title"
dir_collision = "number"
[download]
max_concurrent = 0
timeout = 30
retries = 3
[notification]
on_complete = true
on_error = true
[advanced]
clipboard_poll_ms = 2000
"##;

const DEFAULT_SCRAPER: &str = r##"# Snatch Crawl Rules
[[rules]]
name = "General"
domain = "*"
selectors = [
    { expression = "img[data-src]", attribute = "data-src" },
    { expression = "img[src]", attribute = "src" },
]
"##;

fn get_app_dir() -> PathBuf { std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| PathBuf::from(".")) }
fn ensure_configs(app_dir: &Path) { std::fs::create_dir_all(app_dir).ok(); let sp = app_dir.join("settings.toml"); if !sp.exists() { std::fs::write(&sp, DEFAULT_SETTINGS).expect("create settings"); } let rp = app_dir.join("scraper.toml"); if !rp.exists() { std::fs::write(&rp, DEFAULT_SCRAPER).expect("create scraper"); } }
fn load_settings(app_dir: &Path) -> Settings { let p = app_dir.join("settings.toml"); let c = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {}", p.display(), e)); toml::from_str(&c).unwrap_or_else(|e| panic!("parse {}: {}", p.display(), e)) }
fn load_rules(app_dir: &Path) -> Vec<ScraperRule> { let p = app_dir.join("scraper.toml"); let c = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {}", p.display(), e)); let cfg: ScraperConfig = toml::from_str(&c).unwrap_or_else(|e| panic!("parse {}: {}", p.display(), e)); cfg.rules }
fn init_db(app_dir: &Path) -> Connection { let conn = Connection::open(app_dir.join("snatch.db")).expect("open db"); conn.execute_batch("CREATE TABLE IF NOT EXISTS page_downloads (id INTEGER PRIMARY KEY AUTOINCREMENT, page_url TEXT NOT NULL UNIQUE, title TEXT, rule_name TEXT, image_count INTEGER DEFAULT 0, download_dir TEXT, created_at TEXT NOT NULL DEFAULT (datetime('now','localtime')));").expect("init db"); conn }
fn is_downloaded(conn: &Connection, url: &str) -> bool { conn.query_row("SELECT COUNT(*) FROM page_downloads WHERE page_url=?1", params![url], |r| r.get::<_,i64>(0)).unwrap_or(0) > 0 }
fn record_download(conn: &Connection, url: &str, title: &str, rule: &str, count: usize, dir: &str) { let _ = conn.execute("INSERT OR IGNORE INTO page_downloads (page_url,title,rule_name,image_count,download_dir) VALUES (?1,?2,?3,?4,?5)", params![url, title, rule, count as i64, dir]); }

// ---- Crawler ----
static RE_STYLE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?s)<style[^>]*>.*?</style>").unwrap());
static RE_SCRIPT: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?s)<script[^>]*>.*?</script>").unwrap());
static RE_PATH_PAGE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"/\d+/$").unwrap());

async fn crawl(url: &str, rule: &ScraperRule, settings: &Settings, client: &Client) -> Result<CrawlResult, BoxError> {
    log("[crawl]", url);
    let page_urls = collect_page_urls(url, rule, client).await?;
    log("[pages]", &format!("collecting {} pages", page_urls.len()));
    // In image mode these are image URLs; in text mode each entry is one page's body text.
    let mut items = Vec::new();
    let mut title = String::new();
    for (pi, page_url) in page_urls.iter().enumerate() {
        if pi > 0 { let delay = rule.delay_ms.unwrap_or(500); tokio::time::sleep(Duration::from_millis(delay)).await; }
        let resp = client.get(page_url).header("Accept-Language", "zh-CN,zh;q=0.9").send().await?;
        if !resp.status().is_success() { log("[page-err]", &format!("HTTP {}: {}", resp.status(), page_url)); continue; }
        let mut html = resp.text().await?;
        // Pre-process for text mode
        if rule.mode == "text" {
            html = html.replace("</p><p>", "\n\n").replace("</p>\n<p>", "\n\n");
            html = html.replace("<br>", "\n").replace("<br/>", "\n").replace("<br />", "\n");
            html = RE_STYLE.replace_all(&html, "").to_string();
            html = RE_SCRIPT.replace_all(&html, "").to_string();
        }
        let (page_title, direct_images, detail_urls) = {
            let doc = Html::parse_document(&html);
            let t = if title.is_empty() { sanitize_filename(&extract_title(&doc)) } else { String::new() };
            let (imgs, urls) = extract_sync(&doc, rule, page_url);
            (t, imgs, urls)
        };
        if !page_title.is_empty() { title = page_title; }
        log("[page]", &format!("{}: {} bytes, {} img, {} detail", pi+1, html.len(), direct_images.len(), detail_urls.len()));
        items.extend(direct_images);
        if !detail_urls.is_empty() { let detail_imgs = fetch_detail_pages(&detail_urls, rule, client).await; items.extend(detail_imgs); }
    }
    // Image URLs can be sorted/deduped freely; text pages must keep page order, so only touch image mode.
    if rule.mode != "text" { items.sort(); items.dedup(); }
    log("[items]", &format!("total {}", items.len()));
    if items.is_empty() { return Err("nothing extracted".into()); }
    if rule.mode == "text" && rule.convert.as_deref() == Some("simplify") { title = zhconv(&title, "zh-Hans".parse().unwrap()); }
    download_all(title, &items, url, settings, client, &rule.mode, rule.convert.as_deref()).await
}

async fn collect_page_urls(start_url: &str, rule: &ScraperRule, client: &Client) -> Result<Vec<String>, BoxError> {
    let mut urls = vec![start_url.to_string()];
    if let Some(ref p) = rule.pagination {
        match p.pagination_type.as_str() {
            "query" => { if let (Some(param), Some(start), Some(end)) = (&p.param, p.start, p.end) { urls.clear(); for page in start..=end { let sep = if start_url.contains('?') { "&" } else { "?" }; urls.push(format!("{}{}{}={}", start_url, sep, param, page)); } } }
            "path" => { if let (Some(start), Some(end)) = (p.start, p.end) { let base = RE_PATH_PAGE.replace(start_url, "/").to_string(); urls.clear(); for page in start..=end { urls.push(if page == 1 { base.to_string() } else { format!("{}{}/", base, page) }); } } }
            "next_link" => { let max = p.max_pages.unwrap_or(10); let mut current = start_url.to_string(); let mut seen: HashSet<String> = HashSet::new(); seen.insert(current.clone()); for _ in 1..max { let resp = match client.get(&current).header("Accept-Language", "zh-CN,zh;q=0.9").send().await { Ok(r) => r.text().await.ok().unwrap_or_default(), Err(_) => break }; let doc = Html::parse_document(&resp); let ns = p.next_selector.as_deref().unwrap_or("a.next"); let next_href = Selector::parse(ns).ok().and_then(|sel| doc.select(&sel).filter_map(|el| el.value().attr("href")).next()); match next_href { Some(href) => { let next = resolve_url(href, &current); if next.is_empty() || !seen.insert(next.clone()) { break; } current = next; urls.push(current.clone()); } None => break } } }
            _ => {}
        }
    }
    Ok(urls)
}

fn extract_sync(doc: &Html, rule: &ScraperRule, base_url: &str) -> (Vec<String>, Vec<String>) {
    // Text mode
    if rule.mode == "text" {
        if let Some(ref cs) = rule.content_selector {
            if let Ok(sel) = Selector::parse(cs) {
                let text: String = doc.select(&sel).flat_map(|el| el.text()).collect::<Vec<_>>().join("\n");
                if !text.trim().is_empty() {
                    let mut cleaned = text.trim().to_string();
                    for s in &rule.strip { cleaned = cleaned.replace(s.as_str(), ""); }
                    return (vec![cleaned], vec![]);
                }
            }
        }
        return (vec![], vec![]);
    }
    // Image mode
    let mut images = extract_images_impl(doc, &rule.selectors, rule.container.as_ref(), base_url, &rule.exclude);
    let mut detail_urls = Vec::new();
    if let Some(ref fd) = rule.follow_detail {
        if let Ok(link_sel) = Selector::parse(&fd.link_selector) {
            for el in doc.select(&link_sel) {
                if let Some(href) = el.value().attr("href") { let u = resolve_url(href, base_url); if !u.is_empty() { detail_urls.push(u); } }
            }
            images.clear();
        }
    }
    images.sort(); images.dedup(); detail_urls.sort(); detail_urls.dedup();
    (images, detail_urls)
}

async fn fetch_detail_pages(detail_urls: &[String], rule: &ScraperRule, client: &Client) -> Vec<String> {
    let mut images = Vec::new();
    let fd = rule.follow_detail.as_ref().unwrap();
    let selectors = if fd.selectors.is_empty() { &rule.selectors } else { &fd.selectors };
    let container = fd.container.as_ref().or(rule.container.as_ref());
    let delay = rule.delay_ms.unwrap_or(300);
    for (di, detail_url) in detail_urls.iter().enumerate() {
        if di > 0 { tokio::time::sleep(Duration::from_millis(delay)).await; }
        if let Ok(resp) = client.get(detail_url).send().await { if let Ok(html) = resp.text().await { let imgs = { let doc = Html::parse_document(&html); extract_images_impl(&doc, selectors, container, detail_url, &rule.exclude) }; images.extend(imgs); } }
    }
    images.sort(); images.dedup(); images
}

fn extract_images_impl(doc: &Html, selectors: &[SelectorDef], container: Option<&String>, base_url: &str, exclude: &[String]) -> Vec<String> {
    let mut excluded = HashSet::new();
    for ex in exclude { if let Ok(s) = Selector::parse(ex) { for e in doc.select(&s) { excluded.insert(e.id()); } } }
    let mut images = Vec::new();
    for sd in selectors {
        let expr = if let Some(c) = container { format!("{} {}", c, sd.expression) } else { sd.expression.clone() };
        let sel = match Selector::parse(&expr) { Ok(s) => s, Err(_) => continue };
        for el in doc.select(&sel) {
            if excluded.contains(&el.id()) { continue; }
            if let Some(val) = el.value().attr(&sd.attribute) {
                let val = val.trim(); if val.is_empty() || val.starts_with("data:") || val.starts_with("blob:") { continue; }
                let u = if sd.attribute == "srcset" { parse_srcset_best(val) } else { val.to_string() };
                let full = resolve_url(&u, base_url); if !full.is_empty() { images.push(full); }
            }
        }
    }
    images
}

// ---- Download ----
async fn download_all(title: String, images: &[String], page_url: &str, settings: &Settings, client: &Client, mode: &str, convert: Option<&str>) -> Result<CrawlResult, BoxError> {
    let base = expand_path(&settings.general.download_dir);
    let dir_name = reserve_dir_name(&title, &settings.general.dir_naming, &settings.general.dir_collision, &base);
    let dest = base.join(&dir_name); tokio::fs::create_dir_all(&dest).await?; release_dir_lock(&dest);
    let is_text = mode == "text";
    if is_text {
        let mut all_text = images.join("\n\n==========\n\n");
        if let Some("simplify") = convert { all_text = zhconv(&all_text, "zh-Hans".parse().unwrap()); }
        let text_path = dest.join(format!("{}.txt", sanitize_filename(&title)));
        if std::fs::write(&text_path, &all_text).is_err() { let _ = tokio::fs::remove_dir_all(&dest).await; return Err("write text failed".into()); }
        log("[txt]", &format!("saved {} chars", all_text.len()));
        return Ok(CrawlResult { title: sanitize_filename(&title), image_count: 1, download_dir: dest.to_string_lossy().to_string() });
    }
    let max_c = if settings.download.max_concurrent == 0 { num_cpus::get().min(6).max(2) } else { settings.download.max_concurrent.min(6).max(1) };
    let sem = Arc::new(tokio::sync::Semaphore::new(max_c)); let mut handles = Vec::new();
    for (i, img_url) in images.iter().enumerate() {
        let c = client.clone(); let u = img_url.clone(); let d = dest.join(filename_from_url(&u, i));
        let s = sem.clone(); let ret = settings.download.retries; let to = settings.download.timeout; let ref_url = page_url.to_string();
        handles.push(tokio::spawn(async move { let _p = s.acquire().await.expect("semaphore"); download_with_retry(&c, &u, &d, to, ret, &ref_url).await }));
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

// ---- Clipboard Monitor ----
async fn clipboard_monitor(state: Arc<AppState>) {
    let mut clipboard = match arboard::Clipboard::new() { Ok(c) => c, Err(e) => { log("[err]", &format!("clipboard: {}", e)); return; } };
    let mut last = String::new(); let poll = state.settings.advanced.clipboard_poll_ms;
    log("[watch]", &format!("poll {}ms", poll));
    loop {
        if state.cancel.is_cancelled() { return; }
        tokio::time::sleep(Duration::from_millis(poll)).await;
        let content = match clipboard.get_text() { Ok(c) => c.trim().to_string(), Err(_) => continue };
        if content == last || !is_url(&content) { continue; }
        last = content.clone();
        let rule = match find_matching_rule(&state.rules, &content) { Some(r) => r.clone(), None => continue };
        if is_downloaded(&state.db.lock().unwrap(), &content) { log("[skip]", "already downloaded"); continue; }
        { let mut proc = state.processing.lock().unwrap(); if proc.contains(&content) { continue; } proc.insert(content.clone()); }
        let st = state.clone(); let url = content.clone();
        tokio::spawn(async move { process_url(&st, &url, &rule).await; st.processing.lock().unwrap().remove(&url); });
    }
}

async fn process_url(state: &AppState, url: &str, rule: &ScraperRule) {
    log("[match]", &format!("{} -> {}", url, rule.name));
    match crawl(url, rule, &state.settings, &state.client).await {
        Ok(r) => { let db = state.db.lock().unwrap(); record_download(&db, url, &r.title, &rule.name, r.image_count, &r.download_dir); let msg = format!("{} - {} items", r.title, r.image_count); log("[ok]", &msg); if state.settings.notification.on_complete { notify("Snatch Done", &msg); } }
        Err(e) => { let msg = format!("{}: {}", url, e); log("[fail]", &msg); if state.settings.notification.on_error { notify("Snatch Error", &msg); } }
    }
}

fn notify(title: &str, body: &str) { log("[notify]", &format!("{}: {}", title, body)); }

// ---- System Tray ----
fn run_tray(state: Arc<AppState>, app_dir: PathBuf, _rt: tokio::runtime::Runtime) {
    let settings: &'static MenuItem = Box::leak(Box::new(MenuItem::new("Settings", true, None)));
    let exit: &'static MenuItem = Box::leak(Box::new(MenuItem::new("Exit", true, None)));
    let sid = settings.id(); let eid = exit.id();
    let menu = Menu::new(); menu.append(settings).unwrap(); menu.append(&PredefinedMenuItem::separator()).unwrap(); menu.append(exit).unwrap();
    let _tray = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("Snatch").with_icon(create_icon()).build().expect("tray");
    log("start", "Snatch v0.3.0");
    log("config", &app_dir.display().to_string());
    let ev = EventLoopBuilder::new().build();
    ev.run(move |_, _, cf| {
        *cf = ControlFlow::WaitUntil(std::time::Instant::now() + Duration::from_millis(200));
        while let Ok(e) = MenuEvent::receiver().try_recv() { if e.id == sid { open_dir(&app_dir); } else if e.id == eid { state.cancel.cancel(); *cf = ControlFlow::Exit; } }
        while let Ok(_) = TrayIconEvent::receiver().try_recv() {}
    });
}

fn create_icon() -> Icon { const S: usize = 32; let mut rgba = vec![0u8; S*S*4]; let c = S as f64/2.0; for y in 0..S { for x in 0..S { let d = ((x as f64-c).powi(2)+(y as f64-c).powi(2)).sqrt(); let i = (y*S+x)*4; if d < 14.0 { rgba[i]=30; rgba[i+1]=180; rgba[i+2]=80; rgba[i+3]=255; if (x>=9&&x<=12&&y>=7&&y<=25)||(x>=9&&x<=20&&y>=7&&y<=10)||(x>=9&&x<=20&&y>=15&&y<=18)||(x>=18&&x<=20&&y>=7&&y<=18) { rgba[i]=255; rgba[i+1]=255; rgba[i+2]=255; rgba[i+3]=255; } } } } Icon::from_rgba(rgba, S as u32, S as u32).expect("icon") }

// ---- Utilities ----
fn log(tag: &str, msg: &str) { println!("[{}] {}", tag, msg); }
fn now_secs() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() }
fn is_url(s: &str) -> bool { let s = s.trim(); (s.starts_with("http://") || s.starts_with("https://")) && s.contains('.') }
fn find_matching_rule<'a>(rules: &'a [ScraperRule], url: &str) -> Option<&'a ScraperRule> { let parsed = Url::parse(url).ok()?; let host = parsed.host_str()?.to_lowercase(); for rule in rules { if rule.domain == "*" { continue; } let d = rule.domain.to_lowercase(); if host == d || host.ends_with(&format!(".{}", d)) { if let Some(ref pc) = rule.path_contains { if !url.to_lowercase().contains(&pc.to_lowercase()) { continue; } } return Some(rule); } } rules.iter().find(|r| r.domain == "*") }
fn resolve_url(u: &str, base: &str) -> String { if u.starts_with("http") { return u.to_string(); } if u.starts_with("//") { return format!("https:{}", u); } Url::parse(base).ok().and_then(|b| b.join(u).ok()).map(|x| x.to_string()).unwrap_or_default() }
fn filename_from_url(url: &str, idx: usize) -> String { let path = url.split('?').next().unwrap_or(url); let name = path.rsplit('/').next().unwrap_or(""); let ext = if let Some(pos) = name.rfind('.') { &name[pos..] } else { ".jpg" }; if !name.is_empty() && name.contains('.') { let clean: String = name[..name.rfind('.').unwrap_or(0)].chars().map(|c| if c.is_alphanumeric()||c=='-'||c=='_' {c} else {'_'}).collect(); if clean.len()>1 { return format!("{:04}_{}{}", idx, clean, ext); } } format!("{:04}{}", idx, ext) }
fn sanitize_filename(name: &str) -> String { let s: String = name.chars().map(|c| { let b = c as u32; if c.is_control()||b==0||b==47||b==58||b==42||b==63||b==34||b==60||b==62||b==124||b==92 {'_'} else {c} }).collect(); let s = s.trim().trim_matches('.'); if s.is_empty() { "untitled".to_string() } else { s.chars().take(100).collect() } }
fn expand_path(p: &str) -> PathBuf { if p.starts_with("~/") || p == "~" { if let Some(h) = dirs::home_dir() { return h.join(p.strip_prefix("~/").unwrap_or("")); } } PathBuf::from(p) }
fn open_dir(p: &Path) { #[cfg(target_os = "windows")] { let _ = std::process::Command::new("explorer").arg(p.to_string_lossy().to_string()).spawn(); } #[cfg(not(target_os = "windows"))] { let _ = std::process::Command::new("xdg-open").arg(p.to_string_lossy().to_string()).spawn(); } }
fn parse_srcset_best(s: &str) -> String { let mut best_url = ""; let mut best_w = 0u64; for part in s.split(',') { let mut segs = part.trim().split_whitespace(); let url = segs.next().unwrap_or(""); let desc = segs.next().unwrap_or(""); let w = if let Some(d) = desc.strip_suffix('w') { d.parse::<u64>().unwrap_or(0) } else if let Some(d) = desc.strip_suffix('x') { (d.parse::<f64>().unwrap_or(1.0)*1000.0) as u64 } else { 0 }; if w >= best_w { best_w = w; best_url = url; } } if best_url.is_empty() { s.split(',').last().map(|x| x.trim().split_whitespace().next().unwrap_or("").to_string()).unwrap_or_default() } else { best_url.to_string() } }
fn extract_title(doc: &Html) -> String { let sel = Selector::parse("title").unwrap(); doc.select(&sel).next().map(|e| e.text().collect::<String>().trim().to_string()).unwrap_or_else(|| "untitled".to_string()) }
static DIR_LOCK: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
// Once the directory exists on disk, its own presence guards the name, so drop the in-memory reservation.
fn release_dir_lock(path: &Path) { if let Ok(mut l) = DIR_LOCK.lock() { l.remove(path); } }
fn reserve_dir_name(title: &str, naming: &str, collision: &str, base: &Path) -> String { let mut locked = DIR_LOCK.lock().unwrap(); let bn = match naming { "title_timestamp" => format!("{}_{:010}", title, now_secs()), _ => title.to_string() }; let candidate = |n: Option<usize>| -> String { match collision { "timestamp" => match n { Some(n) => format!("{}_{:010}_{}", bn, now_secs(), n), None => format!("{}_{:010}", bn, now_secs()) }, "merge" => bn.clone(), _ => match n { Some(n) => format!("{}_{}", bn, n), None => bn.clone() } } }; let mut name = candidate(None); let mut path = base.join(&name); if !path.exists() && !locked.contains(&path) { locked.insert(path.clone()); return name; } if collision == "merge" { return name; } for n in 2..10000 { name = candidate(Some(n)); path = base.join(&name); if !path.exists() && !locked.contains(&path) { locked.insert(path.clone()); return name; } } name = format!("{}_{:010}", bn, now_secs()); locked.insert(base.join(&name)); name }

// ---- Main ----
fn main() {
    let app_dir = get_app_dir(); ensure_configs(&app_dir); let settings = load_settings(&app_dir); let rules = load_rules(&app_dir); let db = init_db(&app_dir);
    log("start", &format!("Snatch v0.3.0 — {} rules", rules.len()));
    let client = Client::builder().emulation(Emulation::Chrome136).redirect(wreq::redirect::Policy::limited(10)).cookie_store(true).build().expect("client");
    let cancel = CancellationToken::new();
    let state = Arc::new(AppState { settings, rules, db: Mutex::new(db), client, processing: Mutex::new(HashSet::new()), cancel: cancel.clone() });
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio");
    let sc = state.clone(); rt.spawn(async move { clipboard_monitor(sc).await; });
    run_tray(state, app_dir, rt);
}
