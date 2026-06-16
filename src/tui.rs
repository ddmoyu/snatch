// Terminal dashboard: task table, progress, and log pane (ratatui + tui-logger).
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};

use crate::db::{delete_download, recent_downloads, HistoryRow};
use crate::state::{AppState, TaskStatus};
use crate::util::{clear_logs, log_snapshot, open_dir};

enum Focus { Tasks, Logs, History }

fn load_history(state: &AppState) -> Vec<HistoryRow> {
    let db = state.db.lock().unwrap();
    recent_downloads(&db, 200)
}

pub fn run_tui(state: Arc<AppState>) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    // Even with panic = "abort" the hook still runs, so the terminal is restored before exit.
    let orig = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| { ratatui::restore(); orig(info); }));
    let mut tsel: usize = 0;
    let mut hsel: usize = 0;
    let mut focus = Focus::Tasks;
    let mut history = load_history(&state);
    let mut tick: u64 = 0;
    let res = loop {
        tick = tick.wrapping_add(1);
        if tick % 25 == 0 { history = load_history(&state); } // auto-refresh ~every 2s
        let tcount = state.tasks.lock().unwrap().len();
        if tcount > 0 && tsel >= tcount { tsel = tcount - 1; }
        if !history.is_empty() && hsel >= history.len() { hsel = history.len() - 1; }
        terminal.draw(|f| ui(f, &state, &focus, tsel, hsel, &history, tick))?;
        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press { continue; }
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                    KeyCode::Tab => { focus = match focus { Focus::Tasks => Focus::Logs, Focus::Logs => Focus::History, Focus::History => Focus::Tasks }; }
                    KeyCode::Char('r') => {
                        let url = match focus {
                            Focus::Tasks => { let ts = state.tasks.lock().unwrap(); ts.get(tsel).map(|t| t.lock().unwrap().url.clone()) }
                            Focus::History => history.get(hsel).map(|h| h.url.clone()),
                            Focus::Logs => None,
                        };
                        if let Some(u) = url { if !u.is_empty() { let _ = state.retry_tx.send(u); } }
                    }
                    KeyCode::Delete => {
                        if let Focus::History = focus {
                            let entry = history.get(hsel).map(|h| (h.dir.clone(), h.url.clone()));
                            if let Some((dir, url)) = entry {
                                if !dir.is_empty() { let _ = std::fs::remove_dir_all(&dir); }
                                { let db = state.db.lock().unwrap(); delete_download(&db, &url); }
                                history = load_history(&state);
                            }
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => match focus {
                        Focus::Tasks => { if tsel > 0 { tsel -= 1; } }
                        Focus::History => { if hsel > 0 { hsel -= 1; } }
                        Focus::Logs => {}
                    },
                    KeyCode::Down | KeyCode::Char('j') => match focus {
                        Focus::Tasks => { if tcount > 0 && tsel + 1 < tcount { tsel += 1; } }
                        Focus::History => { if hsel + 1 < history.len() { hsel += 1; } }
                        Focus::Logs => {}
                    },
                    KeyCode::Char('c') => match focus {
                        Focus::Tasks => { state.tasks.lock().unwrap().retain(|t| { let s = t.lock().unwrap().status; s != TaskStatus::Done && s != TaskStatus::Failed }); }
                        Focus::Logs => clear_logs(),
                        Focus::History => {}
                    },
                    KeyCode::Enter => {
                        let dir = match focus {
                            Focus::Tasks => { let ts = state.tasks.lock().unwrap(); ts.get(tsel).and_then(|t| { let g = t.lock().unwrap(); if g.download_dir.is_empty() { None } else { Some(g.download_dir.clone()) } }) }
                            Focus::History => history.get(hsel).map(|h| h.dir.clone()).filter(|d| !d.is_empty()),
                            Focus::Logs => None,
                        };
                        if let Some(d) = dir { open_dir(Path::new(&d)); }
                    }
                    _ => {}
                }
            }
        }
    };
    ratatui::restore();
    res
}

struct Snap { source: String, url: String, title: String, status: TaskStatus, done: usize, total: usize, error: String, kind: String, output: String, elapsed: Option<Duration> }

