// Global settings + per-site "source" definitions (one TOML file per source under sources/).
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

// ---- Global settings (settings.toml) ----
// settings.toml is OPTIONAL. When it's absent (or a field is omitted) these defaults apply, and an
// empty `download_dir` is resolved to `<app_dir>/data` in load_settings.
fn default_naming() -> String { "title".to_string() }
fn default_collision() -> String { "number".to_string() }
fn default_timeout() -> u64 { 30 }
fn default_retries() -> u32 { 3 }
fn default_poll() -> u64 { 2000 }

#[derive(Deserialize, Default)] pub struct Settings {
    #[serde(default)] pub general: GeneralSettings,
    #[serde(default)] pub download: DownloadSettings,
    #[serde(default)] pub advanced: AdvancedSettings,
}
#[derive(Deserialize)] pub struct GeneralSettings {
    #[serde(default)] pub download_dir: String, // empty -> <app_dir>/data (resolved in load_settings)
    #[serde(default = "default_naming")] pub dir_naming: String,
    #[serde(default = "default_collision")] pub dir_collision: String,
}
impl Default for GeneralSettings { fn default() -> Self { Self { download_dir: String::new(), dir_naming: default_naming(), dir_collision: default_collision() } } }
#[derive(Deserialize)] pub struct DownloadSettings {
    #[serde(default)] pub max_concurrent: usize,
    #[serde(default = "default_timeout")] pub timeout: u64,
    #[serde(default = "default_retries")] pub retries: u32,
}
impl Default for DownloadSettings { fn default() -> Self { Self { max_concurrent: 0, timeout: default_timeout(), retries: default_retries() } } }
#[derive(Deserialize)] pub struct AdvancedSettings {
    #[serde(default = "default_poll")] pub clipboard_poll_ms: u64,
}
impl Default for AdvancedSettings { fn default() -> Self { Self { clipboard_poll_ms: default_poll() } } }

// ---- Source model ----
fn default_true() -> bool { true }
fn default_get() -> String { "text".to_string() }

// One field extraction: locate (selector) -> get (text/@attr) -> regex purify.
#[derive(Deserialize, Clone)] pub struct Field {
    pub selector: String,
    #[serde(default = "default_get")] pub get: String,
    #[serde(default)] pub regex: Option<String>,
    #[serde(default)] pub replace: Option<String>,
    #[serde(default)] pub engine: Option<String>, // css (default) | xpath
    #[serde(default)] pub js: Option<String>,     // JS post-process; `result`/`baseUrl` in scope
}

#[derive(Deserialize, Clone)] pub struct Column {
    pub name: String,
    #[serde(default)] pub selector: Option<String>, // relative to the row element; None = the row itself
    #[serde(default = "default_get")] pub get: String,
    #[serde(default)] pub regex: Option<String>,
    #[serde(default)] pub replace: Option<String>,
    #[serde(default)] pub js: Option<String>,
}

#[derive(Deserialize, Clone)] pub struct Pagination {
    #[serde(rename = "type")] pub kind: String, // query | path | next_link
    pub param: Option<String>,
    pub start: Option<usize>,
    pub end: Option<usize>,
    pub next: Option<String>,
    pub max: Option<usize>,
}

#[derive(Deserialize, Clone)] pub struct DataRules {
    #[serde(default)] pub container: Option<String>,
    pub row: String,
    pub columns: Vec<Column>,
}

#[derive(Deserialize, Clone)] pub struct Sections {
    pub each: String,
    #[serde(default)] pub title: Option<String>,
    #[serde(default)] pub date: Option<String>,
    pub content: String,
    #[serde(default)] pub get: Option<String>,
}
#[derive(Deserialize, Clone)] pub struct Chapters {
    pub links: String,
    #[serde(default)] pub title: Option<String>,
    pub content: String,
    #[serde(default)] pub get: Option<String>,
}
#[derive(Deserialize, Clone)] pub struct TextRules {
    #[serde(default)] pub title: Option<String>,
    #[serde(default)] pub author: Option<String>,
    #[serde(default)] pub date: Option<String>,
    #[serde(default)] pub content: Option<String>,
    #[serde(default)] pub get: Option<String>,
    #[serde(default)] pub sections: Option<Sections>,
    #[serde(default)] pub chapters: Option<Chapters>,
    #[serde(default)] pub convert: Option<String>,
    #[serde(default)] pub strip: Vec<String>,
    #[serde(default)] pub js: Option<String>, // JS post-process on each content body
}

