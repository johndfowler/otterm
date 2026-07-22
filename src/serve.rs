//! The lifeguard: serve the den over HTTP/WebSocket so a capture can be
//! watched live from any browser on the tailnet. Read-only by
//! construction — the server never writes to the store, and captures are
//! served as data under the same trust model as the TUI.

// Wired into `main` by the `otterm serve` subcommand (Task 6); until then
// only the tests drive this module.
#![allow(dead_code)]

use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use crate::store::{RunState, Store};

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
}