fn ui(f: &mut ratatui::Frame, state: &AppState, focus: &Focus, tsel: usize, hsel: usize, history: &[HistoryRow], tick: u64) {
    use ratatui::layout::{Alignment, Constraint, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table};

    const ACCENT: Color = Color::Rgb(46, 204, 113);
    const DIMC: Color = Color::Rgb(128, 128, 128);
    const SEL_BG: Color = Color::Rgb(38, 42, 54);
    let border = Style::default().fg(Color::Rgb(70, 74, 82));

    let area = f.area();
    // header (full width) / main (left column + right History) / footer (full width)
    let root = Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)]).split(area);
    let (header_area, footer_area) = (root[0], root[2]);
    let cols = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).split(root[1]);
    let left = Layout::vertical([Constraint::Min(4), Constraint::Length(10)]).split(cols[0]);
    let (tasks_area, logs_area, hist_area) = (left[0], left[1], cols[1]);
    let tasks_border = if matches!(focus, Focus::Tasks) { Style::default().fg(ACCENT) } else { border };
    let logs_border = if matches!(focus, Focus::Logs) { Style::default().fg(ACCENT) } else { border };
    let hist_border = if matches!(focus, Focus::History) { Style::default().fg(ACCENT) } else { border };

    let snaps: Vec<Snap> = state.tasks.lock().unwrap().iter().map(|t| {
        let g = t.lock().unwrap();
        Snap { source: g.source.clone(), url: g.url.clone(), title: g.title.clone(), status: g.status, done: g.done, total: g.total, error: g.error.clone(), kind: g.kind.clone(), output: g.output.clone(), elapsed: g.elapsed() }
    }).collect();

    // --- Header (info left, status chips right) ---
    let (mut nq, mut nr, mut nd, mut nf) = (0u32, 0u32, 0u32, 0u32);
    let mut got = 0usize;
    for s in &snaps { got += if s.kind == "image" && s.output != "csv" { s.done } else { (s.status == TaskStatus::Done) as usize }; match s.status { TaskStatus::Queued => nq += 1, TaskStatus::Running => nr += 1, TaskStatus::Done => nd += 1, TaskStatus::Failed => nf += 1 } }
    let hdr = Layout::horizontal([Constraint::Min(0), Constraint::Length(34)]).split(header_area);
    let name: Vec<char> = "SNATCH".chars().collect();
    let mut lspans: Vec<Span> = vec![Span::raw(" ")];
    for (i, ch) in name.iter().enumerate() {
        let t = i as f64 / (name.len() - 1) as f64;
        lspans.push(Span::styled(ch.to_string(), Style::default().fg(grad(t)).add_modifier(Modifier::BOLD)));
    }
    lspans.push(Span::styled("  v0.3.0", Style::default().fg(DIMC)));
    lspans.push(Span::raw("   "));
    lspans.push(Span::styled(format!("⏱ {}", fmt_dur(state.started.elapsed())), Style::default().fg(Color::Gray)));
    lspans.push(Span::raw("   "));
    lspans.push(Span::styled(format!("↓ {} files", got), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)));
    f.render_widget(Paragraph::new(Line::from(lspans)), hdr[0]);
    let chips = Line::from(vec![
        Span::styled(format!("✓ {}  ", nd), Style::default().fg(Color::Green)),
        Span::styled(format!("● {}  ", nr), Style::default().fg(Color::Yellow)),
        Span::styled(format!("• {}  ", nq), Style::default().fg(DIMC)),
        Span::styled(format!("✗ {}", nf), Style::default().fg(Color::Red)),
    ]);
    f.render_widget(Paragraph::new(chips).alignment(Alignment::Right), hdr[1]);

    // --- Left top: Tasks ---
    const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let title_blk = Line::from(Span::styled(format!(" Tasks ({}) ", snaps.len()), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)));
    if snaps.is_empty() {
        let block = Block::bordered().border_type(BorderType::Rounded).border_style(tasks_border).title(title_blk);
        let inner = block.inner(tasks_area);
        f.render_widget(block, tasks_area);
        let hint = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled("Waiting for clipboard…", Style::default().fg(DIMC).add_modifier(Modifier::BOLD))),
            Line::from(Span::styled("copy a matching URL", Style::default().fg(DIMC))),
        ]).alignment(Alignment::Center);
        f.render_widget(hint, inner);
    } else {
        let rows: Vec<Row> = snaps.iter().enumerate().map(|(i, s)| {
            let (icon, color) = match s.status {
                TaskStatus::Queued => ("•".to_string(), DIMC),
                TaskStatus::Running => (SPIN[(tick as usize) % SPIN.len()].to_string(), rgb(mix((170, 130, 40), (255, 220, 110), pulse(tick)))),
                TaskStatus::Done => ("✓".to_string(), Color::Green),
                TaskStatus::Failed => ("✗".to_string(), Color::Red),
            };
            let (title, tstyle) = if s.title.is_empty() { (truncate(&s.url, 30), Style::default().fg(DIMC)) } else { (truncate(&s.title, 30), Style::default().fg(Color::White)) };
            let progress: Line = match s.status {
                TaskStatus::Failed => Line::from(Span::styled(truncate(&s.error, 30), Style::default().fg(Color::Red))),
                TaskStatus::Queued => Line::from(Span::styled("queued", Style::default().fg(DIMC))),
                _ => match s.kind.as_str() {
                    "data" => Line::from(Span::styled(format!("{} rows", s.total.max(s.done)), Style::default().fg(Color::Cyan))),
                    "text" => if s.total > 1 { bar_line(s.done, s.total, tick, s.status == TaskStatus::Running) } else { Line::from(Span::styled("text", Style::default().fg(Color::Cyan))) },
                    _ => if s.output == "csv" { Line::from(Span::styled(format!("{} links → csv", s.done), Style::default().fg(Color::Cyan))) }
                         else { bar_line(s.done, s.total, tick, s.status == TaskStatus::Running) },
                },
            };
            let time = s.elapsed.map(fmt_dur).unwrap_or_else(|| "—".to_string());
            let row = Row::new(vec![
                Cell::from(Span::styled(icon, Style::default().fg(color).add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled(truncate(&s.source, 14), Style::default().fg(Color::Rgb(140, 170, 255)))),
                Cell::from(Span::styled(title, tstyle)),
                Cell::from(progress),
                Cell::from(Span::styled(time, Style::default().fg(DIMC))),
            ]);
            if i == tsel { row.style(Style::default().bg(SEL_BG).add_modifier(Modifier::BOLD)) } else { row }
        }).collect();
        let widths = [Constraint::Length(2), Constraint::Length(12), Constraint::Min(12), Constraint::Length(18), Constraint::Length(6)];
        let table = Table::new(rows, widths)
            .column_spacing(1)
            .header(Row::new(vec![Cell::from(""), Cell::from("SOURCE"), Cell::from("TITLE"), Cell::from("PROGRESS"), Cell::from("TIME")]).style(Style::default().fg(DIMC).add_modifier(Modifier::BOLD)).bottom_margin(1))
            .block(Block::bordered().border_type(BorderType::Rounded).border_style(tasks_border).title(title_blk));
        f.render_widget(table, tasks_area);
    }

    // --- Log pane ---
    render_logs(f, logs_area, logs_border);

    // --- Right column: History (full height) ---
    render_history(f, hist_area, history, hsel, hist_border);

    // --- Footer key hints ---
    let key = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    let lbl = Style::default().fg(DIMC);
    let footer = Line::from(vec![
        Span::styled(" q", key), Span::styled(" quit   ", lbl),
        Span::styled("↑↓", key), Span::styled(" select   ", lbl),
        Span::styled("⏎", key), Span::styled(" open dir   ", lbl),
        Span::styled("Tab", key), Span::styled(" focus   ", lbl),
        Span::styled("r", key), Span::styled(" retry   ", lbl),
        Span::styled("Del", key), Span::styled(" delete   ", lbl),
        Span::styled("c", key), Span::styled(" clear", lbl),
    ]);
    f.render_widget(Paragraph::new(footer), footer_area);
}