#[derive(Deserialize, Clone)] pub struct ImageDetail {
    pub link: String,
    #[serde(default)] pub container: Option<String>,
    pub images: Vec<Field>,
    #[serde(default)] pub exclude: Vec<String>,
    #[serde(default)] pub combine: Option<String>,
}
#[derive(Deserialize, Clone)] pub struct ImageRules {
    #[serde(default)] pub container: Option<String>,
    pub images: Vec<Field>,
    #[serde(default)] pub exclude: Vec<String>,
    #[serde(default)] pub detail: Option<ImageDetail>,
    // How to combine the `images` selectors: "merge" (default, all + dedup) or "first" (first that hits).
    #[serde(default)] pub combine: Option<String>,
}

#[derive(Deserialize, Clone)] pub struct Source {
    pub name: String,
    #[serde(rename = "type")] pub kind: String, // data | text | image
    pub domains: Vec<String>,
    #[serde(rename = "match", default)] pub match_: Option<String>,
    #[serde(default = "default_true")] pub enabled: bool,
    #[serde(default)] pub format: Option<String>, // html (default) | json
    #[serde(default)] pub headers: HashMap<String, String>, // extra request headers (values may use ${ENV})

    #[serde(default)] pub output: Option<String>,
    #[serde(default)] pub delay_ms: Option<u64>,
    #[serde(default)] pub pagination: Option<Pagination>,
    #[serde(default)] pub data: Option<DataRules>,
    #[serde(default)] pub text: Option<TextRules>,
    #[serde(default)] pub image: Option<ImageRules>,
}
impl Source {
    // Effective output, defaulting by source type.
    pub fn output(&self) -> &str {
        match self.output.as_deref() {
            Some(o) => o,
            None => match self.kind.as_str() { "data" => "csv", "text" => "txt", _ => "files" },
        }
    }
}

pub fn get_app_dir() -> PathBuf { std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| PathBuf::from(".")) }

// Ensures the `sources/` rules directory exists (empty). No example rule is written, and
// settings.toml is intentionally NOT created — its absence means "save under <app_dir>/data"
// (see load_settings).
pub fn ensure_configs(app_dir: &Path) {
    std::fs::create_dir_all(app_dir).ok();
    std::fs::create_dir_all(app_dir.join("sources")).ok();
}

// Loads settings.toml if present; otherwise uses built-in defaults. Either way, an unset
// `download_dir` resolves to the program-sibling `data/` dir (created on demand at save time).
pub fn load_settings(app_dir: &Path) -> Settings {
    let p = app_dir.join("settings.toml");
    let mut s: Settings = match std::fs::read_to_string(&p) {
        Ok(c) => toml::from_str(&c).unwrap_or_else(|e| panic!("parse {}: {}", p.display(), e)),
        Err(_) => Settings::default(),
    };
    if s.general.download_dir.trim().is_empty() {
        s.general.download_dir = app_dir.join("data").to_string_lossy().into_owned();
    }
    s
}

// Loads every enabled `sources/*.toml`. A malformed source is logged and skipped, not fatal.
pub fn load_sources(app_dir: &Path) -> Vec<Source> {
    let dir = app_dir.join("sources");
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd.flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml")).collect(),
        Err(_) => return Vec::new(),
    };
    paths.sort();
    let mut sources = Vec::new();
    for path in paths {
        let content = match std::fs::read_to_string(&path) { Ok(c) => c, Err(_) => continue };
        match toml::from_str::<Source>(&content) {
            Ok(s) if s.enabled => sources.push(s),
            Ok(_) => {}
            Err(e) => crate::util::log("[source-err]", &format!("parse {}: {}", path.display(), e)),
        }
    }
    sources
}
