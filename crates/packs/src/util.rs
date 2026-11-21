// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Small helpers: text folding, calendar dates, hashing. No dependency beyond sha2.

use std::time::{SystemTime, UNIX_EPOCH};

/// A light NFKC: full-width ASCII (U+FF01..U+FF5E) becomes ASCII and the
/// ideographic space becomes a space. It is used to compare text only (the
/// text itself is never rewritten), so full-width digits or letters cannot
/// slip past the digit, URL and banned-phrase checks.
pub fn fold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{3000}' => ' ',
            _ => c,
        })
        .collect()
}

/// Lower-case hex SHA-256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

pub(crate) fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Days since 1970-01-01 of a calendar date (proleptic Gregorian).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The calendar date of a day number counted from 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn valid_date(y: i64, m: u32, d: u32) -> bool {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let max = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    d >= 1 && d <= max
}

/// YYYY-MM-DD, a real calendar date.
pub fn parse_date(s: &str) -> Option<(i64, u32, u32)> {
    let b = s.as_bytes();
    if !s.is_ascii() || b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        let t = &s[r];
        if t.bytes().all(|c| c.is_ascii_digit()) {
            t.parse().ok()
        } else {
            None
        }
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    valid_date(y as i64, m, d).then_some((y as i64, m, d))
}

/// YYYY-MM-DDTHH:MM:SSZ (UTC).
pub fn valid_rfc3339_utc(s: &str) -> bool {
    let b = s.as_bytes();
    if !s.is_ascii() || b.len() != 20 || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return false;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        let t = &s[r];
        if t.bytes().all(|c| c.is_ascii_digit()) {
            t.parse().ok()
        } else {
            None
        }
    };
    parse_date(&s[..10]).is_some()
        && matches!(num(11..13), Some(h) if h < 24)
        && matches!(num(14..16), Some(m) if m < 60)
        && matches!(num(17..19), Some(s) if s < 60)
}

/// Whole days since 1970-01-01 (UTC) on this machine's clock.
pub fn now_days() -> i64 {
    now_secs().div_euclid(86_400)
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Now, as written into a bundle's built_at.
pub fn now_rfc3339() -> String {
    let secs = now_secs();
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", t / 3600, t % 3600 / 60, t % 60)
}

/// Today's first version number, YYYYMMDD01.
pub fn today_version() -> u64 {
    let (y, m, d) = civil_from_days(now_days());
    (y as u64) * 1_000_000 + (m as u64) * 10_000 + (d as u64) * 100 + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_turns_full_width_into_ascii() {
        assert_eq!(fold("１２３４５６"), "123456");
        assert_eq!(fold("ｈｏｌｄ　ｏｎ［ｘ］"), "hold on[x]");
        assert_eq!(fold("喂，聽得到嗎？"), "喂,聽得到嗎?");
    }

    #[test]
    fn calendar_round_trips() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        for (y, m, d) in [(2026, 11, 20), (2024, 2, 29), (2000, 3, 1), (1999, 12, 31)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
        assert_eq!(days_from_civil(2026, 11, 21) - days_from_civil(2026, 11, 20), 1);
    }

    #[test]
    fn dates_are_real_dates() {
        assert!(parse_date("2024-02-29").is_some());
        assert!(parse_date("2025-02-29").is_none());
        assert!(parse_date("2025-13-01").is_none());
        assert!(parse_date("2025-1-01").is_none());
        assert!(valid_rfc3339_utc("2026-11-20T08:00:00Z"));
        assert!(!valid_rfc3339_utc("2026-11-20 08:00:00Z"));
        assert!(!valid_rfc3339_utc("2026-11-20T25:00:00Z"));
    }

    #[test]
    fn the_clock_gives_a_plausible_version() {
        let v = today_version();
        assert!((2_000_010_100..3_000_000_000).contains(&v), "{v}");
        assert!(valid_rfc3339_utc(&now_rfc3339()));
    }
}