// Log pane: tail of the in-memory log buffer (newest at the bottom). `c` clears it when focused.
fn render_logs(f: &mut ratatui::Frame, area: ratatui::layout::Rect, border: ratatui::style::Style) {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, BorderType, Paragraph};
    const ACCENT: Color = Color::Rgb(46, 204, 113);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(border).title(Line::from(Span::styled(" Logs ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines = log_snapshot();
    let start = lines.len().saturating_sub(inner.height as usize);
    let rows: Vec<Line> = lines[start..].iter().map(|(warn, t)| {
        let c = if *warn { Color::Rgb(232, 178, 78) } else { Color::Rgb(120, 180, 130) };
        Line::from(Span::styled(t.clone(), Style::default().fg(c)))
    }).collect();
    f.render_widget(Paragraph::new(rows), inner);
}

// History panel: recent downloads from SQLite; Enter opens the selected row's folder.
fn render_history(f: &mut ratatui::Frame, area: ratatui::layout::Rect, history: &[HistoryRow], hsel: usize, border: ratatui::style::Style) {
    use ratatui::layout::{Alignment, Constraint};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table};
    const ACCENT: Color = Color::Rgb(46, 204, 113);
    const DIMC: Color = Color::Rgb(128, 128, 128);
    const SEL_BG: Color = Color::Rgb(38, 42, 54);
    let title = Line::from(Span::styled(format!(" History ({}) ", history.len()), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)));
    if history.is_empty() {
        let block = Block::bordered().border_type(BorderType::Rounded).border_style(border).title(title);
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(Paragraph::new(vec![Line::from(""), Line::from(Span::styled("No history yet", Style::default().fg(DIMC)))]).alignment(Alignment::Center), inner);
        return;
    }
    let rows: Vec<Row> = history.iter().enumerate().map(|(i, h)| {
        let row = Row::new(vec![
            Cell::from(Span::styled(truncate(&h.created_at, 19), Style::default().fg(DIMC))),
            Cell::from(Span::styled(truncate(&h.source, 14), Style::default().fg(Color::Rgb(140, 170, 255)))),
            Cell::from(Span::styled(truncate(&h.title, 40), Style::default().fg(Color::White))),
            Cell::from(Span::styled(h.count.to_string(), Style::default().fg(Color::Cyan))),
        ]);
        if i == hsel { row.style(Style::default().bg(SEL_BG).add_modifier(Modifier::BOLD)) } else { row }
    }).collect();
    let widths = [Constraint::Length(19), Constraint::Length(14), Constraint::Min(20), Constraint::Length(6)];
    let table = Table::new(rows, widths)
        .column_spacing(1)
        .header(Row::new(vec![Cell::from("TIME"), Cell::from("SOURCE"), Cell::from("TITLE"), Cell::from("COUNT")]).style(Style::default().fg(DIMC).add_modifier(Modifier::BOLD)).bottom_margin(1))
        .block(Block::bordered().border_type(BorderType::Rounded).border_style(border).title(title));
    f.render_widget(table, area);
}

