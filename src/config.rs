// Settings + scraper-rule definitions, their on-disk defaults, and loaders.
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize)] pub struct Settings { pub general: GeneralSettings, pub download: DownloadSettings, pub advanced: AdvancedSettings }
#[derive(Deserialize)] pub struct GeneralSettings { pub download_dir: String, pub dir_naming: String, pub dir_collision: String }
#[derive(Deserialize)] pub struct DownloadSettings { pub max_concurrent: usize, pub timeout: u64, pub retries: u32 }
#[derive(Deserialize)] pub struct AdvancedSettings { pub clipboard_poll_ms: u64 }

#[derive(Deserialize, Clone)] struct ScraperConfig { rules: Vec<ScraperRule> }
#[derive(Deserialize, Clone)] pub struct ScraperRule { pub name: String, pub domain: String, pub container: Option<String>, #[serde(default)] pub selectors: Vec<SelectorDef>, #[serde(default)] pub pagination: Option<PaginationConfig>, #[serde(default)] pub follow_detail: Option<FollowDetailConfig>, #[serde(default)] pub exclude: Vec<String>, #[serde(default)] pub path_contains: Option<String>, #[serde(default)] pub delay_ms: Option<u64>, #[serde(default)] pub strip: Vec<String>, #[serde(default = "default_mode")] pub mode: String, #[serde(default)] pub content_selector: Option<String>, #[serde(default)] pub convert: Option<String>, #[serde(default)] pub output: Option<String> }
fn default_mode() -> String { "image".to_string() }
// How to persist a rule's results; defaults preserve the original behaviour (image -> files, text -> txt).
pub fn effective_output(rule: &ScraperRule) -> &str { rule.output.as_deref().unwrap_or(if rule.mode == "text" { "txt" } else { "files" }) }
#[derive(Deserialize, Clone)] pub struct PaginationConfig { #[serde(rename = "type")] pub pagination_type: String, pub param: Option<String>, pub start: Option<usize>, pub end: Option<usize>, pub next_selector: Option<String>, pub max_pages: Option<usize> }
#[derive(Deserialize, Clone)] pub struct FollowDetailConfig { pub link_selector: String, pub container: Option<String>, #[serde(default)] pub selectors: Vec<SelectorDef> }
#[derive(Deserialize, Clone)] pub struct SelectorDef { pub expression: String, pub attribute: String }

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

pub fn get_app_dir() -> PathBuf { std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| PathBuf::from(".")) }
pub fn ensure_configs(app_dir: &Path) { std::fs::create_dir_all(app_dir).ok(); let sp = app_dir.join("settings.toml"); if !sp.exists() { std::fs::write(&sp, DEFAULT_SETTINGS).expect("create settings"); } let rp = app_dir.join("scraper.toml"); if !rp.exists() { std::fs::write(&rp, DEFAULT_SCRAPER).expect("create scraper"); } }
pub fn load_settings(app_dir: &Path) -> Settings { let p = app_dir.join("settings.toml"); let c = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {}", p.display(), e)); toml::from_str(&c).unwrap_or_else(|e| panic!("parse {}: {}", p.display(), e)) }
pub fn load_rules(app_dir: &Path) -> Vec<ScraperRule> { let p = app_dir.join("scraper.toml"); let c = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {}", p.display(), e)); let cfg: ScraperConfig = toml::from_str(&c).unwrap_or_else(|e| panic!("parse {}: {}", p.display(), e)); cfg.rules }
