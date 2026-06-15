// Snatch v0.3.0 - Clipboard image/novel crawler with a ratatui dashboard
#![allow(unused_must_use)]
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use dirs; use num_cpus;
use log::{info, warn};
use rusqlite::{params, Connection}; use scraper::{Html, Selector}; use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use url::Url;
use zhconv::zhconv; use wreq::Client; use wreq_util::Emulation;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

// How many crawls may run at once; extra clipboard hits sit in the Queued state.
const MAX_TASKS: usize = 3;

struct AppState { settings: Settings, rules: Vec<ScraperRule>, db: Mutex<Connection>, client: Client, processing: Mutex<HashSet<String>>, cancel: CancellationToken, tasks: Mutex<Vec<Task>>, task_sem: Arc<tokio::sync::Semaphore>, started: Instant }

// ---- Task model (drives the dashboard) ----
#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskStatus { Queued, Running, Done, Failed }
struct TaskData { url: String, rule_name: String, mode: String, output: String, title: String, status: TaskStatus, done: usize, total: usize, error: String, download_dir: String, started: Option<Instant>, finished: Option<Instant> }
impl TaskData {
    fn new(url: &str, rule_name: &str, mode: &str, output: &str) -> Self {
        TaskData { url: url.to_string(), rule_name: rule_name.to_string(), mode: mode.to_string(), output: output.to_string(), title: String::new(), status: TaskStatus::Queued, done: 0, total: 0, error: String::new(), download_dir: String::new(), started: None, finished: None }
    }
    fn elapsed(&self) -> Option<Duration> {
        match (self.started, self.finished) {
            (Some(s), Some(f)) => Some(f.saturating_duration_since(s)),
            (Some(s), None) => Some(s.elapsed()),
            _ => None,
        }
    }
}
type Task = Arc<Mutex<TaskData>>;

#[derive(Deserialize)] struct Settings { general: GeneralSettings, download: DownloadSettings, advanced: AdvancedSettings }
#[derive(Deserialize)] struct GeneralSettings { download_dir: String, dir_naming: String, dir_collision: String }
#[derive(Deserialize)] struct DownloadSettings { max_concurrent: usize, timeout: u64, retries: u32 }
#[derive(Deserialize)] struct AdvancedSettings { clipboard_poll_ms: u64 }
#[derive(Deserialize, Clone)] struct ScraperConfig { rules: Vec<ScraperRule> }
#[derive(Deserialize, Clone)] struct ScraperRule { name: String, domain: String, container: Option<String>, #[serde(default)] selectors: Vec<SelectorDef>, #[serde(default)] pagination: Option<PaginationConfig>, #[serde(default)] follow_detail: Option<FollowDetailConfig>, #[serde(default)] exclude: Vec<String>, #[serde(default)] path_contains: Option<String>, #[serde(default)] delay_ms: Option<u64>, #[serde(default)] strip: Vec<String>, #[serde(default = "default_mode")] mode: String, #[serde(default)] content_selector: Option<String>, #[serde(default)] convert: Option<String>, #[serde(default)] output: Option<String> }
fn default_mode() -> String { "image".to_string() }
// How to persist a rule's results; defaults preserve the original behaviour (image -> files, text -> txt).
fn effective_output(rule: &ScraperRule) -> &str { rule.output.as_deref().unwrap_or(if rule.mode == "text" { "txt" } else { "files" }) }
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
[advanced]
# macOS-only clipboard poll interval (ms); Windows/X11 are event-driven. Range 200-5000.
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

