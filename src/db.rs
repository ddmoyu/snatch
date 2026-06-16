// SQLite-backed dedup ledger of already-downloaded pages.
use std::path::Path;

use rusqlite::{params, Connection};

pub fn init_db(app_dir: &Path) -> Connection { let conn = Connection::open(app_dir.join("snatch.db")).expect("open db"); let _ = conn.busy_timeout(std::time::Duration::from_secs(5)); conn.execute_batch("CREATE TABLE IF NOT EXISTS page_downloads (id INTEGER PRIMARY KEY AUTOINCREMENT, page_url TEXT NOT NULL UNIQUE, title TEXT, rule_name TEXT, image_count INTEGER DEFAULT 0, download_dir TEXT, created_at TEXT NOT NULL DEFAULT (datetime('now','localtime')));").expect("init db"); conn }
pub fn is_downloaded(conn: &Connection, url: &str) -> bool { conn.query_row("SELECT COUNT(*) FROM page_downloads WHERE page_url=?1", params![url], |r| r.get::<_,i64>(0)).unwrap_or(0) > 0 }
pub fn record_download(conn: &Connection, url: &str, title: &str, rule: &str, count: usize, dir: &str) { let _ = conn.execute("INSERT OR IGNORE INTO page_downloads (page_url,title,rule_name,image_count,download_dir) VALUES (?1,?2,?3,?4,?5)", params![url, title, rule, count as i64, dir]); }

pub struct HistoryRow { pub url: String, pub title: String, pub source: String, pub count: i64, pub dir: String, pub created_at: String }

// Most recent downloads, newest first (drives the History panel).
pub fn recent_downloads(conn: &Connection, limit: usize) -> Vec<HistoryRow> {
    let mut out = Vec::new();
    let mut stmt = match conn.prepare("SELECT page_url, title, rule_name, image_count, download_dir, created_at FROM page_downloads ORDER BY id DESC LIMIT ?1") { Ok(s) => s, Err(_) => return out };
    let rows = stmt.query_map(params![limit as i64], |r| Ok(HistoryRow {
        url: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
        title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        source: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        count: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
        dir: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        created_at: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
    }));
    if let Ok(rows) = rows { for row in rows.flatten() { out.push(row); } }
    out
}

// Removes a history record (the local files are deleted separately by the caller).
pub fn delete_download(conn: &Connection, url: &str) { let _ = conn.execute("DELETE FROM page_downloads WHERE page_url=?1", params![url]); }
