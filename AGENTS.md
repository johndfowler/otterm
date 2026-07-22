# AGENTS.md — Otterm ~( o.o )~

## Project overview

Otterm ("the output librarian") is a single-binary Rust CLI. `otterm run --
<cmd>` runs a command on a pseudo-terminal so it keeps its colors, progress
bars, and prompts, mirrors the output to the real terminal in real time, and
captures every byte (ANSI escapes included) to an on-disk library. Bare
`otterm` opens a ratatui TUI to browse, search, re-run, and delete captures.
Since 0.3 the library is live: captures register the moment they start and
the viewer tails running ones. Since 0.5, `otterm serve` — "the lifeguard" —
streams any capture live to a browser over the tailnet (read-only
HTTP/WebSocket, QR splash on startup).

Key behaviors to preserve:

- `otterm run` exits with the child's exit code — it must stay transparent
  to scripts, CI, and `&&` chains (see `src/main.rs`).
- The pty is the point: captured commands must keep believing they're on a
  real terminal. Stdin is forwarded (interactive on a TTY, proper EOF when
  piped), SIGWINCH resizes propagate to the child pty (`src/capture.rs`).
- `otterm init zsh` prints `shell/otterm.zsh` (embedded via `include_str!`),
  which wraps eligible interactive commands in `otterm run --` via a zle
  `accept-line` widget ("ambient capture"). Only plain, single, external
  commands are wrapped; anything whose meaning wrapping would change (pipes,
  redirects, `&&`/`;`, builtins, functions, aliases, env-assignment
  prefixes) runs untouched. Opt-outs: leading space, `otterm-off`/
  `otterm-on`, `OTTERM_IGNORE`.

## Layout

- `src/main.rs` — clap CLI (`run`, `last`, `serve`, `init`; no subcommand →
  TUI), `print_last`.
- `src/capture.rs` — the pty tee loop: spawn via `portable-pty`, mirror to
  stdout, write the log per chunk, SIGWINCH forwarding, raw-mode guard,
  termios echo-off for non-interactive runs, footer (suppressed by
  `OTTERM_QUIET`).
- `src/store.rs` — on-disk store and `RunMeta`/`RunState`. Layout under the
  data dir: `index.jsonl` (append-only catalog, one `RunMeta` per line,
  rewritten on delete), `runs/<id>/output.log` + `runs/<id>/meta.json`,
  `running/<id>` marker files for in-flight captures. Run ids are
  `{millis:013}-{pid:05}`. `write_meta` writes `meta.json.tmp` and renames,
  so readers (TUI, lifeguard) never see a torn meta.
- `src/serve.rs` — the lifeguard: a read-only HTTP/WebSocket server (std
  `TcpListener`, one thread per connection). Routes: `/` (index of the
  den), `/watch/<id>` (terminal page), `/stream/<id>` (WS: replay, then
  live tail, then a done frame), `/assets/…`. Binds the Tailscale IPv4 by
  default (`detect_bind`), prints a QR of the URL on startup.
- `src/ws.rs` — just enough RFC 6455: the `Sec-WebSocket-Accept` handshake
  and a frame codec. The server sends unmasked binary/text frames and
  parses masked client frames for ping/pong/close; the read side is the
  future input seam.
- `web/` — the browser side: `watch.html` (the watch page, embedded via
  `include_str!`), `den.css`, and `wterm/` — the vendored wterm 0.3.0
  terminal (Apache-2.0, pinned from npm; regenerate by re-running the
  vendor commands).
- `src/fleet.rs` — "the raft": tailnet peers parsed from
  `tailscale status --json`, ssh URIs, unicode QR rendering. Test hook:
  `OTTERM_FAKE_PEERS="name=ip[,offline];..."`.
- `src/banner.rs` — const ASCII art (title, otter mascot, wave strip).
- `src/tui/` — the TUI: `mod.rs` (event loop, ~10 fps tick, hands the
  terminal to a child `otterm run -- ssh …` when boarding a peer),
  `app.rs` (state, input handling, search, live tailing, `DenStats`),
  `ui.rs` (all rendering), `theme.rs` (the otter color palette).
- `shell/otterm.zsh` — zsh ambient-capture integration.

Data dir: `$OTTERM_DATA_DIR` if set, else the platform data dir
(`~/Library/Application Support/otterm` on macOS, `~/.local/share/otterm`
on Linux). Logs over 16 MB (`MAX_READ` in `src/tui/app.rs`) are viewed and
searched tail-first; nothing is truncated on disk.

## Build and test

```sh
cargo build --release   # binary at target/release/otterm
cargo test              # 17 tests: fleet/tui units, the WS codec, and the
                        # lifeguard's HTTP + streaming endpoints over loopback
cargo clippy            # no config; one known warning today (items after the
                        # test module in tui/app.rs) — don't add new ones
```

