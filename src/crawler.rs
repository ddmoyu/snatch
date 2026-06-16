// Fetch + extract per source type. Each extractor collects pages, pulls values through the
// unified pipeline (locate -> get -> regex), and hands an `Extracted` payload to `download::persist`.
use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use wreq::Client;
use zhconv::zhconv;

use crate::config::{Chapters, Column, DataRules, Field, ImageRules, Pagination, Source, TextRules};
use crate::state::Task;
use crate::util::{build_headers, extract_title, log, parse_srcset_best, resolve_url, sanitize_filename, BoxError};

pub struct CrawlResult { pub title: String, pub count: usize, pub download_dir: String }

// What an extractor produced; `download::persist` turns it into files.
pub enum Extracted {
    Data { headers: Vec<String>, rows: Vec<Vec<String>> },
    Text(String),
    Images(Vec<String>),
}

static RE_STYLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<style[^>]*>.*?</style>").unwrap());
static RE_SCRIPT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<script[^>]*>.*?</script>").unwrap());
static RE_PATH_PAGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/\d+/$").unwrap());

pub async fn crawl(url: &str, source: &Source, settings: &crate::config::Settings, client: &Client, task: &Task) -> Result<CrawlResult, BoxError> {
    log("[crawl]", &format!("{} ({})", url, source.kind));
    let (title, extracted) = match source.kind.as_str() {
        "data" => extract_data(url, source, client, task).await?,
        "text" => extract_text(url, source, client, task).await?,
        "image" => extract_image(url, source, client, task).await?,
        other => return Err(format!("unknown source type '{}'", other).into()),
    };
    crate::download::persist(title, extracted, source, settings, client, url, task).await
}

// ---- Shared fetch / pagination ----

async fn fetch(client: &Client, url: &str, headers: &[(String, String)]) -> Option<String> {
    let mut req = client.get(url).header("Accept-Language", "zh-CN,zh;q=0.9");
    // A same-origin Referer gets past common anti-hotlink / Cloudflare checks that 403 "direct" hits;
    // a source-configured Referer overrides it.
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("referer")) {
        if let Some(origin) = url::Url::parse(url).ok().and_then(|u| u.host_str().map(|h| format!("{}://{}/", u.scheme(), h))) {
            req = req.header("Referer", origin);
        }
    }
    for (k, v) in headers { req = req.header(k.as_str(), v.as_str()); }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() { log("[page-err]", &format!("HTTP {}: {}", resp.status(), url)); return None; }
    resp.text().await.ok()
}

async fn sleep(ms: u64) { tokio::time::sleep(Duration::from_millis(ms)).await; }

async fn collect_page_urls(start_url: &str, pg: Option<&Pagination>, client: &Client, headers: &[(String, String)]) -> Vec<String> {
    let mut urls = vec![start_url.to_string()];
    let pg = match pg { Some(p) => p, None => return urls };
    match pg.kind.as_str() {
        "query" => { if let (Some(param), Some(s), Some(e)) = (&pg.param, pg.start, pg.end) { urls.clear(); for page in s..=e { let sep = if start_url.contains('?') { "&" } else { "?" }; urls.push(format!("{}{}{}={}", start_url, sep, param, page)); } } }
        "path" => { if let (Some(s), Some(e)) = (pg.start, pg.end) { let base = RE_PATH_PAGE.replace(start_url, "/").to_string(); urls.clear(); for page in s..=e { urls.push(if page == 1 { base.clone() } else { format!("{}{}/", base, page) }); } } }
        "next_link" => {
            let max = pg.max.unwrap_or(10);
            let ns = pg.next.as_deref().unwrap_or("a.next");
            let mut cur = start_url.to_string();
            let mut seen: HashSet<String> = HashSet::new();
            seen.insert(cur.clone());
            for _ in 1..max {
                let html = match fetch(client, &cur, headers).await { Some(h) => h, None => break };
                let next = {
                    let doc = Html::parse_document(&html);
                    Selector::parse(ns).ok().and_then(|sel| doc.select(&sel).filter_map(|el| el.value().attr("href")).next().map(|h| resolve_url(h, &cur)))
                };
                match next {
                    Some(n) if !n.is_empty() && seen.insert(n.clone()) => { cur = n; urls.push(cur.clone()); }
                    _ => break,
                }
            }
        }
        _ => {}
    }
    urls
}

