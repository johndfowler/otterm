//! On-disk run store. Layout:
//!
//! ```text
//! <data dir>/
//!   index.jsonl          one RunMeta per line, append-only (rewritten on delete)
//!   runs/<id>/output.log raw captured bytes, ANSI escapes included
//!   runs/<id>/meta.json  the same RunMeta, so a run dir is self-describing
//! ```
//!
//! The data dir is `$OTTERM_DATA_DIR` if set, else the platform data dir
//! (e.g. `~/Library/Application Support/otterm` on macOS).

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct RunMeta {
    pub id: String,
    pub cmd: Vec<String>,
    pub cwd: String,
    pub started_ms: u64,
    pub duration_ms: u64,
    /// None while running or when the child was killed by a signal.
    pub exit_code: Option<i32>,
    pub bytes: u64,
    /// False between record_start and record. Old (v0.2) metas lack the
    /// field; they're all completed, which `state()` handles via `pid`.
    #[serde(default)]
    pub done: bool,
    /// Pid of the capturing `otterm run` process, for liveness checks.
    #[serde(default)]
    pub pid: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Done,
    Running,
    /// The capturing process vanished without finishing the record —
    /// killed, crashed, or the machine went down.
    Died,
}

impl RunMeta {
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// The command as the user typed it, for display and filtering.
    pub fn cmdline(&self) -> String {
        self.cmd.join(" ")
    }

    pub fn state(&self) -> RunState {
        match (self.done, self.pid) {
            (true, _) | (false, None) => RunState::Done, // None: legacy v0.2 meta
            (false, Some(pid)) if pid_alive(pid) => RunState::Running,
            (false, Some(_)) => RunState::Died,
        }
    }
}

/// Signal 0 probes existence without sending anything. A recycled pid can
/// briefly masquerade as a live capture — rare enough to live with.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open() -> io::Result<Store> {
        let root = match std::env::var_os("OTTERM_DATA_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => dirs::data_dir()
                .ok_or_else(|| io::Error::other("no platform data directory"))?
                .join("otterm"),
        };
        Store::open_at(root)
    }

    /// Root a store at an arbitrary directory — tests and sandboxes.
    pub fn open_at(root: PathBuf) -> io::Result<Store> {
        fs::create_dir_all(root.join("runs"))?;
        fs::create_dir_all(root.join("running"))?;
        Ok(Store { root })
    }

    /// Millisecond timestamp + pid: sortable, unique enough for one machine.
    pub fn new_id(&self) -> String {
        format!("{:013}-{:05}", now_ms(), std::process::id() % 100_000)
    }

    pub fn run_dir(&self, id: &str) -> PathBuf {
        self.root.join("runs").join(id)
    }

    pub fn output_path(&self, id: &str) -> PathBuf {
        self.run_dir(id).join("output.log")
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("index.jsonl")
    }

    /// Marker file registering an in-flight capture. A tiny directory of
    /// markers means discovering live runs never requires scanning every
    /// run dir ever recorded.
    fn marker_path(&self, id: &str) -> PathBuf {
        self.root.join("running").join(id)
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.run_dir(id).join("meta.json")
    }

    fn write_meta(&self, meta: &RunMeta) -> io::Result<()> {
        // Write tmp + rename so concurrent readers never see a torn meta.json.
        let path = self.meta_path(&meta.id);
        fs::write(
            path.with_extension("json.tmp"),
            serde_json::to_string(meta)?,
        )?;
        fs::rename(path.with_extension("json.tmp"), path)
    }

    pub fn load_meta(&self, id: &str) -> Option<RunMeta> {
        let data = fs::read_to_string(self.meta_path(id)).ok()?;
        serde_json::from_str(&data).ok()
    }

    /// Register a capture the moment it starts, so the TUI can see and
    /// follow it live.
    pub fn record_start(&self, meta: &RunMeta) -> io::Result<()> {
        self.write_meta(meta)?;
        fs::write(self.marker_path(&meta.id), b"")
    }

    /// Persist a finished run: final meta.json, an index line, and the
    /// running-marker removed. The index append is a single O_APPEND write,
    /// so concurrent `otterm run` processes don't interleave partial lines.
    pub fn record(&self, meta: &RunMeta) -> io::Result<()> {
        self.write_meta(meta)?;
        let mut line = serde_json::to_string(meta)?;
        line.push('\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.index_path())?
            .write_all(line.as_bytes())?;
        fs::remove_file(self.marker_path(&meta.id)).ok();
        Ok(())
    }

    /// In-flight (and died-in-flight) captures, via the marker directory.
    /// Markers whose run completed or vanished are cleaned up as we go.
    pub fn list_running(&self) -> io::Result<Vec<RunMeta>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.root.join("running"))? {
            let Ok(entry) = entry else { continue };
            let id = entry.file_name().to_string_lossy().into_owned();
            match self.load_meta(&id) {
                Some(meta) if meta.state() != RunState::Done => out.push(meta),
                // Completed (marker leaked in a race) or gone entirely.
                _ => {
                    fs::remove_file(entry.path()).ok();
                }
            }
        }
        Ok(out)
    }

    /// All runs, oldest first. Tolerates corrupt lines and runs whose
    /// directory was deleted out from under the index.
    pub fn list(&self) -> io::Result<Vec<RunMeta>> {
        let data = match fs::read_to_string(self.index_path()) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(data
            .lines()
            .filter_map(|l| serde_json::from_str::<RunMeta>(l).ok())
            .filter(|m| self.run_dir(&m.id).is_dir())
            .collect())
    }

    pub fn delete(&self, id: &str) -> io::Result<()> {
        fs::remove_dir_all(self.run_dir(id)).ok(); // already gone is fine
        fs::remove_file(self.marker_path(id)).ok();
        let kept: Vec<RunMeta> = self.list()?.into_iter().filter(|m| m.id != id).collect();
        let mut out = BufWriter::new(File::create(self.index_path())?);
        for meta in &kept {
            serde_json::to_writer(&mut out, meta)?;
            out.write_all(b"\n")?;
        }
        out.flush()
    }

    /// Read a run's output, capped at `max` bytes. For oversized logs we keep
    /// the *tail* (the end is usually what you're looking for) and report
    /// truncation so the UI can say so.
    pub fn read_output(&self, id: &str, max: u64) -> io::Result<(Vec<u8>, bool)> {
        let mut f = File::open(self.output_path(id))?;
        let len = f.metadata()?.len();
        let truncated = len > max;
        if truncated {
            f.seek(SeekFrom::End(-(max as i64)))?;
        }
        let mut buf = Vec::with_capacity(len.min(max) as usize);
        f.read_to_end(&mut buf)?;
        if truncated {
            // Don't start mid-escape-sequence: drop up to the first newline.
            if let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                buf.drain(..=nl);
            }
        }
        Ok((buf, truncated))
    }
}
