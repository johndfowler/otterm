//! All rendering. Mutates only scroll-tracking state (table offsets, viewer
//! height); everything else is read-only.

use ratatui::{
    layout::{Alignment, Constraint, Flex, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Cell, Paragraph, Row, Table},
    Frame,
};

use crate::banner;
use crate::capture::human_bytes;
use crate::store::{now_ms, RunState};
use crate::tui::app::{age, duration, App, Mode, View};
use crate::tui::theme;

pub fn render(frame: &mut Frame, app: &mut App) {
    frame.render_widget(Block::new().style(Style::new().bg(theme::DEEP)), frame.area());
    match app.view {
        View::Splash => render_splash(frame, app),
        View::List | View::Results | View::Viewer => {
            let [header, content, status] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .areas(frame.area());
            match app.view {
                View::List => {
                    render_header(frame, header, app);
                    render_list(frame, content, app);
                }
                View::Results => {
                    render_header(frame, header, app);
                    render_results(frame, content, app);
                }
                View::Viewer => {
                    render_viewer_header(frame, header, app);
                    render_viewer(frame, content, app);
                }
                View::Splash => unreachable!(),
            }
            render_status(frame, status, app);
        }
    }
}

// ---------------------------------------------------------------- splash --

fn render_splash(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let title_lines = banner::TITLE.lines().skip(1).count() as u16;
    let otter_lines = banner::OTTER.lines().skip(1).count() as u16;
    let height = title_lines + otter_lines + 5;

    let [center] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let rows = Layout::vertical([
        Constraint::Length(title_lines),
        Constraint::Length(otter_lines),
        Constraint::Length(1), // waves
        Constraint::Length(1),
        Constraint::Length(1), // tagline
        Constraint::Length(1),
        Constraint::Length(1), // hint
    ])
    .split(center);

    let title = Text::from_iter(banner::TITLE.lines().skip(1))
        .style(Style::new().fg(theme::RIVER).add_modifier(Modifier::BOLD));
    frame.render_widget(Paragraph::new(title).alignment(Alignment::Center), rows[0]);

    // Center the art as a block, not per line — per-line centering would
    // shift each row of the drawing independently and distort it.
    frame.render_widget(otter_art(theme::FUR), centered_h(rows[1], otter_width()));

    frame.render_widget(
        Paragraph::new(wave_line(app.tick)).alignment(Alignment::Center),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new(Line::from(banner::TAGLINE).style(Style::new().fg(theme::CREAM).italic()))
            .alignment(Alignment::Center),
        rows[4],
    );
    if app.tick % 8 != 7 {
        frame.render_widget(
            Paragraph::new(Line::from(banner::HINT).style(Style::new().fg(theme::FUR_DARK)))
                .alignment(Alignment::Center),
            rows[6],
        );
    }
}

/// Hard column clipping looks like text ran into its neighbor; mark it.
fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

fn otter_art(color: ratatui::style::Color) -> Paragraph<'static> {
    Paragraph::new(Text::from_iter(banner::OTTER.lines().skip(1)).style(Style::new().fg(color)))
}

fn otter_width() -> u16 {
    banner::OTTER.lines().map(|l| l.len()).max().unwrap_or(0) as u16
}

/// Horizontally center a `width`-wide strip inside `area`.
fn centered_h(area: Rect, width: u16) -> Rect {
    let [c] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    c
}

/// A scrolling water line: two windows into the constant wave pattern,
/// offset by the tick counter — no per-frame string building.
fn wave_line(tick: u64) -> Line<'static> {
    let len = banner::WAVES.len(); // ASCII-only, byte slicing is safe
    let off = tick as usize % len;
    Line::from(vec![
        Span::raw(&banner::WAVES[off..]),
        Span::raw(&banner::WAVES[..off]),
    ])
    .style(Style::new().fg(theme::RIVER))
}

// ------------------------------------------------------------ list view --

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![
        Span::styled(" ~( o.o )~ ", Style::new().fg(theme::FUR).bold()),
        Span::styled(
            format!("library · {} runs", app.runs.len()),
            Style::new().fg(theme::CREAM),
        ),
    ];
    if !app.filter.is_empty() {
        spans.push(Span::styled(
            format!(" · filter '{}' ({} shown)", app.filter, app.filtered.len()),
            Style::new().fg(theme::CLAM),
        ));
    }
    if app.view == View::Results {
        spans.push(Span::styled(
            format!(" · {} hits for '{}'", app.hits.len(), app.content_query),
            Style::new().fg(theme::CLAM),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(theme::FUR_DARK)),
        area,
    );
}

