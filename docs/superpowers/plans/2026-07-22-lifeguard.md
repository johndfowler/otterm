# The Lifeguard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `otterm serve` — a read-only HTTP/WebSocket server that streams any capture (live or finished) to a browser on the tailnet, rendered by the vendored wterm terminal.

**Architecture:** Hand-rolled HTTP + WebSocket (RFC 6455) server on `std::net::TcpListener`, thread-per-connection, no async runtime. Binary WS frames carry raw capture bytes (replay `output.log`, then tail it on a 100 ms tick); one JSON text frame signals completion. The browser runs vendored `@wterm/dom` 0.3.0 (Zig→WASM core, embedded as base64 in the JS) fed by `@wterm/core`'s `WebSocketTransport`.

**Tech Stack:** Rust 2021 (std only + `sha1` + `base64`), vendored wterm 0.3.0 assets (Apache-2.0), existing `qrcode` crate for the startup QR.

**Spec:** `docs/superpowers/specs/2026-07-22-lifeguard-design.md`

## Global Constraints

- `publish = false` stays in `Cargo.toml` (crates.io guard; the name is already reserved by the yanked 0.4.0).
- New dependencies: exactly `sha1 = "0.10"` and `base64 = "0.22"`. Nothing else.
- Read-only by construction: the server never writes to the store, never executes anything.
- Never bind `0.0.0.0`/`::` silently — only via explicit `--bind`, with a loud warning.
- Errors are plain `io::Result` with `io::Error::other(...)`; no anyhow/thiserror.
- 4-space indent, standard rustfmt; `cargo fmt` + `cargo clippy` clean (one pre-existing clippy warning in `tui/app.rs` is tolerated — add none).
- Tests inline in `#[cfg(test)] mod tests` in the module they cover.
- Run ids validate as `[0-9-]+` before any filesystem or store access (`valid_id`).
- Commit after every task; do not push.

---

### Task 1: WebSocket handshake (`src/ws.rs`)

**Files:**
- Create: `src/ws.rs`
- Modify: `Cargo.toml` (add deps), `src/main.rs:4-8` (module list)

**Interfaces:**
- Consumes: nothing (leaf module).
- Produces: `ws::accept_key(client_key: &str) -> String` — used by `serve.rs` in Task 5.

- [ ] **Step 1: Add the two dependencies**

In `Cargo.toml`, append to `[dependencies]` (after the `qrcode` line):

```toml
# WebSocket handshake for the lifeguard's live streaming (RFC 6455)
sha1 = "0.10"
base64 = "0.22"
```

- [ ] **Step 2: Write the failing test**

Create `src/ws.rs`:

```rust
//! Just enough WebSocket (RFC 6455) for the lifeguard: the server sends
//! unmasked binary and text frames, and parses masked client frames to
//! honor ping/pong/close. The read side is the future input seam.

/// Largest client frame we bother with; watchers only send pongs and
/// (one day) input. Anything bigger is a bug or an attack.
pub const MAX_MESSAGE: usize = 1 << 20;

pub const OP_TEXT: u8 = 0x1;
pub const OP_BINARY: u8 = 0x2;
pub const OP_CLOSE: u8 = 0x8;
pub const OP_PING: u8 = 0x9;
pub const OP_PONG: u8 = 0xA;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_key_matches_rfc6455_vector() {
        // The worked example from RFC 6455 §1.3.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test accept_key 2>&1 | tail -5`
Expected: compile error — `cannot find function accept_key in this scope`

- [ ] **Step 4: Implement `accept_key`**

Add above the test module in `src/ws.rs`:

```rust
/// Sec-WebSocket-Accept per RFC 6455 §4.2.2:
/// base64(sha1(client_key + the well-known GUID)).
pub fn accept_key(client_key: &str) -> String {
    use base64::Engine;
    use sha1::Digest;
    let mut h = sha1::Sha1::new();
    h.update(client_key.trim().as_bytes());
    h.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64::engine::general_purpose::STANDARD.encode(h.finalize())
}
```

Register the module in `src/main.rs` — change:

```rust
mod banner;
mod capture;
mod fleet;
mod store;
mod tui;
```

to:

