// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Encrypted backups (`.bageo`): a backup zip ([`crate::backup`]) sealed with
//! a password, written and read a chunk at a time, so memory stays the same
//! however big the file is.
//!
//! The file (integers little-endian):
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0 | 8 | `BAGEOENC` |
//! | 8 | 1 | format version: 1 |
//! | 9 | 1 | key derivation: 1 = Argon2id, version 0x13 |
//! | 10 | 4 | Argon2 memory in KiB (65536 = 64 MiB) |
//! | 14 | 4 | Argon2 passes (3) |
//! | 18 | 4 | Argon2 lanes (1) |
//! | 22 | 16 | salt (random) |
//! | 38 | 1 | cipher: 1 = XChaCha20-Poly1305 in the STREAM construction |
//! | 39 | 4 | chunk size: bytes of the zip per chunk (1048576) |
//! | 43 | 19 | nonce prefix (random) |
//! | 62 | … | the chunks |
//!
//! The 32-byte key is Argon2id(password as UTF-8, salt) with the costs above.
//! Each chunk is `chunk size` bytes of the zip (the last one fewer, maybe
//! none) sealed with XChaCha20-Poly1305 and followed by its 16-byte tag. Its
//! 24-byte nonce is the prefix, the chunk's number from 0 (4 bytes,
//! big-endian), then 1 for the last chunk and 0 for the others. The 62-byte
//! header is the associated data of every chunk. A changed byte, a cut-off
//! file, a missing last chunk or a changed header all fail the tags.
//!
//! A file that asks for more than 256 MiB of memory, more than 4 passes or
//! more than 4 lanes is refused before any work: it could tie up the computer.

use std::io::{self, Read, Write};
use std::ops::RangeInclusive;
use std::sync::Mutex;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::generic_array::GenericArray;
use chacha20poly1305::aead::stream::{DecryptorBE32, EncryptorBE32};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305};

/// The first bytes of an encrypted backup.
pub const MAGIC: &[u8; 8] = b"BAGEOENC";
/// The newest file version this agent reads (and the one it writes).
pub const VERSION: u8 = 1;
const KDF_ARGON2ID: u8 = 1;
const CIPHER_XCHACHA_STREAM: u8 = 1;
pub const HEADER_LEN: usize = 62;
const SALT_LEN: usize = 16;
const PREFIX_LEN: usize = 19;
const TAG_LEN: usize = 16;
/// Argon2id costs of new files: 64 MiB, 3 passes, 1 lane. Unit tests write a
/// lighter memory (the file says which), so they stay quick in debug builds.
pub const MEMORY_KIB_NEW: u32 = 64 * 1024;
pub const MEMORY_KIB: u32 = if cfg!(test) { 8 * 1024 } else { MEMORY_KIB_NEW };
pub const PASSES: u32 = 3;
pub const LANES: u32 = 1;
/// Bytes of the zip in each chunk.
pub const CHUNK: u32 = 1 << 20;
/// What a file may ask for.
const MEMORY_KIB_OK: RangeInclusive<u32> = 8 * 1024..=256 * 1024;
const PASSES_OK: RangeInclusive<u32> = 1..=4;
const LANES_OK: RangeInclusive<u32> = 1..=4;
const CHUNK_OK: RangeInclusive<u32> = 4 * 1024..=16 << 20;
/// The shortest password a backup is encrypted with.
pub const PASSWORD_MIN_CHARS: usize = 8;

/// Why an encrypted backup could not be opened.
#[derive(Debug)]
pub enum CryptErr {
    /// The password does not open it (or its first chunk was changed).
    WrongPassword,
    /// Changed, cut short, or not made this way.
    Damaged,
    /// Made by a newer Bageo.
    TooNew,
    Io(io::Error),
}

impl From<io::Error> for CryptErr {
    fn from(e: io::Error) -> Self {
        CryptErr::Io(e)
    }
}

/// A password backups may be encrypted with (at least [`PASSWORD_MIN_CHARS`]).
pub fn password_ok(pw: &str) -> bool {
    pw.trim().chars().count() >= PASSWORD_MIN_CHARS
}

