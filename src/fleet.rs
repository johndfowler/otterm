//! The raft: your machines, herded. Peers come from `tailscale status
//! --json`; boarding one is an ssh session captured like any other run.

use std::io;
use std::process::Command;

#[derive(Clone)]
pub struct Peer {
    /// Short hostname, for display.
    pub name: String,
    /// What we ssh to: the MagicDNS name when present, else the first IP.
    pub addr: String,
    pub ip: String,
    pub os: String,
    pub online: bool,
    pub is_self: bool,
}

impl Peer {
    /// ssh:// URI — what the QR code encodes and what phone terminals parse.
    pub fn ssh_uri(&self) -> String {
        let user = std::env::var("USER").unwrap_or_else(|_| "root".into());
        format!("ssh://{user}@{}", self.addr)
    }

    pub fn ssh_target(&self) -> String {
        let user = std::env::var("USER").unwrap_or_else(|_| "root".into());
        format!("{user}@{}", self.addr)
    }
}

/// All nodes on the tailnet, this machine first, then online before
/// offline, alphabetical within each group.
pub fn peers() -> io::Result<Vec<Peer>> {
    // Test hook: "name=ip[,online][;...]" fakes a tailnet for TUI tests.
    if let Ok(fake) = std::env::var("OTTERM_FAKE_PEERS") {
        return Ok(parse_fake(&fake));
    }
    let json = tailscale_status()?;
    let root: serde_json::Value =
        serde_json::from_slice(&json).map_err(|e| io::Error::other(e.to_string()))?;

    let mut out = Vec::new();
    if let Some(node) = root.get("Self") {
        out.extend(parse_node(node, true));
    }
    if let Some(peers) = root.get("Peer").and_then(|p| p.as_object()) {
        out.extend(peers.values().filter_map(|n| parse_node(n, false)));
    }
    out.sort_by(|a, b| (b.is_self, b.online, &a.name).cmp(&(a.is_self, a.online, &b.name)));
    Ok(out)
}

fn tailscale_status() -> io::Result<Vec<u8>> {
    tailscale_output(&["status", "--json"])
}

/// Run `tailscale <args>` against the first binary that answers:
/// Homebrew CLI first, then the Mac app's bundled binary.
pub(crate) fn tailscale_output(args: &[&str]) -> io::Result<Vec<u8>> {
    for bin in [
        "tailscale",
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ] {
        if let Ok(out) = Command::new(bin).args(args).output() {
            if out.status.success() {
                return Ok(out.stdout);
            }
        }
    }
    Err(io::Error::other(
        "tailscale not found or not running — is the tailnet up?",
    ))
}

fn parse_node(node: &serde_json::Value, is_self: bool) -> Option<Peer> {
    let s = |key: &str| {
        node.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned()
    };
    let name = s("HostName");
    if name.is_empty() {
        return None;
    }
    let dns = s("DNSName");
    let ip = node
        .get("TailscaleIPs")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let addr = if dns.is_empty() {
        ip.clone()
    } else {
        dns.trim_end_matches('.').to_owned()
    };
    Some(Peer {
        name,
        addr,
        ip,
        os: s("OS"),
        // Self has no meaningful Online flag; it's here, so it's on.
        online: is_self
            || node
                .get("Online")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        is_self,
    })
}

fn parse_fake(spec: &str) -> Vec<Peer> {
    spec.split(';')
        .filter_map(|entry| {
            let mut parts = entry.split(',');
            let (name, ip) = parts.next()?.split_once('=')?;
            Some(Peer {
                name: name.to_owned(),
                addr: ip.to_owned(),
                ip: ip.to_owned(),
                os: "linux".to_owned(),
                online: parts.next() != Some("offline"),
                is_self: false,
            })
        })
        .collect()
}

/// Render a QR code as half-block unicode text, ready for a Paragraph.
pub fn qr_text(content: &str) -> Result<String, String> {
    qrcode::QrCode::new(content.as_bytes())
        .map(|code| {
            code.render::<qrcode::render::unicode::Dense1x2>()
                .quiet_zone(true)
                .build()
        })
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    /// The QR we draw must actually scan back to the ssh URI — rendered to
    /// pixels and decoded with an independent decoder.
    #[test]
    fn qr_roundtrip() {
        let content = "ssh://otter@raft-pi.tailnet.ts.net";
        let code = qrcode::QrCode::new(content.as_bytes()).unwrap();
        let w = code.width();
        let colors = code.to_colors();
        let scale = 4usize;
        let quiet = 4 * scale;
        let dim = (w * scale + 2 * quiet) as u32;
        let mut img = image::GrayImage::from_pixel(dim, dim, image::Luma([255]));
        for y in 0..w {
            for x in 0..w {
                if colors[y * w + x] == qrcode::Color::Dark {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            img.put_pixel(
                                (quiet + x * scale + dx) as u32,
                                (quiet + y * scale + dy) as u32,
                                image::Luma([0]),
                            );
                        }
                    }
                }
            }
        }
        let mut prepared = rqrr::PreparedImage::prepare(img);
        let grids = prepared.detect_grids();
        assert_eq!(grids.len(), 1);
        let (_, decoded) = grids[0].decode().unwrap();
        assert_eq!(decoded, content);
    }

    #[test]
    fn fake_peers_parse() {
        let peers = super::parse_fake("pi-den=100.64.0.7;pi-attic=100.64.0.8,offline");
        assert_eq!(peers.len(), 2);
        assert!(peers[0].online && !peers[1].online);
        assert_eq!(peers[1].name, "pi-attic");
    }
}
