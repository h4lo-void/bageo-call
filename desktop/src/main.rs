// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod i18n;
mod icon;
mod local;
mod notify;
mod poll;
mod state;
mod status;
mod toast;
mod tray;
mod window;

use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Manager, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_opener::OpenerExt as _;

use crate::config::Config;
use crate::state::{lock, Shared};
use crate::status::Snapshot;

#[derive(Serialize)]
struct ConfigView {
    address: String,
    has_password: bool,
    autostart: bool,
    notify_calls: bool,
    notify_sms: bool,
    notify_summary: bool,
}

#[tauri::command]
fn get_snapshot(state: State<'_, Shared>) -> Snapshot {
    lock(&state.snapshot).clone()
}

#[tauri::command]
fn get_config(state: State<'_, Shared>) -> ConfigView {
    let cfg = lock(&state.config).clone();
    ConfigView {
        address: cfg.address,
        has_password: lock(&state.password).is_some(),
        autostart: cfg.autostart,
        notify_calls: cfg.notify_calls,
        notify_sms: cfg.notify_sms,
        notify_summary: cfg.notify_summary,
    }
}

#[tauri::command(rename_all = "snake_case")]
async fn save_config(
    app: AppHandle,
    state: State<'_, Shared>,
    address: String,
    password: Option<String>,
    clear_password: bool,
    autostart: bool,
    notify_calls: bool,
    notify_sms: bool,
    notify_summary: bool,
) -> Result<(), String> {
    let address = address.trim();
    let address = if address.is_empty() {
        String::new()
    } else {
        status::normalize_address(address).ok_or_else(|| "addr".to_string())?
    };

    // Save what can be saved first, then report the first thing that failed:
    // a keychain or login-item problem must not throw away the other settings.
    let mut failed: Option<&'static str> = None;

    let launcher = app.autolaunch();
    let result = if autostart { launcher.enable() } else { launcher.disable() };
    let autostart_on = autostart && result.is_ok();
    if autostart && !autostart_on {
        failed = Some("autostart");
    }

    let (opened_console, agent_path) = {
        let old = lock(&state.config);
        (old.opened_console, old.agent_path.clone())
    };
    let cfg = Config { address, autostart: autostart_on, notify_calls, notify_sms, notify_summary, opened_console, agent_path };
    if config::save(&app, &cfg).is_err() {
        failed = failed.or(Some("config"));
    }
    *lock(&state.config) = cfg;

    if clear_password {
        let deleted = tauri::async_runtime::spawn_blocking(config::delete_password).await;
        if !matches!(deleted, Ok(Ok(()))) {
            failed = failed.or(Some("keychain"));
        }
        *lock(&state.password) = None;
    }
    if let Some(p) = password.filter(|p| !p.is_empty()) {
        let to_store = p.clone();
        let stored = tauri::async_runtime::spawn_blocking(move || config::store_password(&to_store)).await;
        if !matches!(stored, Ok(Ok(()))) {
            failed = failed.or(Some("keychain"));
        }
        // Used until the program closes even when the keychain refused it.
        *lock(&state.password) = Some(p);
    }

    state.reset.store(true, Ordering::SeqCst);
    state.notify.notify_one();
    match failed {
        Some(code) => Err(code.to_string()),
        None => Ok(()),
    }
}

/// Opens the console in the default browser, then hides the window. Does nothing without an address.
pub fn launch_console(app: &AppHandle) {
    let address = lock(&app.state::<Shared>().snapshot).address.clone();
    if address.is_empty() {
        return;
    }
    let _ = app.opener().open_url(address, None::<&str>);
    window::hide(app);
}

#[tauri::command]
fn open_console(app: AppHandle) {
    launch_console(&app);
}

#[tauri::command]
fn toast_get(state: State<'_, Shared>) -> Option<notify::ToastData> {
    lock(&state.toast).data()
}

/// Opens the notification's link (or the console when it has none), then closes the notification.
#[tauri::command]
async fn toast_open(app: AppHandle) {
    let shared = app.state::<Shared>();
    let url = lock(&shared.toast).current.as_ref().map(|n| n.url.clone()).unwrap_or_default();
    let target = if url.is_empty() { lock(&shared.snapshot).address.clone() } else { url };
    if status::is_web_url(&target) {
        let _ = app.opener().open_url(target, None::<&str>);
    }
    toast::close(&app);
}

#[tauri::command]
async fn toast_close(app: AppHandle) {
    toast::close(&app);
}

#[tauri::command]
fn refresh(state: State<'_, Shared>) {
    state.notify.notify_one();
}

#[tauri::command]
fn hide(app: AppHandle) {
    window::hide(&app);
}

/// Quits. When this program started Bageo, Bageo is stopped first.
pub fn quit_app(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        local::stop(&app);
        app.exit(0);
    });
}

#[tauri::command]
fn quit(app: AppHandle) {
    quit_app(&app);
}

/// Downloads the Android tools into the program folder. Progress shows in the snapshot's `adb` field.
#[tauri::command]
fn install_adb(app: AppHandle) {
    tauri::async_runtime::spawn(local::install_adb(app));
}

#[tauri::command]
fn fit(app: AppHandle, height: f64) {
    window::fit(&app, height);
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            window::show(app, None);
        }))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_config,
            save_config,
            open_console,
            toast_get,
            toast_open,
            toast_close,
            refresh,
            hide,
            quit,
            install_adb,
            fit
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let handle = app.handle().clone();
            let (mut cfg, existed) = config::load(&handle);
            let local = local::detect(&cfg.agent_path);
            let found = local.as_ref().map(|l| l.agent_path()).unwrap_or_default();
            let remember = !found.is_empty() && found != cfg.agent_path;
            if remember {
                cfg.agent_path = found;
            }
            if !existed || remember {
                let _ = config::save(&handle, &cfg);
            }
            let locale = sys_locale::get_locale().unwrap_or_default();
            let lang = status::resolve_lang(None, &locale);
            let first = Snapshot::blank(status::Level::Unknown, status::Reason::Starting, lang, false, "");
            app.manage(Shared::new(cfg, first.clone(), local));

            tray::build(&handle, &first)?;

            if let Some(win) = app.get_webview_window("main") {
                let h = handle.clone();
                win.on_window_event(move |event| match event {
                    WindowEvent::Focused(false) => window::auto_hide(&h),
                    WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        window::hide(&h);
                    }
                    _ => {}
                });
            }

            poll::spawn(handle.clone());
            if !existed {
                window::show(&handle, None);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to run Bageo Status")
        .run(|app, event| {
            // Whatever ends this program, a Bageo it started must not be left behind.
            if let tauri::RunEvent::Exit = event {
                local::stop(app);
            }
        });
}