// ---- Unified extraction pipeline ----

// Splits a `get` spec into (key, is_attribute). `@href` -> ("href", true); `text` -> ("text", false).
fn parse_get(get: &str) -> (&str, bool) {
    match get.strip_prefix('@') { Some(a) => (a, true), None => (get, false) }
}

fn content_value(el: ElementRef, key: &str) -> Option<String> {
    match key {
        "text" => Some(el.text().collect::<String>()),
        "ownText" => Some(el.children().filter_map(|n| n.value().as_text().map(|t| t.to_string())).collect()),
        "html" | "innerHtml" => Some(el.inner_html()),
        "outerHtml" => Some(el.html()),
        _ => None,
    }
}

// Pulls one value from an element per `get`. Attribute keys are treated as URLs (skip data:/blob:,
// reduce srcset, resolve against base); content keys are returned trimmed.
fn extract_get(el: ElementRef, get: &str, base_url: &str) -> Option<String> {
    let (key, is_attr) = parse_get(get);
    if is_attr {
        let raw = el.value().attr(key)?;
        let t = raw.trim();
        if t.is_empty() || t.starts_with("data:") || t.starts_with("blob:") { return None; }
        let u = if key == "srcset" { parse_srcset_best(t) } else { t.to_string() };
        let full = resolve_url(&u, base_url);
        if full.is_empty() { None } else { Some(full) }
    } else {
        Some(content_value(el, key)?.trim().to_string())
    }
}

fn purify(value: String, regex: Option<&str>, replace: &str) -> String {
    match regex.and_then(|r| Regex::new(r).ok()) {
        Some(re) => re.replace_all(&value, replace).into_owned(),
        None => value,
    }
}

fn scoped(container: Option<&String>, expr: &str) -> String {
    match container { Some(c) => format!("{} {}", c, expr), None => expr.to_string() }
}

// Locates + extracts via XPath (skyscraper). The xpath selector is self-contained (CSS container/
// exclude don't apply). Attribute nodes and @attr gets are URL-resolved; element text is verbatim.
fn xpath_values(html: &str, selector: &str, get: &str, base_url: &str) -> Vec<String> {
    use skyscraper::xpath::grammar::data_model::XpathItem;
    use skyscraper::xpath::grammar::XpathItemTreeNode;
    let tree = match skyscraper::html::parse(html) { Ok(t) => t, Err(_) => return Vec::new() };
    let xp = match skyscraper::xpath::parse(selector) { Ok(x) => x, Err(_) => return Vec::new() };
    let items = match xp.apply(&tree) { Ok(i) => i, Err(_) => return Vec::new() };
    let (key, is_attr) = parse_get(get);
    let mut out = Vec::new();
    for item in items {
        let (raw, url_like) = match item {
            XpathItem::Node(node) => match node {
                XpathItemTreeNode::AttributeNode(a) => (Some(a.value.to_string()), true),
                XpathItemTreeNode::ElementNode(el) => {
                    if is_attr { (el.get_attribute(&tree, key).map(|s| s.to_string()), true) } else { (node.text(&tree), false) }
                }
                _ => (node.text(&tree), false),
            },
            _ => (None, false),
        };
        if let Some(v) = raw {
            let v = v.trim();
            if v.is_empty() { continue; }
            let val = if url_like {
                if v.starts_with("data:") || v.starts_with("blob:") { continue; }
                resolve_url(v, base_url)
            } else { v.to_string() };
            if !val.is_empty() { out.push(val); }
        }
    }
    out
}

