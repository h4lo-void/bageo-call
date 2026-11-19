// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The small notification window in the screen corner.

use std::time::Duration;

use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::notify::{Kind, Notice};
use crate::state::{lock, Shared};
use crate::window::{work_rect, Rect};

const LABEL: &str = "toast";
const WIDTH: f64 = 340.0;
const HEIGHT: f64 = 104.0;
/// A call summary shows up to four lines of text, so its card is taller.
const HEIGHT_SUMMARY: f64 = 156.0;
/// Distance from the work area edges, in logical pixels.
const MARGIN: f64 = 6.0;
/// The window is closed after this long even if the page never asks for it.
const FAILSAFE: Duration = Duration::from_secs(45);

/// Top-left corner of the toast. Bottom right on Windows, top right on macOS. Physical pixels.
pub fn place(work: Rect, scale: f64, w: f64, h: f64, mac: bool) -> (f64, f64) {
    let m = MARGIN * scale;
    let x = work.x + work.w - w - m;
    let y = if mac { work.y + m } else { work.y + work.h - h - m };
    (x, y)
}

/// Window height for a kind of notification.
fn height_for(kind: Kind) -> f64 {
    if kind == Kind::Summary {
        HEIGHT_SUMMARY
    } else {
        HEIGHT
    }
}

/// Shows a notification: opens the window, or hands the new one to the open window.
/// Never takes keyboard focus.
pub fn show(app: &AppHandle, notice: Notice, lang: &str) {
    let shared = app.state::<Shared>();
    let (data, generation) = {
        let mut st = lock(&shared.toast);
        st.push(notice, lang);
        (st.data(), st.generation)
    };
    let Some(data) = data else { return };
    let height = height_for(data.kind);

    match app.get_webview_window(LABEL) {
        Some(win) => {
            resize(app, &win, height);
            let _ = win.emit("toast", &data);
        }
        None => create(app, height),
    }

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FAILSAFE).await;
        let current = lock(&app.state::<Shared>().toast).generation;
        if current == generation {
            close(&app);
        }
    });
}

/// Gives the open window the height of the notification it now shows, keeping it in its corner.
fn resize(app: &AppHandle, win: &WebviewWindow, height: f64) {
    let _ = win.set_size(LogicalSize::new(WIDTH, height));
    if let Some(monitor) = app.primary_monitor().ok().flatten() {
        let scale = monitor.scale_factor();
        let (x, y) = place(work_rect(&monitor), scale, (WIDTH * scale).round(), (height * scale).round(), cfg!(target_os = "macos"));
        let _ = win.set_position(LogicalPosition::new(x / scale, y / scale));
    }
}

fn create(app: &AppHandle, height: f64) {
    let mut builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("toast.html".into()))
        .title("Bageo")
        .inner_size(WIDTH, height)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        // The card never becomes the active window, so the first click must count.
        .accept_first_mouse(true)
        .visible_on_all_workspaces(true);
    if let Some(monitor) = app.primary_monitor().ok().flatten() {
        let scale = monitor.scale_factor();
        let (x, y) = place(work_rect(&monitor), scale, (WIDTH * scale).round(), (height * scale).round(), cfg!(target_os = "macos"));
        builder = builder.position(x / scale, y / scale);
    }
    let _ = builder.build();
}

/// Closes the window and forgets the notification.
pub fn close(app: &AppHandle) {
    lock(&app.state::<Shared>().toast).clear();
    if let Some(win) = app.get_webview_window(LABEL) {
        let _ = win.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn windows_puts_toast_in_bottom_right_corner() {
        let (x, y) = place(r(0.0, 0.0, 1920.0, 1040.0), 1.0, 340.0, 104.0, false);
        assert_eq!((x, y), (1920.0 - 340.0 - 6.0, 1040.0 - 104.0 - 6.0));
    }

    #[test]
    fn mac_puts_toast_in_top_right_corner_below_the_menu_bar() {
        let (x, y) = place(r(0.0, 25.0, 1440.0, 875.0), 1.0, 340.0, 104.0, true);
        assert_eq!((x, y), (1440.0 - 340.0 - 6.0, 25.0 + 6.0));
    }

    #[test]
    fn margin_scales_with_the_display() {
        let (x, y) = place(r(0.0, 0.0, 2880.0, 1760.0), 2.0, 680.0, 208.0, false);
        assert_eq!((x, y), (2880.0 - 680.0 - 12.0, 1760.0 - 208.0 - 12.0));
    }

    #[test]
    fn work_area_with_offset_origin() {
        let (x, _) = place(r(100.0, 0.0, 1000.0, 800.0), 1.0, 340.0, 104.0, false);
        assert_eq!(x, 100.0 + 1000.0 - 340.0 - 6.0);
    }

    #[test]
    fn only_call_summaries_get_the_taller_card() {
        assert_eq!(height_for(Kind::Call), HEIGHT);
        assert_eq!(height_for(Kind::Sms), HEIGHT);
        assert_eq!(height_for(Kind::Summary), HEIGHT_SUMMARY);
    }
}
