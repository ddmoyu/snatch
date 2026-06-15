// Terminal dashboard: task table, progress, and log pane (ratatui + tui-logger).
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};

use crate::state::{AppState, TaskStatus};
use crate::util::open_dir;

pub fn run_tui(state: Arc<AppState>) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    // Even with panic = "abort" the hook still runs, so the terminal is restored before exit.
    let orig = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| { ratatui::restore(); orig(info); }));
    let mut selected: usize = 0;
    let mut tick: u64 = 0;
    let res = loop {
        tick = tick.wrapping_add(1);
        let count = state.tasks.lock().unwrap().len();
        if count > 0 && selected >= count { selected = count - 1; }
        terminal.draw(|f| ui(f, &state, selected, tick))?;
        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press { continue; }
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                    KeyCode::Up | KeyCode::Char('k') => { if selected > 0 { selected -= 1; } }
                    KeyCode::Down | KeyCode::Char('j') => { if count > 0 && selected + 1 < count { selected += 1; } }
                    KeyCode::Char('c') => { state.tasks.lock().unwrap().retain(|t| { let s = t.lock().unwrap().status; s != TaskStatus::Done && s != TaskStatus::Failed }); }
                    KeyCode::Enter => {
                        let dir = { let ts = state.tasks.lock().unwrap(); ts.get(selected).and_then(|t| { let g = t.lock().unwrap(); if g.download_dir.is_empty() { None } else { Some(g.download_dir.clone()) } }) };
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

struct Snap { rule: String, url: String, title: String, status: TaskStatus, done: usize, total: usize, error: String, mode: String, output: String, elapsed: Option<Duration> }

fn ui(f: &mut ratatui::Frame, state: &AppState, selected: usize, tick: u64) {
    use ratatui::layout::{Alignment, Constraint, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table};

    const ACCENT: Color = Color::Rgb(46, 204, 113);
    const DIMC: Color = Color::Rgb(128, 128, 128);
    const SEL_BG: Color = Color::Rgb(38, 42, 54);
    let border = Style::default().fg(Color::Rgb(70, 74, 82));

    let area = f.area();
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(4), Constraint::Length(10), Constraint::Length(1)]).split(area);

    let snaps: Vec<Snap> = state.tasks.lock().unwrap().iter().map(|t| {
        let g = t.lock().unwrap();
        Snap { rule: g.rule_name.clone(), url: g.url.clone(), title: g.title.clone(), status: g.status, done: g.done, total: g.total, error: g.error.clone(), mode: g.mode.clone(), output: g.output.clone(), elapsed: g.elapsed() }
    }).collect();

    // --- Header (info left, status chips right) ---
    let (mut nq, mut nr, mut nd, mut nf) = (0u32, 0u32, 0u32, 0u32);
    let mut got = 0usize;
    for s in &snaps { got += if s.mode == "text" || s.output == "csv" { (s.status == TaskStatus::Done) as usize } else { s.done }; match s.status { TaskStatus::Queued => nq += 1, TaskStatus::Running => nr += 1, TaskStatus::Done => nd += 1, TaskStatus::Failed => nf += 1 } }
    let hdr = Layout::horizontal([Constraint::Min(0), Constraint::Length(34)]).split(chunks[0]);
    let left = Line::from(vec![
        Span::styled(" SNATCH ", Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled(" v0.3.0", Style::default().fg(DIMC)),
        Span::raw("   "),
        Span::styled(format!("⏱ {}", fmt_dur(state.started.elapsed())), Style::default().fg(Color::Gray)),
        Span::raw("   "),
        Span::styled(format!("↓ {} files", got), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
    ]);
    f.render_widget(Paragraph::new(left), hdr[0]);
    let chips = Line::from(vec![
        Span::styled(format!("✓ {}  ", nd), Style::default().fg(Color::Green)),
        Span::styled(format!("● {}  ", nr), Style::default().fg(Color::Yellow)),
        Span::styled(format!("• {}  ", nq), Style::default().fg(DIMC)),
        Span::styled(format!("✗ {}", nf), Style::default().fg(Color::Red)),
    ]);
    f.render_widget(Paragraph::new(chips).alignment(Alignment::Right), hdr[1]);

    // --- Task table ---
    const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let title_blk = Line::from(Span::styled(format!(" Tasks ({}) ", snaps.len()), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)));
    if snaps.is_empty() {
        let block = Block::bordered().border_type(BorderType::Rounded).border_style(border).title(title_blk);
        let inner = block.inner(chunks[1]);
        f.render_widget(block, chunks[1]);
        let hint = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled("Waiting for clipboard…", Style::default().fg(DIMC).add_modifier(Modifier::BOLD))),
            Line::from(Span::styled("copy a matching URL to start a task", Style::default().fg(DIMC))),
        ]).alignment(Alignment::Center);
        f.render_widget(hint, inner);
    } else {
        let rows: Vec<Row> = snaps.iter().enumerate().map(|(i, s)| {
            let (icon, color) = match s.status {
                TaskStatus::Queued => ("•".to_string(), DIMC),
                TaskStatus::Running => (SPIN[(tick as usize) % SPIN.len()].to_string(), Color::Yellow),
                TaskStatus::Done => ("✓".to_string(), Color::Green),
                TaskStatus::Failed => ("✗".to_string(), Color::Red),
            };
            let (title, tstyle) = if s.title.is_empty() { (truncate(&s.url, 30), Style::default().fg(DIMC)) } else { (truncate(&s.title, 30), Style::default().fg(Color::White)) };
            let progress: Line = match s.status {
                TaskStatus::Failed => Line::from(Span::styled(truncate(&s.error, 30), Style::default().fg(Color::Red))),
                TaskStatus::Queued => Line::from(Span::styled("queued", Style::default().fg(DIMC))),
                _ => if s.mode == "text" { Line::from(Span::styled(format!("{} pages", s.total.max(s.done)), Style::default().fg(Color::Cyan))) }
                     else if s.output == "csv" { Line::from(Span::styled(format!("{} links → csv", s.done), Style::default().fg(Color::Cyan))) }
                     else { bar_line(s.done, s.total) },
            };
            let time = s.elapsed.map(fmt_dur).unwrap_or_else(|| "—".to_string());
            let row = Row::new(vec![
                Cell::from(Span::styled(icon, Style::default().fg(color).add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled(truncate(&s.rule, 14), Style::default().fg(Color::Rgb(140, 170, 255)))),
                Cell::from(Span::styled(title, tstyle)),
                Cell::from(progress),
                Cell::from(Span::styled(time, Style::default().fg(DIMC))),
            ]);
            if i == selected { row.style(Style::default().bg(SEL_BG).add_modifier(Modifier::BOLD)) } else { row }
        }).collect();
        let widths = [Constraint::Length(2), Constraint::Length(14), Constraint::Length(32), Constraint::Min(18), Constraint::Length(7)];
        let table = Table::new(rows, widths)
            .column_spacing(1)
            .header(Row::new(vec![Cell::from(""), Cell::from("RULE"), Cell::from("TITLE"), Cell::from("PROGRESS"), Cell::from("TIME")]).style(Style::default().fg(DIMC).add_modifier(Modifier::BOLD)).bottom_margin(1))
            .block(Block::bordered().border_type(BorderType::Rounded).border_style(border).title(title_blk));
        f.render_widget(table, chunks[1]);
    }

    // --- Log pane ---
    let logw = tui_logger::TuiLoggerWidget::default()
        .block(Block::bordered().border_type(BorderType::Rounded).border_style(border).title(Line::from(Span::styled(" Logs ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))))
        .output_separator(' ')
        .output_timestamp(Some("%H:%M:%S".to_string()))
        .output_level(Some(tui_logger::TuiLoggerLevelOutput::Abbreviated))
        .output_target(false)
        .output_file(false)
        .output_line(false)
        .style_error(Style::default().fg(Color::Red))
        .style_warn(Style::default().fg(Color::Yellow))
        .style_info(Style::default().fg(Color::Green));
    f.render_widget(logw, chunks[2]);

    // --- Footer key hints ---
    let key = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    let lbl = Style::default().fg(DIMC);
    let footer = Line::from(vec![
        Span::styled(" q", key), Span::styled(" quit    ", lbl),
        Span::styled("↑↓", key), Span::styled(" select    ", lbl),
        Span::styled("⏎", key), Span::styled(" open dir    ", lbl),
        Span::styled("c", key), Span::styled(" clear finished", lbl),
    ]);
    f.render_widget(Paragraph::new(footer), chunks[3]);
}

fn fmt_dur(d: Duration) -> String { let s = d.as_secs(); format!("{:02}:{:02}", s / 60, s % 60) }
fn truncate(s: &str, n: usize) -> String { if s.chars().count() <= n { s.to_string() } else { let t: String = s.chars().take(n.saturating_sub(1)).collect(); format!("{}…", t) } }
fn bar_line(done: usize, total: usize) -> ratatui::text::Line<'static> {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
    const W: usize = 12;
    let ratio = if total == 0 { 0.0 } else { (done as f64 / total as f64).min(1.0) };
    let filled = ((ratio * W as f64).round() as usize).min(W);
    let color = if ratio >= 0.999 { Color::Green } else if ratio >= 0.5 { Color::Rgb(120, 200, 120) } else { Color::Rgb(230, 200, 80) };
    Line::from(vec![
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled("░".repeat(W - filled), Style::default().fg(Color::Rgb(60, 60, 60))),
        Span::styled(format!(" {}/{}", done, total), Style::default().fg(Color::Gray)),
    ])
}
