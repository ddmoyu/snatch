// Page fetching and extraction: pagination, image/text extraction, detail-page following.
use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Duration;

use scraper::{Html, Selector};
use wreq::Client;
use zhconv::zhconv;

use crate::config::{effective_output, ScraperRule, SelectorDef, Settings};
use crate::download::download_all;
use crate::state::Task;
use crate::util::{extract_title, log, parse_srcset_best, resolve_url, sanitize_filename, BoxError};

pub struct CrawlResult { pub title: String, pub image_count: usize, pub download_dir: String }

static RE_STYLE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?s)<style[^>]*>.*?</style>").unwrap());
static RE_SCRIPT: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?s)<script[^>]*>.*?</script>").unwrap());
static RE_PATH_PAGE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"/\d+/$").unwrap());

pub async fn crawl(url: &str, rule: &ScraperRule, settings: &Settings, client: &Client, task: &Task) -> Result<CrawlResult, BoxError> {
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
