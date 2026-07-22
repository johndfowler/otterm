//! `otterm run` — spawn a command on a pty, mirror its output to the real
//! terminal in real time, and capture every byte to the store.
//!
//! The pty matters: it's why captured commands keep their colors and
//! progress bars instead of detecting a pipe and going quiet.

use std::io::{self, Read, Write};
use std::sync::Arc;

use crossterm::terminal;
use crossterm::tty::IsTty;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::store::{now_ms, RunMeta, Store};

/// Restores cooked mode even if the capture loop errors out.
struct RawGuard(bool);

impl RawGuard {
    fn new(enable: bool) -> Self {
        if enable {
            let _ = terminal::enable_raw_mode();
        }
        RawGuard(enable)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        if self.0 {
            let _ = terminal::disable_raw_mode();
        }
    }
}

fn pty_size() -> PtySize {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }
}

/// Run `argv` under capture. Returns the child's exit code so main can
/// propagate it — `otterm run` is transparent to scripts and CI.
pub fn run_command(store: &Store, argv: &[String]) -> io::Result<i32> {
    let pty = native_pty_system();
    let pair = pty
        .openpty(pty_size())
        .map_err(|e| io::Error::other(e.to_string()))?;

    // When stdin isn't a terminal there's nothing a human needs echoed back,
    // and echo actively corrupts the capture: portable-pty's writer signals
    // EOF by writing the VEOF char (^D) on drop, which the pty would echo
    // straight into our log. Interactive sessions keep echo, like `script`.
    let interactive = io::stdin().is_tty() && io::stdout().is_tty();
    #[cfg(unix)]
    if !interactive {
        if let Some(fd) = pair.master.as_raw_fd() {
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(fd, &mut t) == 0 {
                    t.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ECHOCTL);
                    libc::tcsetattr(fd, libc::TCSANOW, &t);
                }
            }
        }
    }

    let mut cmd = CommandBuilder::new(&argv[0]);
    cmd.args(&argv[1..]);
    let cwd = std::env::current_dir()?;
    cmd.cwd(&cwd);

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| io::Error::other(format!("failed to spawn {:?}: {e}", argv[0])))?;
    // Drop our slave handle so the master reader sees EOF when the child exits.
    drop(pair.slave);

    let mut writer = pair
        .master
        .take_writer()
        .map_err(|e| io::Error::other(e.to_string()))?;
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| io::Error::other(e.to_string()))?;
    // MasterPty isn't Sync, so the resize thread gets it behind a mutex.
    let master = Arc::new(std::sync::Mutex::new(pair.master));

    // Forward terminal resizes to the pty so full-screen children re-flow.
    #[cfg(unix)]
    {
        let master = Arc::clone(&master);
        let mut signals = signal_hook::iterator::Signals::new([signal_hook::consts::SIGWINCH])?;
        std::thread::spawn(move || {
            for _ in signals.forever() {
                if let Ok(m) = master.lock() {
                    let _ = m.resize(pty_size());
                }
            }
        });
    }

    // Forward our stdin to the child. Raw mode only when we're actually on a
    // terminal — that routes ^C to the child's pty instead of killing us, and
    // lets interactive children (REPLs, prompts) work. Detached thread: it
    // dies with the process, which is fine since it holds nothing to flush.
    let _raw = RawGuard::new(interactive);
    std::thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 1024];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if writer.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Register the run before the first byte arrives, so the TUI lists it
    // (and can follow its log) while it's still running.
    let id = store.new_id();
    std::fs::create_dir_all(store.run_dir(&id))?;
    let mut meta = RunMeta {
        id: id.clone(),
        cmd: argv.to_vec(),
        cwd: cwd.to_string_lossy().into_owned(),
        started_ms: now_ms(),
        duration_ms: 0,
        exit_code: None,
        bytes: 0,
        done: false,
        pid: Some(std::process::id()),
    };
    store.record_start(&meta)?;

    // The tee loop: every chunk goes to the user's terminal and the log.
    // The log is flushed per chunk so a live follower sees output promptly.
    let mut log = io::BufWriter::new(std::fs::File::create(store.output_path(&id))?);
    let mut stdout = io::stdout().lock();
    let mut bytes: u64 = 0;
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            // EIO here is the normal macOS/Linux "pty closed" signal, not an error.
            Ok(0) | Err(_) => break,
            Ok(n) => {
                stdout.write_all(&buf[..n])?;
                stdout.flush()?;
                log.write_all(&buf[..n])?;
                log.flush()?;
                bytes += n as u64;
            }
        }
    }

    let status = child.wait().map_err(|e| io::Error::other(e.to_string()))?;
    drop(_raw); // back to cooked mode before printing our footer

    meta.duration_ms = now_ms().saturating_sub(meta.started_ms);
    meta.exit_code = Some(status.exit_code() as i32);
    meta.bytes = bytes;
    meta.done = true;
    store.record(&meta)?;

    // One dim line to stderr so stdout stays clean for pipes. Suppressed
    // under ambient shell capture (OTTERM_QUIET), where it would follow
    // every single command.
    if std::env::var_os("OTTERM_QUIET").is_none() {
        eprintln!(
            "\x1b[2m~( o.o )~  captured {} · exit {} · otterm to browse\x1b[0m",
            human_bytes(bytes),
            status.exit_code(),
        );
    }
    Ok(status.exit_code() as i32)
}

pub fn human_bytes(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.1} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}