// Runs a set of fields over a document (image/list extraction). CSS fields use the parsed `doc`;
// xpath fields (engine = "xpath") re-parse `html` with skyscraper. Each value is regex-purified.
fn extract_fields(doc: &Html, html: &str, fields: &[Field], container: Option<&String>, base_url: &str, exclude: &[String]) -> Vec<String> {
    let mut excluded = HashSet::new();
    for ex in exclude { if let Ok(s) = Selector::parse(ex) { for e in doc.select(&s) { excluded.insert(e.id()); } } }
    let mut out = Vec::new();
    for f in fields {
        if f.engine.as_deref() == Some("xpath") {
            for v in xpath_values(html, &f.selector, &f.get, base_url) {
                let v = purify(v, f.regex.as_deref(), f.replace.as_deref().unwrap_or(""));
                if !v.is_empty() { out.push(v); }
            }
            continue;
        }
        let sel = match Selector::parse(&scoped(container, &f.selector)) { Ok(s) => s, Err(_) => continue };
        for el in doc.select(&sel) {
            if excluded.contains(&el.id()) { continue; }
            if let Some(v) = extract_get(el, &f.get, base_url) {
                let v = purify(v, f.regex.as_deref(), f.replace.as_deref().unwrap_or(""));
                if !v.is_empty() { out.push(v); }
            }
        }
    }
    out
}

// Combines image selectors: "first" returns the first selector that yields any results (a fallback
// chain, e.g. @data-src then @src); anything else merges them all (the default).
fn extract_fields_combined(doc: &Html, html: &str, fields: &[Field], container: Option<&String>, base_url: &str, exclude: &[String], combine: Option<&str>) -> Vec<String> {
    if combine == Some("first") {
        for f in fields {
            let v = extract_fields(doc, html, std::slice::from_ref(f), container, base_url, exclude);
            if !v.is_empty() { return v; }
        }
        return Vec::new();
    }
    extract_fields(doc, html, fields, container, base_url, exclude)
}

