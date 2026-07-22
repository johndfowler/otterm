//! Just enough WebSocket (RFC 6455) for the lifeguard: the server sends
//! unmasked binary and text frames, and parses masked client frames to
//! honor ping/pong/close. The read side is the future input seam.

use std::io;

/// Largest client frame we bother with; watchers only send pongs and
/// (one day) input. Anything bigger is a bug or an attack.
pub const MAX_MESSAGE: usize = 1 << 20;

pub const OP_CONT: u8 = 0x0;
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
    Ok(Some((
        Frame {
            fin,
            opcode,
            payload,
        },
        total,
    )))
}

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
        let (frame, _) = parse_frame(&masked_frame(true, OP_PING, b"yo"))
            .unwrap()
            .unwrap();
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
        let buf = [0x82, 127, 0, 0, 0, 0, 0, 0x20, 0x00, 0x00]; // 2 MiB
        assert!(parse_frame(&buf).is_err());
    }
}