/// `head` (the first bytes of a file) is an encrypted backup's.
pub fn is_sealed(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

/// Random bytes from the system.
pub fn random(buf: &mut [u8]) -> io::Result<()> {
    getrandom::getrandom(buf).map_err(|_| io::Error::other("no random numbers"))
}

struct Header {
    bytes: [u8; HEADER_LEN],
    memory: u32,
    passes: u32,
    lanes: u32,
    salt: [u8; SALT_LEN],
    chunk: u32,
    prefix: [u8; PREFIX_LEN],
}

impl Header {
    fn new(salt: [u8; SALT_LEN], prefix: [u8; PREFIX_LEN]) -> Header {
        let mut b = [0u8; HEADER_LEN];
        b[..8].copy_from_slice(MAGIC);
        b[8] = VERSION;
        b[9] = KDF_ARGON2ID;
        b[10..14].copy_from_slice(&MEMORY_KIB.to_le_bytes());
        b[14..18].copy_from_slice(&PASSES.to_le_bytes());
        b[18..22].copy_from_slice(&LANES.to_le_bytes());
        b[22..38].copy_from_slice(&salt);
        b[38] = CIPHER_XCHACHA_STREAM;
        b[39..43].copy_from_slice(&CHUNK.to_le_bytes());
        b[43..62].copy_from_slice(&prefix);
        Header { bytes: b, memory: MEMORY_KIB, passes: PASSES, lanes: LANES, salt, chunk: CHUNK, prefix }
    }

    fn parse(b: [u8; HEADER_LEN]) -> Result<Header, CryptErr> {
        if !is_sealed(&b) {
            return Err(CryptErr::Damaged);
        }
        if b[8] > VERSION {
            return Err(CryptErr::TooNew);
        }
        let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        let (memory, passes, lanes, chunk) = (u32_at(10), u32_at(14), u32_at(18), u32_at(39));
        let known = b[8] == VERSION && b[9] == KDF_ARGON2ID && b[38] == CIPHER_XCHACHA_STREAM;
        if !known || !MEMORY_KIB_OK.contains(&memory) || !PASSES_OK.contains(&passes) || !LANES_OK.contains(&lanes) || !CHUNK_OK.contains(&chunk) {
            return Err(CryptErr::Damaged);
        }
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&b[22..38]);
        let mut prefix = [0u8; PREFIX_LEN];
        prefix.copy_from_slice(&b[43..62]);
        Ok(Header { bytes: b, memory, passes, lanes, salt, chunk, prefix })
    }
}

/// One key at a time: each takes up to a file's memory cost.
static KDF: Mutex<()> = Mutex::new(());

fn derive(password: &str, h: &Header) -> Result<[u8; 32], CryptErr> {
    let params = Params::new(h.memory, h.passes, h.lanes, Some(32)).map_err(|_| CryptErr::Damaged)?;
    let _one = KDF.lock().unwrap_or_else(|e| e.into_inner());
    let mut key = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.trim().as_bytes(), &h.salt, &mut key)
        .map_err(|_| CryptErr::Damaged)?;
    Ok(key)
}

