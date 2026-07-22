//! TUI state and input handling. Rendering lives in `ui.rs`; the only thing
//! it mutates are the ratatui table states (scroll offsets) and the viewer's
//! last-known height.

use ansi_to_tui::IntoText;
use crossterm::event::KeyCode;
use ratatui::text::Line;
use ratatui::widgets::TableState;

use crate::store::{now_ms, RunMeta, RunState, Store};

/// Cap on how much of a log we load or search — the tail is kept (see
/// `Store::read_output`). 16 MB covers almost any real run.
pub const MAX_READ: u64 = 16 * 1024 * 1024;
const MAX_HITS: usize = 500;
const HITS_PER_RUN: usize = 5;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Splash,
    List,
    Results,
    Viewer,
    /// The tailnet fleet — board machines over ssh.
    Raft,
    /// Stats about your library, presided over by 🦦.
    Den,
}

/// What the status-bar input line is currently collecting, if anything.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    FilterInput,
    SearchInput,
    ViewerSearchInput,
    ConfirmDelete,
}

/// Library-wide stats for the Den view, computed on entry.
pub struct DenStats {
    pub total: usize,
    pub bytes: u64,
    pub ok: usize,
    pub failed: usize,
    pub today: usize,
    /// (command, times run) — the command you keep coming back to.
    pub most_run: Option<(String, usize)>,
    /// (command, duration_ms) — the longest sit.
    pub longest: Option<(String, u64)>,
}

impl DenStats {
    pub fn compute(runs: &[RunMeta]) -> DenStats {
        let mut by_cmd: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut most: Option<(String, usize)> = None;
        let day_ago = now_ms().saturating_sub(24 * 60 * 60 * 1000);
        let mut stats = DenStats {
            total: runs.len(),
            bytes: 0,
            ok: 0,
            failed: 0,
            today: 0,
            most_run: None,
            longest: None,
        };
        for m in runs {
            stats.bytes += m.bytes;
            match m.exit_code {
                Some(0) => stats.ok += 1,
                Some(_) => stats.failed += 1,
                None => {}
            }
            if m.started_ms >= day_ago {
                stats.today += 1;
            }
            let cmdline = m.cmdline();
            let n = by_cmd.entry(cmdline.clone()).or_insert(0);
            *n += 1;
            if most.as_ref().is_none_or(|(_, best)| *n > *best) {
                most = Some((cmdline.clone(), *n));
            }
            if stats
                .longest
                .as_ref()
                .is_none_or(|(_, d)| m.duration_ms > *d)
            {
                stats.longest = Some((cmdline, m.duration_ms));
            }
        }
        stats.most_run = most;
        stats
    }
}

/// One content-search match: which run, which line, what it said.
pub struct Hit {
    pub meta: RunMeta,
    pub line: usize,
    pub text: String,
}

pub struct Viewer {
    pub meta: RunMeta,
    /// ANSI-decoded lines for display.
    pub lines: Vec<Line<'static>>,
    /// Escape-stripped lines for searching; parallel to `lines`.
    pub stripped: Vec<String>,
    pub scroll: usize,
    pub query: String,
    pub matches: Vec<usize>,
    pub match_idx: usize,
    pub truncated: bool,
    /// Whether Esc should return to search results or the run list.
    pub from_results: bool,
    /// Content rows available last frame; render keeps this current so key
    /// handlers can page and clamp correctly.
    pub height: usize,
    /// True while the underlying capture is still running — the viewer
    /// tails the log on every tick.
    pub live: bool,
    /// Pin the view to the bottom as new output arrives. Scrolling up
    /// unpins; `f` or `G` re-pins.
    pub follow: bool,
    /// Log size at last decode, to skip re-decoding when nothing changed.
    pub last_size: u64,
}

