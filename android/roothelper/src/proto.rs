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
use std::time::{Duration, Instant};

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

/// Once a frame has begun, the whole of it must have arrived within this.
pub const FRAME_TIME: Duration = Duration::from_secs(5);
/// Once a reply has begun, the whole of it must have been sent within this.
pub const WRITE_TIME: Duration = Duration::from_secs(5);

/// A connection whose reads and writes can be given a time limit.
pub trait Conn: Read + Write {
    /// Limits how long each following read may wait; `None`: no limit.
    fn set_read_limit(&mut self, limit: Option<Duration>) -> io::Result<()>;
    /// Limits how long each following write may wait; `None`: no limit.
    fn set_write_limit(&mut self, limit: Option<Duration>) -> io::Result<()>;
}

fn timed_out(k: io::ErrorKind) -> bool {
    matches!(k, io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

fn left(by: Instant) -> Option<Duration> {
    Some(by.saturating_duration_since(Instant::now())).filter(|d| !d.is_zero())
}

fn earliest(a: Instant, b: Option<Instant>) -> Instant {
    b.map_or(a, |b| a.min(b))
}

/// Fills `buf` by `by`, a point on the monotonic clock: however the bytes trickle in,
/// no later.
fn read_exact_by<C: Conn>(c: &mut C, buf: &mut [u8], by: Instant) -> Result<(), ProtoError> {
    let mut got = 0;
    while got < buf.len() {
        let limit = left(by).ok_or(ProtoError::Io(io::ErrorKind::TimedOut))?;
        c.set_read_limit(Some(limit)).map_err(|e| ProtoError::Io(e.kind()))?;
        match c.read(&mut buf[got..]) {
            Ok(0) => return Err(ProtoError::Io(io::ErrorKind::UnexpectedEof)),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if timed_out(e.kind()) => return Err(ProtoError::Io(io::ErrorKind::TimedOut)),
            Err(e) => return Err(ProtoError::Io(e.kind())),
        }
    }
    Ok(())
}

/// Reads one frame from a connection. It must begin by `begin_by` (`None`: no limit),
/// and the whole of it must have arrived [`FRAME_TIME`] after its first byte, and by
/// `finish_by` when that is earlier.
pub fn read_frame_by<C: Conn>(
    c: &mut C,
    max: usize,
    begin_by: Option<Instant>,
    finish_by: Option<Instant>,
) -> Result<Option<(Kind, Vec<u8>)>, ProtoError> {
    read_frame_within(c, max, begin_by, finish_by, FRAME_TIME)
}

fn read_frame_within<C: Conn>(
    c: &mut C,
    max: usize,
    begin_by: Option<Instant>,
    finish_by: Option<Instant>,
    frame_time: Duration,
) -> Result<Option<(Kind, Vec<u8>)>, ProtoError> {
    let mut h = [0u8; HEADER_LEN];
    loop {
        let limit = match begin_by {
            Some(by) => Some(left(by).ok_or(ProtoError::Idle)?),
            None => None,
        };
        c.set_read_limit(limit).map_err(|e| ProtoError::Io(e.kind()))?;
        match c.read(&mut h[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if timed_out(e.kind()) => return Err(ProtoError::Idle),
            Err(e) => return Err(ProtoError::Io(e.kind())),
        }
    }
    let by = earliest(Instant::now() + frame_time, finish_by);
    read_exact_by(c, &mut h[1..], by)?;
    let (kind, len) = parse_header(&h, max)?;
    let mut payload = vec![0u8; len];
    read_exact_by(c, &mut payload, by)?;
    Ok(Some((kind, payload)))
}

/// Writes one frame; the whole of it must have gone out by `by`.
pub fn write_frame_by<C: Conn>(c: &mut C, kind: Kind, payload: &[u8], by: Instant) -> io::Result<()> {
    let max = if kind == Kind::Request { MAX_REQUEST } else { MAX_REPLY };
    if payload.len() > max {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too long"));
    }
    let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
    buf.extend_from_slice(&header(kind, payload.len()));
    buf.extend_from_slice(payload);
    let mut sent = 0;
    while sent < buf.len() {
        let limit = left(by).ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        c.set_write_limit(Some(limit))?;
        match c.write(&buf[sent..]) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(n) => sent += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    c.flush()
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

    /// A connection that has nothing more to read once `data` is used up (a read then
    /// times out), and that waits `drip` before every read and gives `step` bytes.
    struct Slow {
        data: Vec<u8>,
        pos: usize,
        drip: Duration,
        step: usize,
        limits: Vec<Option<Duration>>,
        out: Vec<u8>,
    }
    fn slow(data: Vec<u8>, drip: Duration, step: usize) -> Slow {
        Slow { data, pos: 0, drip, step, limits: Vec::new(), out: Vec::new() }
    }
    impl Read for Slow {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            std::thread::sleep(self.drip);
            if self.pos >= self.data.len() {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            let n = buf.len().min(self.data.len() - self.pos).min(self.step);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }
    impl Write for Slow {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            std::thread::sleep(self.drip);
            let n = buf.len().min(self.step);
            self.out.extend_from_slice(&buf[..n]);
            Ok(n)
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
        fn set_write_limit(&mut self, _limit: Option<Duration>) -> io::Result<()> {
            Ok(())
        }
    }

    fn in_(d: Duration) -> Option<Instant> {
        Some(Instant::now() + d)
    }

    #[test]
    fn timed_reads_end_quiet_or_stalled_connections() {
        let five = Duration::from_secs(5);
        let mut quiet = slow(Vec::new(), Duration::ZERO, usize::MAX);
        assert_eq!(read_frame_by(&mut quiet, MAX_REQUEST, in_(five), None), Err(ProtoError::Idle));
        assert!(quiet.limits[0].is_some_and(|l| l <= five && l > Duration::from_secs(4)));
        let mut stalled = slow(frame(1, b"ping")[..5].to_vec(), Duration::ZERO, usize::MAX);
        assert_eq!(
            read_frame_by(&mut stalled, MAX_REQUEST, in_(five), None),
            Err(ProtoError::Io(io::ErrorKind::TimedOut))
        );
        assert!(stalled.limits.iter().all(|l| l.is_some_and(|l| l <= FRAME_TIME)));
        let mut good = slow(frame(1, b"ping"), Duration::ZERO, usize::MAX);
        let (k, p) = read_frame_by(&mut good, MAX_REQUEST, None, None).unwrap().unwrap();
        assert_eq!((k, p.as_slice()), (Kind::Request, &b"ping"[..]));
        assert_eq!(good.limits[0], None, "no limit to begin");
        assert!(ProtoError::Idle.is_fatal());
        let mut late = slow(frame(1, b"ping"), Duration::ZERO, usize::MAX);
        let past = Instant::now();
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(read_frame_by(&mut late, MAX_REQUEST, Some(past), None), Err(ProtoError::Idle));
    }

    #[test]
    fn a_frame_sent_a_byte_at_a_time_cannot_take_longer_than_its_time() {
        // Every byte comes in well within any single read's limit, but the whole frame
        // would take 20 ms x 12 bytes: it is cut off when its time is up.
        let mut drip = slow(frame(1, b"ping"), Duration::from_millis(20), 1);
        let start = Instant::now();
        let r = read_frame_within(&mut drip, MAX_REQUEST, None, None, Duration::from_millis(100));
        assert_eq!(r, Err(ProtoError::Io(io::ErrorKind::TimedOut)));
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(100) && took < Duration::from_millis(400), "{took:?}");
        // The connection's own deadline cuts it off sooner.
        let mut drip = slow(frame(1, b"ping"), Duration::from_millis(20), 1);
        let start = Instant::now();
        let r = read_frame_by(&mut drip, MAX_REQUEST, None, in_(Duration::from_millis(60)));
        assert_eq!(r, Err(ProtoError::Io(io::ErrorKind::TimedOut)));
        assert!(start.elapsed() < Duration::from_millis(300));
        // Fast enough: read whole.
        let mut ok = slow(frame(1, b"ping"), Duration::from_millis(1), 1);
        assert!(read_frame_within(&mut ok, MAX_REQUEST, None, None, Duration::from_secs(2)).unwrap().is_some());
    }

    #[test]
    fn a_reply_must_go_out_whole_in_time() {
        let mut c = slow(Vec::new(), Duration::ZERO, 3);
        write_frame_by(&mut c, Kind::Ok, b"pong", Instant::now() + Duration::from_secs(1)).unwrap();
        assert_eq!(c.out, frame(2, b"pong"));
        let mut stuck = slow(Vec::new(), Duration::from_millis(20), 1);
        let start = Instant::now();
        let e = write_frame_by(&mut stuck, Kind::Ok, &[b'x'; 64], Instant::now() + Duration::from_millis(80)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_millis(400));
        assert!(stuck.out.len() < 64 + HEADER_LEN);
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
