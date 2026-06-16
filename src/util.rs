// Small cross-cutting helpers: logging, URL/path utilities, HTML helpers.
use std::path::{Path, PathBuf};

use log::{info, warn};
use scraper::{Html, Selector};
use url::Url;

use crate::config::Source;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

// Routes tagged messages into the `log` facade (rendered by the TUI log pane).
pub fn log(tag: &str, msg: &str) { if tag.contains("err") || tag.contains("fail") { warn!("{} {}", tag, msg); } else { info!("{} {}", tag, msg); } }

pub fn now_secs() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() }
pub fn is_url(s: &str) -> bool { let s = s.trim(); (s.starts_with("http://") || s.starts_with("https://")) && s.contains('.') }
// First source whose domains match the URL host and whose optional `match` substring is present.
pub fn find_matching_source<'a>(sources: &'a [Source], url: &str) -> Option<&'a Source> {
    let host = Url::parse(url).ok()?.host_str()?.to_lowercase();
    let lurl = url.to_lowercase();
    sources.iter().find(|s| {
        s.domains.iter().any(|d| { let d = d.to_lowercase(); host == d || host.ends_with(&format!(".{}", d)) })
            && s.match_.as_ref().map_or(true, |m| lurl.contains(&m.to_lowercase()))
    })
}
pub fn resolve_url(u: &str, base: &str) -> String { if u.starts_with("http") { return u.to_string(); } if u.starts_with("//") { return format!("https:{}", u); } Url::parse(base).ok().and_then(|b| b.join(u).ok()).map(|x| x.to_string()).unwrap_or_default() }
pub fn filename_from_url(url: &str, idx: usize) -> String { let path = url.split('?').next().unwrap_or(url); let name = path.rsplit('/').next().unwrap_or(""); let ext = if let Some(pos) = name.rfind('.') { &name[pos..] } else { ".jpg" }; if !name.is_empty() && name.contains('.') { let clean: String = name[..name.rfind('.').unwrap_or(0)].chars().map(|c| if c.is_alphanumeric()||c=='-'||c=='_' {c} else {'_'}).collect(); if clean.len()>1 { return format!("{:04}_{}{}", idx, clean, ext); } } format!("{:04}{}", idx, ext) }
pub fn sanitize_filename(name: &str) -> String { let s: String = name.chars().map(|c| { let b = c as u32; if c.is_control()||b==0||b==47||b==58||b==42||b==63||b==34||b==60||b==62||b==124||b==92 {'_'} else {c} }).collect(); let s = s.trim().trim_matches('.'); if s.is_empty() { "untitled".to_string() } else { s.chars().take(100).collect() } }
// Expands a leading ~, building the path component-by-component so the result uses native
// separators (a literal "~/Desktop/Snatch" must not leave a forward slash on Windows).
pub fn expand_path(p: &str) -> PathBuf {
    let rest = if p == "~" { Some("") } else { p.strip_prefix("~/") };
    if let Some(rest) = rest {
        if let Some(h) = dirs::home_dir() {
            let mut path = h;
            for comp in rest.split('/') { if !comp.is_empty() { path.push(comp); } }
            return path;
        }
    }
    PathBuf::from(p)
}
pub fn open_dir(p: &Path) {
    #[cfg(target_os = "windows")]
    { let path = p.to_string_lossy().replace('/', "\\"); let _ = std::process::Command::new("explorer").arg(&path).spawn(); }
    #[cfg(target_os = "macos")]
    { let _ = std::process::Command::new("open").arg(p.as_os_str()).spawn(); }
    #[cfg(all(unix, not(target_os = "macos")))]
    { let _ = std::process::Command::new("xdg-open").arg(p.as_os_str()).spawn(); }
}
pub fn parse_srcset_best(s: &str) -> String { let mut best_url = ""; let mut best_w = 0u64; for part in s.split(',') { let mut segs = part.trim().split_whitespace(); let url = segs.next().unwrap_or(""); let desc = segs.next().unwrap_or(""); let w = if let Some(d) = desc.strip_suffix('w') { d.parse::<u64>().unwrap_or(0) } else if let Some(d) = desc.strip_suffix('x') { (d.parse::<f64>().unwrap_or(1.0)*1000.0) as u64 } else { 0 }; if w >= best_w { best_w = w; best_url = url; } } if best_url.is_empty() { s.split(',').last().map(|x| x.trim().split_whitespace().next().unwrap_or("").to_string()).unwrap_or_default() } else { best_url.to_string() } }
pub fn extract_title(doc: &Html) -> String { let sel = Selector::parse("title").unwrap(); doc.select(&sel).next().map(|e| e.text().collect::<String>().trim().to_string()).unwrap_or_else(|| "untitled".to_string()) }