pub struct App {
    pub store: Store,
    pub running: bool,
    pub view: View,
    pub mode: Mode,
    pub tick: u64,
    /// Newest first.
    pub runs: Vec<RunMeta>,
    pub filter: String,
    /// Indices into `runs` that pass the filter.
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub table_state: TableState,
    pub input: String,
    pub status: Option<String>,
    pub hits: Vec<Hit>,
    pub hit_selected: usize,
    pub hits_state: TableState,
    pub content_query: String,
    pub viewer: Option<Viewer>,
    /// The tailnet fleet (Raft view).
    pub peers: Vec<crate::fleet::Peer>,
    pub peer_selected: usize,
    pub peers_state: TableState,
    pub peers_err: Option<String>,
    /// A QR overlay: (caption, rendered half-block text).
    pub qr: Option<(String, String)>,
    /// Set by the Raft view; the event loop suspends the TUI, runs the ssh
    /// session through capture, and resumes.
    pub pending_ssh: Option<String>,
    pub den: Option<DenStats>,
}

impl App {
    pub fn new(store: Store) -> std::io::Result<App> {
        let mut app = App {
            store,
            running: true,
            view: View::Splash,
            mode: Mode::Normal,
            tick: 0,
            runs: Vec::new(),
            filter: String::new(),
            filtered: Vec::new(),
            selected: 0,
            table_state: TableState::default(),
            input: String::new(),
            status: None,
            hits: Vec::new(),
            hit_selected: 0,
            hits_state: TableState::default(),
            content_query: String::new(),
            viewer: None,
            peers: Vec::new(),
            peer_selected: 0,
            peers_state: TableState::default(),
            peers_err: None,
            qr: None,
            pending_ssh: None,
            den: None,
        };
        app.reload()?;
        Ok(app)
    }

    pub fn take_pending_ssh(&mut self) -> Option<String> {
        self.pending_ssh.take()
    }

    pub fn reload(&mut self) -> std::io::Result<()> {
        let keep = self.selected_meta().map(|m| m.id.clone());
        let mut runs = self.store.list()?;
        // Live/died captures aren't in the index yet; give them their
        // current log size so the list shows bytes accumulating.
        for mut meta in self.store.list_running()? {
            if let Ok(fm) = std::fs::metadata(self.store.output_path(&meta.id)) {
                meta.bytes = fm.len();
            }
            runs.push(meta);
        }
        runs.sort_by_key(|m| std::cmp::Reverse(m.started_ms)); // newest first
        self.runs = runs;
        self.apply_filter();
        // Auto-reloads shouldn't yank the cursor off the run it was on.
        if let Some(id) = keep {
            if let Some(pos) = self.filtered.iter().position(|&i| self.runs[i].id == id) {
                self.selected = pos;
            }
        }
        Ok(())
    }

    fn apply_filter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.filtered = self
            .runs
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                needle.is_empty()
                    || m.cmdline().to_lowercase().contains(&needle)
                    || m.cwd.to_lowercase().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    pub fn selected_meta(&self) -> Option<&RunMeta> {
        self.filtered.get(self.selected).map(|&i| &self.runs[i])
    }

