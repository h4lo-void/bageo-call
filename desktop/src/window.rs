// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Popup window: placing it next to the tray icon, showing and hiding it.

use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, Monitor, PhysicalPosition, PhysicalSize, WebviewWindow};

use crate::state::{lock, Shared};

pub const WIDTH: f64 = 356.0;
pub const DEFAULT_HEIGHT: f64 = 420.0;
pub const MIN_HEIGHT: f64 = 120.0;
pub const MAX_HEIGHT: f64 = 640.0;

/// Gap between the tray icon and the window, in logical pixels.
const ICON_GAP: f64 = 4.0;
/// Space kept free at the left and right screen edges, in logical pixels.
const SIDE_MARGIN: f64 = 8.0;
/// Space kept free at the corner when there is no icon position, in logical pixels.
const CORNER_MARGIN: f64 = 12.0;
/// A tray click this soon after the window hid itself is the click that caused it.
const REOPEN_GUARD: Duration = Duration::from_millis(300);

/// A rectangle in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    fn right(&self) -> f64 {
        self.x + self.w
    }
    fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    /// The tray icon rectangle from the last click.
    Icon(Rect),
    /// No icon position known: use a corner of the main screen.
    Corner,
}

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.max(lo).min(hi.max(lo))
}

/// Works out the top-left corner of the window. All values are physical pixels.
/// `screen` is the whole monitor, `work` the part not covered by the taskbar or menu bar.
pub fn place(anchor: Anchor, screen: Rect, work: Rect, scale: f64, w: f64, h: f64, mac: bool) -> (f64, f64) {
    let (x, y) = match anchor {
        Anchor::Icon(icon) => {
            let side = SIDE_MARGIN * scale;
            let gap = ICON_GAP * scale;
            let cx = icon.x + icon.w / 2.0;
            let cy = icon.y + icon.h / 2.0;
            let x = clamp(cx - w / 2.0, screen.x + side, screen.right() - w - side);
            let y = if cy < screen.y + screen.h / 2.0 {
                icon.bottom() + gap
            } else {
                icon.y - gap - h
            };
            (x, y)
        }
        Anchor::Corner => {
            let m = CORNER_MARGIN * scale;
            let x = work.right() - w - m;
            let y = if mac { work.y + m } else { work.bottom() - h - m };
            (x, y)
        }
    };
    (clamp(x, work.x, work.right() - w), clamp(y, work.y, work.bottom() - h))
}

fn monitor_rect(m: &Monitor) -> Rect {
    let p = m.position();
    let s = m.size();
    Rect { x: p.x as f64, y: p.y as f64, w: s.width as f64, h: s.height as f64 }
}

pub fn work_rect(m: &Monitor) -> Rect {
    let a = m.work_area();
    Rect { x: a.position.x as f64, y: a.position.y as f64, w: a.size.width as f64, h: a.size.height as f64 }
}

fn monitor_at(win: &WebviewWindow, x: f64, y: f64) -> Option<Monitor> {
    let found = win.available_monitors().ok().and_then(|list| {
        list.into_iter().find(|m| {
            let r = monitor_rect(m);
            x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
        })
    });
    found.or_else(|| win.primary_monitor().ok().flatten())
}

/// Converts the rectangle from a tray click into an anchor.
pub fn anchor_from_rect(app: &AppHandle, rect: &tauri::Rect) -> Anchor {
    let scale = app
        .get_webview_window("main")
        .and_then(|w| w.scale_factor().ok())
        .unwrap_or(1.0);
    let (x, y) = match &rect.position {
        tauri::Position::Physical(p) => (p.x as f64, p.y as f64),
        tauri::Position::Logical(p) => (p.x * scale, p.y * scale),
    };
    let (w, h) = match &rect.size {
        tauri::Size::Physical(s) => (s.width as f64, s.height as f64),
        tauri::Size::Logical(s) => (s.width * scale, s.height * scale),
    };
    Anchor::Icon(Rect { x, y, w, h })
}

/// Sizes and positions the window from the remembered anchor and height.
pub fn reposition(app: &AppHandle, win: &WebviewWindow) {
    let shared = app.state::<Shared>();
    let anchor = (*lock(&shared.anchor)).unwrap_or(Anchor::Corner);
    let height = *lock(&shared.height);
    let monitor = match anchor {
        Anchor::Icon(r) => monitor_at(win, r.x + r.w / 2.0, r.y + r.h / 2.0),
        Anchor::Corner => win.primary_monitor().ok().flatten(),
    }
    .or_else(|| win.current_monitor().ok().flatten());
    let Some(monitor) = monitor else { return };

    let scale = monitor.scale_factor();
    let w = (WIDTH * scale).round();
    let h = (height * scale).round();
    let (x, y) = place(anchor, monitor_rect(&monitor), work_rect(&monitor), scale, w, h, cfg!(target_os = "macos"));
    let _ = win.set_size(PhysicalSize::new(w as u32, h as u32));
    let _ = win.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

/// Shows the window (at the new anchor when given) and tells the page.
pub fn show(app: &AppHandle, anchor: Option<Anchor>) {
    let Some(win) = app.get_webview_window("main") else { return };
    if let Some(a) = anchor {
        *lock(&app.state::<Shared>().anchor) = Some(a);
    }
    reposition(app, &win);
    let _ = win.show();
    let _ = win.set_focus();
    let _ = win.emit("shown", ());
}

pub fn hide(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
}

/// Called when the window loses focus.
pub fn auto_hide(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
        *lock(&app.state::<Shared>().hidden_at) = Some(Instant::now());
    }
}

