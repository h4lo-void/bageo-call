// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Verifies the phone app ships only the core languages listed in xml/locales_config.xml
//! (additional languages live in bageo-extras and are no longer bundled in the installer).

use std::fs;
use std::path::{Path, PathBuf};

use bageo_packs::export::{CORE_APP_LOCALES, GENERATED_MARK};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn res() -> PathBuf {
    repo().join("android/app/src/main/res")
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn android_ships_only_core_locales() {
    let cfg = read(&res().join("xml/locales_config.xml"));
    let mut got: Vec<String> =
        cfg.lines().filter_map(|l| Some(l.trim().strip_prefix("<locale android:name=\"")?.strip_suffix("\"/>")?.to_string())).collect();
    got.sort();
    let mut want: Vec<String> = CORE_APP_LOCALES.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(got, want);
    assert!(read(&repo().join("android/app/src/main/AndroidManifest.xml")).contains("android:localeConfig=\"@xml/locales_config\""));
    let allowed = ["de", "es", "fr", "ja", "ko", "night", "pt", "zh-rCN", "zh-rTW"];
    for d in fs::read_dir(res()).unwrap().flatten() {
        let name = d.file_name().to_string_lossy().to_string();
        if let Some(suffix) = name.strip_prefix("values-") {
            assert!(allowed.contains(&suffix), "unexpected extra locale folder values-{suffix}");
        }
        if name.starts_with("values") {
            let xml = d.path().join("strings.xml");
            if let Ok(text) = fs::read_to_string(&xml) {
                assert!(!text.contains(GENERATED_MARK), "{} still contains exported strings", xml.display());
            }
        }
    }
    assert!(!repo().join("desktop/ui/i18n.ext.json").exists(), "desktop/ui/i18n.ext.json must not be bundled");
}
