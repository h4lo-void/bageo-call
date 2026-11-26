// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Who the helper serves: only the Bageo app of the main user, and only when the system
//! says plainly that it is a normal (not debuggable) app with a user ID of its own.
//! Anything unclear is a refusal.
//!
//! The source is `/data/system/packages.list`, which PackageManager writes, one line per
//! package (fields separated by spaces):
//!
//! ```text
//! <package> <user ID> <debuggable 0|1> <data folder> <SELinux info> <groups> ...
//! ```
//!
//! A debuggable app can be entered with `adb shell run-as`, which then runs as the app's
//! user ID; so a debuggable Bageo is refused. (The module currently ships a debug build
//! of the app, so the helper refuses until the module ships a release build.) The owner
//! of the app's device-protected folder must match the list.

/// The app's package.
pub const APP_PACKAGE: &str = "com.pixelcallai.streamer";

/// Android's app ID range in the main user (`FIRST_APPLICATION_UID`..=`LAST_APPLICATION_UID`).
const FIRST_APP_UID: u32 = 10_000;
const LAST_APP_UID: u32 = 19_999;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageEntry {
    pub name: String,
    pub uid: u32,
    pub debuggable: bool,
    pub data_dir: String,
}

/// Why the app was not accepted. Only these fixed names are logged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    NoPackageList,
    Malformed,
    NotInstalled,
    Duplicate,
    Debuggable,
    SharedUserId,
    NotAppUid,
    WrongDataDir,
    FolderUnknown,
    FolderMismatch,
}

impl Refusal {
    pub fn name(self) -> &'static str {
        match self {
            Refusal::NoPackageList => "package list unreadable",
            Refusal::Malformed => "package list malformed",
            Refusal::NotInstalled => "app not installed",
            Refusal::Duplicate => "app listed twice",
            Refusal::Debuggable => "app is a debuggable build",
            Refusal::SharedUserId => "app shares its user ID",
            Refusal::NotAppUid => "app user ID out of range",
            Refusal::WrongDataDir => "app data folder unexpected",
            Refusal::FolderUnknown => "app folder owner unknown",
            Refusal::FolderMismatch => "app folder owner differs",
        }
    }
}

/// Every line of `packages.list`; any line that does not have the expected fields
/// makes the whole file unusable.
pub fn parse_packages_list(text: &str) -> Result<Vec<PackageEntry>, Refusal> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(' ').collect();
        if f.len() < 4 || f[0].is_empty() {
            return Err(Refusal::Malformed);
        }
        let uid = f[1].parse::<u32>().map_err(|_| Refusal::Malformed)?;
        let debuggable = match f[2] {
            "0" => false,
            "1" => true,
            _ => return Err(Refusal::Malformed),
        };
        out.push(PackageEntry { name: f[0].to_string(), uid, debuggable, data_dir: f[3].to_string() });
    }
    Ok(out)
}

/// The app's user ID, or why it must not be served. `list`: the contents of
/// `packages.list` (`None`: could not be read); `folder_owner`: the owner of
/// `/data/user_de/0/<package>` (`None`: could not be read).
pub fn app_uid(list: Option<&str>, folder_owner: Option<u32>) -> Result<u32, Refusal> {
    let entries = parse_packages_list(list.ok_or(Refusal::NoPackageList)?)?;
    let mut mine = entries.iter().filter(|e| e.name == APP_PACKAGE);
    let app = mine.next().ok_or(Refusal::NotInstalled)?;
    if mine.next().is_some() {
        return Err(Refusal::Duplicate);
    }
    if !(FIRST_APP_UID..=LAST_APP_UID).contains(&app.uid) {
        return Err(Refusal::NotAppUid);
    }
    if app.debuggable {
        return Err(Refusal::Debuggable);
    }
    if entries.iter().any(|e| e.uid == app.uid && e.name != APP_PACKAGE) {
        return Err(Refusal::SharedUserId);
    }
    if app.data_dir != format!("/data/user/0/{APP_PACKAGE}") {
        return Err(Refusal::WrongDataDir);
    }
    match folder_owner {
        None => Err(Refusal::FolderUnknown),
        Some(o) if o != app.uid => Err(Refusal::FolderMismatch),
        Some(_) => Ok(app.uid),
    }
}

/// Whether the other end of a connection (its `SO_PEERCRED` user ID) is the app in the
/// main user. The app's copy in a work profile or another user has another ID
/// (`user * 100000 + app ID`) and is refused.
pub fn peer_allowed(peer_uid: u32, app_uid: u32) -> bool {
    (FIRST_APP_UID..=LAST_APP_UID).contains(&app_uid) && peer_uid == app_uid
}

