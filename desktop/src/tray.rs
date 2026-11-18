// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Tray icon, its tooltip and its right-click menu.

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::AppHandle;

use crate::i18n;
use crate::icon;
use crate::status::{Level, Snapshot};
use crate::window;

const TRAY_ID: &str = "main";
/// Windows cuts tooltips at 127 characters.
const TOOLTIP_MAX: usize = 120;

/// `local`: this program also runs Bageo, so quitting stops both.
fn menu(app: &AppHandle, lang: &str, local: bool) -> tauri::Result<Menu<tauri::Wry>> {
    let show = MenuItem::with_id(app, "menu_show", i18n::t(lang, "menu_show"), true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", i18n::t(lang, "open"), true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit_key = if local { "quit_all" } else { "quit" };
    let quit = MenuItem::with_id(app, "quit", i18n::t(lang, quit_key), true, None::<&str>)?;
    Menu::with_items(app, &[&show, &open, &sep, &quit])
}

fn image(level: Level) -> Image<'static> {
    let size = icon::tray_size();
    Image::new_owned(icon::render_rgba(level, size), size, size)
}

fn tooltip(snap: &Snapshot) -> String {
    let text = format!("Bageo · {}", i18n::reason_text(snap));
    text.chars().take(TOOLTIP_MAX).collect()
}

pub fn build(app: &AppHandle, snap: &Snapshot) -> tauri::Result<()> {
    let builder = TrayIconBuilder::with_id(TRAY_ID)
        .icon(image(snap.level))
        .tooltip(tooltip(snap))
        .menu(&menu(app, &snap.lang, snap.local)?)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "menu_show" => window::show(app, None),
            "open" => crate::launch_console(app),
            "quit" => crate::quit_app(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, rect, .. } = event {
                let app = tray.app_handle();
                let anchor = window::anchor_from_rect(app, &rect);
                window::toggle(app, anchor);
            }
        });
    #[cfg(target_os = "macos")]
    let builder = builder.icon_as_template(false);
    builder.build(app)?;
    Ok(())
}

/// Refreshes the icon (only when the level changed), the tooltip, and the menu (only when its texts changed).
pub fn update(app: &AppHandle, snap: &Snapshot, level_changed: bool, menu_changed: bool) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    if level_changed {
        let _ = tray.set_icon(Some(image(snap.level)));
        #[cfg(target_os = "macos")]
        let _ = tray.set_icon_as_template(false);
    }
    let _ = tray.set_tooltip(Some(tooltip(snap)));
    if menu_changed {
        if let Ok(m) = menu(app, &snap.lang, snap.local) {
            let _ = tray.set_menu(Some(m));
        }
    }
}
