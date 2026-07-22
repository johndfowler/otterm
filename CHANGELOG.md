# Changelog

All notable changes to otterm are documented here, in the style of
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] - 2026-07-22

The lifeguard: watch any capture live in a browser over the tailnet.

### Added

- `otterm serve [--port N] [--bind IP]` — a read-only HTTP/WebSocket server
  (std `TcpListener`, one thread per connection, no web framework) streaming
  any capture to a browser. Routes: `/` (index of the den), `/watch/<id>`
  (terminal page), `/stream/<id>` (WebSocket: replay, then live tail, then a
  done frame), `/assets/…`.
- Just enough RFC 6455 in `src/ws.rs`: the `Sec-WebSocket-Accept` handshake
  and a frame codec with roundtrip tests.
- The browser side in `web/`: `watch.html`, `den.css`, and a vendored,
  pinned wterm 0.3.0 terminal (Apache-2.0).
- Tailnet bind detection (`detect_bind`): the lifeguard binds this machine's
  Tailscale IPv4 by default, prints a QR of the URL on startup, and warns on
  all-interfaces binds (`0.0.0.0`, `[::]`) — loopback only as an explicit
  `--bind`.

### Changed

- `meta.json` writes are now atomic (write-tmp-then-rename), so readers —
  the TUI, the lifeguard — never see a torn meta.

## [0.4.0] - 2026-07-22

The raft, the den, and a vibe pass — stay cool.

### Added

- Ambient capture for zsh via `eval "$(otterm init zsh)"`: a zle
  `accept-line` widget wraps eligible interactive commands in
  `otterm run --` automatically. Only plain, single, external commands are
  wrapped — pipes, redirects, `&&`/`;`, builtins, functions, aliases, and
  env-assignment prefixes run untouched. Opt out per command with a leading
  space, per session with `otterm-off` / `otterm-on`, or extend the
  blocklist with `OTTERM_IGNORE`.
- The raft (`t` in the TUI): tailnet peers parsed from
  `tailscale status --json`. `Enter` boards one — the TUI steps aside and an
  `ssh` session runs through the capture engine, archived like any other
  run. `p` shows a QR of the `ssh://user@host` URI for boarding from a phone.
- The den (`o` in the TUI): capture stats — runs, bytes archived, success
  rate, comfort command — presided over by 🦦.
- Animated otter mascot and wave strip (`src/banner.rs`, `src/tui/theme.rs`).
- `AGENTS.md` contributor/architecture notes.

## [0.3.0] - 2026-07-22

The library is live.

### Added

- Captures register the moment they start: `running/<id>` marker files, a
  spinner and real-time byte count in the list, and a viewer that tails
  running captures as they grow (`f` toggles follow).
- Captures whose process died before finishing are flagged with `!`.
- `R` re-runs any past command, in its original directory, as a new live
  capture.
- Full-text search (`s`) across all captured output; results open scrolled
  to the matching line.

### Changed

- The on-disk library grew `index.jsonl` (append-only catalog) and
  `running/` markers alongside `runs/<id>/output.log` + `meta.json`; old
  v0.2 metas without `done`/`pid` are still read via serde defaults.

[0.5.0]: https://github.com/johndfowler/otterm/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/johndfowler/otterm/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/johndfowler/otterm/releases/tag/v0.3.0