/// Whether a peer's SELinux context (`SO_PEERSEC`) is a privileged app's: the domain
/// `priv_app`, or a versioned `priv_app_<n>` as on newer Android.
pub fn context_allowed(ctx: &str) -> bool {
    let ctx = ctx.trim_end_matches('\0');
    let mut parts = ctx.splitn(4, ':');
    let (Some("u"), Some("r"), Some(ty), Some(level)) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if level.is_empty() {
        return false;
    }
    ty == "priv_app"
        || ty.strip_prefix("priv_app_").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = "com.android.shell 2000 0 /data/user/0/com.android.shell platform:privapp none 0 34 1 @system\n\
        com.pixelcallai.streamer 10234 0 /data/user/0/com.pixelcallai.streamer default:privapp:targetSdkVersion=34 3003 0 27 1 @system\n\
        com.example.other 10235 0 /data/user/0/com.example.other default:targetSdkVersion=34 3003 0 1 1 @null\n";

    #[test]
    fn reads_the_package_list() {
        let e = parse_packages_list(OK).unwrap();
        assert_eq!(e.len(), 3);
        assert_eq!(e[1].uid, 10234);
        assert!(!e[1].debuggable);
        assert_eq!(e[1].data_dir, "/data/user/0/com.pixelcallai.streamer");
        assert_eq!(parse_packages_list("a 1 2 /d x"), Err(Refusal::Malformed), "debug flag must be 0 or 1");
        assert_eq!(parse_packages_list("a x 0 /d x"), Err(Refusal::Malformed));
        assert_eq!(parse_packages_list("a 10000 0"), Err(Refusal::Malformed));
        assert_eq!(parse_packages_list(" 10000 0 /d"), Err(Refusal::Malformed));
    }

    #[test]
    fn accepts_only_a_clear_case() {
        assert_eq!(app_uid(Some(OK), Some(10234)), Ok(10234));
    }

    #[test]
    fn refuses_everything_unclear() {
        assert_eq!(app_uid(None, Some(10234)), Err(Refusal::NoPackageList));
        assert_eq!(app_uid(Some(OK), None), Err(Refusal::FolderUnknown), "no fallback either way");
        assert_eq!(app_uid(Some(OK), Some(10235)), Err(Refusal::FolderMismatch));
        let debug = OK.replace("streamer 10234 0 ", "streamer 10234 1 ");
        assert_eq!(app_uid(Some(&debug), Some(10234)), Err(Refusal::Debuggable));
        let twice = format!("{OK}com.pixelcallai.streamer 10234 0 /data/user/0/com.pixelcallai.streamer x 0\n");
        assert_eq!(app_uid(Some(&twice), Some(10234)), Err(Refusal::Duplicate));
        let shared = OK.replace("com.example.other 10235", "com.example.other 10234");
        assert_eq!(app_uid(Some(&shared), Some(10234)), Err(Refusal::SharedUserId));
        let system = OK.replace("streamer 10234 0", "streamer 1000 0");
        assert_eq!(app_uid(Some(&system), Some(1000)), Err(Refusal::NotAppUid));
        let moved = OK.replace("/data/user/0/com.pixelcallai.streamer", "/data/data/x");
        assert_eq!(app_uid(Some(&moved), Some(10234)), Err(Refusal::WrongDataDir));
        let broken = format!("{OK}garbage\n");
        assert_eq!(app_uid(Some(&broken), Some(10234)), Err(Refusal::Malformed));
        assert_eq!(app_uid(Some(""), Some(10234)), Err(Refusal::NotInstalled));
    }

    #[test]
    fn only_the_main_user_app_is_served() {
        assert!(peer_allowed(10234, 10234));
        assert!(!peer_allowed(10235, 10234), "another app");
        assert!(!peer_allowed(0, 10234), "root is not the app");
        assert!(!peer_allowed(2000, 10234), "adb shell");
        assert!(!peer_allowed(1_010_234, 10234), "the app's copy in a work profile (user 10)");
        assert!(!peer_allowed(0, 0));
    }

    #[test]
    fn privileged_app_contexts_only() {
        assert!(context_allowed("u:r:priv_app:s0:c512,c768"));
        assert!(context_allowed("u:r:priv_app_36:s0:c512,c768\0"));
        assert!(!context_allowed("u:r:untrusted_app:s0:c1,c256,c512,c768"));
        assert!(!context_allowed("u:r:runas_app:s0:c512,c768"));
        assert!(!context_allowed("u:r:shell:s0"));
        assert!(!context_allowed("u:r:priv_app_x:s0"));
        assert!(!context_allowed("u:r:priv_app_:s0"));
        assert!(!context_allowed("u:object_r:priv_app:s0"));
        assert!(!context_allowed("u:r:priv_app"));
        assert!(!context_allowed(""));
    }
}