// First match of a CSS selector in a document, as trimmed text.
fn doc_text(doc: &Html, selector: Option<&str>) -> Option<String> {
    let sel = Selector::parse(selector?).ok()?;
    let t = doc.select(&sel).next()?.text().collect::<String>().trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

// First match of a selector in a document, via `get`.
fn doc_get(doc: &Html, selector: &str, get: &str, base_url: &str) -> Option<String> {
    let sel = Selector::parse(selector).ok()?;
    extract_get(doc.select(&sel).next()?, get, base_url)
}

// First matching descendant of an element, as trimmed text.
fn el_text(el: ElementRef, selector: &str) -> Option<String> {
    let sel = Selector::parse(selector).ok()?;
    let t = el.select(&sel).next()?.text().collect::<String>().trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

// First matching descendant of an element, via `get`.
fn el_get(el: ElementRef, selector: &str, get: &str, base_url: &str) -> Option<String> {
    let sel = Selector::parse(selector).ok()?;
    extract_get(el.select(&sel).next()?, get, base_url)
}

fn preprocess_text_html(html: &str) -> String {
    let mut h = html.replace("</p><p>", "\n\n").replace("</p>\n<p>", "\n\n");
    h = h.replace("<br>", "\n").replace("<br/>", "\n").replace("<br />", "\n");
    h = RE_STYLE.replace_all(&h, "").to_string();
    h = RE_SCRIPT.replace_all(&h, "").to_string();
    h
}

// ---- data ----
async fn extract_data(url: &str, source: &Source, client: &Client, task: &Task) -> Result<(String, Extracted), BoxError> {
    let data: &DataRules = source.data.as_ref().ok_or("data source missing [data]")?;
    if source.format.as_deref() == Some("json") {
        return extract_data_json(url, source, data, client, task).await;
    }
    let delay = source.delay_ms.unwrap_or(300);
    let headers = build_headers(&source.headers);
    let pages = collect_page_urls(url, source.pagination.as_ref(), client, &headers).await;
    let col_names: Vec<String> = data.columns.iter().map(|c| c.name.clone()).collect();
    let mut rows = Vec::new();
    let mut title = String::new();
    for (i, page) in pages.iter().enumerate() {
        if i > 0 { sleep(delay).await; }
        let html = match fetch(client, page, &headers).await { Some(h) => h, None => continue };
        let doc = Html::parse_document(&html);
        if title.is_empty() { title = sanitize_filename(&extract_title(&doc)); }
        let row_sel = match Selector::parse(&scoped(data.container.as_ref(), &data.row)) { Ok(s) => s, Err(_) => continue };
        let mut page_rows = 0usize;
        for row_el in doc.select(&row_sel) {
            rows.push(data.columns.iter().map(|c| extract_column(row_el, c, page)).collect());
            page_rows += 1;
        }
        log("[page]", &format!("{}: {} bytes, {} rows", i + 1, html.len(), page_rows));
        task.lock().unwrap().total = rows.len();
    }
    if rows.is_empty() { return Err("nothing extracted".into()); }
    Ok((title, Extracted::Data { headers: col_names, rows }))
}

fn extract_column(row: ElementRef, col: &Column, base_url: &str) -> String {
    let raw = match &col.selector {
        Some(s) => el_get(row, s, &col.get, base_url),
        None => extract_get(row, &col.get, base_url),
    }
    .unwrap_or_default();
    purify(raw, col.regex.as_deref(), col.replace.as_deref().unwrap_or(""))
}

// JSON data source: parse the response as JSON and pull rows/columns via JSONPath.
async fn extract_data_json(url: &str, source: &Source, data: &DataRules, client: &Client, task: &Task) -> Result<(String, Extracted), BoxError> {
    use serde_json_path::JsonPath;
    let delay = source.delay_ms.unwrap_or(300);
    let headers = build_headers(&source.headers);
    let pages = collect_page_urls(url, source.pagination.as_ref(), client, &headers).await;
    let row_path = JsonPath::parse(&data.row).map_err(|e| format!("bad row JSONPath '{}': {}", data.row, e))?;
    let col_paths: Vec<_> = data.columns.iter().map(|c| (c, JsonPath::parse(&c.get))).collect();
    let col_names: Vec<String> = data.columns.iter().map(|c| c.name.clone()).collect();
    let mut rows: Vec<Vec<String>> = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        if i > 0 { sleep(delay).await; }
        let body = match fetch(client, page, &headers).await { Some(b) => b, None => continue };
        let v: serde_json::Value = match serde_json::from_str(&body) { Ok(v) => v, Err(e) => { log("[json-err]", &e.to_string()); continue; } };
        for rv in row_path.query(&v).all() {
            let cells = col_paths.iter().map(|(c, p)| {
                let raw = match p { Ok(p) => { let nl = p.query(rv); nl.first().map(json_scalar).unwrap_or_default() } Err(_) => String::new() };
                purify(raw, c.regex.as_deref(), c.replace.as_deref().unwrap_or(""))
            }).collect::<Vec<_>>();
            rows.push(cells);
        }
        log("[page]", &format!("{}: {} bytes, {} rows", i + 1, body.len(), rows.len()));
        task.lock().unwrap().total = rows.len();
    }
    if rows.is_empty() { return Err("nothing extracted".into()); }
    Ok((sanitize_filename(&source.name), Extracted::Data { headers: col_names, rows }))
}

// Renders a JSON scalar as a CSV cell; arrays/objects fall back to their JSON text.
fn json_scalar(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        _ => v.to_string(),
    }
}

