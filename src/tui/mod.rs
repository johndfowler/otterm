//! TUI entry point and event loop.

mod app;
mod theme;
mod ui;

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};

use crate::store::Store;
use app::App;

/// Animation cadence (~10 fps). The loop blocks in `event::poll` between
/// ticks, so idle CPU cost is one wakeup per tick — input is still handled
/// the instant it arrives.
const TICK_RATE: Duration = Duration::from_millis(100);

pub fn run(store: Store) -> io::Result<()> {
    // ratatui::init installs a panic hook that restores the terminal, so a
    // crash never leaves the shell in raw mode / the alternate screen.
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, store);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, store: Store) -> io::Result<()> {
    let mut app = App::new(store)?;
    let mut last_tick = Instant::now();

    while app.running {
        terminal.draw(|frame| ui::render(frame, &mut app))?;

        // Sleep until the next tick unless input arrives first.
        let timeout = TICK_RATE.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            match event::read()? {
                // Filter to Press: Windows terminals also report Release.
                Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key.code),
                // Resize just falls through — the next draw uses the new size.
                _ => {}
            }
        }
        if last_tick.elapsed() >= TICK_RATE {
            app.on_tick();
            last_tick = Instant::now();
        }
    }
    Ok(())
}
