// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Signed release indexes: what a signature covers, which signature files
//! are refused, and the two commands that make keys and signatures. Every
//! key here is made by the test itself.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use bageo_packs::signing::*;

const PACK_INDEX: &str = "bageo-pack-index.json";
const EXTRAS_INDEX: &str = "bageo-extras-index.json";
const VOICES_INDEX: &str = "bageo-voices-index.json";
const BODY: &[u8] = br#"{"format":1,"version":2026112801,"file":"bageo-pack.json","bytes":10,"sha256":"00","min_bageo":"1.0.0"}"#;

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn tmp(name: &str) -> Dir {
    let d = std::env::temp_dir().join(format!("bageo-signing-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    Dir(d)
}

fn key() -> (Seed, PublicKey) {
    let seed = new_seed().unwrap();
    (seed, public_key_of(&seed).unwrap())
}

fn packs() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_packs"));
    c.env_remove(SIGNING_KEY_ENV);
    c
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

#[test]
fn a_signature_checks_out_only_for_its_own_index_and_key() {
    let (a, pa) = key();
    let (b, pb) = key();
    let sig = sign_index(&a, PACK_INDEX, BODY).unwrap();
    assert_eq!(sig.len(), 129);
    assert!(sig.ends_with('\n'));
    assert_eq!(verify_index(PACK_INDEX, BODY, sig.as_bytes(), &[pa]), Ok(()));
    // Either of two keys.
    assert_eq!(verify_index(PACK_INDEX, BODY, sig.as_bytes(), &[pb, pa]), Ok(()));
    let by_b = sign_index(&b, PACK_INDEX, BODY).unwrap();
    assert_eq!(verify_index(PACK_INDEX, BODY, by_b.as_bytes(), &[pa, pb]), Ok(()));
    // Another key.
    assert_eq!(verify_index(PACK_INDEX, BODY, sig.as_bytes(), &[pb]), Err(SigError::Mismatch));
    // Another index's name with the same bytes.
    for other in [EXTRAS_INDEX, VOICES_INDEX] {
        assert_eq!(verify_index(other, BODY, sig.as_bytes(), &[pa]), Err(SigError::Mismatch), "{other}");
    }
    // Every single changed bit, a byte more or a byte less.
    for i in 0..BODY.len() {
        for bit in 0..8 {
            let mut changed = BODY.to_vec();
            changed[i] ^= 1 << bit;
            assert_eq!(verify_index(PACK_INDEX, &changed, sig.as_bytes(), &[pa]), Err(SigError::Mismatch), "byte {i} bit {bit}");
        }
    }
    let mut longer = BODY.to_vec();
    longer.push(b' ');
    assert_eq!(verify_index(PACK_INDEX, &longer, sig.as_bytes(), &[pa]), Err(SigError::Mismatch));
    assert_eq!(verify_index(PACK_INDEX, &BODY[..BODY.len() - 1], sig.as_bytes(), &[pa]), Err(SigError::Mismatch));
    // A changed signature.
    let mut bent = sig.into_bytes();
    bent[10] = if bent[10] == b'0' { b'1' } else { b'0' };
    assert_eq!(verify_index(PACK_INDEX, BODY, &bent, &[pa]), Err(SigError::Mismatch));
}

#[test]
fn no_key_refuses_every_index() {
    let (a, _) = key();
    let sig = sign_index(&a, PACK_INDEX, BODY).unwrap();
    assert_eq!(verify_index(PACK_INDEX, BODY, sig.as_bytes(), &[]), Err(SigError::NoKey));
    assert_eq!(verify_index(PACK_INDEX, BODY, b"", &[]), Err(SigError::NoKey));
}

#[test]
fn malformed_signature_files_are_refused() {
    let (a, pa) = key();
    let good = sign_index(&a, PACK_INDEX, BODY).unwrap();
    let hex = good.trim_end();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("a newline", b"\n".to_vec()),
        ("without its newline", hex.as_bytes().to_vec()),
        ("one character short", hex.as_bytes()[1..].to_vec()),
        ("one character more", format!("{hex}0\n").into_bytes()),
        ("upper case", format!("{}\n", hex.to_uppercase()).into_bytes()),
        ("two newlines", format!("{hex}\n\n").into_bytes()),
        ("a Windows line end", format!("{hex}\r\n").into_bytes()),
        ("a space first", format!(" {hex}\n").into_bytes()),
        ("base64-like", b"QUJD".repeat(32)),
        ("raw bytes", vec![0u8; 64]),
        ("too large", vec![b'a'; MAX_SIG_BYTES as usize + 1]),
        ("much too large", vec![b'a'; 1 << 20]),
    ];
    for (what, file) in cases {
        let want = if what == "without its newline" { Ok(()) } else { Err(SigError::Malformed) };
        assert_eq!(verify_index(PACK_INDEX, BODY, &file, &[pa]), want, "{what}");
    }
    // Well formed, but no key made it.
    let zeros = format!("{}\n", "0".repeat(128));
    assert_eq!(verify_index(PACK_INDEX, BODY, zeros.as_bytes(), &[pa]), Err(SigError::Mismatch));
}

#[test]
fn index_names_must_be_plain_file_names() {
    let (a, pa) = key();
    let sig = sign_index(&a, PACK_INDEX, BODY).unwrap();
    for name in ["", "../bageo-pack-index.json", "a/b.json", "a\\b.json", "a\nb.json", ".sig", "x".repeat(65).as_str()] {
        assert_eq!(sign_index(&a, name, BODY), Err(SigError::BadName), "{name:?}");
        assert_eq!(verify_index(name, BODY, sig.as_bytes(), &[pa]), Err(SigError::BadName), "{name:?}");
    }
}

#[test]
fn the_signed_message_is_the_label_the_name_and_the_bytes() {
    assert_eq!(signed_message("bageo-voices-index.json", b"{}"), b"bageo-packs-index-v1\nbageo-voices-index.json\n{}".to_vec());
    assert_ne!(signed_message(PACK_INDEX, BODY), signed_message(VOICES_INDEX, BODY));
    // A name with a newline could pass for another name and other bytes,
    // which is why only plain file names are signed or checked.
    assert_eq!(signed_message("a.json", b"b\nc"), signed_message("a.json\nb", b"c"));
    let (a, _) = key();
    assert_eq!(sign_index(&a, "a.json\nb", b"c"), Err(SigError::BadName));
}

#[test]
fn keys_read_back_from_their_text() {
    const ZERO: PublicKey = public_key("0000000000000000000000000000000000000000000000000000000000000000");
    assert_eq!(ZERO, [0u8; 32]);
    let (seed, public) = key();
    assert_eq!(public_key(&to_hex(&public)), public);
    assert_eq!(parse_seed(&format!("  {}\n", to_hex(&seed))), Some(seed));
    assert_eq!(parse_seed(&to_hex(&seed).to_uppercase()), None);
    assert_eq!(parse_seed(&to_hex(&seed)[1..]), None);
    assert_eq!(parse_seed(""), None);
    assert_ne!(new_seed().unwrap(), seed, "a new key each time");
}

#[test]
fn new_signing_key_writes_the_private_key_only_into_its_file() {
    let d = tmp("new-key");
    let out = d.0.join("signing.key");
    let o = packs().args(["new-signing-key", "--out"]).arg(&out).output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let saved = fs::read_to_string(&out).unwrap();
    assert_eq!(saved.len(), 65);
    let seed = parse_seed(&saved).expect("64 hex characters");
    let public = to_hex(&public_key_of(&seed).unwrap());
    let said = text(&o);
    assert!(said.contains(&format!("public key: {public}")), "{said}");
    assert!(!said.contains(saved.trim()), "the signing key is never printed");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // An existing file is never overwritten.
    let again = packs().args(["new-signing-key", "--out"]).arg(&out).output().unwrap();
    assert_eq!(again.status.code(), Some(1), "{}", text(&again));
    assert_eq!(fs::read_to_string(&out).unwrap(), saved);
    assert!(!text(&again).contains(saved.trim()));
    // Nowhere to write it: nothing made.
    let none = packs().arg("new-signing-key").output().unwrap();
    assert_eq!(none.status.code(), Some(2));
    assert_eq!(fs::read_dir(&d.0).unwrap().count(), 1);
}

#[test]
fn sign_index_signs_with_the_key_in_the_environment() {
    let d = tmp("sign");
    let (seed, public) = key();
    for name in [PACK_INDEX, EXTRAS_INDEX, VOICES_INDEX] {
        let index = d.0.join(name);
        fs::write(&index, BODY).unwrap();
        let o = packs().arg("sign-index").arg(&index).env(SIGNING_KEY_ENV, format!("{}\n", to_hex(&seed))).output().unwrap();
        assert!(o.status.success(), "{}", text(&o));
        assert!(!text(&o).contains(&to_hex(&seed)), "the signing key is never printed");
        assert!(text(&o).contains(&to_hex(&public)));
        let sig = fs::read(d.0.join(format!("{name}{SIG_SUFFIX}"))).unwrap();
        assert_eq!(verify_index(name, BODY, &sig, &[public]), Ok(()), "{name}");
        let others: Vec<&str> = [PACK_INDEX, EXTRAS_INDEX, VOICES_INDEX].into_iter().filter(|n| *n != name).collect();
        for other in others {
            assert_eq!(verify_index(other, BODY, &sig, &[public]), Err(SigError::Mismatch), "{name} as {other}");
        }
    }
    // --out picks the file.
    let index = d.0.join(PACK_INDEX);
    let out = d.0.join("elsewhere.sig");
    let o = packs().arg("sign-index").arg(&index).arg("--out").arg(&out).env(SIGNING_KEY_ENV, to_hex(&seed)).output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(verify_index(PACK_INDEX, BODY, &fs::read(&out).unwrap(), &[public]), Ok(()));
}

#[test]
fn sign_index_without_a_usable_key_signs_nothing() {
    let d = tmp("no-key");
    let index = d.0.join(PACK_INDEX);
    fs::write(&index, BODY).unwrap();
    let sig = d.0.join(format!("{PACK_INDEX}{SIG_SUFFIX}"));
    let not_a_key = "not-a-key-0123456789";
    for value in [None, Some(""), Some("  \n"), Some(not_a_key), Some(&"A".repeat(64)[..])] {
        let mut c = packs();
        c.arg("sign-index").arg(&index);
        if let Some(v) = value {
            c.env(SIGNING_KEY_ENV, v);
        }
        let o = c.output().unwrap();
        assert_eq!(o.status.code(), Some(1), "{value:?}: {}", text(&o));
        assert!(text(&o).contains(SIGNING_KEY_ENV), "{value:?}");
        assert!(!text(&o).contains(not_a_key), "the value is never printed");
        assert!(!sig.exists(), "{value:?}: no signature written");
    }
    // A missing index: nothing written either.
    let (seed, _) = key();
    let o = packs().arg("sign-index").arg(d.0.join("missing.json")).env(SIGNING_KEY_ENV, to_hex(&seed)).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(!d.0.join("missing.json.sig").exists());
}