    pub fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        // The list stays fresh on its own: new captures appear, spinners
        // spin, completed runs settle — once a second, off the input path.
        if self.view == View::List && self.mode == Mode::Normal && self.tick.is_multiple_of(10) {
            let _ = self.reload();
        }
        if self.view == View::Viewer {
            self.poll_live_viewer();
        }
    }

    /// Tail a live run: re-decode when the log grew, pin to bottom when
    /// following, and notice the capture finishing.
    fn poll_live_viewer(&mut self) {
        let Some(v) = self.viewer.as_mut() else {
            return;
        };
        if !v.live {
            return;
        }
        let size = std::fs::metadata(self.store.output_path(&v.meta.id))
            .map(|m| m.len())
            .unwrap_or(0);
        let grew = size != v.last_size;
        if grew {
            v.last_size = size;
            if let Ok((bytes, truncated)) = self.store.read_output(&v.meta.id, MAX_READ) {
                let (lines, stripped) = decode(normalize_cr(bytes));
                v.lines = lines;
                v.stripped = stripped;
                v.truncated = truncated;
                let query = v.query.clone();
                compute_matches(v, &query);
                if v.follow {
                    v.scroll = v.lines.len().saturating_sub(v.height.max(1));
                }
            }
        }
        // Poll completion at 1Hz (meta.json is tiny, but no need for 10Hz).
        if self.tick.is_multiple_of(10) {
            if let Some(meta) = self.store.load_meta(&v.meta.id) {
                if meta.state() != RunState::Running {
                    v.live = false;
                    v.meta = meta;
                }
            }
        }
    }

    // ------------------------------------------------------------- input --

    pub fn on_key(&mut self, code: KeyCode) {
        self.status = None; // any keypress clears a transient message
        match self.mode {
            Mode::Normal => self.on_key_normal(code),
            Mode::ConfirmDelete => self.on_key_confirm(code),
            _ => self.on_key_input(code),
        }
    }

    fn on_key_normal(&mut self, code: KeyCode) {
        if self.view == View::Splash {
            match code {
                KeyCode::Char('q') | KeyCode::Esc => self.running = false,
                _ => self.view = View::List,
            }
            return;
        }
        // An open QR overlay swallows the next key and closes.
        if self.qr.is_some() {
            self.qr = None;
            return;
        }
        match self.view {
            View::List => self.on_key_list(code),
            View::Results => self.on_key_results(code),
            View::Viewer => self.on_key_viewer(code),
            View::Raft => self.on_key_raft(code),
            View::Den => match code {
                KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('o') => self.view = View::List,
                _ => {}
            },
            View::Splash => unreachable!(),
        }
    }

    fn open_raft(&mut self) {
        match crate::fleet::peers() {
            Ok(peers) => {
                self.peers = peers;
                self.peers_err = None;
            }
            Err(e) => {
                self.peers.clear();
                self.peers_err = Some(e.to_string());
            }
        }
        self.peer_selected = self.peer_selected.min(self.peers.len().saturating_sub(1));
        self.view = View::Raft;
    }

    fn on_key_raft(&mut self, code: KeyCode) {
        let len = self.peers.len();
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.view = View::List,
            KeyCode::Char('j') | KeyCode::Down if len > 0 => {
                self.peer_selected = (self.peer_selected + 1).min(len - 1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.peer_selected = self.peer_selected.saturating_sub(1)
            }
            KeyCode::Char('g') => self.peer_selected = 0,
            KeyCode::Char('G') if len > 0 => self.peer_selected = len - 1,
            KeyCode::Char('r') => self.open_raft(),
            KeyCode::Enter => {
                let Some(peer) = self.peers.get(self.peer_selected) else {
                    return;
                };
                if peer.is_self {
                    self.status = Some("that's this machine — you're already aboard".into());
                } else if !peer.online {
                    self.status = Some(format!("{} is offline", peer.name));
                } else {
                    self.pending_ssh = Some(peer.ssh_target());
                }
            }
            // 'p' for phone: QR-encode the ssh URI so a phone terminal can
            // board the same machine by pointing a camera at the screen.
            KeyCode::Char('p') => {
                let Some(peer) = self.peers.get(self.peer_selected) else {
                    return;
                };
                match crate::fleet::qr_text(&peer.ssh_uri()) {
                    Ok(qr) => self.qr = Some((peer.ssh_uri(), qr)),
                    Err(e) => self.status = Some(format!("QR failed: {e}")),
                }
            }
            _ => {}
        }
    }

    fn on_key_list(&mut self, code: KeyCode) {
        let len = self.filtered.len();
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                // Esc clears an active filter before it quits.
                if code == KeyCode::Esc && !self.filter.is_empty() {
                    self.filter.clear();
                    self.apply_filter();
                } else {
                    self.running = false;
                }
            }
            KeyCode::Char('j') | KeyCode::Down if len > 0 => {
                self.selected = (self.selected + 1).min(len - 1);
            }
            KeyCode::Char('k') | KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char('g') => self.selected = 0,
            KeyCode::Char('G') if len > 0 => self.selected = len - 1,
            KeyCode::Enter => {
                if let Some(meta) = self.selected_meta().cloned() {
                    self.open_viewer(meta, None, None, false);
                }
            }
            KeyCode::Char('/') => {
                self.input = self.filter.clone();
                self.mode = Mode::FilterInput;
            }
            KeyCode::Char('s') => {
                self.input.clear();
                self.mode = Mode::SearchInput;
            }
            KeyCode::Char('x') if len > 0 => {
                if self.selected_meta().map(|m| m.state()) == Some(RunState::Running) {
                    self.status = Some("that run is still going — can't delete it".into());
                } else {
                    self.mode = Mode::ConfirmDelete;
                }
            }
            KeyCode::Char('r') => {
                if let Err(e) = self.reload() {
                    self.status = Some(format!("reload failed: {e}"));
                }
            }
            KeyCode::Char('R') => self.rerun_selected(),
            KeyCode::Char('t') => self.open_raft(),
            KeyCode::Char('o') => {
                self.den = Some(DenStats::compute(&self.runs));
                self.view = View::Den;
            }
            _ => {}
        }
    }

    /// Re-run the selected command by spawning a detached `otterm run` in
    /// the run's original cwd. It registers itself in the store, so it pops
    /// up in the list as a live run within a tick — watchable immediately.
    fn rerun_selected(&mut self) {
        let Some(meta) = self.selected_meta() else {
            return;
        };
        let (cmd, cwd) = (meta.cmd.clone(), meta.cwd.clone());
        let Ok(exe) = std::env::current_exe() else {
            self.status = Some("can't locate the otterm binary".into());
            return;
        };
        let spawned = std::process::Command::new(exe)
            .arg("run")
            .arg("--")
            .args(&cmd)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                // Reap in the background so no zombie lingers while the TUI runs.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                self.status = Some(format!("re-running: {}", cmd.join(" ")));
                let _ = self.reload();
                self.selected = 0; // the new run sorts to the top
            }
            Err(e) => self.status = Some(format!("re-run failed: {e}")),
        }
    }

    fn on_key_results(&mut self, code: KeyCode) {
        let len = self.hits.len();
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.view = View::List,
            KeyCode::Char('j') | KeyCode::Down if len > 0 => {
                self.hit_selected = (self.hit_selected + 1).min(len - 1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.hit_selected = self.hit_selected.saturating_sub(1)
            }
            KeyCode::Char('g') => self.hit_selected = 0,
            KeyCode::Char('G') if len > 0 => self.hit_selected = len - 1,
            KeyCode::Enter => {
                if let Some(hit) = self.hits.get(self.hit_selected) {
                    let (meta, line) = (hit.meta.clone(), hit.line);
                    let q = self.content_query.clone();
                    self.open_viewer(meta, Some(line), Some(q), true);
                }
            }
            _ => {}
        }
    }

    fn on_key_viewer(&mut self, code: KeyCode) {
        let Some(v) = self.viewer.as_mut() else {
            return;
        };
        // Max scroll leaves the last page filling the viewport, not a lone
        // final line at the top.
        let max = v.lines.len().saturating_sub(v.height.max(1));
        let page = v.height.max(1);
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.view = if v.from_results {
                    View::Results
                } else {
                    View::List
                };
                self.viewer = None;
            }
            // Manual scrolling unpins a live tail; G (or f) pins it again.
            KeyCode::Char('j') | KeyCode::Down => {
                v.scroll = (v.scroll + 1).min(max);
                v.follow = false;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                v.scroll = v.scroll.saturating_sub(1);
                v.follow = false;
            }
            KeyCode::Char('d') | KeyCode::PageDown => {
                v.scroll = (v.scroll + page / 2).min(max);
                v.follow = false;
            }
            KeyCode::Char('u') | KeyCode::PageUp => {
                v.scroll = v.scroll.saturating_sub(page / 2);
                v.follow = false;
            }
            KeyCode::Char('g') => {
                v.scroll = 0;
                v.follow = false;
            }
            KeyCode::Char('G') => {
                v.scroll = max;
                v.follow = v.live;
            }
            KeyCode::Char('f') if v.live => {
                v.follow = !v.follow;
                if v.follow {
                    v.scroll = max;
                }
            }
            KeyCode::Char('/') => {
                self.input = v.query.clone();
                self.mode = Mode::ViewerSearchInput;
            }
            KeyCode::Char('n') if !v.matches.is_empty() => {
                v.match_idx = (v.match_idx + 1) % v.matches.len();
                v.scroll = v.matches[v.match_idx].saturating_sub(3);
            }
            KeyCode::Char('N') if !v.matches.is_empty() => {
                v.match_idx = (v.match_idx + v.matches.len() - 1) % v.matches.len();
                v.scroll = v.matches[v.match_idx].saturating_sub(3);
            }
            _ => {}
        }
    }

    fn on_key_confirm(&mut self, code: KeyCode) {
        if code == KeyCode::Char('y') {
            if let Some(meta) = self.selected_meta() {
                let id = meta.id.clone();
                match self.store.delete(&id) {
                    Ok(()) => self.status = Some("run deleted".into()),
                    Err(e) => self.status = Some(format!("delete failed: {e}")),
                }
                let _ = self.reload();
            }
        }
        self.mode = Mode::Normal;
    }

    /// Shared text-input handling for the three prompt modes.
    fn on_key_input(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                if self.mode == Mode::FilterInput {
                    self.filter.clear();
                    self.apply_filter();
                }
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                let mode = self.mode;
                self.mode = Mode::Normal;
                match mode {
                    Mode::SearchInput => self.run_content_search(),
                    Mode::ViewerSearchInput => {
                        let q = self.input.clone();
                        self.set_viewer_query(&q);
                    }
                    _ => {} // filter is already applied incrementally
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.sync_incremental();
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.sync_incremental();
            }
            _ => {}
        }
    }

    /// Filtering is live-as-you-type; the search prompts commit on Enter.
    fn sync_incremental(&mut self) {
        if self.mode == Mode::FilterInput {
            self.filter = self.input.clone();
            self.apply_filter();
        }
    }

    // ------------------------------------------------------------ search --

    /// Scan every run's output (newest first) for the query. Runs are read
    /// tail-capped and escape-stripped; a few hits per run keeps the result
    /// list navigable instead of drowning in one chatty log.
    fn run_content_search(&mut self) {
        let query = self.input.trim().to_lowercase();
        if query.is_empty() {
            return;
        }
        self.content_query = query.clone();
        self.hits.clear();
        for meta in &self.runs {
            let Ok((bytes, _)) = self.store.read_output(&meta.id, MAX_READ) else {
                continue;
            };
            let stripped = strip_ansi_escapes::strip(normalize_cr(bytes));
            let text = String::from_utf8_lossy(&stripped);
            let mut per_run = 0;
            for (i, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(&query) {
                    self.hits.push(Hit {
                        meta: meta.clone(),
                        line: i,
                        text: line.trim_end().to_owned(),
                    });
                    per_run += 1;
                    if per_run >= HITS_PER_RUN {
                        break;
                    }
                }
            }
            if self.hits.len() >= MAX_HITS {
                break;
            }
        }
        self.hit_selected = 0;
        if self.hits.is_empty() {
            self.status = Some(format!("no runs mention '{}'", self.content_query));
        } else {
            self.view = View::Results;
        }
    }

    // ------------------------------------------------------------ viewer --

    pub fn open_viewer(
        &mut self,
        meta: RunMeta,
        jump_to: Option<usize>,
        query: Option<String>,
        from_results: bool,
    ) {
        let (bytes, truncated) = match self.store.read_output(&meta.id, MAX_READ) {
            Ok((b, t)) => (normalize_cr(b), t),
            Err(e) => {
                self.status = Some(format!("can't read output: {e}"));
                return;
            }
        };
        let last_size = bytes.len() as u64;
        let (lines, stripped) = decode(bytes);
        let live = meta.state() == RunState::Running;

        let mut viewer = Viewer {
            meta,
            lines,
            stripped,
            scroll: 0,
            query: String::new(),
            matches: Vec::new(),
            match_idx: 0,
            truncated,
            from_results,
            height: 24,
            live,
            // A live run opens pinned to the tail — that's what you came
            // to watch — unless you jumped to a specific search hit.
            follow: live && jump_to.is_none(),
            last_size,
        };
        if viewer.follow {
            viewer.scroll = viewer.lines.len().saturating_sub(viewer.height);
        }
        if let Some(q) = query {
            compute_matches(&mut viewer, &q);
        }
        if let Some(line) = jump_to {
            viewer.scroll = line.saturating_sub(3);
            // Sync n/N to the jumped-to match if it is one.
            if let Some(idx) = viewer.matches.iter().position(|&m| m == line) {
                viewer.match_idx = idx;
            }
        }
        self.viewer = Some(viewer);
        self.view = View::Viewer;
    }

    fn set_viewer_query(&mut self, query: &str) {
        let Some(v) = self.viewer.as_mut() else {
            return;
        };
        compute_matches(v, query);
        if v.matches.is_empty() {
            self.status = Some(if query.is_empty() {
                "search cleared".into()
            } else {
                format!("no matches for '{query}'")
            });
        } else {
            // Jump to the first match at or after the current position.
            v.match_idx = v.matches.iter().position(|&m| m >= v.scroll).unwrap_or(0);
            v.scroll = v.matches[v.match_idx].saturating_sub(3);
        }
    }
}

