// SQLite-backed dedup ledger of already-downloaded pages.
use std::path::Path;

use rusqlite::{params, Connection};

pub fn init_db(app_dir: &Path) -> Connection { let conn = Connection::open(app_dir.join("snatch.db")).expect("open db"); conn.execute_batch("CREATE TABLE IF NOT EXISTS page_downloads (id INTEGER PRIMARY KEY AUTOINCREMENT, page_url TEXT NOT NULL UNIQUE, title TEXT, rule_name TEXT, image_count INTEGER DEFAULT 0, download_dir TEXT, created_at TEXT NOT NULL DEFAULT (datetime('now','localtime')));").expect("init db"); conn }
pub fn is_downloaded(conn: &Connection, url: &str) -> bool { conn.query_row("SELECT COUNT(*) FROM page_downloads WHERE page_url=?1", params![url], |r| r.get::<_,i64>(0)).unwrap_or(0) > 0 }
pub fn record_download(conn: &Connection, url: &str, title: &str, rule: &str, count: usize, dir: &str) { let _ = conn.execute("INSERT OR IGNORE INTO page_downloads (page_url,title,rule_name,image_count,download_dir) VALUES (?1,?2,?3,?4,?5)", params![url, title, rule, count as i64, dir]); }
