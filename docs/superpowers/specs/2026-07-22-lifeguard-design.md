# The Lifeguard — live capture streaming over the tailnet

Otterm 0.5.0 feature spec. Brainstormed 2026-07-22.

## Why

Otterm already captures every byte of a running command and the TUI already
tails it live — but only on the machine where it runs. The poolside thesis
("heavy job on the cool machine indoors, watched from the phone by the pool")
needs a window into a running capture from another device. asciinema 3.0
shipped exactly this (`stream`) and ttyd proved the read-only-web-terminal
shape; the lifeguard is otterm's version, served straight from the den over
the tailnet.

Decisions locked in during brainstorming:

- **Read-only now, input-ready.** Browsers only watch. The protocol and
  server shape leave a natural landing spot for client input later.
- **Whole-den web index.** One server, one port: index page lists running
  captures (live dot) and the library; each run opens a watch page.
- **wterm rendering.** [`vercel-labs/wterm`](https://github.com/vercel-labs/wterm)
  (Chris Tate / Vercel Labs, Apache-2.0): a Zig→WASM VT core (~12 KB) with a
  vanilla-JS DOM renderer — native text selection, copy/paste, browser find,
  alternate screen (vim/htop render correctly), CSS-variable theming.
  Vendored from the `@wterm/dom` npm tarball (pinned 0.3.0), so no Zig/pnpm
  toolchain touches otterm's build.
- **WebSocket transport.** wterm's native transport (binary framing,
  built-in reconnection), genuinely bidirectional for the input-ready
  future.

## CLI surface

```
otterm serve [--port N] [--bind IP]
```

- Defaults: port `7777`; bind address auto-detected via `tailscale ip -4`,
  falling back to `127.0.0.1` with a printed heads-up when no tailnet is
  present.
- On startup: prints the URL and renders a QR code (existing `qrcode`
  crate) pointing at the served address — phone scans straight into the den.
- `OTTERM_QUIET` suppresses the splash/QR, matching `run`.

## Server architecture (`src/serve.rs`, new)

Hand-rolled HTTP + WebSocket server on `std::net::TcpListener`,
thread-per-connection. No async runtime, no framework.

Routes (GET only):

- `GET /` — index page (running captures highlighted, library below)
- `GET /watch/<id>` — watch page for one run
- `GET /stream/<id>` — WebSocket upgrade; the byte stream for one run
- `GET /assets/*` — vendored wterm JS + wasm, embedded via `include_bytes!`
  (same trick as `include_str!` for the zsh script)

Everything else → 404.

New dependencies stay tiny: `sha1` + `base64` for the RFC 6455 handshake
(both zero-transitive-dependency, well-audited). Frame encode/decode is our
own code, covered by unit tests.

## Streaming protocol

Binary WebSocket frames carry raw capture bytes (ANSI escapes included),
matching `@wterm/core`'s transport expectations.

On connect to `/stream/<id>`:

1. Replay `output.log` from the start. For logs over the viewer's
   `MAX_READ`-style threshold, replay tail-first like the TUI does.
2. If the run is live (`running/<id>` marker exists), follow the file and
   send appends as they land, polling on the same ~100 ms cadence as the
   TUI tick.
3. When the run is finished, send one JSON text control frame
   (`{"done": true, "exit": N}`) and keep the socket open until the client
   leaves, so the page can show the footer.

The read side of the socket handles masking, ping/pong, and close frames
even though no client data is expected. A future `{"input": ...}` client
frame has a defined place to land — that is the input-ready seam; wiring it
to a pty is explicitly out of scope for 0.5.0.

## Web frontend (`web/`, vendored)

- `web/` holds the index page, watch page, CSS (otter palette via CSS
  custom properties — wterm themes are CSS vars), the `@wterm/dom` bundle,
  `wterm.wasm`, and wterm's LICENSE. Pinned version, committed to the repo.
- The index page is static HTML; the run list is server-rendered into it
  per request (the store is the source of truth; no client-side fetch of
  run metadata needed for v1).
- Running captures show a live dot; clicking a run opens its watch page.
- All assets are embedded in the binary — `otterm` stays a single file
  with zero runtime files.

## Security posture

- Read-only by construction: the server never writes to the store and
  never executes anything; captures are served as data under the same
  trust model as the TUI.
- Bind defaults to the Tailscale interface address (encrypted wire;
  tailnet ACLs are the auth). Never binds `0.0.0.0` silently — a wider
  bind requires an explicit `--bind` and prints a loud note.
- No TLS: the tailnet already encrypts. Serving plaintext HTTP on a
  non-tailnet bind is the user's explicit choice (with the loud note).

## Error handling & robustness

- Store corruption tolerance is preserved: bad index lines never break the
  index page; a run dir that vanishes mid-stream just triggers the done
  frame and close.
- Client disconnects are detected on write failure; the connection thread
  exits. wterm's transport reconnects on its own.
- The server is a pure reader — no raw mode, no pty, no alternate screen —
  so the terminal-safety concerns of `capture.rs` don't apply.
- Old v0.2 metas (no `done`/`pid`) render fine via the existing
  `RunState` legacy path.

## Testing

Unit tests inline in `src/serve.rs` (`#[cfg(test)] mod tests`):

- WebSocket accept key against the RFC 6455 test vector.
- Frame encode/decode roundtrip: masked client frames, fragmentation,
  ping/pong, close.
- Store-fixture test: a finished run streams fully and terminates with the
  done frame.

Manual verification (per project norms; pty/browser behavior is verified
by hand):

- `OTTERM_DATA_DIR=$(mktemp -d) cargo run -- serve`, then start a live
  capture, e.g. `cargo run -- run -- bash -c 'for i in $(seq 50); do echo $i; sleep 1; done'`,
  and watch it update in a desktop browser.
- Repeat from a phone over the tailnet via the startup QR.
- Verify a full-screen capture (e.g. `htop`) renders correctly through
  wterm's alternate-screen support.

Before finishing: `cargo fmt`, `cargo clippy` (no new warnings),
`cargo test` green. README and AGENTS.md updated for the new subcommand.

## Explicit non-goals for 0.5.0

- Client input to the running command (seam exists; wiring is later).
- Static HTML export of a run (nice later freebie reusing the watch page).
- TLS, auth tokens, multi-user anything — tailnet ACLs are the boundary.
- Raft deepening, library polish, bash/fish hooks — separate sub-projects
  with their own spec → plan cycles.
