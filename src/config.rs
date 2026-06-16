// Global settings + per-site "source" definitions (one TOML file per source under sources/).
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

// ---- Global settings (settings.toml) ----
#[derive(Deserialize)] pub struct Settings { pub general: GeneralSettings, pub download: DownloadSettings, pub advanced: AdvancedSettings }
#[derive(Deserialize)] pub struct GeneralSettings { pub download_dir: String, pub dir_naming: String, pub dir_collision: String }
#[derive(Deserialize)] pub struct DownloadSettings { pub max_concurrent: usize, pub timeout: u64, pub retries: u32 }
#[derive(Deserialize)] pub struct AdvancedSettings { pub clipboard_poll_ms: u64 }

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
}

#[derive(Deserialize, Clone)] pub struct Column {
    pub name: String,
    #[serde(default)] pub selector: Option<String>, // relative to the row element; None = the row itself
    #[serde(default = "default_get")] pub get: String,
    #[serde(default)] pub regex: Option<String>,
    #[serde(default)] pub replace: Option<String>,
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

// ---- Defaults written on first run ----
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

const EXAMPLE_SOURCE: &str = r##"# 示例源:抓 dll-files 列表导出两列 CSV(name, url)
name = "DLL-Files"
type = "data"
domains = ["dll-files.com"]
match = "/a/"

[data]
row = "a[href$='.dll.html']"
[[data.columns]]
name = "name"
get = "text"
[[data.columns]]
name = "url"
get = "@href"
"##;

pub fn get_app_dir() -> PathBuf { std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| PathBuf::from(".")) }

pub fn ensure_configs(app_dir: &Path) {
    std::fs::create_dir_all(app_dir).ok();
    let sp = app_dir.join("settings.toml");
    if !sp.exists() { std::fs::write(&sp, DEFAULT_SETTINGS).ok(); }
    let sources = app_dir.join("sources");
    if !sources.exists() {
        std::fs::create_dir_all(&sources).ok();
        std::fs::write(sources.join("example.toml"), EXAMPLE_SOURCE).ok();
    }
}

pub fn load_settings(app_dir: &Path) -> Settings {
    let p = app_dir.join("settings.toml");
    let c = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {}", p.display(), e));
    toml::from_str(&c).unwrap_or_else(|e| panic!("parse {}: {}", p.display(), e))
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
