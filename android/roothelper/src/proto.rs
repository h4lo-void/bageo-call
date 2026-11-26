// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The wire format between the helper and the app.
//!
//! Every message is a frame: an 8-byte header, then `length` bytes of payload.
//!
//! ```text
//! 0..2  "BA"
//! 2     protocol version (1)
//! 3     kind: 1 request (app -> helper), 2 ok, 3 error (helper -> app),
//!       4 hello (helper -> app, first frame of every connection)
//! 4..8  payload length, little-endian u32
//! ```
//!
//! A request's payload is UTF-8 fields separated by `\n`: the command, then its
//! arguments (no other control characters). An ok payload is the result (often JSON);
//! an error payload is one line of text. Requests are at most [`MAX_REQUEST`] bytes,
//! replies at most [`MAX_REPLY`]. A frame with a wrong magic, version, kind or length
//! ends the connection after an error frame; a well-framed request with bad fields
//! gets an error frame and the connection goes on.
//!
//! On a PCM connection the hello frame is followed by raw s16le PCM, interleaved.

use std::io::{self, Read, Write};
use std::time::Duration;

pub const MAGIC: [u8; 2] = *b"BA";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 8;
pub const MAX_REQUEST: usize = 4096;
pub const MAX_REPLY: usize = 8 << 20;
/// The command plus up to 128 values for `mixer.set` and its three leading fields.
pub const MAX_FIELDS: usize = 132;
pub const MAX_FIELD_LEN: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Request = 1,
    Ok = 2,
    Error = 3,
    Hello = 4,
}

impl Kind {
    fn from_u8(b: u8) -> Option<Kind> {
        match b {
            1 => Some(Kind::Request),
            2 => Some(Kind::Ok),
            3 => Some(Kind::Error),
            4 => Some(Kind::Hello),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProtoError {
    /// Reading or writing the connection failed (or it closed inside a frame).
    Io(io::ErrorKind),
    /// No frame began in the time allowed.
    Idle,
    BadMagic,
    BadVersion(u8),
    BadKind(u8),
    TooLong(usize),
    NotUtf8,
    BadChar,
    Empty,
    TooManyFields,
    FieldTooLong,
}

impl ProtoError {
    /// The connection cannot go on after this one: the next frame boundary is unknown.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            ProtoError::Io(_)
                | ProtoError::Idle
                | ProtoError::BadMagic
                | ProtoError::BadVersion(_)
                | ProtoError::BadKind(_)
                | ProtoError::TooLong(_)
        )
    }

    pub fn message(&self) -> String {
        match self {
            ProtoError::Io(k) => format!("connection error: {k:?}"),
            ProtoError::Idle => "nothing received in time".into(),
            ProtoError::BadMagic => "not a helper frame".into(),
            ProtoError::BadVersion(v) => format!("protocol version {v} not supported (this helper speaks {VERSION})"),
            ProtoError::BadKind(k) => format!("unknown frame kind {k}"),
            ProtoError::TooLong(n) => format!("frame of {n} bytes is too long"),
            ProtoError::NotUtf8 => "request is not UTF-8".into(),
            ProtoError::BadChar => "request has a control character".into(),
            ProtoError::Empty => "empty request".into(),
            ProtoError::TooManyFields => format!("more than {MAX_FIELDS} fields"),
            ProtoError::FieldTooLong => format!("a field is longer than {MAX_FIELD_LEN} bytes"),
        }
    }
}

/// Checks a header; returns the kind and the payload length.
pub fn parse_header(h: &[u8; HEADER_LEN], max: usize) -> Result<(Kind, usize), ProtoError> {
    if h[0..2] != MAGIC {
        return Err(ProtoError::BadMagic);
    }
    if h[2] != VERSION {
        return Err(ProtoError::BadVersion(h[2]));
    }
    let kind = Kind::from_u8(h[3]).ok_or(ProtoError::BadKind(h[3]))?;
    let len = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
    if len > max {
        return Err(ProtoError::TooLong(len));
    }
    Ok((kind, len))
}

pub fn header(kind: Kind, len: usize) -> [u8; HEADER_LEN] {
    let l = (len as u32).to_le_bytes();
    [MAGIC[0], MAGIC[1], VERSION, kind as u8, l[0], l[1], l[2], l[3]]
}

/// Reads one frame of at most `max` payload bytes. `Ok(None)`: the peer closed the
/// connection between frames.
pub fn read_frame<R: Read>(r: &mut R, max: usize) -> Result<Option<(Kind, Vec<u8>)>, ProtoError> {
    let mut h = [0u8; HEADER_LEN];
    let mut got = 0;
    while got < HEADER_LEN {
        match r.read(&mut h[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(ProtoError::Io(io::ErrorKind::UnexpectedEof)),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ProtoError::Io(e.kind())),
        }
    }
    let (kind, len) = parse_header(&h, max)?;
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload).map_err(|e| ProtoError::Io(e.kind()))?;
    Ok(Some((kind, payload)))
}

