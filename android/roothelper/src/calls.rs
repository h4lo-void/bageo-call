// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Whether the phone is in a call, as the helper sees it for itself (never from the
//! app): the call state the telephony registry keeps per SIM, from
//! `dumpsys telephony.registry` (`mCallState=0` idle, `1` ringing, `2` off the hook).
//! A call counts only while some SIM is off the hook; no state lines at all (the
//! format changed, or the service did not answer) counts as no call, so PCM is refused
//! rather than opened on a guess.

pub fn in_call_from_registry(text: &str) -> bool {
    text.lines().any(|line| {
        line.match_indices("mCallState=").any(|(i, m)| {
            let rest = &line[i + m.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits == "2"
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_the_hook_on_any_sim_is_a_call() {
        let idle = "Phone Id=0\n  mCallState=0\n  mCallIncomingNumber=\nPhone Id=1\n  mCallState=0\n";
        assert!(!in_call_from_registry(idle));
        assert!(!in_call_from_registry("  mCallState=1\n"), "ringing is not a call yet");
        assert!(in_call_from_registry("Phone Id=0\n  mCallState=0\nPhone Id=1\n  mCallState=2\n"));
        assert!(in_call_from_registry("  mCallState=2 mCallIncomingNumber=\n"));
        assert!(!in_call_from_registry("  mCallState=20\n"));
        assert!(!in_call_from_registry(""), "no state lines: no call");
        assert!(!in_call_from_registry("Can't find service: telephony.registry\n"));
    }
}