```rust
mod banner;
mod capture;
mod fleet;
mod store;
mod tui;
mod ws;
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test accept_key 2>&1 | tail -5`
Expected: `test ws::tests::accept_key_matches_rfc6455_vector ... ok`

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/ws.rs src/main.rs
git commit -m "Lifeguard: WebSocket handshake (RFC 6455 accept key)"
```

---

### Task 2: WebSocket frame codec

**Files:**
- Modify: `src/ws.rs`

**Interfaces:**
- Consumes: Task 1's module and opcode constants.
- Produces (used by `serve.rs` in Tasks 4-5):
  - `ws::Frame { pub fin: bool, pub opcode: u8, pub payload: Vec<u8> }`
  - `ws::encode(opcode: u8, payload: &[u8]) -> Vec<u8>` — one unmasked FIN server frame
  - `ws::parse_frame(buf: &[u8]) -> io::Result<Option<(Frame, usize)>>` — `Ok(None)` when `buf` doesn't hold a complete frame yet; `Ok(Some((frame, consumed)))` otherwise; `Err` on protocol violation
  - `ws::MAX_MESSAGE: usize`

- [ ] **Step 1: Write the failing tests**

Replace the test module in `src/ws.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Build a masked client frame the way a browser sends it.
    fn masked_frame(fin: bool, op: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![if fin { 0x80 } else { 0 } | op, 0x80 | payload.len() as u8];
        v.extend_from_slice(&[1, 2, 3, 4]); // mask key
        for (i, b) in payload.iter().enumerate() {
            v.push(b ^ [1u8, 2, 3, 4][i % 4]);
        }
        v
    }

    #[test]
    fn accept_key_matches_rfc6455_vector() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn encode_roundtrips_through_parse() {
        let bytes = encode(OP_BINARY, b"hi");
        assert_eq!(&bytes[..2], &[0x82, 0x02]); // FIN+binary, len 2
        let (frame, used) = parse_frame(&bytes).unwrap().unwrap();
        assert!(frame.fin);
        assert_eq!(frame.opcode, OP_BINARY);
        assert_eq!(frame.payload, b"hi");
        assert_eq!(used, bytes.len());
    }

    #[test]
    fn parse_unmasks_client_frames() {
        let (frame, _) = parse_frame(&masked_frame(true, OP_PING, b"yo")).unwrap().unwrap();
        assert_eq!(frame.opcode, OP_PING);
        assert_eq!(frame.payload, b"yo");
    }

    #[test]
    fn incomplete_frame_is_none_not_error() {
        let mut partial = encode(OP_BINARY, b"hello world");
        partial.truncate(5);
        assert!(parse_frame(&partial).unwrap().is_none());
        assert!(parse_frame(&[]).unwrap().is_none());
        assert!(parse_frame(&[0x82]).unwrap().is_none());
    }

    #[test]
    fn extended_lengths() {
        let big = vec![7u8; 300];
        let (frame, used) = parse_frame(&encode(OP_BINARY, &big)).unwrap().unwrap();
        assert_eq!(frame.payload.len(), 300);
        assert_eq!(used, 300 + 4); // 2 header + 2 extended len
        let huge = vec![8u8; 70_000];
        let (frame, _) = parse_frame(&encode(OP_BINARY, &huge)).unwrap().unwrap();
        assert_eq!(frame.payload.len(), 70_000);
    }

    #[test]
    fn oversized_frame_is_an_error() {
        // Declared length above MAX_MESSAGE, no payload needed.
        let buf = [0x82, 127, 0, 0, 0, 0, 0, 0, 0x20, 0x00]; // 2 MiB
        assert!(parse_frame(&buf).is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test ws:: 2>&1 | tail -5`
Expected: compile error — `cannot find function encode in this scope`

- [ ] **Step 3: Implement the codec**

Add below `accept_key` in `src/ws.rs`:

```rust
pub struct Frame {
    pub fin: bool,
    pub opcode: u8,
    pub payload: Vec<u8>,
}

/// Encode one unmasked FIN server frame.
pub fn encode(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    out.push(0x80 | opcode);
    let len = payload.len();
    if len < 126 {
        out.push(len as u8);
    } else if len <= u16::MAX as usize {
        out.push(126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

/// Parse one complete frame from the front of `buf`, unmasking client
/// frames. `Ok(None)` means "not enough bytes yet — keep reading".
pub fn parse_frame(buf: &[u8]) -> io::Result<Option<(Frame, usize)>> {
    if buf.len() < 2 {
        return Ok(None);
    }
    let fin = buf[0] & 0x80 != 0;
    let opcode = buf[0] & 0x0f;
    let masked = buf[1] & 0x80 != 0;
    let mut len = (buf[1] & 0x7f) as u64;
    let mut head = 2;
    if len == 126 {
        if buf.len() < 4 {
            return Ok(None);
        }
        len = u16::from_be_bytes([buf[2], buf[3]]) as u64;
        head = 4;
    } else if len == 127 {
        if buf.len() < 10 {
            return Ok(None);
        }
        len = u64::from_be_bytes(buf[2..10].try_into().unwrap());
        head = 10;
    }
    if len > MAX_MESSAGE as u64 {
        return Err(io::Error::other("ws frame too large"));
    }
    let mask_len = if masked { 4 } else { 0 };
    let total = head + mask_len + len as usize;
    if buf.len() < total {
        return Ok(None);
    }
    let mut payload = buf[head + mask_len..total].to_vec();
    if masked {
        let key = buf[head..head + 4].to_vec();
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= key[i % 4];
        }
    }
    Ok(Some((Frame { fin, opcode, payload }, total)))
}
```

(`use std::io;` goes at the top of the file with the other items — add it above the constants.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test ws:: 2>&1 | tail -10`
Expected: 6 tests pass (including the Task 1 vector)

- [ ] **Step 5: Commit**

```bash
git add src/ws.rs
git commit -m "Lifeguard: WebSocket frame codec with roundtrip tests"
```

---

### Task 3: Vendor wterm assets and the web pages

**Files:**
- Create: `web/wterm/dom/{index,wterm,debug,input,renderer}.js`, `web/wterm/dom/terminal.css`, `web/wterm/core/{index,transport,wasm-bridge,wasm-inline,terminal-core}.js`, `web/wterm/LICENSE`
- Create: `web/watch.html`, `web/den.css`

**Interfaces:**
- Consumes: `@wterm/dom` 0.3.0 and `@wterm/core` 0.3.0 npm tarballs.
- Produces: files served by `serve.rs` in Task 4 at `/assets/...` (exact names below). `watch.html` connects to `ws://<host>/stream/<id>` where `<id>` is the last URL path segment; binary WS frames go to the terminal, a JSON text frame `{"done":true,"exit":N}` marks completion.

- [ ] **Step 1: Download and vendor the pinned wterm 0.3.0 files**

Run from the repo root:

```bash
set -e
mkdir -p web/wterm/dom web/wterm/core
work=$(mktemp -d)
curl -sL https://registry.npmjs.org/@wterm/dom/-/dom-0.3.0.tgz | tar xz -C "$work"
mv "$work/package" "$work/dom"
curl -sL https://registry.npmjs.org/@wterm/core/-/core-0.3.0.tgz | tar xz -C "$work"
mv "$work/package" "$work/core"
cp "$work"/dom/dist/{index,wterm,debug,input,renderer}.js web/wterm/dom/
cp "$work"/dom/src/terminal.css web/wterm/dom/
cp "$work"/core/dist/{index,transport,wasm-bridge,wasm-inline,terminal-core}.js web/wterm/core/
cp "$work"/dom/LICENSE web/wterm/LICENSE
rm -rf "$work"
```

- [ ] **Step 2: Verify the vendored files are what we expect**

Run: `grep -c WASM_BASE64 web/wterm/core/wasm-inline.js && grep "export { WTerm }" web/wterm/dom/index.js && grep -c "import { WasmBridge } from \"@wterm/core\"" web/wterm/dom/wterm.js`
Expected: `1` (wasm embedded as base64 — no separate .wasm file needed), the WTerm export line, `1` (bare `@wterm/core` import — resolved by the import map in watch.html)

- [ ] **Step 3: Write `web/watch.html`**

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>otterm — watching</title>
<link rel="stylesheet" href="/assets/dom/terminal.css">
<link rel="stylesheet" href="/assets/den.css">
</head>
<body>
<header>🦦 <span id="state" class="live">● live</span></header>
<div id="terminal" class="wterm" role="textbox" aria-label="Captured command output" aria-multiline="true" aria-roledescription="terminal"></div>
<footer id="footer"></footer>
<script type="importmap">
{ "imports": { "@wterm/core": "/assets/core/index.js" } }
</script>
<script type="module">
import { WTerm } from '/assets/dom/index.js';
import { WebSocketTransport } from '/assets/core/index.js';

const id = location.pathname.split('/').pop();
const stateEl = document.getElementById('state');
const footerEl = document.getElementById('footer');

const term = new WTerm(document.getElementById('terminal'), { autoResize: true });
await term.init();

const proto = location.protocol === 'https:' ? 'wss' : 'ws';
const ws = new WebSocketTransport({
  url: `${proto}://${location.host}/stream/${id}`,
  onData(data) {
    if (typeof data === 'string') {
      // control frame — byte frames always arrive binary
      const msg = JSON.parse(data);
      if (msg.done) {
        stateEl.textContent = msg.exit === 0 ? '✓ done' : `✗ exit ${msg.exit ?? '?'}`;
        stateEl.className = msg.exit === 0 ? 'ok' : 'err';
        footerEl.textContent = 'stay cool. ~( o.o )~';
      }
    } else {
      term.write(data);
    }
  },
  onClose() {
    if (stateEl.className === 'live') stateEl.textContent = '… reconnecting';
  },
});
ws.connect();
</script>
</body>
</html>
```

- [ ] **Step 4: Write `web/den.css`** (the otter palette from `src/tui/theme.rs`)

```css
/* The den's web colors — keep in sync with src/tui/theme.rs */
:root {
  --fur: #c98d5c;
  --fur-dark: #6e4a34;
  --cream: #eee2ca;
  --river: #56a8b2;
  --deep: #1e3a46;
  --clam: #e4a060;
  --ok: #78be78;
  --err: #e06c60;
}
body {
  background: var(--deep);
  color: var(--cream);
  font-family: system-ui, sans-serif;
  margin: 0 auto;
  padding: 24px;
  max-width: 900px;
}
header { color: var(--river); font-size: 18px; margin-bottom: 16px; }
header .sub { font-size: 13px; }
a.run {
  display: flex;
  gap: 10px;
  align-items: baseline;
  padding: 8px 12px;
  border-radius: 8px;
  text-decoration: none;
  color: var(--cream);
}
a.run:hover { background: rgba(86, 168, 178, 0.15); }
a.run code { color: var(--clam); }
a.run .meta { margin-left: auto; color: var(--river); font-size: 12px; }
.live { color: var(--ok); }
.ok { color: var(--ok); }
.err { color: var(--err); }
.empty { color: var(--river); }
#terminal { margin-top: 12px; }
footer { color: var(--fur); margin-top: 12px; font-style: italic; }
/* otter-flavored wterm theme (wterm themes are CSS custom properties) */
.wterm { --term-bg: #16303b; --term-fg: #eee2ca; --term-cursor: #56a8b2; }
```

- [ ] **Step 5: Commit**

```bash
git add web/
git commit -m "Lifeguard: vendored wterm 0.3.0 + den web pages"
```

---

### Task 4: HTTP server core (`src/serve.rs`)

**Files:**
- Create: `src/serve.rs`
- Modify: `src/store.rs` (add `open_at`), `src/main.rs:4-9` (add `mod serve;`)

**Interfaces:**
- Consumes: `Store`, `RunMeta`, `RunState` from `store.rs`; `web/` assets from Task 3.
- Produces:
  - `Store::open_at(root: PathBuf) -> io::Result<Store>` (test fixture constructor, used by Tasks 4-5 tests)
  - `serve::serve(listener: TcpListener, store: &Store) -> io::Result<()>` (accept loop; used by Task 6)
  - `serve::handle_conn(stream: TcpStream, store: &Store) -> io::Result<()>` (single connection; the unit tests drive this directly over loopback)
  - `serve::valid_id(id: &str) -> bool`
  - `serve::esc(s: &str) -> String`
  - `serve::render_index(store: &Store) -> String`
  - Routes: `GET /` index, `GET /watch/<id>` → `web/watch.html`, `GET /assets/<name>` → vendored files, everything else 404. `/stream/<id>` returns 400 until Task 5 wires it.

- [ ] **Step 1: Add `Store::open_at` (test fixture support)**

In `src/store.rs`, change `Store::open` to delegate:

```rust
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
```

- [ ] **Step 2: Write the failing tests**

Create `src/serve.rs`:

```rust
//! The lifeguard: serve the den over HTTP/WebSocket so a capture can be
//! watched live from any browser on the tailnet. Read-only by
//! construction — the server never writes to the store, and captures are
//! served as data under the same trust model as the TUI.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use crate::store::{RunState, Store};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::RunMeta;
    use std::fs;
    use std::io::Read;
    use std::path::PathBuf;

    /// A store rooted in a fresh temp dir; returns the root for cleanup.
    fn fixture(name: &str) -> (Store, PathBuf) {
        let root = std::env::temp_dir()
            .join(format!("otterm-serve-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        (Store::open_at(root.clone()).unwrap(), root)
    }

    fn done_meta(store: &Store, cmdline: &str) -> RunMeta {
        let meta = RunMeta {
            id: store.new_id(),
            cmd: cmdline.split(' ').map(String::from).collect(),
            cwd: "/tmp".into(),
            started_ms: crate::store::now_ms(),
            duration_ms: 5,
            exit_code: Some(0),
            bytes: 3,
            done: true,
            pid: None,
        };
        fs::create_dir_all(store.run_dir(&meta.id)).unwrap();
        fs::write(store.output_path(&meta.id), b"abc").unwrap();
        meta
    }

    /// Fire one raw request at a handler running on loopback.
    fn request(store: &Store, raw: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::scope(|scope| {
            scope.spawn(|| {
                let (stream, _) = listener.accept().unwrap();
                handle_conn(stream, store).unwrap();
            });
            let mut c = TcpStream::connect(addr).unwrap();
            c.write_all(raw.as_bytes()).unwrap();
            let mut out = String::new();
            c.read_to_string(&mut out).unwrap();
            out
        })
    }

    #[test]
    fn esc_escapes_html() {
        assert_eq!(esc("<a&\"b\">"), "&lt;a&amp;&quot;b&quot;&gt;");
    }

    #[test]
    fn valid_id_accepts_only_run_id_chars() {
        assert!(valid_id("0170123456789-00123"));
        assert!(!valid_id("../etc/passwd"));
        assert!(!valid_id(""));
        assert!(!valid_id("abc"));
    }

    #[test]
    fn index_lists_runs_newest_first() {
        let (store, root) = fixture("index");
        let older = done_meta(&store, "echo older");
        store.record(&older).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2)); // distinct ids
        let newer = done_meta(&store, "echo newer");
        store.record(&newer).unwrap();
        let html = render_index(&store);
        assert!(html.contains("echo older"));
        assert!(html.contains(&format!("/watch/{}", newer.id)));
        assert!(html.find("echo newer") < html.find("echo older"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn http_routes() {
        let (store, root) = fixture("routes");
        let meta = done_meta(&store, "ls -la");
        store.record(&meta).unwrap();

        let index = request(&store, "GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(index.starts_with("HTTP/1.1 200"), "{index}");
        assert!(index.contains("ls -la"));

        let watch = request(&store, &format!("GET /watch/{} HTTP/1.1\r\nHost: x\r\n\r\n", meta.id));
        assert!(watch.contains("wterm"), "{watch}");

        let js = request(&store, "GET /assets/core/index.js HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(js.contains("content-type: text/javascript"), "{js}");

        let css = request(&store, "GET /assets/den.css HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(css.contains("--deep"), "{css}");

        let missing = request(&store, "GET /nope HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");

        let traversal = request(&store, "GET /assets/../Cargo.toml HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(traversal.starts_with("HTTP/1.1 404"), "{traversal}");

        fs::remove_dir_all(root).unwrap();
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test serve:: 2>&1 | tail -5`
Expected: compile errors — `cannot find function esc in this scope`, etc.

- [ ] **Step 4: Implement the server core**

Add to `src/serve.rs` above the test module:

```rust
/// Run ids are `{millis:013}-{pid:05}` — anything else is not a run and
/// must never reach the filesystem (no `../` games).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit() || b == b'-')
}

/// HTML-escape user-controlled text (captured command lines).
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const INDEX_HEAD: &str = "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n\
<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<title>otterm — the den</title>\n\
<link rel=\"stylesheet\" href=\"/assets/den.css\">\n\
</head>\n<body>\n\
<header>🦦 the den <span class=sub>— live from the lifeguard. stay cool.</span></header>\n";

const INDEX_TAIL: &str = "</body>\n</html>\n";

const WATCH_HTML: &str = include_str!("../web/watch.html");

/// The vendored wterm build (Apache-2.0, pinned 0.3.0 — see web/wterm/LICENSE).
fn asset(name: &str) -> Option<(&'static [u8], &'static str)> {
    const JS: &str = "text/javascript";
    Some(match name {
        "core/index.js" => (include_bytes!("../web/wterm/core/index.js").as_slice(), JS),
        "core/transport.js" => (include_bytes!("../web/wterm/core/transport.js").as_slice(), JS),
        "core/wasm-bridge.js" => (include_bytes!("../web/wterm/core/wasm-bridge.js").as_slice(), JS),
        "core/wasm-inline.js" => (include_bytes!("../web/wterm/core/wasm-inline.js").as_slice(), JS),
        "core/terminal-core.js" => (include_bytes!("../web/wterm/core/terminal-core.js").as_slice(), JS),
        "dom/index.js" => (include_bytes!("../web/wterm/dom/index.js").as_slice(), JS),
        "dom/wterm.js" => (include_bytes!("../web/wterm/dom/wterm.js").as_slice(), JS),
        "dom/debug.js" => (include_bytes!("../web/wterm/dom/debug.js").as_slice(), JS),
        "dom/input.js" => (include_bytes!("../web/wterm/dom/input.js").as_slice(), JS),
        "dom/renderer.js" => (include_bytes!("../web/wterm/dom/renderer.js").as_slice(), JS),
        "dom/terminal.css" => (include_bytes!("../web/wterm/dom/terminal.css").as_slice(), "text/css"),
        "den.css" => (include_bytes!("../web/den.css").as_slice(), "text/css"),
        _ => return None,
    })
}

/// The den, server-rendered per request: live captures first, then the
/// library newest-first. The store tolerates corruption; so does this
/// page — a bad line or vanished run dir never breaks it.
pub fn render_index(store: &Store) -> String {
    let mut html = String::from(INDEX_HEAD);
    let running = store.list_running().unwrap_or_default();
    let mut runs = store.list().unwrap_or_default();
    runs.reverse(); // newest first
    if running.is_empty() && runs.is_empty() {
        html.push_str(
            "<p class=empty>no captures yet — <code>otterm run -- your-command</code> and it'll show up here.</p>",
        );
    }
    for meta in running.iter().chain(runs.iter()) {
        let live = meta.state() == RunState::Running;
        let dot = if live { "<span class=live>●</span>" } else { "" };
        let status = match meta.exit_code {
            Some(0) => "<span class=ok>✓</span>",
            Some(_) => "<span class=err>✗</span>",
            None => "",
        };
        html.push_str(&format!(
            "<a class=run href=\"/watch/{}\">{}<code>{}</code><span class=meta>{} {} B</span></a>\n",
            esc(&meta.id),
            dot,
            esc(&meta.cmdline()),
            status,
            meta.bytes,
        ));
    }
    html.push_str(INDEX_TAIL);
    html
}

struct Request {
    path: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Read one GET request. Headers are capped at 64 lines — this is a den
/// server, not a public web server.
fn read_request(r: &mut BufReader<TcpStream>) -> io::Result<Option<Request>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path), Some(_)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(io::Error::other("bad request line"));
    };
    if method != "GET" {
        return Err(io::Error::other("method not allowed"));
    }
    let mut headers = Vec::new();
    for _ in 0..64 {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h == "\r\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    Ok(Some(Request { path: path.to_string(), headers }))
}

fn respond(mut s: &TcpStream, status: u16, mime: &str, body: &[u8]) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {mime}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(body)
}

fn respond_html(s: &TcpStream, body: &str) -> io::Result<()> {
    respond(s, 200, "text/html; charset=utf-8", body.as_bytes())
}

pub fn handle_conn(stream: TcpStream, store: &Store) -> io::Result<()> {
    // The BufReader reads from a clone; upgrade requests carry no body and
    // watchers send no frames before the handshake, so no bytes are lost
    // when it's dropped.
    let mut reader = BufReader::new(stream.try_clone()?);
    let Some(req) = read_request(&mut reader)? else {
        return Ok(());
    };
    let path = req.path.split('?').next().unwrap_or("/");
    if path == "/" {
        respond_html(&stream, &render_index(store))
    } else if let Some(id) = path.strip_prefix("/watch/") {
        if valid_id(id) {
            respond_html(&stream, WATCH_HTML)
        } else {
            respond(&stream, 404, "text/plain", b"no such run")
        }
    } else if let Some(name) = path.strip_prefix("/assets/") {
        match asset(name) {
            Some((bytes, mime)) => respond(&stream, 200, mime, bytes),
            None => respond(&stream, 404, "text/plain", b"not found"),
        }
    } else if path.starts_with("/stream/") {
        // Task 5 wires the WebSocket stream; until then, be explicit.
        respond(&stream, 400, "text/plain", b"expected a websocket upgrade")
    } else {
        respond(&stream, 404, "text/plain", b"not found")
    }
}

/// Accept loop — one thread per watcher. A personal tool's worth of
/// traffic; no runtime required. A dead watcher is not an error.
pub fn serve(listener: TcpListener, store: &Store) -> io::Result<()> {
    thread::scope(|scope| {
        for conn in listener.incoming() {
            match conn {
                Ok(stream) => {
                    scope.spawn(|| {
                        handle_conn(stream, store).ok();
                    });
                }
                Err(e) => eprintln!("otterm serve: {e}"),
            }
        }
    });
    Ok(())
}
```

Register the module in `src/main.rs` — change the module list to:

```rust
mod banner;
mod capture;
mod fleet;
mod serve;
mod store;
mod tui;
mod ws;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test serve:: 2>&1 | tail -8`
Expected: 4 tests pass (`esc_escapes_html`, `valid_id_accepts_only_run_id_chars`, `index_lists_runs_newest_first`, `http_routes`)

- [ ] **Step 6: Commit**

```bash
git add src/serve.rs src/store.rs src/main.rs
git commit -m "Lifeguard: HTTP core — den index, watch page, vendored assets"
```

---

### Task 5: The `/stream/<id>` WebSocket endpoint

**Files:**
- Modify: `src/serve.rs`

**Interfaces:**
- Consumes: `ws::{encode, parse_frame, Frame, OP_BINARY, OP_TEXT, OP_PING, OP_PONG, OP_CLOSE, accept_key, MAX_MESSAGE}` (Tasks 1-2); `Store::{load_meta, read_output, output_path}`; `RunState`.
- Produces: `GET /stream/<id>` upgrades to WebSocket; replays the log as binary frames, tails live runs on a 100 ms tick, ends with a `{"done":true,"exit":N}` text frame. Used by `web/watch.html`.

- [ ] **Step 1: Write the failing tests**

Add inside the `mod tests` in `src/serve.rs`:

```rust
    /// Build a masked client frame the way a browser sends it.
    fn masked_frame(op: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0x80 | op, 0x80 | payload.len() as u8, 1, 2, 3, 4];
        for (i, b) in payload.iter().enumerate() {
            v.push(b ^ [1u8, 2, 3, 4][i % 4]);
        }
        v
    }

    /// Start a one-shot server for `store`'s root on loopback and complete
    /// a WebSocket handshake for `/stream/<id>`. (The handler thread
    /// re-opens the store from its root, since a detached thread can't
    /// borrow the fixture's `Store`.)

```rust
    /// Start a one-shot server for `store`'s root on loopback and complete
    /// a WebSocket handshake for `/stream/<id>`.
    fn ws_connect(root: &std::path::Path, id: &str) -> TcpStream {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let root = root.to_path_buf();
        let id_owned = id.to_string();
        thread::spawn(move || {
            let store = Store::open_at(root).unwrap();
            let (stream, _) = listener.accept().unwrap();
            handle_conn(stream, &store).unwrap();
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        write!(
            c,
            "GET /stream/{id_owned} HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        .unwrap();
        // Consume the 101 response headers.
        let mut byte = [0u8; 1];
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            c.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        assert!(head.starts_with("HTTP/1.1 101"), "{head}");
        assert!(head.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "{head}");
        c
    }

    /// Read exactly one server frame.
    fn read_frame(c: &mut TcpStream) -> crate::ws::Frame {
        let mut inbox = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if let Some((frame, used)) = crate::ws::parse_frame(&inbox).unwrap() {
                let _ = used;
                return frame;
            }
            let n = c.read(&mut chunk).unwrap();
            assert!(n > 0, "connection closed waiting for a frame");
            inbox.extend_from_slice(&chunk[..n]);
        }
    }

    #[test]
    fn finished_run_replays_then_reports_done() {
        let (store, root) = fixture("stream-done");
        let meta = done_meta(&store, "echo hi");
        store.record(&meta).unwrap();
        let mut c = ws_connect(&root, &meta.id);
        let data = read_frame(&mut c);
        assert_eq!(data.opcode, crate::ws::OP_BINARY);
        assert_eq!(data.payload, b"abc");
        let done = read_frame(&mut c);
        assert_eq!(done.opcode, crate::ws::OP_TEXT);
        assert_eq!(String::from_utf8(done.payload).unwrap(), "{\"done\":true,\"exit\":0}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_run_tails_then_completes() {
        let (store, root) = fixture("stream-live");
        let mut meta = done_meta(&store, "sleep lots");
        meta.done = false;
        meta.pid = Some(std::process::id()); // alive → RunState::Running
        store.record_start(&meta).unwrap();

        let mut c = ws_connect(&root, &meta.id);

        // New bytes land in the log → a binary frame follows.
        fs::write(store.output_path(&meta.id), b"first\r\n").unwrap();
        let f1 = read_frame(&mut c);
        assert_eq!(f1.payload, b"first\r\n");
        fs::write(store.output_path(&meta.id), b"first\r\nsecond\r\n").unwrap();
        let f2 = read_frame(&mut c);
        assert_eq!(f2.payload, b"second\r\n");

        // The capture finishes → final bytes + the done frame.
        let mut final_meta = meta.clone();
        final_meta.done = true;
        final_meta.exit_code = Some(3);
        store.record(&final_meta).unwrap();
        let done = read_frame(&mut c);
        assert_eq!(done.opcode, crate::ws::OP_TEXT);
        assert_eq!(String::from_utf8(done.payload).unwrap(), "{\"done\":true,\"exit\":3}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ping_gets_pong() {
        let (store, root) = fixture("stream-ping");
        let meta = done_meta(&store, "echo hi");
        store.record(&meta).unwrap();
        let mut c = ws_connect(&root, &meta.id);
        read_frame(&mut c); // replay
        read_frame(&mut c); // done
        c.write_all(&masked_frame(crate::ws::OP_PING, b"yo")).unwrap();
        // A finished run keeps its socket open until the watcher leaves,
        // so the pong must arrive even after the done frame.
        let pong = read_frame(&mut c);
        assert_eq!(pong.opcode, crate::ws::OP_PONG);
        assert_eq!(pong.payload, b"yo");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unknown_run_is_404() {
        let (store, root) = fixture("stream-404");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let root2 = root.clone();
        thread::spawn(move || {
            let store = Store::open_at(root2).unwrap();
            let (stream, _) = listener.accept().unwrap();
            handle_conn(stream, &store).unwrap();
        });
        let mut c = TcpStream::connect(addr).unwrap();
        write!(c, "GET /stream/0000000000000-00000 HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").unwrap();
        let mut out = String::new();
        c.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 404"), "{out}");
        fs::remove_dir_all(root).unwrap();
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test serve:: 2>&1 | tail -5`
Expected: the two stream tests fail — the endpoint currently answers 400, so `ws_connect`'s `assert!(head.starts_with("HTTP/1.1 101"))` panics.

- [ ] **Step 3: Implement the streaming endpoint**

In `src/serve.rs`, update the top-level `std::io` import so the read half's `.read()` resolves (`Read` moves out of the test module's scope into shared use — leave the test module's own `use std::io::Read;` in place; a duplicate import in a child module is fine):

```rust
use std::io::{self, BufRead, BufReader, Read, Write};
```

Then add near the top (with the other constants):

```rust
/// Tail polling cadence, matching the TUI tick.
const TICK: std::time::Duration = std::time::Duration::from_millis(100);
```

Replace the `/stream/` arm in `handle_conn` with:

```rust
    } else if let Some(id) = path.strip_prefix("/stream/") {
        stream_run(stream, store, id, &req)
    } else {
```

Add these functions below `handle_conn`:

```rust
fn stream_run(mut s: TcpStream, store: &Store, id: &str, req: &Request) -> io::Result<()> {
    if !valid_id(id) || store.load_meta(id).is_none() {
        return respond(&s, 404, "text/plain", b"no such run");
    }
    let Some(key) = req.header("sec-websocket-key") else {
        return respond(&s, 400, "text/plain", b"expected a websocket upgrade");
    };
    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {}\r\n\r\n",
        crate::ws::accept_key(key)
    );
    s.write_all(head.as_bytes())?;

    // The read half times out each tick so the loop can service pings,
    // closes, and (one day) input between output chunks.
    s.set_read_timeout(Some(TICK))?;
    let mut read_half = s.try_clone()?;
    let mut inbox: Vec<u8> = Vec::new();
    let mut frag: Option<Vec<u8>> = None; // partial watcher message, by RFC 6455 continuation
    let mut offset = send_replay(&mut s, store, id)?;

    loop {
        let mut chunk = [0u8; 4096];
        match read_half.read(&mut chunk) {
            Ok(0) => return Ok(()), // watcher hung up
            Ok(n) => inbox.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(e) => return Err(e),
        }
        while let Some((frame, used)) = crate::ws::parse_frame(&inbox)? {
            inbox.drain(..used);
            match frame.opcode {
                crate::ws::OP_PING => {
                    s.write_all(&crate::ws::encode(crate::ws::OP_PONG, &frame.payload))?
                }
                crate::ws::OP_CLOSE => return Ok(()),
                crate::ws::OP_TEXT | crate::ws::OP_BINARY if !frame.fin => {
                    frag = Some(frame.payload);
                }
                crate::ws::OP_CONT => {
                    if let Some(data) = &mut frag {
                        data.extend_from_slice(&frame.payload);
                        if data.len() > crate::ws::MAX_MESSAGE {
                            return Err(io::Error::other("ws message too large"));
                        }
                        if frame.fin {
                            frag = None;
                        }
                    }
                }
                // Complete watcher messages: the input seam — ignored for now.
                _ => {}
            }
        }
        offset = send_appends(&mut s, store, id, offset)?;
        match store.load_meta(id) {
            Some(meta) if meta.state() == RunState::Running => {}
            Some(meta) => {
                // Finished or died: flush the last bytes, then say so.
                send_appends(&mut s, store, id, offset)?;
                let exit = meta
                    .exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "null".into());
                let done = format!("{{\"done\":true,\"exit\":{exit}}}");
                s.write_all(&crate::ws::encode(crate::ws::OP_TEXT, done.as_bytes()))?;
                // Keep the socket until the watcher leaves, per the spec.
                loop {
                    let mut chunk = [0u8; 4096];
                    match read_half.read(&mut chunk) {
                        Ok(0) => return Ok(()),
                        Ok(n) => inbox.extend_from_slice(&chunk[..n]),
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                            ) => {}
                        Err(e) => return Err(e),
                    }
                    while let Some((frame, used)) = crate::ws::parse_frame(&inbox)? {
                        inbox.drain(..used);
                        match frame.opcode {
                            crate::ws::OP_PING => s
                                .write_all(&crate::ws::encode(crate::ws::OP_PONG, &frame.payload))?,
                            crate::ws::OP_CLOSE => return Ok(()),
                            _ => {}
                        }
                    }
                }
            }
            // Run dir vanished mid-stream: tell the watcher we're done.
            None => {
                s.write_all(&crate::ws::encode(
                    crate::ws::OP_TEXT,
                    b"{\"done\":true,\"exit\":null}",
                ))?;
                return Ok(());
            }
        }
    }
}

/// Send the captured-so-far bytes (tail-first for oversized logs, same
/// rule as the TUI) and return the file offset to continue tailing from.
fn send_replay(s: &mut TcpStream, store: &Store, id: &str) -> io::Result<u64> {
    let (buf, _truncated) = store.read_output(id, REPLAY_MAX)?;
    send_chunks(s, &buf)?;
    Ok(store.output_path(id).metadata()?.len())
}

/// Send whatever landed in the log since `offset`; return the new offset.
fn send_appends(s: &mut TcpStream, store: &Store, id: &str, offset: u64) -> io::Result<u64> {
    use std::io::{Seek, SeekFrom};
    let mut f = std::fs::File::open(store.output_path(id))?;
    let len = f.metadata()?.len();
    if len <= offset {
        return Ok(offset);
    }
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    send_chunks(s, &buf)?;
    Ok(len)
}

fn send_chunks(s: &mut TcpStream, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        let n = data.len().min(crate::ws::MAX_MESSAGE);
        s.write_all(&crate::ws::encode(crate::ws::OP_BINARY, &data[..n]))?;
        data = &data[n..];
    }
    Ok(())
}
```

The `OP_CONT` constant is used but not yet defined in `ws.rs` — add it to `src/ws.rs` with the other opcodes:

```rust
pub const OP_CONT: u8 = 0x0;
```

`REPLAY_MAX` is defined for the first time in this task — add it above `TICK` in `src/serve.rs`:

```rust
/// Browsers get the tail of oversized logs, same rule as the TUI viewer.
const REPLAY_MAX: u64 = 16 * 1024 * 1024;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test serve:: 2>&1 | tail -12`
Expected: all serve tests pass — the 4 from Task 4 plus `finished_run_replays_then_reports_done`, `live_run_tails_then_completes`, `ping_gets_pong`, `unknown_run_is_404`

- [ ] **Step 5: Commit**

```bash
git add src/serve.rs src/ws.rs
git commit -m "Lifeguard: /stream/<id> — replay, live tail, done frame over WebSocket"
```

---

### Task 6: CLI wiring — `otterm serve`

**Files:**
- Modify: `src/main.rs` (Serve subcommand + dispatch), `src/serve.rs` (add `run`, `detect_bind`, `splash`)

**Interfaces:**
- Consumes: `serve::{serve, handle_conn}` (Task 4-5), `fleet::qr_text`, `Store::open`.
- Produces: `otterm serve [--port N] [--bind IP]`; `serve::run(store: &Store, bind: &str, port: u16) -> io::Result<()>`; `serve::detect_bind() -> String`.

- [ ] **Step 1: Add the subcommand**

In `src/main.rs`, add to the `Cmd` enum after `Last`:

```rust
    /// Serve the den over the tailnet — watch captures live in a browser
    Serve {
        /// Port to listen on
        #[arg(long, default_value_t = 7777)]
        port: u16,
        /// Address to bind (default: this machine's Tailscale IPv4)
        #[arg(long)]
        bind: Option<String>,
    },
```

And in the `match cli.cmd` dispatch, add before the `Init` unreachable arm:

```rust
        Some(Cmd::Serve { port, bind }) => {
            let bind = bind.unwrap_or_else(serve::detect_bind);
            serve::run(&store, &bind, port)
        }
```

- [ ] **Step 2: Add `run`, `detect_bind`, and `splash` to `src/serve.rs`**

```rust
use crate::fleet;

/// Default listen address: this machine's Tailscale IPv4, so the den is
/// only reachable inside the tailnet. Without a tailnet, loopback with a
/// heads-up — never 0.0.0.0 silently.
pub fn detect_bind() -> String {
    let ip = std::process::Command::new("tailscale")
        .args(["ip", "-4"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.lines().next().map(str::trim).to_string());
    match ip {
        Some(ip) if !ip.is_empty() => ip,
        _ => {
            eprintln!("otterm serve: no tailnet found — serving on 127.0.0.1 only");
            "127.0.0.1".to_string()
        }
    }
}

pub fn run(store: &Store, bind: &str, port: u16) -> io::Result<()> {
    let addr = format!("{bind}:{port}");
    let listener = TcpListener::bind(&addr)?;
    if bind == "0.0.0.0" || bind == "::" {
        eprintln!(
            "otterm serve: WARNING — listening on all interfaces; \
             anyone who can reach this machine can watch your captures."
        );
    }
    let url = format!("http://{addr}");
    if std::env::var_os("OTTERM_QUIET").is_some() {
        println!("{url}"); // pipeable, like `run`'s quiet mode
    } else {
        splash(&url);
    }
    serve(listener, store)
}

fn splash(url: &str) {
    println!();
    println!("  🦦 the lifeguard is on duty");
    println!("  {url}");
    if let Ok(qr) = fleet::qr_text(url) {
        println!("{qr}");
    }
    println!("  scan to watch from the pool. stay cool.");
    println!();
}
```

- [ ] **Step 3: Build and smoke-test**

Run: `cargo build 2>&1 | tail -3`
Expected: `Finished` with no warnings about otterm's own code

Run: `OTTERM_DATA_DIR=$(mktemp -d) cargo run --quiet -- serve --port 17777 & sleep 2; curl -s http://127.0.0.1:17777/ | head -8; curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:17777/assets/core/index.js; kill %1`
Expected: the index HTML (with the empty-den message), `200`

Note: `serve` binds to the tailscale IP by default if one exists — in that case curl the printed URL instead of 127.0.0.1, or pass `--bind 127.0.0.1` for the smoke test.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs src/serve.rs
git commit -m "Lifeguard: otterm serve — tailnet bind detection, QR splash"
```

---

### Task 7: Docs, version, and end-to-end verification

**Files:**
- Modify: `README.md`, `AGENTS.md`, `Cargo.toml` (version)

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Update `README.md`**

Add a `serve` entry wherever the other subcommands are documented:

```markdown
otterm serve [--port N] [--bind IP]   # the lifeguard: watch any capture live
                                      # in a browser over the tailnet (QR on startup)
```

- [ ] **Step 2: Update `AGENTS.md`**

- Project overview: mention `otterm serve` (the lifeguard, read-only tailnet streaming).
- Layout: add `src/serve.rs` (HTTP/WebSocket server, index + watch pages, `/stream/<id>` replay/tail/done), `src/ws.rs` (RFC 6455 handshake + frame codec), `web/` (vendored wterm 0.3.0, Apache-2.0 — pinned from npm, regenerate by re-running the vendor commands; `watch.html` + `den.css`).
- Technology stack: add `sha1` + `base64` (WS handshake) to direct dependencies.
- Testing instructions: `cargo test` now covers the WS codec and streaming endpoint over loopback; manual: `otterm serve` + browser + phone QR.
- Security considerations: the server is read-only, never writes the store, binds the tailscale IP by default, and `valid_id` gates all run-id input.

- [ ] **Step 3: Bump the version**

In `Cargo.toml`: `version = "0.5.0"`. Leave `publish = false` in place — publishing is a deliberate later act.

- [ ] **Step 4: Full verification**

```bash
cargo fmt
cargo clippy 2>&1 | tail -5   # no NEW warnings (one pre-existing in tui/app.rs is known)
cargo test 2>&1 | tail -4     # all green: 3 existing + 6 ws + 8 serve = 17 passed
```

Expected test count: 3 (existing) + 6 (ws) + 8 (serve) = 17 passing.

Manual end-to-end:

```bash
export OTTERM_DATA_DIR=$(mktemp -d)
cargo run --quiet -- serve --bind 127.0.0.1 --port 17777 &
cargo run --quiet -- run -- bash -c 'for i in $(seq 20); do echo "tick $i"; sleep 0.5; done'
open http://127.0.0.1:17777/   # click the run while it ticks — it should update live
```

Then with a full-screen app: `cargo run --quiet -- run -- htop` (or `less README.md`) — verify the alt-screen renders correctly in the browser. Finally, from a phone on the tailnet: scan the startup QR, watch a live capture.

- [ ] **Step 5: Commit**

```bash
git add README.md AGENTS.md Cargo.toml Cargo.lock
git commit -m "Otterm 0.5.0 — the lifeguard: live capture streaming over the tailnet"
```

---

## Notes for the implementer

- `ws::parse_frame` accepting both masked and unmasked frames is deliberate: clients must mask (RFC), servers must not; the tests parse server frames with the same function.
- `send_appends` re-opens the file each tick rather than holding an fd: the capturing process owns the log's lifecycle, and a stale fd across a delete/recreate would silently tail nothing.
- The store's `read_output` tail-first rule (`REPLAY_MAX`) means a browser opening a 100 MB finished build log gets the end of it — the part you're watching for — instantly.