fn render_list(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.filtered.is_empty() {
        let msg = if app.runs.is_empty() {
            "the library is empty — run something through me:\n\n  otterm run -- cargo test\n  otterm run -- npm run build"
        } else {
            "nothing matches the filter (Esc clears it)"
        };
        render_empty_state(frame, area, msg);
        return;
    }

    const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let spin = SPINNER[(app.tick / 2) as usize % SPINNER.len()];
    let rows = app.filtered.iter().map(|&i| {
        let m = &app.runs[i];
        let (mark, color, dur) = match m.state() {
            // A live run's duration column counts up in real time.
            RunState::Running => (spin, theme::CLAM, duration(now_ms().saturating_sub(m.started_ms))),
            RunState::Died => ("!", theme::ERR, "died".to_owned()),
            RunState::Done if m.success() => ("✓", theme::OK, duration(m.duration_ms)),
            RunState::Done => ("✗", theme::ERR, duration(m.duration_ms)),
        };
        Row::new(vec![
            Cell::from(mark).style(Style::new().fg(color).bold()),
            Cell::from(age(m.started_ms)).style(Style::new().fg(theme::FUR)),
            Cell::from(dur).style(Style::new().fg(theme::FUR)),
            Cell::from(human_bytes(m.bytes)).style(Style::new().fg(theme::FUR)),
            Cell::from(m.cmdline()).style(Style::new().fg(theme::CREAM)),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(4),
            Constraint::Length(7),
            Constraint::Length(9),
            Constraint::Min(10),
        ],
    )
    .column_spacing(2)
    .row_highlight_style(Style::new().bg(theme::FUR).fg(theme::DEEP).bold());

    app.table_state.select(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut app.table_state);
}

fn render_empty_state(frame: &mut Frame, area: Rect, msg: &str) {
    let otter_lines = banner::OTTER.lines().skip(1).count() as u16;
    let text_lines = msg.lines().count() as u16;
    let [center] = Layout::vertical([Constraint::Length(otter_lines + 1 + text_lines)])
        .flex(Flex::Center)
        .areas(area);
    let [art, _, text] = Layout::vertical([
        Constraint::Length(otter_lines),
        Constraint::Length(1),
        Constraint::Length(text_lines),
    ])
    .areas(center);
    frame.render_widget(otter_art(theme::FUR_DARK), centered_h(art, otter_width()));
    frame.render_widget(
        Paragraph::new(msg.to_owned())
            .style(Style::new().fg(theme::CREAM))
            .alignment(Alignment::Center),
        text,
    );
}

// --------------------------------------------------------- results view --

fn render_results(frame: &mut Frame, area: Rect, app: &mut App) {
    let rows = app.hits.iter().map(|h| {
        Row::new(vec![
            Cell::from(ellipsize(&h.meta.cmdline(), 28)).style(Style::new().fg(theme::RIVER)),
            Cell::from(age(h.meta.started_ms)).style(Style::new().fg(theme::FUR)),
            Cell::from(format!("{}:", h.line + 1)).style(Style::new().fg(theme::FUR)),
            Cell::from(h.text.clone()).style(Style::new().fg(theme::CREAM)),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Max(28),
            Constraint::Length(4),
            Constraint::Length(6),
            Constraint::Min(20),
        ],
    )
    .column_spacing(2)
    .row_highlight_style(Style::new().bg(theme::FUR).fg(theme::DEEP).bold());

    app.hits_state.select(Some(app.hit_selected));
    frame.render_stateful_widget(table, area, &mut app.hits_state);
}

// ---------------------------------------------------------- viewer view --

fn render_viewer_header(frame: &mut Frame, area: Rect, app: &App) {
    let Some(v) = app.viewer.as_ref() else { return };
    let m = &v.meta;
    let (mark, color, detail) = if v.live {
        let follow = if v.follow { "following" } else { "paused — f to follow" };
        (
            "●",
            theme::CLAM,
            format!(
                "  · live {} · {} · {}",
                duration(now_ms().saturating_sub(m.started_ms)),
                human_bytes(v.last_size),
                follow,
            ),
        )
    } else {
        let (mk, c) = match m.state() {
            RunState::Died => ("!", theme::ERR),
            _ if m.success() => ("✓", theme::OK),
            _ => ("✗", theme::ERR),
        };
        (
            mk,
            c,
            format!(
                "  · {} ago · {} · {} · exit {}",
                age(m.started_ms),
                duration(m.duration_ms),
                human_bytes(m.bytes),
                m.exit_code.map_or("?".into(), |c| c.to_string()),
            ),
        )
    };
    let mut spans = vec![
        Span::styled(format!(" {mark} "), Style::new().fg(color).bold()),
        Span::styled(m.cmdline(), Style::new().fg(theme::CREAM).bold()),
        Span::styled(detail, Style::new().fg(theme::FUR)),
    ];
    if v.truncated {
        spans.push(Span::styled(
            "  [tail only — log exceeds 16MB]",
            Style::new().fg(theme::CLAM),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(theme::FUR_DARK)),
        area,
    );
}

fn render_viewer(frame: &mut Frame, area: Rect, app: &mut App) {
    let Some(v) = app.viewer.as_mut() else { return };
    v.height = area.height as usize; // key handlers page and follow by this
    v.scroll = v.scroll.min(v.lines.len().saturating_sub(area.height as usize));

    // Only the visible window is cloned into the frame — the full decoded
    // log stays put in the viewer state.
    let end = (v.scroll + area.height as usize).min(v.lines.len());
    let window: Vec<Line> = v.lines[v.scroll..end].to_vec();
    frame.render_widget(Paragraph::new(window), area);

    // Overlay the current search match in reverse video so it pops through
    // whatever colors the log itself uses.
    if let Some(&line) = v.matches.get(v.match_idx) {
        if line >= v.scroll && line < end {
            let y = area.y + (line - v.scroll) as u16;
            let row = Rect::new(area.x, y, area.width, 1);
            let text = v.stripped.get(line).cloned().unwrap_or_default();
            frame.render_widget(
                Paragraph::new(text).style(Style::new().bg(theme::CLAM).fg(theme::DEEP).bold()),
                row,
            );
        }
    }
}

// ------------------------------------------------------------ status bar --

fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    let line = match app.mode {
        Mode::FilterInput => prompt_line("filter", &app.input),
        Mode::SearchInput => prompt_line("search all output", &app.input),
        Mode::ViewerSearchInput => prompt_line("search", &app.input),
        Mode::ConfirmDelete => Line::from(Span::styled(
            " delete this run and its output? y / any other key cancels ",
            Style::new().fg(theme::DEEP).bg(theme::ERR).bold(),
        )),
        Mode::Normal => {
            if let Some(msg) = &app.status {
                Line::from(Span::styled(
                    format!(" {msg}"),
                    Style::new().fg(theme::CLAM).italic(),
                ))
            } else {
                hints_line(app)
            }
        }
    };
    frame.render_widget(
        Paragraph::new(line).style(Style::new().bg(theme::FUR_DARK)),
        area,
    );
}

fn prompt_line(label: &str, input: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {label} "),
            Style::new().fg(theme::DEEP).bg(theme::CLAM).bold(),
        ),
        Span::styled(format!(" {input}"), Style::new().fg(theme::CREAM)),
        Span::styled("▌", Style::new().fg(theme::CLAM)),
    ])
}

fn hints_line(app: &App) -> Line<'static> {
    let hints = match app.view {
        View::List => "  Enter view · R re-run · / filter · s search output · x delete · q quit",
        View::Results => "  Enter open at match · Esc back",
        View::Viewer => match app.viewer.as_ref() {
            Some(v) if v.live => "  f follow · j/k scroll · G bottom · / search · Esc back",
            _ => "  j/k scroll · d/u page · g/G ends · / search · n/N matches · Esc back",
        },
        View::Splash => "",
    };
    let mut n_of_m = String::new();
    if app.view == View::Viewer {
        if let Some(v) = app.viewer.as_ref() {
            if !v.matches.is_empty() {
                n_of_m = format!("  [{}/{} matches]", v.match_idx + 1, v.matches.len());
            }
        }
    }
    Line::from(vec![
        Span::styled(" otterm ", Style::new().fg(theme::DEEP).bg(theme::CLAM).bold()),
        Span::styled(hints.to_owned(), Style::new().fg(theme::CREAM)),
        Span::styled(n_of_m, Style::new().fg(theme::CLAM)),
    ])
}