// ---- image ----
async fn extract_image(url: &str, source: &Source, client: &Client, task: &Task) -> Result<(String, Extracted), BoxError> {
    let img: &ImageRules = source.image.as_ref().ok_or("image source missing [image]")?;
    let delay = source.delay_ms.unwrap_or(300);
    let headers = build_headers(&source.headers);
    let pages = collect_page_urls(url, source.pagination.as_ref(), client, &headers).await;
    let mut images = Vec::new();
    let mut title = String::new();
    for (i, page) in pages.iter().enumerate() {
        if i > 0 { sleep(delay).await; }
        let html = match fetch(client, page, &headers).await { Some(h) => h, None => continue };
        let (direct, detail_urls) = {
            let doc = Html::parse_document(&html);
            if title.is_empty() { title = sanitize_filename(&extract_title(&doc)); }
            match &img.detail {
                Some(d) => {
                    let mut det = Vec::new();
                    if let Ok(sel) = Selector::parse(&scoped(img.container.as_ref(), &d.link)) {
                        for el in doc.select(&sel) { if let Some(h) = el.value().attr("href") { let u = resolve_url(h, page); if !u.is_empty() { det.push(u); } } }
                    }
                    det.sort();
                    det.dedup();
                    (Vec::new(), det)
                }
                None => (extract_fields_combined(&doc, &html, &img.images, img.container.as_ref(), page, &img.exclude, img.combine.as_deref()), Vec::new()),
            }
        };
        images.extend(direct);
        if let Some(d) = &img.detail {
            let cont = d.container.as_ref().or(img.container.as_ref());
            for durl in &detail_urls {
                sleep(delay).await;
                if let Some(dhtml) = fetch(client, durl, &headers).await {
                    let doc = Html::parse_document(&dhtml);
                    images.extend(extract_fields_combined(&doc, &dhtml, &d.images, cont, durl, &d.exclude, d.combine.as_deref()));
                }
            }
        }
        log("[page]", &format!("{}: {} bytes, {} img", i + 1, html.len(), images.len()));
        task.lock().unwrap().total = images.len();
    }
    images.sort();
    images.dedup();
    if images.is_empty() { return Err("nothing extracted".into()); }
    Ok((title, Extracted::Images(images)))
}

// ---- text ----
async fn extract_text(url: &str, source: &Source, client: &Client, task: &Task) -> Result<(String, Extracted), BoxError> {
    let t: &TextRules = source.text.as_ref().ok_or("text source missing [text]")?;
    let delay = source.delay_ms.unwrap_or(300);
    let headers = build_headers(&source.headers);
    if let Some(ch) = &t.chapters {
        return extract_chapters(url, t, ch, client, task, delay, &headers).await;
    }
    let pages = collect_page_urls(url, source.pagination.as_ref(), client, &headers).await;
    let (mut title, mut author, mut date) = (String::new(), String::new(), String::new());
    let mut parts: Vec<String> = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        if i > 0 { sleep(delay).await; }
        let html = match fetch(client, page, &headers).await { Some(h) => h, None => continue };
        let html = preprocess_text_html(&html);
        let doc = Html::parse_document(&html);
        if title.is_empty() { title = sanitize_filename(&doc_text(&doc, t.title.as_deref()).unwrap_or_else(|| extract_title(&doc))); }
        if author.is_empty() { if let Some(a) = doc_text(&doc, t.author.as_deref()) { author = a; } }
        if date.is_empty() { if let Some(d) = doc_text(&doc, t.date.as_deref()) { date = d; } }
        if let Some(sec) = &t.sections {
            let get = sec.get.as_deref().unwrap_or("text");
            if let Ok(each) = Selector::parse(&sec.each) {
                for el in doc.select(&each) {
                    let body = el_get(el, &sec.content, get, page).unwrap_or_default();
                    if body.trim().is_empty() { continue; }
                    let st = sec.title.as_deref().and_then(|s| el_text(el, s));
                    let sd = sec.date.as_deref().and_then(|s| el_text(el, s));
                    parts.push(format_section(st, sd, body.trim()));
                }
            }
        } else if let Some(content) = &t.content {
            let get = t.get.as_deref().unwrap_or("text");
            if let Some(body) = doc_get(&doc, content, get, page) { if !body.trim().is_empty() { parts.push(body.trim().to_string()); } }
        }
        task.lock().unwrap().total = parts.len().max(1);
    }
    // single = one continuous article across pages (light join); sections = distinct blocks (divider).
    let sep = if t.sections.is_some() { "\n\n----------\n\n" } else { "\n\n" };
    finish_text(title, author, date, parts, t, sep)
}

