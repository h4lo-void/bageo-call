// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Who the helper serves: only the Bageo app, recognised by its Linux user ID.
//!
//! The ID is looked up on every connection (it changes when the app is reinstalled),
//! from `/data/system/packages.list` and the owner of the app's own folder. When both
//! are known they must agree.

/// The app's package.
pub const APP_PACKAGE: &str = "com.pixelcallai.streamer";

/// Android's app ID range in user 0 (`FIRST_APPLICATION_UID`..=`LAST_APPLICATION_UID`).
const FIRST_APP_UID: u32 = 10_000;
const LAST_APP_UID: u32 = 19_999;

fn is_app_uid(uid: u32) -> bool {
    (FIRST_APP_UID..=LAST_APP_UID).contains(&uid)
}

/// The user ID `packages.list` gives the package: `<package> <uid> <debuggable> <data dir> ...`.
pub fn uid_from_packages_list(text: &str, package: &str) -> Option<u32> {
    let mut found = None;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some(package) {
            continue;
        }
        let uid = parts.next()?.parse::<u32>().ok()?;
        if found.is_some() {
            // The same package twice: the file cannot be trusted.
            return None;
        }
        found = Some(uid);
    }
    found.filter(|&u| is_app_uid(u))
}

/// The app's user ID from the two sources (either may be unknown); `None` when they
/// disagree or neither is an app ID.
pub fn resolve_app_uid(from_list: Option<u32>, dir_owner: Option<u32>) -> Option<u32> {
    let list = from_list.filter(|&u| is_app_uid(u));
    let dir = dir_owner.filter(|&u| is_app_uid(u));
    match (list, dir) {
        (Some(a), Some(b)) if a != b => None,
        (Some(a), _) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Whether the other end of a connection (its `SO_PEERCRED` user ID) is the app.
pub fn peer_allowed(peer_uid: u32, app_uid: Option<u32>) -> bool {
    match app_uid {
        Some(app) => is_app_uid(app) && peer_uid == app,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "com.android.shell 2000 0 /data/user_de/0/com.android.shell platform:privapp none\n\
        com.pixelcallai.streamer 10234 0 /data/user/0/com.pixelcallai.streamer platform:privapp:targetSdkVersion=34 3002,3003\n\
        com.pixelcallai.streamer.evil 10999 0 /data/user/0/x default none\n";

    #[test]
    fn finds_the_app_in_packages_list() {
        assert_eq!(uid_from_packages_list(LIST, APP_PACKAGE), Some(10234));
        assert_eq!(uid_from_packages_list(LIST, "com.android.shell"), None, "not an app ID");
        assert_eq!(uid_from_packages_list(LIST, "com.example.none"), None);
        assert_eq!(uid_from_packages_list("com.pixelcallai.streamer x 0", APP_PACKAGE), None);
        let twice = format!("{LIST}com.pixelcallai.streamer 10500 0 /data/user/0/y default none\n");
        assert_eq!(uid_from_packages_list(&twice, APP_PACKAGE), None);
    }

    #[test]
    fn sources_must_agree() {
        assert_eq!(resolve_app_uid(Some(10234), Some(10234)), Some(10234));
        assert_eq!(resolve_app_uid(Some(10234), None), Some(10234));
        assert_eq!(resolve_app_uid(None, Some(10234)), Some(10234));
        assert_eq!(resolve_app_uid(Some(10234), Some(10235)), None);
        assert_eq!(resolve_app_uid(None, Some(0)), None);
        assert_eq!(resolve_app_uid(Some(1000), Some(1000)), None);
        assert_eq!(resolve_app_uid(None, None), None);
    }

    #[test]
    fn only_the_app_is_served() {
        let app = Some(10234);
        assert!(peer_allowed(10234, app));
        assert!(!peer_allowed(10235, app), "another app");
        assert!(!peer_allowed(0, app), "root is not the app");
        assert!(!peer_allowed(1000, app), "system");
        assert!(!peer_allowed(2000, app), "adb shell");
        assert!(!peer_allowed(1_010_234, app), "the app in another user");
        assert!(!peer_allowed(10234, None), "app not installed");
        assert!(!peer_allowed(0, Some(0)), "a broken lookup never allows root");
    }
}
