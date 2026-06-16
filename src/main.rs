// Snatch v0.3.0 - Clipboard image/novel crawler with a ratatui dashboard.
// Entry point: load config, wire up shared state, start the clipboard listener and the TUI.
#![allow(unused_must_use)]

mod clipboard;
mod config;
mod crawler;
mod db;
mod download;
mod state;
mod tui;
mod util;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use wreq::Client;
use wreq_util::Emulation;

use crate::clipboard::{clipboard_dispatcher, retry_dispatcher, ClipHandler};
use crate::config::{ensure_configs, get_app_dir, load_settings, load_sources, Source};
use crate::crawler::Extracted;
use crate::db::{init_db, is_downloaded, record_download};
use crate::state::{AppState, Task, TaskData};
use crate::tui::run_tui;
use crate::util::{find_matching_source, log, log_snapshot};

// How many crawls may run at once; extra clipboard hits sit in the Queued state.
const MAX_TASKS: usize = 3;

fn main() {
    // Headless subcommands run one URL and exit (no TUI). All share the same fetch path/TLS as the
    // live app, so what you see matches a normal clipboard-triggered run:
    //   fetch <URL> — print the raw HTML the program fetches (to write selectors against).
    //   test  <URL> — fetch + extract via a rule, print result, save NOTHING. Self-test rules.
    //   run   <URL> — full real run: download + save files + record to DB, then exit. Keep data.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        Some("fetch") | Some("--fetch") | Some("html") => std::process::exit(run_fetch(&argv[1..])),
        Some("test") | Some("--test") => std::process::exit(run_test(&argv[1..])),
        Some("run") | Some("--run") | Some("get") => std::process::exit(run_once(&argv[1..])),
        _ => {}
    }

    let app_dir = get_app_dir(); ensure_configs(&app_dir); let settings = load_settings(&app_dir); let sources = load_sources(&app_dir); let db = init_db(&app_dir);
    log("start", &format!("Snatch v0.3.0 — {} sources", sources.len()));
    log("config", &app_dir.display().to_string());
    let client = Client::builder().emulation(Emulation::Chrome136).redirect(wreq::redirect::Policy::limited(10)).cookie_store(true).build().expect("client");
    let cancel = CancellationToken::new();
    let (retry_tx, retry_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let state = Arc::new(AppState { settings, sources, db: Mutex::new(db), client, processing: Mutex::new(HashSet::new()), cancel: cancel.clone(), tasks: Mutex::new(Vec::new()), task_sem: Arc::new(tokio::sync::Semaphore::new(MAX_TASKS)), retry_tx, started: Instant::now() });
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio");
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let sc = state.clone(); rt.spawn(async move { clipboard_dispatcher(sc, rx).await; });
    let rc = state.clone(); rt.spawn(async move { retry_dispatcher(rc, retry_rx).await; });

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

// Builds a client identical to the live one so the test path shares the TLS fingerprint.
fn test_client() -> Client {
    Client::builder().emulation(Emulation::Chrome136).redirect(wreq::redirect::Policy::limited(10)).cookie_store(true).build().expect("client")
}

// Picks the source for a headless run: an explicit `--source <file>` else auto-match against
// sources/ (the same domains/match rule the live app uses). Prints why it failed and returns None.
// `snatch fetch <URL> [--strip] [--limit N]` — prints the raw HTML the program fetches (same TLS),
// so selectors can be written against the exact bytes the extractor will see. Step ① of authoring.
fn run_fetch(args: &[String]) -> i32 {
    let (mut url, mut strip, mut limit) = (None::<String>, false, 0usize);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--strip" => { strip = true; i += 1; }
            "--limit" | "-n" => { limit = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(0); i += 2; }
            a if !a.starts_with('-') && url.is_none() => { url = Some(a.to_string()); i += 1; }
            _ => i += 1,
        }
    }
    let url = match url { Some(u) => u, None => { eprintln!("usage: snatch fetch <URL> [--strip] [--limit N]"); return 2; } };

    let client = test_client();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio");
    match rt.block_on(crawler::fetch_html(&client, &url, &[])) {
        Some(html) => {
            let html = if strip { crawler::strip_noise(&html) } else { html };
            let total = html.chars().count();
            let shown = if limit > 0 { html.chars().take(limit).collect::<String>() } else { html };
            print!("{shown}");
            if limit > 0 && total > limit { eprintln!("\n[fetch] (truncated: showed {limit}/{total} chars; raise --limit for more)"); }
            else { eprintln!("\n[fetch] OK: {total} chars"); }
            0
        }
        None => {
            for (warn, line) in log_snapshot() { eprintln!("{} {}", if warn { "!" } else { " " }, line); }
            eprintln!("[fetch] FAIL: could not fetch {url} (see [page-err] above — maybe blocked / needs headers)");
            1
        }
    }
}

// Picks the source for a headless run: an explicit `--source <file>` else auto-match against
// sources/ (the same domains/match rule the live app uses). Prints why it failed and returns None.
fn pick_source(url: &str, source_path: &Option<String>, app_dir: &std::path::Path) -> Option<Source> {
    match source_path {
        Some(p) => match std::fs::read_to_string(p).ok().and_then(|c| toml::from_str::<Source>(&c).map_err(|e| eprintln!("parse {}: {}", p, e)).ok()) {
            Some(s) => Some(s),
            None => { eprintln!("could not load source file: {}", p); None }
        },
        None => match find_matching_source(&load_sources(app_dir), url).cloned() {
            Some(s) => Some(s),
            None => { println!("NO SOURCE matches {url}\n      check `domains` (host = domain or ends with .domain) and `match`."); None }
        },
    }
}