async fn extract_chapters(url: &str, t: &TextRules, ch: &Chapters, client: &Client, task: &Task, delay: u64, headers: &[(String, String)]) -> Result<(String, Extracted), BoxError> {
    let html = fetch(client, url, headers).await.ok_or("toc fetch failed")?;
    let (title, links) = {
        let doc = Html::parse_document(&html);
        let title = sanitize_filename(&doc_text(&doc, t.title.as_deref()).unwrap_or_else(|| extract_title(&doc)));
        let mut links = Vec::new();
        if let Ok(sel) = Selector::parse(&ch.links) {
            for el in doc.select(&sel) { if let Some(h) = el.value().attr("href") { let u = resolve_url(h, url); if !u.is_empty() { links.push(u); } } }
        }
        (title, links)
    };
    if links.is_empty() { return Err("no chapters found".into()); }
    task.lock().unwrap().total = links.len();
    let get = ch.get.as_deref().unwrap_or("text");
    let mut parts = Vec::new();
    for (i, link) in links.iter().enumerate() {
        if i > 0 { sleep(delay).await; }
        let chtml = match fetch(client, link, headers).await { Some(h) => h, None => continue };
        let chtml = preprocess_text_html(&chtml);
        let doc = Html::parse_document(&chtml);
        let body = doc_get(&doc, &ch.content, get, link).unwrap_or_default();
        if body.trim().is_empty() { continue; }
        let ctitle = doc_text(&doc, ch.title.as_deref());
        parts.push(match ctitle { Some(c) => format!("{}\n\n{}", c, body.trim()), None => body.trim().to_string() });
        task.lock().unwrap().done = i + 1;
        log("[chapter]", &format!("{}/{}", i + 1, links.len()));
    }
    // strip + convert + header handled by finish_text (no author/date for chapters)
    finish_text(title, String::new(), String::new(), parts, t, "\n\n\n")
}

fn format_section(title: Option<String>, date: Option<String>, body: &str) -> String {
    let mut head = String::new();
    if let Some(t) = title { head.push_str(t.trim()); }
    if let Some(d) = date { if !head.is_empty() { head.push_str("  "); } head.push_str(d.trim()); }
    if head.is_empty() { body.to_string() } else { format!("【{}】\n{}", head, body) }
}

