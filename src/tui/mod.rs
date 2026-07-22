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

        // Boarding a raft machine: hand the real terminal to an `otterm run
        // -- ssh …` child (which captures the session), then take it back.
        // A subprocess, not in-process capture — the child's stdin reader
        // must die with it, or it would steal keystrokes from the TUI.
        if let Some(target) = app.take_pending_ssh() {
            ratatui::restore();
            println!("🦦  boarding {target} — the session will be captured. exit to return.");
            let result = std::env::current_exe().and_then(|exe| {
                std::process::Command::new(exe)
                    .args(["run", "--", "ssh", &target])
                    .status()
            });
            *terminal = ratatui::init();
            terminal.clear()?;
            if let Err(e) = result {
                app.status = Some(format!("couldn't launch ssh: {e}"));
            }
            let _ = app.reload();
        }
    }
    Ok(())
}
