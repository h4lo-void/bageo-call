// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Draws the tray icon: the Bageo logo inside a frame whose colour shows the state.

use resvg::{tiny_skia, usvg};

use crate::status::Level;

const TEMPLATE: &str = include_str!("../icons/tray.svg");

/// Pixel size of the tray icon on this system.
pub fn tray_size() -> u32 {
    if cfg!(target_os = "macos") {
        44
    } else {
        32
    }
}

pub fn frame_color(level: Level) -> &'static str {
    match level {
        Level::Ok => "#2FA463",
        Level::Warn => "#E39A1F",
        Level::Bad => "#D8473D",
        Level::Unknown => "#8C909A",
    }
}

/// Renders the icon as straight (not premultiplied) RGBA bytes, `size` x `size`.
pub fn render_rgba(level: Level, size: u32) -> Vec<u8> {
    let svg = TEMPLATE.replace("{{frame}}", frame_color(level));
    let empty = vec![0u8; (size * size * 4) as usize];
    let Ok(tree) = usvg::Tree::from_str(&svg, &usvg::Options::default()) else {
        return empty;
    };
    let Some(mut pixmap) = tiny_skia::Pixmap::new(size, size) else {
        return empty;
    };
    let scale = size as f32 / tree.size().width();
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let mut data = pixmap.take();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(data: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    }

    #[test]
    fn frame_colour_follows_level() {
        let size = 32;
        for (level, rgb) in [
            (Level::Ok, [0x2F, 0xA4, 0x63]),
            (Level::Warn, [0xE3, 0x9A, 0x1F]),
            (Level::Bad, [0xD8, 0x47, 0x3D]),
            (Level::Unknown, [0x8C, 0x90, 0x9A]),
        ] {
            let data = render_rgba(level, size);
            assert_eq!(data.len(), (size * size * 4) as usize);
            // A point on the left frame edge, between the outer border and the logo tile.
            let p = pixel(&data, size, 2, 16);
            assert_eq!(p[3], 255);
            for (got, want) in p[..3].iter().zip(rgb) {
                assert!((*got as i32 - want as i32).abs() <= 2, "{level:?}: {p:?}");
            }
        }
    }

    #[test]
    fn corners_are_transparent_and_centre_is_opaque() {
        let size = 44;
        let data = render_rgba(Level::Ok, size);
        assert_eq!(pixel(&data, size, 0, 0)[3], 0);
        assert_eq!(pixel(&data, size, 22, 22)[3], 255);
    }
}