/// Decode captured bytes for the viewer: ANSI-styled lines for display and
/// escape-stripped strings for search. Falls back to plain lossy text on
/// malformed escape data rather than refusing to show the run.
fn decode(bytes: Vec<u8>) -> (Vec<Line<'static>>, Vec<String>) {
    let lines: Vec<Line<'static>> = bytes.into_text().map(|t| t.lines).unwrap_or_else(|_| {
        String::from_utf8_lossy(&bytes)
            .lines()
            .map(|l| Line::raw(l.to_owned()))
            .collect()
    });
    let plain = strip_ansi_escapes::strip(&bytes);
    let stripped: Vec<String> = String::from_utf8_lossy(&plain)
        .lines()
        .map(str::to_owned)
        .collect();
    (lines, stripped)
}

/// The pty records CRLF line endings, and ansi-to-tui keeps the `\r` inside
/// the decoded spans, which corrupts ratatui's cell diffing. Normalize before
/// any decode: `\r\n` → `\n`, and a lone `\r` (progress-bar style rewrites)
/// also → `\n` so each overwrite frame displays as its own line.
fn normalize_cr(mut bytes: Vec<u8>) -> Vec<u8> {
    let mut w = 0;
    for r in 0..bytes.len() {
        match bytes[r] {
            b'\r' if bytes.get(r + 1) == Some(&b'\n') => {} // drop, \n follows
            b'\r' => {
                bytes[w] = b'\n';
                w += 1;
            }
            b => {
                bytes[w] = b;
                w += 1;
            }
        }
    }
    bytes.truncate(w);
    bytes
}

