//! The lifeguard: serve the den over HTTP/WebSocket so a capture can be
//! watched live from any browser on the tailnet. Read-only by
//! construction — the server never writes to the store, and captures are
//! served as data under the same trust model as the TUI.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use crate::fleet;
use crate::store::{RunState, Store};

/// Browsers get the tail of oversized logs, same rule as the TUI viewer.
const REPLAY_MAX: u64 = 16 * 1024 * 1024;

/// Tail polling cadence, matching the TUI tick.
const TICK: std::time::Duration = std::time::Duration::from_millis(100);

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
        "core/transport.js" => (
            include_bytes!("../web/wterm/core/transport.js").as_slice(),
            JS,
        ),
        "core/wasm-bridge.js" => (
            include_bytes!("../web/wterm/core/wasm-bridge.js").as_slice(),
            JS,
        ),
        "core/wasm-inline.js" => (
            include_bytes!("../web/wterm/core/wasm-inline.js").as_slice(),
            JS,
        ),
        "core/terminal-core.js" => (
            include_bytes!("../web/wterm/core/terminal-core.js").as_slice(),
            JS,
        ),
        "dom/index.js" => (include_bytes!("../web/wterm/dom/index.js").as_slice(), JS),
        "dom/wterm.js" => (include_bytes!("../web/wterm/dom/wterm.js").as_slice(), JS),
        "dom/debug.js" => (include_bytes!("../web/wterm/dom/debug.js").as_slice(), JS),
        "dom/input.js" => (include_bytes!("../web/wterm/dom/input.js").as_slice(), JS),
        "dom/renderer.js" => (
            include_bytes!("../web/wterm/dom/renderer.js").as_slice(),
            JS,
        ),
        "dom/terminal.css" => (
            include_bytes!("../web/wterm/dom/terminal.css").as_slice(),
            "text/css",
        ),
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
        let dot = if live {
            "<span class=live>●</span>"
        } else {
            ""
        };
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
    Ok(Some(Request {
        path: path.to_string(),
        headers,
    }))
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
    } else if let Some(id) = path.strip_prefix("/stream/") {
        stream_run(stream, store, id, &req)
    } else {
        respond(&stream, 404, "text/plain", b"not found")
    }
}

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
                            crate::ws::OP_PING => {
                                s.write_all(&crate::ws::encode(crate::ws::OP_PONG, &frame.payload))?
                            }
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
        .and_then(|s| s.lines().next().map(str::trim).map(str::to_string));
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
    if matches!(bind, "0.0.0.0" | "::" | "[::]") {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::RunMeta;
    use std::fs;
    use std::io::Read;
    use std::path::PathBuf;

    /// A store rooted in a fresh temp dir; returns the root for cleanup.
    fn fixture(name: &str) -> (Store, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("otterm-serve-test-{}-{name}", std::process::id()));
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
        c.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
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

        let watch = request(
            &store,
            &format!("GET /watch/{} HTTP/1.1\r\nHost: x\r\n\r\n", meta.id),
        );
        assert!(watch.contains("wterm"), "{watch}");

        let js = request(
            &store,
            "GET /assets/core/index.js HTTP/1.1\r\nHost: x\r\n\r\n",
        );
        assert!(js.contains("content-type: text/javascript"), "{js}");

        let css = request(&store, "GET /assets/den.css HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(css.contains("--deep"), "{css}");

        let missing = request(&store, "GET /nope HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");

        let traversal = request(
            &store,
            "GET /assets/../Cargo.toml HTTP/1.1\r\nHost: x\r\n\r\n",
        );
        assert!(traversal.starts_with("HTTP/1.1 404"), "{traversal}");

        fs::remove_dir_all(root).unwrap();
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
        assert_eq!(
            String::from_utf8(done.payload).unwrap(),
            "{\"done\":true,\"exit\":0}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_run_tails_then_completes() {
        let (store, root) = fixture("stream-live");
        let mut meta = done_meta(&store, "sleep lots");
        meta.done = false;
        meta.pid = Some(std::process::id()); // alive → RunState::Running
        store.record_start(&meta).unwrap();
        // done_meta seeds the log with "abc"; a live tail starts from an
        // empty log, or the replay frame races the first write below.
        fs::write(store.output_path(&meta.id), b"").unwrap();

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
        assert_eq!(
            String::from_utf8(done.payload).unwrap(),
            "{\"done\":true,\"exit\":3}"
        );
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
        c.write_all(&masked_frame(crate::ws::OP_PING, b"yo"))
            .unwrap();
        // A finished run keeps its socket open until the watcher leaves,
        // so the pong must arrive even after the done frame.
        let pong = read_frame(&mut c);
        assert_eq!(pong.opcode, crate::ws::OP_PONG);
        assert_eq!(pong.payload, b"yo");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unknown_run_is_404() {
        let (_store, root) = fixture("stream-404");
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
}