// Joins parts, applies strip/convert, and wraps with a title/author/date header.
fn finish_text(mut title: String, author: String, date: String, parts: Vec<String>, t: &TextRules, sep: &str) -> Result<(String, Extracted), BoxError> {
    let mut body = parts.join(sep);
    for s in &t.strip { body = body.replace(s.as_str(), ""); }
    if t.convert.as_deref() == Some("simplify") {
        body = zhconv(&body, "zh-Hans".parse().unwrap());
        title = zhconv(&title, "zh-Hans".parse().unwrap());
    }
    if body.trim().is_empty() { return Err("nothing extracted".into()); }
    let mut out = String::new();
    out.push_str(&title);
    out.push('\n');
    if !author.is_empty() { out.push_str(&author); out.push('\n'); }
    if !date.is_empty() { out.push_str(&date); out.push('\n'); }
    out.push('\n');
    out.push_str(&body);
    Ok((title, Extracted::Text(out)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Field;

    fn field(selector: &str, get: &str) -> Field {
        Field { selector: selector.into(), get: get.into(), regex: None, replace: None, engine: None }
    }

    #[test]
    fn get_text_and_attr() {
        let html = r#"<div id="c"><a href="/x.html">Hi <b>there</b></a></div>"#;
        let doc = Html::parse_document(html);
        let t = extract_fields(&doc, html, &[field("a", "text")], None, "https://e.com/", &[]);
        assert_eq!(t, vec!["Hi there"]);
        let h = extract_fields(&doc, html, &[field("a", "@href")], None, "https://e.com/", &[]);
        assert_eq!(h, vec!["https://e.com/x.html"]);
    }

    #[test]
    fn xpath_element_attr_and_attribute_node() {
        let html = r#"<div id="c"><img src="/a.jpg"><a href="/x">Link</a></div>"#;
        let doc = Html::parse_document(html);
        let mut img = field("//img", "@src"); // element node + @attr
        img.engine = Some("xpath".into());
        assert_eq!(extract_fields(&doc, html, &[img], None, "https://e.com/", &[]), vec!["https://e.com/a.jpg"]);
        let mut href = field("//a/@href", "text"); // attribute node directly
        href.engine = Some("xpath".into());
        assert_eq!(extract_fields(&doc, html, &[href], None, "https://e.com/", &[]), vec!["https://e.com/x"]);
        let mut txt = field("//a", "text"); // element text
        txt.engine = Some("xpath".into());
        assert_eq!(extract_fields(&doc, html, &[txt], None, "https://e.com/", &[]), vec!["Link"]);
    }

    #[test]
    fn get_attr_prefix_disambiguates() {
        let (k, is_attr) = parse_get("@src");
        assert_eq!((k, is_attr), ("src", true));
        let (k, is_attr) = parse_get("text");
        assert_eq!((k, is_attr), ("text", false));
    }

    #[test]
    fn regex_purifies_field() {
        let html = r#"<span class="p">Price $1,299.00</span>"#;
        let doc = Html::parse_document(html);
        let mut f = field(".p", "text");
        f.regex = Some(r"[^0-9.]".into());
        f.replace = Some("".into());
        assert_eq!(extract_fields(&doc, html, &[f], None, "https://e.com/", &[]), vec!["1299.00"]);
    }

    #[test]
    fn data_column_relative_to_row() {
        let doc = Html::parse_document(r#"<ul><li class="r"><a href="/p1">One</a></li><li class="r"><a href="/p2">Two</a></li></ul>"#);
        let sel = Selector::parse("li.r").unwrap();
        let name_col = Column { name: "name".into(), selector: Some("a".into()), get: "text".into(), regex: None, replace: None };
        let url_col = Column { name: "url".into(), selector: Some("a".into()), get: "@href".into(), regex: None, replace: None };
        let rows: Vec<Vec<String>> = doc.select(&sel).map(|r| vec![extract_column(r, &name_col, "https://e.com/"), extract_column(r, &url_col, "https://e.com/")]).collect();
        assert_eq!(rows, vec![vec!["One".to_string(), "https://e.com/p1".into()], vec!["Two".into(), "https://e.com/p2".into()]]);
    }

    #[test]
    fn combine_first_stops_at_first_hit() {
        let html = r#"<img data-src="/lazy.jpg" src="/placeholder.gif">"#;
        let doc = Html::parse_document(html);
        let fields = vec![field("img", "@data-src"), field("img", "@src")];
        // first: data-src hits, so the placeholder src is ignored
        assert_eq!(extract_fields_combined(&doc, html, &fields, None, "https://e.com/", &[], Some("first")), vec!["https://e.com/lazy.jpg"]);
        // merge (default): both are taken
        assert_eq!(extract_fields_combined(&doc, html, &fields, None, "https://e.com/", &[], None), vec!["https://e.com/lazy.jpg", "https://e.com/placeholder.gif"]);
    }

    #[test]
    fn combine_first_falls_back_when_empty() {
        let html = r#"<img src="/a.jpg">"#;
        let doc = Html::parse_document(html);
        let fields = vec![field("img", "@data-src"), field("img", "@src")];
        // data-src yields nothing, so it falls back to src
        assert_eq!(extract_fields_combined(&doc, html, &fields, None, "https://e.com/", &[], Some("first")), vec!["https://e.com/a.jpg"]);
    }

    #[test]
    fn json_path_rows_and_columns() {
        use serde_json_path::JsonPath;
        let v: serde_json::Value = serde_json::from_str(r#"[{"t":"A","n":5},{"t":"B","n":7}]"#).unwrap();
        let rows = JsonPath::parse("$[*]").unwrap().query(&v).all();
        let col = |rv: &serde_json::Value, p: &str| { let path = JsonPath::parse(p).unwrap(); let nl = path.query(rv); nl.first().map(json_scalar).unwrap_or_default() };
        let titles: Vec<String> = rows.iter().map(|r| col(*r, "$.t")).collect();
        let nums: Vec<String> = rows.iter().map(|r| col(*r, "$.n")).collect();
        assert_eq!(titles, vec!["A", "B"]);
        assert_eq!(nums, vec!["5", "7"]); // numbers render as strings
    }
}
