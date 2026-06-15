// Clipboard listening, URL gating/dispatch, and per-URL task execution.
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{effective_output, ScraperRule};
use crate::crawler::crawl;
use crate::db::{is_downloaded, record_download};
use crate::state::{AppState, Task, TaskData, TaskStatus};
use crate::util::{find_matching_rule, is_url, log};

// On each OS clipboard-change event, read the text and forward it to the async dispatcher.
// Push-based on Windows (AddClipboardFormatListener) and Linux/X11 (XFixes); on macOS
// clipboard-master polls the change counter internally at `sleep_interval`.
pub struct ClipHandler { pub tx: tokio::sync::mpsc::UnboundedSender<String>, pub poll_ms: u64 }
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
pub async fn clipboard_dispatcher(state: Arc<AppState>, mut rx: tokio::sync::mpsc::UnboundedReceiver<String>) {
    let mut last = String::new();
    while let Some(raw) = rx.recv().await {
        if state.cancel.is_cancelled() { return; }
        let content = raw.trim().to_string();
        if content == last || !is_url(&content) { continue; }
        last = content.clone();
        let rule = match find_matching_rule(&state.rules, &content) { Some(r) => r.clone(), None => continue };
        if is_downloaded(&state.db.lock().unwrap(), &content) { log("[skip]", "already downloaded"); continue; }
        { let mut proc = state.processing.lock().unwrap(); if proc.contains(&content) { continue; } proc.insert(content.clone()); }
        let task: Task = Arc::new(std::sync::Mutex::new(TaskData::new(&content, &rule.name, &rule.mode, effective_output(&rule))));
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