There is no CI, no rustfmt/clippy config, and no integration-test harness —
`cargo test` is the whole suite. Release profile uses `lto`, one codegen
unit, and stripped symbols.

## Technology stack

Rust 2021. Direct dependencies: `ratatui` 0.29 + `crossterm` 0.28 (**kept in
lockstep** — ratatui 0.29 pins crossterm 0.28; bumping one without the other
puts two crossterm copies in the graph), `portable-pty` (pty spawning),
`ansi-to-tui` + `strip-ansi-escapes` (rendering/searching captures),
`clap` (derive), `serde`/`serde_json`, `dirs`, `qrcode`, `sha1` + `base64`
(the lifeguard's WebSocket handshake — the HTTP/WS server itself is std
`TcpListener`, no web framework). Unix-only: `signal-hook` (SIGWINCH) and
`libc` (termios, `kill(pid, 0)` liveness). Dev-dependencies `rqrr` + `image`
exist only for the QR roundtrip test. Targets macOS/Linux; Windows is not a
goal (pty and signal code are `cfg(unix)`).

## Code style guidelines

- Match the existing voice: module-level `//!` docs explaining *why* (not
  what), and short inline comments only where the code's intent isn't
  obvious. The codebase favors wry, otter-flavored naming ("the raft", "the
  den", `stay cool.`) — keep it, but never at the cost of clarity.
- 4-space indent, standard rustfmt style; run `cargo fmt` before finishing.
- Errors are plain `io::Result` with `io::Error::other(...)` for
  external-crate errors; no `anyhow`/`thiserror`.
- Terminal safety matters: raw mode / alternate screen must always be
  restored — use guards (`RawGuard` in `capture.rs`, `ratatui::init`'s
  panic hook in `tui/mod.rs`).
- The store tolerates corruption by design: `list()` skips bad index lines
  and vanished run dirs, `delete()` ignores already-missing files. Preserve
  that robustness; never let one bad run break the library.
- Old v0.2 metas lack `done`/`pid` — keep the `#[serde(default)]`
  back-compat in `RunMeta` and the legacy path in `state()`.
- The TUI is single-threaded by design; only the capture path spawns
  threads (stdin forwarder, SIGWINCH listener). `MasterPty` isn't `Sync` —
  it lives behind `Arc<Mutex<_>>` when shared.

## Testing instructions

- `cargo test` runs 17 tests: QR render→decode roundtrip, fake-peers
  parsing, and CRLF normalization before ANSI decode, plus the WS frame
  codec (`src/ws.rs`) and the lifeguard's HTTP + `/stream/<id>` streaming
  endpoint exercised over loopback (`src/serve.rs`). Add tests inline in a
  `#[cfg(test)] mod tests` in the module they cover.
- Manual verification is the norm for pty/TUI behavior:
  `cargo run -- run -- ls -la` (colors preserved, exit code propagated),
  `cargo run` for the TUI, `OTTERM_DATA_DIR=$(mktemp -d)` to sandbox the
  library, `OTTERM_FAKE_PEERS` to fake a tailnet for the Raft view.
- Verify `otterm run` transparency: `otterm run -- false; echo $?` must be
  the child's code, and `otterm run -- cat <<< hi` must not capture the pty
  echo of the VEOF char.
- Verify the lifeguard by hand: `otterm serve` (QR splash on startup), open
  the URL in a browser, click a live run and watch it tick; from a phone on
  the tailnet, scan the QR. Sandbox with `OTTERM_DATA_DIR=$(mktemp -d)` and
  `--bind 127.0.0.1` when you don't want the tailnet involved.

## Security considerations

- The store executes nothing it reads; captures are data. Treat
  `output.log` as untrusted terminal output — it is only ever written to a
  terminal or decoded with `ansi-to-tui`, never eval'd.
- The zsh hook must never alter semantics of what the user typed — when in
  doubt, don't wrap (see the bail-out list in `shell/otterm.zsh`).
- `fleet.rs` reads `tailscale status --json` and launches `ssh` with
  names/addresses from it; don't build shell strings — spawn
  `Command` with argv, as the code does.
- The lifeguard (`src/serve.rs`) is read-only: it never mutates capture
  data. The one store write on the serving path is the index page's
  `Store::list_running()` garbage-collecting leaked `running/<id>` markers
  — the same sweep the TUI does. It binds this machine's Tailscale IPv4 by
  default (loopback only as an explicit `--bind`), and `valid_id` gates
  every run id before it touches the filesystem (no `../` games); captured
  command lines are HTML-escaped on the index page.
- No credentials, no telemetry. The only network code is the lifeguard,
  and it only listens.
