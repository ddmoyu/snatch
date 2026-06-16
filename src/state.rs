// Shared application state and the task model that drives the dashboard.
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tokio_util::sync::CancellationToken;
use wreq::Client;

use crate::config::{Settings, Source};

pub struct AppState {
    pub settings: Settings,
    pub sources: Vec<Source>,
    pub db: Mutex<Connection>,
    pub client: Client,
    pub processing: Mutex<HashSet<String>>,
    pub cancel: CancellationToken,
    pub tasks: Mutex<Vec<Task>>,
    pub task_sem: Arc<tokio::sync::Semaphore>,
    pub retry_tx: tokio::sync::mpsc::UnboundedSender<String>,
    pub started: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus { Queued, Running, Done, Failed }

pub struct TaskData {
    pub url: String,
    pub source: String, // source name (shown in the SOURCE column)
    pub kind: String,   // data | text | image (drives progress rendering)
    pub output: String, // csv | txt | files
    pub title: String,
    pub status: TaskStatus,
    pub done: usize,
    pub total: usize,
    pub error: String,
    pub download_dir: String,
    pub started: Option<Instant>,
    pub finished: Option<Instant>,
}
impl TaskData {
    pub fn new(url: &str, source: &str, kind: &str, output: &str) -> Self {
        TaskData { url: url.to_string(), source: source.to_string(), kind: kind.to_string(), output: output.to_string(), title: String::new(), status: TaskStatus::Queued, done: 0, total: 0, error: String::new(), download_dir: String::new(), started: None, finished: None }
    }
    pub fn elapsed(&self) -> Option<Duration> {
        match (self.started, self.finished) {
            (Some(s), Some(f)) => Some(f.saturating_duration_since(s)),
            (Some(s), None) => Some(s.elapsed()),
            _ => None,
        }
    }
}
pub type Task = Arc<Mutex<TaskData>>;