async fn crawl(url: &str, rule: &ScraperRule, settings: &Settings, client: &Client, task: &Task) -> Result<CrawlResult, BoxError> {
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
        if !page_title.is_empty() { title = page_title; task.lock().unwrap().title = title.clone(); }
        if rule.mode == "text" {
            let chars: usize = direct_images.iter().map(|s| s.chars().count()).sum();
            log("[page]", &format!("{}: {} bytes, {} chars", pi + 1, html.len(), chars));
        } else if detail_urls.is_empty() {
            log("[page]", &format!("{}: {} bytes, {} img", pi + 1, html.len(), direct_images.len()));
        } else {
            log("[page]", &format!("{}: {} bytes, {} img, {} detail links", pi + 1, html.len(), direct_images.len(), detail_urls.len()));
        }
        items.extend(direct_images);
        if !detail_urls.is_empty() { let detail_imgs = fetch_detail_pages(&detail_urls, rule, client).await; items.extend(detail_imgs); }
    }
    // Image URLs can be sorted/deduped freely; text pages must keep page order, so only touch image mode.
    if rule.mode != "text" { items.sort(); items.dedup(); }
    log("[items]", &format!("total {}", items.len()));
    if items.is_empty() { return Err("nothing extracted".into()); }
    if rule.mode == "text" && rule.convert.as_deref() == Some("simplify") { title = zhconv(&title, "zh-Hans".parse().unwrap()); }
    download_all(title, &items, url, settings, client, &rule.mode, effective_output(rule), rule.convert.as_deref(), task).await
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
async fn download_all(title: String, images: &[String], page_url: &str, settings: &Settings, client: &Client, mode: &str, output: &str, convert: Option<&str>, task: &Task) -> Result<CrawlResult, BoxError> {
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

// ---- Clipboard listener (event-driven via clipboard-master) ----
// On each OS clipboard-change event, read the text and forward it to the async dispatcher.
// Push-based on Windows (AddClipboardFormatListener) and Linux/X11 (XFixes); on macOS
// clipboard-master polls the change counter internally at `sleep_interval`.
struct ClipHandler { tx: tokio::sync::mpsc::UnboundedSender<String>, poll_ms: u64 }
impl clipboard_master::ClipboardHandler for ClipHandler {
    fn on_clipboard_change(&mut self) -> clipboard_master::CallbackResult {
        if let Ok(mut c) = arboard::Clipboard::new() {
            if let Ok(text) = c.get_text() { let _ = self.tx.send(text); }
        }
        clipboard_master::CallbackResult::Next
    }
    fn on_clipboard_error(&mut self, error: std::io::Error) -> clipboard_master::CallbackResult {
        log("[clip-err]", &error.to_string());
        clipboard_master::CallbackResult::Next
    }
    // Only consulted by the macOS polling fallback; reuses the configured clipboard_poll_ms.
    fn sleep_interval(&self) -> Duration { Duration::from_millis(self.poll_ms.clamp(200, 5000)) }
}

// Receives raw clipboard text from the listener thread and applies the URL/rule/dedup gating,
// then enqueues a crawl. Lives on the tokio runtime so it can spawn tasks and touch shared state.
async fn clipboard_dispatcher(state: Arc<AppState>, mut rx: tokio::sync::mpsc::UnboundedReceiver<String>) {
    let mut last = String::new();
    while let Some(raw) = rx.recv().await {
        if state.cancel.is_cancelled() { return; }
        let content = raw.trim().to_string();
        if content == last || !is_url(&content) { continue; }
        last = content.clone();
        let rule = match find_matching_rule(&state.rules, &content) { Some(r) => r.clone(), None => continue };
        if is_downloaded(&state.db.lock().unwrap(), &content) { log("[skip]", "already downloaded"); continue; }
        { let mut proc = state.processing.lock().unwrap(); if proc.contains(&content) { continue; } proc.insert(content.clone()); }
        let task: Task = Arc::new(Mutex::new(TaskData::new(&content, &rule.name, &rule.mode, effective_output(&rule))));
        state.tasks.lock().unwrap().push(task.clone());
        let st = state.clone(); let url = content.clone();
        tokio::spawn(async move {
            let _guard = ProcGuard { state: st.clone(), url: url.clone() };
            let _permit = match st.task_sem.clone().acquire_owned().await { Ok(p) => p, Err(_) => return };
            { let mut g = task.lock().unwrap(); g.status = TaskStatus::Running; g.started = Some(Instant::now()); }
            process_url(&st, &url, &rule, &task).await;
        });
    }
}

// Removes the URL from the in-flight set even if the crawl task unwinds.
struct ProcGuard { state: Arc<AppState>, url: String }
impl Drop for ProcGuard { fn drop(&mut self) { if let Ok(mut p) = self.state.processing.lock() { p.remove(&self.url); } } }

async fn process_url(state: &AppState, url: &str, rule: &ScraperRule, task: &Task) {
    log("[match]", &format!("{} -> {}", url, rule.name));
    match crawl(url, rule, &state.settings, &state.client, task).await {
        Ok(r) => {
            { let db = state.db.lock().unwrap(); record_download(&db, url, &r.title, &rule.name, r.image_count, &r.download_dir); }
            log("[ok]", &format!("{} - {} items", r.title, r.image_count));
            { let mut g = task.lock().unwrap(); g.status = TaskStatus::Done; g.title = r.title.clone(); g.download_dir = r.download_dir.clone(); g.finished = Some(Instant::now()); }
        }
        Err(e) => {
            log("[fail]", &format!("{}: {}", url, e));
            { let mut g = task.lock().unwrap(); g.status = TaskStatus::Failed; g.error = e.to_string(); g.finished = Some(Instant::now()); }
        }
    }
}

// ---- TUI Dashboard ----
fn run_tui(state: Arc<AppState>) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    // Even with panic = "abort" the hook still runs, so the terminal is restored before exit.
    let orig = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| { ratatui::restore(); orig(info); }));
    let mut selected: usize = 0;
    let mut tick: u64 = 0;
    let res = loop {
        tick = tick.wrapping_add(1);
        let count = state.tasks.lock().unwrap().len();
        if count > 0 && selected >= count { selected = count - 1; }
        terminal.draw(|f| ui(f, &state, selected, tick))?;
        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press { continue; }
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                    KeyCode::Up | KeyCode::Char('k') => { if selected > 0 { selected -= 1; } }
                    KeyCode::Down | KeyCode::Char('j') => { if count > 0 && selected + 1 < count { selected += 1; } }
                    KeyCode::Char('c') => { state.tasks.lock().unwrap().retain(|t| { let s = t.lock().unwrap().status; s != TaskStatus::Done && s != TaskStatus::Failed }); }
                    KeyCode::Enter => {
                        let dir = { let ts = state.tasks.lock().unwrap(); ts.get(selected).and_then(|t| { let g = t.lock().unwrap(); if g.download_dir.is_empty() { None } else { Some(g.download_dir.clone()) } }) };
                        if let Some(d) = dir { open_dir(Path::new(&d)); }
                    }
                    _ => {}
                }
            }
        }
    };
    ratatui::restore();
    res
}