fn compute_matches(v: &mut Viewer, query: &str) {
    v.query = query.to_lowercase();
    v.match_idx = 0;
    v.matches = if v.query.is_empty() {
        Vec::new()
    } else {
        v.stripped
            .iter()
            .enumerate()
            .filter(|(_, l)| l.to_lowercase().contains(&v.query))
            .map(|(i, _)| i)
            .collect()
    };
}

/// "3s", "5m", "2h", "4d" — how long ago a run started.
pub fn age(started_ms: u64) -> String {
    let s = now_ms().saturating_sub(started_ms) / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_cr;
    use ansi_to_tui::IntoText;

    /// Regression: pty CRLF endings must not leave `\r` in decoded spans —
    /// stray carriage returns corrupt ratatui's cell rendering.
    #[test]
    fn crlf_normalized_before_decode() {
        let bytes = b"deploy\r\nERROR: refused\r\nprogress 1\rprogress 2\r\n".to_vec();
        let text = normalize_cr(bytes).into_text().unwrap();
        let lines: Vec<String> = text
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(lines.iter().all(|l| !l.contains('\r')));
        // A lone \r (progress rewrite) becomes its own line.
        assert_eq!(
            lines[..4],
            ["deploy", "ERROR: refused", "progress 1", "progress 2"]
        );
    }
}

/// "84ms", "2.4s", "3m07s" — how long a run took.
pub fn duration(ms: u64) -> String {
    match ms {
        0..=999 => format!("{ms}ms"),
        1000..=59_999 => format!("{:.1}s", ms as f64 / 1000.0),
        _ => format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000),
    }
}