fn fmt_dur(d: Duration) -> String { let s = d.as_secs(); format!("{:02}:{:02}", s / 60, s % 60) }
fn truncate(s: &str, n: usize) -> String { if s.chars().count() <= n { s.to_string() } else { let t: String = s.chars().take(n.saturating_sub(1)).collect(); format!("{}…", t) } }
// --- color/animation helpers ---
fn rgb(c: (u8, u8, u8)) -> ratatui::style::Color { ratatui::style::Color::Rgb(c.0, c.1, c.2) }
fn lerp8(a: u8, b: u8, t: f64) -> u8 { (a as f64 + (b as f64 - a as f64) * t).round().clamp(0.0, 255.0) as u8 }
fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> (u8, u8, u8) { (lerp8(a.0, b.0, t), lerp8(a.1, b.1, t), lerp8(a.2, b.2, t)) }
// Eased 0..1 breathing value driven by the frame counter.
fn pulse(tick: u64) -> f64 { ((tick as f64 * 0.5).sin() * 0.5) + 0.5 }
// Static gradient sampled 0..1 across a vivid pink -> violet -> cyan ramp (for the logo letters).
fn grad(t: f64) -> ratatui::style::Color {
    let stops = [(255u8, 70u8, 150u8), (140, 100, 255), (0, 220, 235)];
    let t = t.clamp(0.0, 1.0) * (stops.len() - 1) as f64;
    let i = (t.floor() as usize).min(stops.len() - 2);
    rgb(mix(stops[i], stops[i + 1], t - i as f64))
}

// Sub-cell-smooth progress bar (1/8th block resolution). When `running`, a brighter
// cell sweeps across the filled portion for a flowing-energy effect.
fn bar_line(done: usize, total: usize, tick: u64, running: bool) -> ratatui::text::Line<'static> {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
    const W: usize = 14;
    const BLK: [&str; 8] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let ratio = if total == 0 { 0.0 } else { (done as f64 / total as f64).min(1.0) };
    let eighths = ((ratio * (W * 8) as f64).round() as usize).min(W * 8);
    let full = eighths / 8;
    let rem = eighths % 8;
    let base = if ratio >= 0.999 { (46, 204, 113) } else if ratio >= 0.5 { (120, 200, 120) } else { (230, 200, 80) };
    let mut spans: Vec<Span> = Vec::new();
    if running && full > 0 {
        let hl = (tick as usize) % full;
        for k in 0..full {
            let c = if k == hl { rgb(mix(base, (255, 255, 255), 0.6)) } else { rgb(base) };
            spans.push(Span::styled("█", Style::default().fg(c)));
        }
    } else if full > 0 {
        spans.push(Span::styled("█".repeat(full), Style::default().fg(rgb(base))));
    }
    if rem > 0 { spans.push(Span::styled(BLK[rem], Style::default().fg(rgb(base)))); }
    let used = full + if rem > 0 { 1 } else { 0 };
    if used < W { spans.push(Span::styled("░".repeat(W - used), Style::default().fg(Color::Rgb(50, 54, 62)))); }
    spans.push(Span::styled(format!(" {}/{}", done, total), Style::default().fg(Color::Gray)));
    Line::from(spans)
}
