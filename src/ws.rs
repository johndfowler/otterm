//! Just enough WebSocket (RFC 6455) for the lifeguard: the server sends
//! unmasked binary and text frames, and parses masked client frames to
//! honor ping/pong/close. The read side is the future input seam.

// Nothing consumes this module until `serve.rs` (the lifeguard) lands;
// keep the build warning-free in the meantime.
#![allow(dead_code)]

/// Largest client frame we bother with; watchers only send pongs and
/// (one day) input. Anything bigger is a bug or an attack.
pub const MAX_MESSAGE: usize = 1 << 20;

pub const OP_TEXT: u8 = 0x1;
pub const OP_BINARY: u8 = 0x2;
pub const OP_CLOSE: u8 = 0x8;
pub const OP_PING: u8 = 0x9;
pub const OP_PONG: u8 = 0xA;

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
