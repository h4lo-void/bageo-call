// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Signed release indexes. The bageo-packs release workflow signs every index
//! it publishes (bageo-pack-index.json, bageo-extras-index.json,
//! bageo-voices-index.json) with an Ed25519 key kept as a repository secret,
//! and uploads the signature next to it as `<index>.sig`. Bageo uses an index
//! only when its signature checks out with one of the keys written into the
//! program ([verify_index]).
//!
//! - What is signed is [signed_message]: a fixed label, the index's file name
//!   and its bytes as published. A signature therefore fits only that one
//!   index file: it cannot be moved to another index or another content.
//! - A `.sig` file is the 64-byte signature as 128 lowercase hex characters,
//!   optionally followed by one newline; at most [MAX_SIG_BYTES] are read.
//! - A signing key is its 32-byte Ed25519 seed as 64 lowercase hex
//!   characters; a public key is 64 lowercase hex characters too.

use std::fmt::{self, Write as _};

use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};

/// Appended to an index's file name to name its signature file.
pub const SIG_SUFFIX: &str = ".sig";
/// The environment variable `packs sign-index` reads the signing key from.
pub const SIGNING_KEY_ENV: &str = "BAGEO_PACKS_SIGNING_KEY";
/// A signature file larger than this is refused unread.
pub const MAX_SIG_BYTES: u64 = 256;
/// The first line of every signed message; a new layout gets a new label.
const LABEL: &[u8] = b"bageo-packs-index-v1\n";

/// An Ed25519 public key.
pub type PublicKey = [u8; 32];
/// An Ed25519 signing key (its seed).
pub type Seed = [u8; 32];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigError {
    /// No key to check with: every index is refused.
    NoKey,
    /// The index name is not a plain file name.
    BadName,
    /// The signature file is empty, too long or not 128 hex characters.
    Malformed,
    /// No trusted key made this signature over this index.
    Mismatch,
    /// The signing key could not be used.
    BadKey,
}

impl fmt::Display for SigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SigError::NoKey => "this Bageo has no key to check updates with",
            SigError::BadName => "not a plain index file name",
            SigError::Malformed => "the signature file is not a signature",
            SigError::Mismatch => "the signature does not match",
            SigError::BadKey => "the signing key cannot be used",
        })
    }
}

impl std::error::Error for SigError {}

/// An index file name: what the release lists, never a path.
fn plain_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && !s.starts_with('.') && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The bytes a signature covers: the label, the index's file name, a newline
/// and the index exactly as published.
pub fn signed_message(index_name: &str, index: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(LABEL.len() + index_name.len() + 1 + index.len());
    m.extend_from_slice(LABEL);
    m.extend_from_slice(index_name.as_bytes());
    m.push(b'\n');
    m.extend_from_slice(index);
    m
}

const fn nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

/// Exactly 2*N lowercase hex characters.
fn decode_hex<const N: usize>(hex: &[u8]) -> Option<[u8; N]> {
    if hex.len() != 2 * N {
        return None;
    }
    let mut out = [0u8; N];
    for (o, pair) in out.iter_mut().zip(hex.chunks_exact(2)) {
        *o = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(out)
}

/// Lowercase hex.
pub fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// A public key written into a program, as 64 lowercase hex characters; a
/// malformed one stops the build.
pub const fn public_key(hex: &str) -> PublicKey {
    let b = hex.as_bytes();
    assert!(b.len() == 64, "a public key is 64 hex characters");
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        match (nibble(b[2 * i]), nibble(b[2 * i + 1])) {
            (Some(hi), Some(lo)) => out[i] = (hi << 4) | lo,
            _ => panic!("a public key is lowercase hex"),
        }
        i += 1;
    }
    out
}

/// The signature in a `.sig` file: 128 lowercase hex characters and at most
/// one newline after them.
pub fn parse_signature(file: &[u8]) -> Option<[u8; 64]> {
    decode_hex::<64>(file.strip_suffix(b"\n").unwrap_or(file))
}

/// A signing key as kept in a file or a secret: 64 lowercase hex characters,
/// spaces and newlines around them ignored.
pub fn parse_seed(text: &str) -> Option<Seed> {
    decode_hex::<32>(text.trim().as_bytes())
}

/// A new signing key from the system's random numbers.
pub fn new_seed() -> Result<Seed, SigError> {
    let mut seed = [0u8; 32];
    SystemRandom::new().fill(&mut seed).map_err(|_| SigError::BadKey)?;
    Ok(seed)
}

fn key_pair(seed: &Seed) -> Result<Ed25519KeyPair, SigError> {
    Ed25519KeyPair::from_seed_unchecked(seed).map_err(|_| SigError::BadKey)
}

/// The public key of a signing key.
pub fn public_key_of(seed: &Seed) -> Result<PublicKey, SigError> {
    let pair = key_pair(seed)?;
    let mut out = [0u8; 32];
    out.copy_from_slice(pair.public_key().as_ref());
    Ok(out)
}

/// The `.sig` file for the index `index_name` holding `index`.
pub fn sign_index(seed: &Seed, index_name: &str, index: &[u8]) -> Result<String, SigError> {
    if !plain_name(index_name) {
        return Err(SigError::BadName);
    }
    let sig = key_pair(seed)?.sign(&signed_message(index_name, index));
    Ok(format!("{}\n", to_hex(sig.as_ref())))
}

/// Checks the `.sig` file `sig_file` of the index `index_name` holding
/// `index` against `keys`; any one of them may have made it.
pub fn verify_index(index_name: &str, index: &[u8], sig_file: &[u8], keys: &[PublicKey]) -> Result<(), SigError> {
    if keys.is_empty() {
        return Err(SigError::NoKey);
    }
    if !plain_name(index_name) {
        return Err(SigError::BadName);
    }
    if sig_file.len() as u64 > MAX_SIG_BYTES {
        return Err(SigError::Malformed);
    }
    let sig = parse_signature(sig_file).ok_or(SigError::Malformed)?;
    let msg = signed_message(index_name, index);
    if keys.iter().any(|k| UnparsedPublicKey::new(&ED25519, k).verify(&msg, &sig).is_ok()) {
        Ok(())
    } else {
        Err(SigError::Mismatch)
    }
}