/// Left click on the tray icon.
pub fn toggle(app: &AppHandle, anchor: Anchor) {
    let Some(win) = app.get_webview_window("main") else { return };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
        return;
    }
    let recently_hidden = lock(&app.state::<Shared>().hidden_at)
        .map(|t| t.elapsed() < REOPEN_GUARD)
        .unwrap_or(false);
    if recently_hidden {
        return;
    }
    show(app, Some(anchor));
}

/// Changes the window height and keeps it attached to the icon.
pub fn fit(app: &AppHandle, height: f64) {
    let height = if height.is_finite() { height.clamp(MIN_HEIGHT, MAX_HEIGHT) } else { DEFAULT_HEIGHT };
    *lock(&app.state::<Shared>().height) = height;
    if let Some(win) = app.get_webview_window("main") {
        if win.is_visible().unwrap_or(false) {
            reposition(app, &win);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn taskbar_icon_puts_window_above_and_never_over_the_taskbar() {
        let screen = r(0.0, 0.0, 1920.0, 1080.0);
        let work = r(0.0, 0.0, 1920.0, 1040.0);
        // Icon in a floating taskbar that leaves room below the work area edge: 4px gap above the icon.
        let icon = r(1000.0, 900.0, 24.0, 24.0);
        let (x, y) = place(Anchor::Icon(icon), screen, work, 1.0, 356.0, 420.0, false);
        assert_eq!(x, 1012.0 - 178.0);
        assert_eq!(y, 900.0 - 4.0 - 420.0);
        // Icon inside the taskbar strip: the window ends at the top of the taskbar.
        let icon = r(1000.0, 1048.0, 24.0, 24.0);
        let (_, y) = place(Anchor::Icon(icon), screen, work, 1.0, 356.0, 420.0, false);
        assert_eq!(y, 1040.0 - 420.0);
    }

    #[test]
    fn menu_bar_icon_puts_window_below_with_gap() {
        let screen = r(0.0, 0.0, 1440.0, 900.0);
        let work = r(0.0, 24.0, 1440.0, 876.0);
        let icon = r(1000.0, 0.0, 24.0, 24.0);
        let (x, y) = place(Anchor::Icon(icon), screen, work, 1.0, 356.0, 420.0, true);
        assert_eq!(x, 1012.0 - 178.0);
        assert_eq!(y, 28.0);
    }

    #[test]
    fn side_margin_is_kept_at_both_edges() {
        let screen = r(0.0, 0.0, 1920.0, 1080.0);
        let work = r(0.0, 0.0, 1920.0, 1040.0);
        let left = place(Anchor::Icon(r(0.0, 1048.0, 24.0, 24.0)), screen, work, 1.0, 356.0, 420.0, false);
        assert_eq!(left.0, 8.0);
        let right = place(Anchor::Icon(r(1896.0, 1048.0, 24.0, 24.0)), screen, work, 1.0, 356.0, 420.0, false);
        assert_eq!(right.0, 1920.0 - 356.0 - 8.0);
    }

    #[test]
    fn window_stays_inside_work_area() {
        let screen = r(0.0, 0.0, 800.0, 600.0);
        let work = r(0.0, 0.0, 800.0, 560.0);
        // Icon near the top of a small screen with a tall window: pushed up into the work area.
        let (_, y) = place(Anchor::Icon(r(400.0, 570.0, 24.0, 24.0)), screen, work, 1.0, 356.0, 640.0, false);
        assert_eq!(y, 0.0);
        let (_, y) = place(Anchor::Icon(r(400.0, 0.0, 24.0, 24.0)), screen, work, 1.0, 356.0, 640.0, true);
        assert_eq!(y, 0.0);
    }

    #[test]
    fn scale_factor_scales_gaps_and_margins() {
        let screen = r(0.0, 0.0, 2880.0, 1800.0);
        let work = r(0.0, 48.0, 2880.0, 1752.0);
        let icon = r(0.0, 0.0, 48.0, 48.0);
        let (x, y) = place(Anchor::Icon(icon), screen, work, 2.0, 712.0, 840.0, true);
        assert_eq!(x, 16.0);
        assert_eq!(y, 56.0);
    }

    #[test]
    fn corner_anchor_depends_on_platform() {
        let screen = r(0.0, 0.0, 1440.0, 900.0);
        let work = r(0.0, 24.0, 1440.0, 836.0);
        let mac = place(Anchor::Corner, screen, work, 1.0, 356.0, 420.0, true);
        assert_eq!(mac, (1440.0 - 356.0 - 12.0, 24.0 + 12.0));
        let win = place(Anchor::Corner, screen, work, 1.0, 356.0, 420.0, false);
        assert_eq!(win, (1440.0 - 356.0 - 12.0, 860.0 - 420.0 - 12.0));
    }

    #[test]
    fn second_monitor_with_offset_origin() {
        let screen = r(-1920.0, 0.0, 1920.0, 1080.0);
        let work = r(-1920.0, 0.0, 1920.0, 1040.0);
        let icon = r(-1900.0, 1048.0, 24.0, 24.0);
        let (x, _) = place(Anchor::Icon(icon), screen, work, 1.0, 356.0, 420.0, false);
        assert_eq!(x, -1920.0 + 8.0);
    }
}