struct Snap { rule: String, url: String, title: String, status: TaskStatus, done: usize, total: usize, error: String, mode: String, output: String, elapsed: Option<Duration> }

fn ui(f: &mut ratatui::Frame, state: &AppState, selected: usize, tick: u64) {
    use ratatui::layout::{Alignment, Constraint, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table};

    const ACCENT: Color = Color::Rgb(46, 204, 113);
    const DIMC: Color = Color::Rgb(128, 128, 128);
    const SEL_BG: Color = Color::Rgb(38, 42, 54);
    let border = Style::default().fg(Color::Rgb(70, 74, 82));

    let area = f.area();
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(4), Constraint::Length(10), Constraint::Length(1)]).split(area);

    let snaps: Vec<Snap> = state.tasks.lock().unwrap().iter().map(|t| {
        let g = t.lock().unwrap();
        Snap { rule: g.rule_name.clone(), url: g.url.clone(), title: g.title.clone(), status: g.status, done: g.done, total: g.total, error: g.error.clone(), mode: g.mode.clone(), output: g.output.clone(), elapsed: g.elapsed() }
    }).collect();

    // --- Header (info left, status chips right) ---
    let (mut nq, mut nr, mut nd, mut nf) = (0u32, 0u32, 0u32, 0u32);
    let mut got = 0usize;
    for s in &snaps { got += if s.mode == "text" || s.output == "csv" { (s.status == TaskStatus::Done) as usize } else { s.done }; match s.status { TaskStatus::Queued => nq += 1, TaskStatus::Running => nr += 1, TaskStatus::Done => nd += 1, TaskStatus::Failed => nf += 1 } }
    let hdr = Layout::horizontal([Constraint::Min(0), Constraint::Length(34)]).split(chunks[0]);
    let left = Line::from(vec![
        Span::styled(" SNATCH ", Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled(" v0.3.0", Style::default().fg(DIMC)),
        Span::raw("   "),
        Span::styled(format!("⏱ {}", fmt_dur(state.started.elapsed())), Style::default().fg(Color::Gray)),
        Span::raw("   "),
        Span::styled(format!("↓ {} files", got), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
    ]);
    f.render_widget(Paragraph::new(left), hdr[0]);
    let chips = Line::from(vec![
        Span::styled(format!("✓ {}  ", nd), Style::default().fg(Color::Green)),
        Span::styled(format!("● {}  ", nr), Style::default().fg(Color::Yellow)),
        Span::styled(format!("• {}  ", nq), Style::default().fg(DIMC)),
        Span::styled(format!("✗ {}", nf), Style::default().fg(Color::Red)),
    ]);
    f.render_widget(Paragraph::new(chips).alignment(Alignment::Right), hdr[1]);

    // --- Task table ---
    const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let title_blk = Line::from(Span::styled(format!(" Tasks ({}) ", snaps.len()), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)));
    if snaps.is_empty() {
        let block = Block::bordered().border_type(BorderType::Rounded).border_style(border).title(title_blk);
        let inner = block.inner(chunks[1]);
        f.render_widget(block, chunks[1]);
        let hint = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled("Waiting for clipboard…", Style::default().fg(DIMC).add_modifier(Modifier::BOLD))),
            Line::from(Span::styled("copy a matching URL to start a task", Style::default().fg(DIMC))),
        ]).alignment(Alignment::Center);
        f.render_widget(hint, inner);
    } else {
        let rows: Vec<Row> = snaps.iter().enumerate().map(|(i, s)| {
            let (icon, color) = match s.status {
                TaskStatus::Queued => ("•".to_string(), DIMC),
                TaskStatus::Running => (SPIN[(tick as usize) % SPIN.len()].to_string(), Color::Yellow),
                TaskStatus::Done => ("✓".to_string(), Color::Green),
                TaskStatus::Failed => ("✗".to_string(), Color::Red),
            };
            let (title, tstyle) = if s.title.is_empty() { (truncate(&s.url, 30), Style::default().fg(DIMC)) } else { (truncate(&s.title, 30), Style::default().fg(Color::White)) };
            let progress: Line = match s.status {
                TaskStatus::Failed => Line::from(Span::styled(truncate(&s.error, 30), Style::default().fg(Color::Red))),
                TaskStatus::Queued => Line::from(Span::styled("queued", Style::default().fg(DIMC))),
                _ => if s.mode == "text" { Line::from(Span::styled(format!("{} pages", s.total.max(s.done)), Style::default().fg(Color::Cyan))) }
                     else if s.output == "csv" { Line::from(Span::styled(format!("{} links → csv", s.done), Style::default().fg(Color::Cyan))) }
                     else { bar_line(s.done, s.total) },
            };
            let time = s.elapsed.map(fmt_dur).unwrap_or_else(|| "—".to_string());
            let row = Row::new(vec![
                Cell::from(Span::styled(icon, Style::default().fg(color).add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled(truncate(&s.rule, 14), Style::default().fg(Color::Rgb(140, 170, 255)))),
                Cell::from(Span::styled(title, tstyle)),
                Cell::from(progress),
                Cell::from(Span::styled(time, Style::default().fg(DIMC))),
            ]);
            if i == selected { row.style(Style::default().bg(SEL_BG).add_modifier(Modifier::BOLD)) } else { row }
        }).collect();
        let widths = [Constraint::Length(2), Constraint::Length(14), Constraint::Length(32), Constraint::Min(18), Constraint::Length(7)];
        let table = Table::new(rows, widths)
            .column_spacing(1)
            .header(Row::new(vec![Cell::from(""), Cell::from("RULE"), Cell::from("TITLE"), Cell::from("PROGRESS"), Cell::from("TIME")]).style(Style::default().fg(DIMC).add_modifier(Modifier::BOLD)).bottom_margin(1))
            .block(Block::bordered().border_type(BorderType::Rounded).border_style(border).title(title_blk));
        f.render_widget(table, chunks[1]);
    }

    // --- Log pane ---
    let logw = tui_logger::TuiLoggerWidget::default()
        .block(Block::bordered().border_type(BorderType::Rounded).border_style(border).title(Line::from(Span::styled(" Logs ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))))
        .output_separator(' ')
        .output_timestamp(Some("%H:%M:%S".to_string()))
        .output_level(Some(tui_logger::TuiLoggerLevelOutput::Abbreviated))
        .output_target(false)
        .output_file(false)
        .output_line(false)
        .style_error(Style::default().fg(Color::Red))
        .style_warn(Style::default().fg(Color::Yellow))
        .style_info(Style::default().fg(Color::Green));
    f.render_widget(logw, chunks[2]);

    // --- Footer key hints ---
    let key = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    let lbl = Style::default().fg(DIMC);
    let footer = Line::from(vec![
        Span::styled(" q", key), Span::styled(" quit    ", lbl),
        Span::styled("↑↓", key), Span::styled(" select    ", lbl),
        Span::styled("⏎", key), Span::styled(" open dir    ", lbl),
        Span::styled("c", key), Span::styled(" clear finished", lbl),
    ]);
    f.render_widget(Paragraph::new(footer), chunks[3]);
}

// ---- Utilities ----
fn log(tag: &str, msg: &str) { if tag.contains("err") || tag.contains("fail") { warn!("{} {}", tag, msg); } else { info!("{} {}", tag, msg); } }
fn fmt_dur(d: Duration) -> String { let s = d.as_secs(); format!("{:02}:{:02}", s / 60, s % 60) }
fn truncate(s: &str, n: usize) -> String { if s.chars().count() <= n { s.to_string() } else { let t: String = s.chars().take(n.saturating_sub(1)).collect(); format!("{}…", t) } }
fn bar_line(done: usize, total: usize) -> ratatui::text::Line<'static> {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
    const W: usize = 12;
    let ratio = if total == 0 { 0.0 } else { (done as f64 / total as f64).min(1.0) };
    let filled = ((ratio * W as f64).round() as usize).min(W);
    let color = if ratio >= 0.999 { Color::Green } else if ratio >= 0.5 { Color::Rgb(120, 200, 120) } else { Color::Rgb(230, 200, 80) };
    Line::from(vec![
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled("░".repeat(W - filled), Style::default().fg(Color::Rgb(60, 60, 60))),
        Span::styled(format!(" {}/{}", done, total), Style::default().fg(Color::Gray)),
    ])
}
fn now_secs() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() }
fn is_url(s: &str) -> bool { let s = s.trim(); (s.starts_with("http://") || s.starts_with("https://")) && s.contains('.') }
fn find_matching_rule<'a>(rules: &'a [ScraperRule], url: &str) -> Option<&'a ScraperRule> { let parsed = Url::parse(url).ok()?; let host = parsed.host_str()?.to_lowercase(); for rule in rules { if rule.domain == "*" { continue; } let d = rule.domain.to_lowercase(); if host == d || host.ends_with(&format!(".{}", d)) { if let Some(ref pc) = rule.path_contains { if !url.to_lowercase().contains(&pc.to_lowercase()) { continue; } } return Some(rule); } } rules.iter().find(|r| r.domain == "*") }
fn resolve_url(u: &str, base: &str) -> String { if u.starts_with("http") { return u.to_string(); } if u.starts_with("//") { return format!("https:{}", u); } Url::parse(base).ok().and_then(|b| b.join(u).ok()).map(|x| x.to_string()).unwrap_or_default() }
fn filename_from_url(url: &str, idx: usize) -> String { let path = url.split('?').next().unwrap_or(url); let name = path.rsplit('/').next().unwrap_or(""); let ext = if let Some(pos) = name.rfind('.') { &name[pos..] } else { ".jpg" }; if !name.is_empty() && name.contains('.') { let clean: String = name[..name.rfind('.').unwrap_or(0)].chars().map(|c| if c.is_alphanumeric()||c=='-'||c=='_' {c} else {'_'}).collect(); if clean.len()>1 { return format!("{:04}_{}{}", idx, clean, ext); } } format!("{:04}{}", idx, ext) }
fn sanitize_filename(name: &str) -> String { let s: String = name.chars().map(|c| { let b = c as u32; if c.is_control()||b==0||b==47||b==58||b==42||b==63||b==34||b==60||b==62||b==124||b==92 {'_'} else {c} }).collect(); let s = s.trim().trim_matches('.'); if s.is_empty() { "untitled".to_string() } else { s.chars().take(100).collect() } }
fn expand_path(p: &str) -> PathBuf { if p.starts_with("~/") || p == "~" { if let Some(h) = dirs::home_dir() { return h.join(p.strip_prefix("~/").unwrap_or("")); } } PathBuf::from(p) }
fn open_dir(p: &Path) {
    let path = p.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    { let _ = std::process::Command::new("explorer").arg(&path).spawn(); }
    #[cfg(target_os = "macos")]
    { let _ = std::process::Command::new("open").arg(&path).spawn(); }
    #[cfg(all(unix, not(target_os = "macos")))]
    { let _ = std::process::Command::new("xdg-open").arg(&path).spawn(); }
}
fn parse_srcset_best(s: &str) -> String { let mut best_url = ""; let mut best_w = 0u64; for part in s.split(',') { let mut segs = part.trim().split_whitespace(); let url = segs.next().unwrap_or(""); let desc = segs.next().unwrap_or(""); let w = if let Some(d) = desc.strip_suffix('w') { d.parse::<u64>().unwrap_or(0) } else if let Some(d) = desc.strip_suffix('x') { (d.parse::<f64>().unwrap_or(1.0)*1000.0) as u64 } else { 0 }; if w >= best_w { best_w = w; best_url = url; } } if best_url.is_empty() { s.split(',').last().map(|x| x.trim().split_whitespace().next().unwrap_or("").to_string()).unwrap_or_default() } else { best_url.to_string() } }
fn extract_title(doc: &Html) -> String { let sel = Selector::parse("title").unwrap(); doc.select(&sel).next().map(|e| e.text().collect::<String>().trim().to_string()).unwrap_or_else(|| "untitled".to_string()) }
static DIR_LOCK: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
// Once the directory exists on disk, its own presence guards the name, so drop the in-memory reservation.
fn release_dir_lock(path: &Path) { if let Ok(mut l) = DIR_LOCK.lock() { l.remove(path); } }
fn reserve_dir_name(title: &str, naming: &str, collision: &str, base: &Path) -> String { let mut locked = DIR_LOCK.lock().unwrap(); let bn = match naming { "title_timestamp" => format!("{}_{:010}", title, now_secs()), _ => title.to_string() }; let candidate = |n: Option<usize>| -> String { match collision { "timestamp" => match n { Some(n) => format!("{}_{:010}_{}", bn, now_secs(), n), None => format!("{}_{:010}", bn, now_secs()) }, "merge" => bn.clone(), _ => match n { Some(n) => format!("{}_{}", bn, n), None => bn.clone() } } }; let mut name = candidate(None); let mut path = base.join(&name); if !path.exists() && !locked.contains(&path) { locked.insert(path.clone()); return name; } if collision == "merge" { return name; } for n in 2..10000 { name = candidate(Some(n)); path = base.join(&name); if !path.exists() && !locked.contains(&path) { locked.insert(path.clone()); return name; } } name = format!("{}_{:010}", bn, now_secs()); locked.insert(base.join(&name)); name }

// ---- Main ----
fn main() {
    let app_dir = get_app_dir(); ensure_configs(&app_dir); let settings = load_settings(&app_dir); let rules = load_rules(&app_dir); let db = init_db(&app_dir);
    tui_logger::init_logger(log::LevelFilter::Info).ok();
    tui_logger::set_default_level(log::LevelFilter::Info);
    log("start", &format!("Snatch v0.3.0 — {} rules", rules.len()));
    log("config", &app_dir.display().to_string());
    let client = Client::builder().emulation(Emulation::Chrome136).redirect(wreq::redirect::Policy::limited(10)).cookie_store(true).build().expect("client");
    let cancel = CancellationToken::new();
    let state = Arc::new(AppState { settings, rules, db: Mutex::new(db), client, processing: Mutex::new(HashSet::new()), cancel: cancel.clone(), tasks: Mutex::new(Vec::new()), task_sem: Arc::new(tokio::sync::Semaphore::new(MAX_TASKS)), started: Instant::now() });
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio");
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let sc = state.clone(); rt.spawn(async move { clipboard_dispatcher(sc, rx).await; });

    // The OS clipboard listener owns a non-Send window handle, so build and run it entirely on
    // its own thread; ship the (Send) shutdown handle back so the TUI can stop it on exit.
    let poll_ms = state.settings.advanced.clipboard_poll_ms;
    let (sd_tx, sd_rx) = std::sync::mpsc::channel::<clipboard_master::Shutdown>();
    let clip_thread = std::thread::spawn(move || {
        let mut master = match clipboard_master::Master::new(ClipHandler { tx, poll_ms }) {
            Ok(m) => m,
            Err(e) => { log("[clip-err]", &e.to_string()); return; }
        };
        let _ = sd_tx.send(master.shutdown_channel());
        let _ = master.run();
    });
    let shutdown = sd_rx.recv().ok();
    log("[watch]", "clipboard listener started");

    let _ = run_tui(state.clone());
    cancel.cancel();
    if let Some(sd) = shutdown { sd.signal(); }
    let _ = clip_thread.join();
    rt.shutdown_timeout(Duration::from_millis(200));
}