/// `buf` filled from `src` with up to `want` bytes (fewer only at the end).
fn read_full(src: &mut dyn Read, buf: &mut Vec<u8>, want: usize) -> io::Result<()> {
    buf.clear();
    buf.resize(want, 0);
    let mut got = 0;
    while got < want {
        match src.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    buf.truncate(got);
    Ok(())
}

/// Everything `src` gives, sealed with `password` into `out`: the bytes written.
pub fn seal(password: &str, src: &mut dyn Read, out: &mut dyn Write) -> Result<u64, CryptErr> {
    let mut salt = [0u8; SALT_LEN];
    random(&mut salt)?;
    let mut prefix = [0u8; PREFIX_LEN];
    random(&mut prefix)?;
    let h = Header::new(salt, prefix);
    let mut key = derive(password, &h)?;
    let mut enc = EncryptorBE32::from_aead(XChaCha20Poly1305::new(Key::from_slice(&key)), GenericArray::from_slice(&h.prefix));
    key.fill(0);
    out.write_all(&h.bytes)?;
    let size = h.chunk as usize;
    let (mut cur, mut next) = (Vec::with_capacity(size + TAG_LEN), Vec::with_capacity(size + TAG_LEN));
    read_full(src, &mut cur, size)?;
    let mut written = HEADER_LEN as u64;
    loop {
        read_full(src, &mut next, size)?;
        if next.is_empty() {
            enc.encrypt_last_in_place(&h.bytes, &mut cur).map_err(|_| CryptErr::Damaged)?;
            out.write_all(&cur)?;
            written += cur.len() as u64;
            break;
        }
        enc.encrypt_next_in_place(&h.bytes, &mut cur).map_err(|_| CryptErr::Damaged)?;
        out.write_all(&cur)?;
        written += cur.len() as u64;
        std::mem::swap(&mut cur, &mut next);
    }
    out.flush()?;
    Ok(written)
}

/// Read the header of `src` only: an encrypted backup this agent can open
/// (else `Damaged` or `TooNew`), before anyone types a password.
pub fn check_header(src: &mut dyn Read) -> Result<(), CryptErr> {
    let mut b = [0u8; HEADER_LEN];
    src.read_exact(&mut b).map_err(|_| CryptErr::Damaged)?;
    Header::parse(b).map(|_| ())
}

/// The zip sealed in `src`, opened with `password` into `out`: its size.
/// Nothing is written before the first chunk proves the password.
pub fn open(password: &str, src: &mut dyn Read, out: &mut dyn Write) -> Result<u64, CryptErr> {
    let mut b = [0u8; HEADER_LEN];
    src.read_exact(&mut b).map_err(|_| CryptErr::Damaged)?;
    let h = Header::parse(b)?;
    let mut key = derive(password, &h)?;
    let mut dec = DecryptorBE32::from_aead(XChaCha20Poly1305::new(Key::from_slice(&key)), GenericArray::from_slice(&h.prefix));
    key.fill(0);
    let size = h.chunk as usize + TAG_LEN;
    let (mut cur, mut next) = (Vec::with_capacity(size), Vec::with_capacity(size));
    read_full(src, &mut cur, size)?;
    let (mut first, mut n) = (true, 0u64);
    // The first chunk tells a wrong password; any later one a changed file.
    let failed = |first: bool| if first { CryptErr::WrongPassword } else { CryptErr::Damaged };
    loop {
        if cur.len() < TAG_LEN {
            return Err(CryptErr::Damaged);
        }
        read_full(src, &mut next, size)?;
        if next.is_empty() {
            dec.decrypt_last_in_place(&h.bytes, &mut cur).map_err(|_| failed(first))?;
            out.write_all(&cur)?;
            n += cur.len() as u64;
            break;
        }
        // Only the last chunk may be short.
        if cur.len() < size {
            return Err(CryptErr::Damaged);
        }
        dec.decrypt_next_in_place(&h.bytes, &mut cur).map_err(|_| failed(first))?;
        out.write_all(&cur)?;
        n += cur.len() as u64;
        first = false;
        std::mem::swap(&mut cur, &mut next);
    }
    out.flush()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PW: &str = "test-fake-pass-0001";

    fn sealed(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        seal(PW, &mut &data[..], &mut out).unwrap();
        out
    }

    fn opened(file: &[u8], pw: &str) -> Result<Vec<u8>, CryptErr> {
        let mut out = Vec::new();
        open(pw, &mut &file[..], &mut out).map(|_| out)
    }

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 + i / 251) as u8).collect()
    }

    #[test]
    fn what_is_sealed_opens_again_at_every_size() {
        let c = CHUNK as usize;
        for n in [0, 1000, c, c + 1, 2 * c + 5] {
            let d = data(n);
            let f = sealed(&d);
            assert!(is_sealed(&f));
            // As many chunks as it takes, and always one (the last).
            assert_eq!(f.len(), HEADER_LEN + n + TAG_LEN * n.div_ceil(c).max(1), "{n}");
            assert_eq!(opened(&f, PW).unwrap(), d, "{n}");
        }
        // Spaces at the ends of the password do not count.
        assert_eq!(opened(&sealed(b"spaces"), &format!("  {PW} ")).unwrap(), b"spaces");
        assert!(!sealed(b"PK\x03\x04 a zip").windows(4).any(|w| w == b"PK\x03\x04"), "nothing of the zip shows");
    }

    #[test]
    fn a_wrong_password_or_a_changed_file_is_found() {
        let c = CHUNK as usize;
        let d = data(2 * c + 100);
        let f = sealed(&d);
        assert!(matches!(opened(&f, "test-fake-pass-0002"), Err(CryptErr::WrongPassword)));
        // One changed byte in each chunk.
        for at in [HEADER_LEN + 5, HEADER_LEN + c + TAG_LEN + 5, f.len() - 1] {
            let mut g = f.clone();
            g[at] ^= 1;
            assert!(matches!(opened(&g, PW), Err(CryptErr::WrongPassword | CryptErr::Damaged)), "{at}");
        }
        // Cut off, or without its last chunk.
        for len in [f.len() - 1, f.len() - 100, HEADER_LEN + c + TAG_LEN, HEADER_LEN + 2 * (c + TAG_LEN), HEADER_LEN + 3, HEADER_LEN, 10] {
            assert!(matches!(opened(&f[..len], PW), Err(CryptErr::WrongPassword | CryptErr::Damaged)), "{len}");
        }
        // A changed header: still in range (the tags fail) or out of it (refused at once).
        for (at, flip) in [(11usize, 0x10u8), (14, 0x07), (18, 0x03), (22, 0x5a), (37, 0x01), (40, 0x20), (43, 0x5a), (61, 0x80)] {
            let mut g = f.clone();
            g[at] ^= flip;
            assert!(matches!(opened(&g, PW), Err(CryptErr::WrongPassword | CryptErr::Damaged)), "{at}");
        }
        for (at, v) in [(13usize, 0x40u8), (14, 11), (18, 9), (9, 2), (38, 2), (42, 0x10), (41, 0x00)] {
            let mut g = f.clone();
            g[at..at + 1].copy_from_slice(&[v]);
            if at == 14 || at == 18 {
                g[at + 1..at + 4].fill(0);
            }
            assert!(matches!(opened(&g, PW), Err(CryptErr::Damaged)), "{at}");
            assert!(matches!(check_header(&mut &g[..]), Err(CryptErr::Damaged)), "{at}");
        }
        let mut newer = f.clone();
        newer[8] = 2;
        assert!(matches!(opened(&newer, PW), Err(CryptErr::TooNew)));
        assert!(check_header(&mut &f[..]).is_ok());
    }

    #[test]
    fn files_say_their_costs_and_too_costly_ones_are_refused() {
        let f = sealed(b"x");
        let at = |i: usize| u32::from_le_bytes([f[i], f[i + 1], f[i + 2], f[i + 3]]);
        assert_eq!((at(10), at(14), at(18), at(39)), (MEMORY_KIB, PASSES, LANES, CHUNK));
        assert_eq!((MEMORY_KIB_NEW, PASSES, LANES, CHUNK), (64 * 1024, 3, 1, 1 << 20));
        let mut g = f.clone();
        g[10..14].copy_from_slice(&(256 * 1024 + 1u32).to_le_bytes());
        assert!(matches!(check_header(&mut &g[..]), Err(CryptErr::Damaged)), "over 256 MiB");
        g[10..14].copy_from_slice(&(64 * 1024u32).to_le_bytes());
        g[14..18].copy_from_slice(&5u32.to_le_bytes());
        assert!(matches!(check_header(&mut &g[..]), Err(CryptErr::Damaged)), "over 4 passes");
    }

    #[test]
    fn passwords_need_eight_characters() {
        assert!(!password_ok(""));
        assert!(!password_ok("1234567"));
        assert!(!password_ok("  1234567  "));
        assert!(password_ok("12345678"));
        assert!(password_ok("密碼密碼密碼密碼"));
    }
}