// `snatch run <URL> [--source <file.toml>] [--force]` — full one-shot run that downloads + saves
// + records to the DB, then exits. Honors the dedup ledger unless --force.
fn run_once(args: &[String]) -> i32 {
    let (mut url, mut source_path, mut force) = (None::<String>, None::<String>, false);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source" | "-s" => { source_path = args.get(i + 1).cloned(); i += 2; }
            "--force" | "-f" => { force = true; i += 1; }
            a if !a.starts_with('-') && url.is_none() => { url = Some(a.to_string()); i += 1; }
            _ => i += 1,
        }
    }
    let url = match url { Some(u) => u, None => { eprintln!("usage: snatch run <URL> [--source <file.toml>] [--force]"); return 2; } };

    let app_dir = get_app_dir();
    ensure_configs(&app_dir);
    let settings = load_settings(&app_dir);
    let source = match pick_source(&url, &source_path, &app_dir) { Some(s) => s, None => return 1 };
    let db = init_db(&app_dir);
    if !force && is_downloaded(&db, &url) {
        println!("[run] SKIP: already downloaded (use --force to re-fetch): {url}");
        return 0;
    }

    println!("[run] url    = {url}");
    println!("[run] source = {} (type={}, output={})", source.name, source.kind, source.output());
    println!("[run] downloading with wreq Chrome136 TLS emulation ...\n");

    let client = test_client();
    let task: Task = std::sync::Arc::new(Mutex::new(TaskData::new(&url, &source.name, &source.kind, source.output())));
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio");
    let result = rt.block_on(crawler::crawl(&url, &source, &settings, &client, &task));

    for (warn, line) in log_snapshot() { println!("{} {}", if warn { "!" } else { " " }, line); }
    println!();

    match result {
        Ok(r) => {
            record_download(&db, &url, &r.title, &source.name, r.count, &r.download_dir);
            println!("[run] OK: {} — {} items", r.title, r.count);
            println!("[run] saved to: {}", r.download_dir);
            0
        }
        Err(e) => { println!("[run] FAIL: {e}"); 1 }
    }
}

// `snatch test <URL> [--source <file.toml>] [--limit N]` — returns a process exit code.
fn run_test(args: &[String]) -> i32 {
    let (mut url, mut source_path, mut limit) = (None::<String>, None::<String>, 10usize);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source" | "-s" => { source_path = args.get(i + 1).cloned(); i += 2; }
            "--limit" | "-n" => { limit = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(limit); i += 2; }
            a if !a.starts_with('-') && url.is_none() => { url = Some(a.to_string()); i += 1; }
            _ => i += 1,
        }
    }
    let url = match url { Some(u) => u, None => { eprintln!("usage: snatch test <URL> [--source <file.toml>] [--limit N]"); return 2; } };

    // Pick the source: an explicit --source file, else auto-match against sources/ (live rule).
    let app_dir = get_app_dir();
    let source = match pick_source(&url, &source_path, &app_dir) { Some(s) => s, None => return 1 };

    println!("========================= 规则测试 / RULE TEST =========================");
    println!("  ⚠ 测试模式:独立进程,不写数据库、不进任务列表、退出即清空,不保存任何内容。");
    println!("  ⚠ TEST MODE: separate process — nothing is written to the DB or saved anywhere.");
    println!("------------------------------------------------------------------------");
    println!("[test] url    = {url}");
    println!("[test] source = {} (type={}, output={})", source.name, source.kind, source.output());
    if source_path.is_none() { println!("[test] matched via domains/match (live selection rule)"); }
    println!("[test] fetching with wreq Chrome136 TLS emulation ...\n");

    let client = test_client();
    let task: Task = std::sync::Arc::new(Mutex::new(TaskData::new(&url, &source.name, &source.kind, source.output())));
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio");
    let result = rt.block_on(crawler::extract(&url, &source, &client, &task));

    // Per-page diagnostics captured during the run ([page]/[page-err]/[json-err]/...).
    for (warn, line) in log_snapshot() { println!("{} {}", if warn { "!" } else { " " }, line); }
    println!();

    let code = match result {
        Ok((title, extracted)) => { print_extracted(&title, &extracted, limit); 0 }
        Err(e) => { println!("[test] FAIL: {e}"); println!("      matched the source but extracted nothing — selectors likely wrong (or page blocked above)."); 1 }
    };
    println!("\n===================== 测试结束 / END TEST(未保存)=====================");
    code
}

// Human/AI-readable summary of what an extractor produced.
fn print_extracted(title: &str, ex: &Extracted, limit: usize) {
    println!("[test] title  = {title}");
    match ex {
        Extracted::Images(urls) => {
            println!("[test] OK: {} image URL(s)\n", urls.len());
            for (i, u) in urls.iter().take(limit).enumerate() { println!("  {:>3}. {}", i + 1, u); }
            if urls.len() > limit { println!("  ... (+{} more)", urls.len() - limit); }
        }
        Extracted::Data { headers, rows } => {
            println!("[test] OK: {} row(s) x {} col(s)\n", rows.len(), headers.len());
            println!("  #    {}", headers.join(" | "));
            for (i, r) in rows.iter().take(limit).enumerate() { println!("  {:>3}. {}", i + 1, r.join(" | ")); }
            if rows.len() > limit { println!("  ... (+{} more)", rows.len() - limit); }
        }
        Extracted::Text(s) => {
            println!("[test] OK: {} chars of text\n", s.chars().count());
            let preview: String = s.chars().take(800).collect();
            println!("---- preview ----\n{preview}\n---- /preview ----");
        }
    }
}
