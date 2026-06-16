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
use crate::config::{ensure_configs, get_app_dir, load_settings, load_sources};
use crate::db::init_db;
use crate::state::AppState;
use crate::tui::run_tui;
use crate::util::log;

// How many crawls may run at once; extra clipboard hits sit in the Queued state.
const MAX_TASKS: usize = 3;

fn main() {
    let app_dir = get_app_dir(); ensure_configs(&app_dir); let settings = load_settings(&app_dir); let sources = load_sources(&app_dir); let db = init_db(&app_dir);
    tui_logger::init_logger(log::LevelFilter::Info).ok();
    tui_logger::set_default_level(log::LevelFilter::Info);
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