/// Once a frame has begun, the rest of it must arrive within this.
pub const FRAME_TIME: Duration = Duration::from_secs(5);

/// A connection whose reads can be given a time limit.
pub trait Conn: Read + Write {
    /// Limits how long each following read may wait; `None`: no limit.
    fn set_read_limit(&mut self, limit: Option<Duration>) -> io::Result<()>;
}

fn timed_out(k: io::ErrorKind) -> bool {
    matches!(k, io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// Reads one frame from a connection: waits at most `idle` for it to begin (`None`:
/// as long as it takes) and [`FRAME_TIME`] for the rest of it.
pub fn read_frame_timed<C: Conn>(
    c: &mut C,
    max: usize,
    idle: Option<Duration>,
) -> Result<Option<(Kind, Vec<u8>)>, ProtoError> {
    c.set_read_limit(idle).map_err(|e| ProtoError::Io(e.kind()))?;
    let mut h = [0u8; HEADER_LEN];
    loop {
        match c.read(&mut h[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if timed_out(e.kind()) => return Err(ProtoError::Idle),
            Err(e) => return Err(ProtoError::Io(e.kind())),
        }
    }
    c.set_read_limit(Some(FRAME_TIME)).map_err(|e| ProtoError::Io(e.kind()))?;
    c.read_exact(&mut h[1..]).map_err(|e| ProtoError::Io(e.kind()))?;
    let (kind, len) = parse_header(&h, max)?;
    let mut payload = vec![0u8; len];
    c.read_exact(&mut payload).map_err(|e| ProtoError::Io(e.kind()))?;
    Ok(Some((kind, payload)))
}

/// Writes one frame. Replies longer than [`MAX_REPLY`] are refused.
pub fn write_frame<W: Write>(w: &mut W, kind: Kind, payload: &[u8]) -> io::Result<()> {
    let max = if kind == Kind::Request { MAX_REQUEST } else { MAX_REPLY };
    if payload.len() > max {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too long"));
    }
    let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
    buf.extend_from_slice(&header(kind, payload.len()));
    buf.extend_from_slice(payload);
    w.write_all(&buf)?;
    w.flush()
}

/// Splits a request payload into its fields.
pub fn parse_request(payload: &[u8]) -> Result<Vec<String>, ProtoError> {
    let text = std::str::from_utf8(payload).map_err(|_| ProtoError::NotUtf8)?;
    if text.is_empty() {
        return Err(ProtoError::Empty);
    }
    if text.chars().any(|c| c != '\n' && (c.is_control() || c == '\u{2028}' || c == '\u{2029}')) {
        return Err(ProtoError::BadChar);
    }
    let fields: Vec<&str> = text.split('\n').collect();
    if fields.len() > MAX_FIELDS {
        return Err(ProtoError::TooManyFields);
    }
    if fields.iter().any(|f| f.len() > MAX_FIELD_LEN) {
        return Err(ProtoError::FieldTooLong);
    }
    if fields[0].is_empty() {
        return Err(ProtoError::Empty);
    }
    Ok(fields.into_iter().map(str::to_string).collect())
}

/// A request payload from its fields (what the app sends; used by the tests).
pub fn encode_request(fields: &[&str]) -> Vec<u8> {
    fields.join("\n").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![b'B', b'A', VERSION, kind];
        v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn reads_frames_in_a_row_then_eof() {
        let mut data = frame(1, b"ping");
        data.extend(frame(1, b"mixer.get\n0\nIncall Mic Mute"));
        let mut r = Cursor::new(data);
        let (k, p) = read_frame(&mut r, MAX_REQUEST).unwrap().unwrap();
        assert_eq!((k, p.as_slice()), (Kind::Request, &b"ping"[..]));
        let (_, p) = read_frame(&mut r, MAX_REQUEST).unwrap().unwrap();
        assert_eq!(parse_request(&p).unwrap(), vec!["mixer.get", "0", "Incall Mic Mute"]);
        assert_eq!(read_frame(&mut r, MAX_REQUEST).unwrap(), None);
    }

    #[test]
    fn rejects_bad_headers() {
        let mut bad = frame(1, b"ping");
        bad[0] = b'X';
        assert_eq!(read_frame(&mut Cursor::new(bad), MAX_REQUEST), Err(ProtoError::BadMagic));
        let mut bad = frame(1, b"ping");
        bad[2] = 9;
        assert_eq!(read_frame(&mut Cursor::new(bad), MAX_REQUEST), Err(ProtoError::BadVersion(9)));
        let bad = frame(7, b"ping");
        assert_eq!(read_frame(&mut Cursor::new(bad), MAX_REQUEST), Err(ProtoError::BadKind(7)));
        assert!(ProtoError::BadKind(7).is_fatal());
    }

    #[test]
    fn refuses_long_frames_without_reading_them() {
        let mut h = header(Kind::Request, 0).to_vec();
        h[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        let e = read_frame(&mut Cursor::new(h), MAX_REQUEST).unwrap_err();
        assert_eq!(e, ProtoError::TooLong(u32::MAX as usize));
        assert!(e.is_fatal());
        let long = frame(1, &vec![b'a'; MAX_REQUEST + 1]);
        assert_eq!(read_frame(&mut Cursor::new(long), MAX_REQUEST), Err(ProtoError::TooLong(MAX_REQUEST + 1)));
    }

    #[test]
    fn truncated_frames_are_errors() {
        let mut short = frame(1, b"mixer.list\n0");
        short.truncate(short.len() - 3);
        assert_eq!(
            read_frame(&mut Cursor::new(short), MAX_REQUEST),
            Err(ProtoError::Io(io::ErrorKind::UnexpectedEof))
        );
        let half = vec![b'B', b'A', VERSION];
        assert_eq!(
            read_frame(&mut Cursor::new(half), MAX_REQUEST),
            Err(ProtoError::Io(io::ErrorKind::UnexpectedEof))
        );
    }

    #[test]
    fn rejects_bad_fields() {
        assert_eq!(parse_request(&[0xff, 0xfe]), Err(ProtoError::NotUtf8));
        assert_eq!(parse_request(b""), Err(ProtoError::Empty));
        assert_eq!(parse_request(b"\nx"), Err(ProtoError::Empty));
        assert_eq!(parse_request(b"ping\r"), Err(ProtoError::BadChar));
        assert_eq!(parse_request(b"mixer.get\n0\nA\0B"), Err(ProtoError::BadChar));
        assert_eq!(parse_request("a\u{2028}b".as_bytes()), Err(ProtoError::BadChar));
        let many = vec!["1"; MAX_FIELDS + 1].join("\n");
        assert_eq!(parse_request(many.as_bytes()), Err(ProtoError::TooManyFields));
        let long = format!("mixer.get\n0\n{}", "n".repeat(MAX_FIELD_LEN + 1));
        assert_eq!(parse_request(long.as_bytes()), Err(ProtoError::FieldTooLong));
        assert!(!ProtoError::BadChar.is_fatal());
    }

    #[test]
    fn writes_frames_and_refuses_oversize() {
        let mut out = Vec::new();
        write_frame(&mut out, Kind::Ok, b"pong").unwrap();
        assert_eq!(out, frame(2, b"pong"));
        let mut out = Vec::new();
        assert!(write_frame(&mut out, Kind::Request, &vec![0u8; MAX_REQUEST + 1]).is_err());
        assert!(out.is_empty());
    }

    /// A connection that has nothing to read, or a frame cut off after `cut` bytes.
    struct Slow {
        data: Vec<u8>,
        pos: usize,
        limits: Vec<Option<Duration>>,
    }
    impl Read for Slow {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.data.len() {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            let n = buf.len().min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }
    impl Write for Slow {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Conn for Slow {
        fn set_read_limit(&mut self, limit: Option<Duration>) -> io::Result<()> {
            self.limits.push(limit);
            Ok(())
        }
    }

    #[test]
    fn timed_reads_end_quiet_or_stalled_connections() {
        let idle = Some(Duration::from_secs(5));
        let mut quiet = Slow { data: Vec::new(), pos: 0, limits: Vec::new() };
        assert_eq!(read_frame_timed(&mut quiet, MAX_REQUEST, idle), Err(ProtoError::Idle));
        assert_eq!(quiet.limits, vec![idle]);
        let mut stalled = Slow { data: frame(1, b"ping")[..5].to_vec(), pos: 0, limits: Vec::new() };
        assert_eq!(
            read_frame_timed(&mut stalled, MAX_REQUEST, idle),
            Err(ProtoError::Io(io::ErrorKind::WouldBlock))
        );
        assert_eq!(stalled.limits, vec![idle, Some(FRAME_TIME)]);
        let mut good = Slow { data: frame(1, b"ping"), pos: 0, limits: Vec::new() };
        let (k, p) = read_frame_timed(&mut good, MAX_REQUEST, None).unwrap().unwrap();
        assert_eq!((k, p.as_slice()), (Kind::Request, &b"ping"[..]));
        assert!(ProtoError::Idle.is_fatal());
    }

    #[test]
    fn random_bytes_never_panic() {
        // A fixed pseudo-random stream (xorshift), cut at every length.
        let mut x: u32 = 0x9e37_79b9;
        let mut data = Vec::new();
        for _ in 0..4096 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            data.push(x as u8);
        }
        for cut in 0..256 {
            let mut r = Cursor::new(&data[..cut]);
            let _ = read_frame(&mut r, MAX_REQUEST);
            let _ = parse_request(&data[..cut]);
        }
        let mut good = frame(1, &data[..64]);
        good[3] = 1;
        let _ = read_frame(&mut Cursor::new(good), MAX_REQUEST);
    }
}
