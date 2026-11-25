//! Small cached icon overlay badges (shortcut arrow for `.lnk`, link badge
//! for symlinks/junctions, and OneDrive cloud/check badges for Files On-Demand).
//!
//! Each badge is a static 16×16 premultiplied-BGRA bitmap rasterized once into
//! a `OnceLock` and uploaded once to the D2D bitmap cache under a fixed key.
//! Drawing per row is a single `Canvas::image` call with zero heap allocation.

use crate::data::RowBuf;
use std::sync::OnceLock;
use tsubame_core::{CLOUD_ONLINE, CLOUD_PINNED, CLOUD_UNPINNED, SYMLINK};
use tsubame_ui::{Canvas, Rect};

const SZ: usize = 16;

/// Which Explorer-style overlay badge an entry's icon carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconOverlay {
    None,
    /// `.lnk` Windows shortcut file.
    Shortcut,
    /// NTFS symlink or directory junction (`SYMLINK` flag).
    Symlink,
    /// OneDrive online-only placeholder (`FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`).
    CloudOnline,
    /// OneDrive "Always keep on this device" (`FILE_ATTRIBUTE_PINNED`).
    CloudPinned,
    /// OneDrive locally available placeholder (`FILE_ATTRIBUTE_UNPINNED`).
    CloudSynced,
}

/// Classifies the icon overlay of a decoded row. Zero allocations.
#[inline]
pub fn classify(row: &RowBuf) -> IconOverlay {
    if !row.is_dir() && row.ext().eq_ignore_ascii_case(".lnk") {
        return IconOverlay::Shortcut;
    }
    if row.flags & SYMLINK != 0 {
        return IconOverlay::Symlink;
    }
    if row.flags & CLOUD_ONLINE != 0 {
        return IconOverlay::CloudOnline;
    }
    if row.flags & CLOUD_PINNED != 0 {
        return IconOverlay::CloudPinned;
    }
    if row.flags & CLOUD_UNPINNED != 0 {
        return IconOverlay::CloudSynced;
    }
    IconOverlay::None
}

/// Premultiplied BGRA pixel from `(r, g, b, a)`.
#[inline]
const fn bgra(r: u8, g: u8, b: u8, a: u8) -> u32 {
    let pr = ((r as u32 * a as u32 + 127) / 255) as u8;
    let pg = ((g as u32 * a as u32 + 127) / 255) as u8;
    let pb = ((b as u32 * a as u32 + 127) / 255) as u8;
    (pb as u32) | ((pg as u32) << 8) | ((pr as u32) << 16) | ((a as u32) << 24)
}

#[inline]
fn set(px: &mut [u32; SZ * SZ], x: usize, y: usize, c: u32) {
    if x < SZ && y < SZ {
        px[y * SZ + x] = c;
    }
}

/// Explorer-style shortcut/link badge: white rounded square with a crisp dark
/// border and a curved up-right arrow in blue (shortcut) or teal (symlink).
fn make_arrow_badge(arrow: u32) -> [u32; SZ * SZ] {
    let mut px = [0u32; SZ * SZ];
    let bg = bgra(250, 250, 252, 245);
    let border = bgra(72, 82, 98, 235);
    for y in 1..15 {
        for x in 1..15 {
            let corner = (x == 1 || x == 14) && (y == 1 || y == 14);
            if corner {
                continue;
            }
            let edge = x == 1 || x == 14 || y == 1 || y == 14;
            set(&mut px, x, y, if edge { border } else { bg });
        }
    }
    // Curved shortcut arrow pointing top-right:
    // stem curving from bottom-left (4, 12) up-right to arrow head at (11, 5)
    let pts: &[(usize, usize)] = &[
        // arrow head (top-right corner at (11, 4))
        (7, 4), (8, 4), (9, 4), (10, 4), (11, 4),
        (8, 5), (9, 5), (10, 5), (11, 5),
        (10, 6), (11, 6),
        (10, 7), (11, 7),
        (11, 8),
        // diagonal shaft curving down-left
        (9, 6), (8, 6),
        (8, 7), (7, 7),
        (7, 8), (6, 8),
        (6, 9), (5, 9),
        (5, 10), (4, 10),
        (5, 11), (4, 11),
        (4, 12),
    ];
    for &(x, y) in pts {
        set(&mut px, x, y, arrow);
    }
    px
}

/// OneDrive online-only badge: light pill with a crisp blue cloud outline.
fn make_cloud_badge() -> [u32; SZ * SZ] {
    let mut px = [0u32; SZ * SZ];
    let blue = bgra(0, 120, 212, 255);
    let fill = bgra(244, 249, 255, 245);
    // Cloud silhouette from 3 circles + flat base
    let inside = |x: f32, y: f32| -> bool {
        let c1 = (x - 5.2).hypot(y - 9.2) <= 3.3;
        let c2 = (x - 8.5).hypot(y - 6.8) <= 3.8;
        let c3 = (x - 11.4).hypot(y - 9.6) <= 2.9;
        let base = (4.5..=11.8).contains(&x) && (8.5..=12.4).contains(&y);
        c1 || c2 || c3 || base
    };
    for y in 0..SZ {
        for x in 0..SZ {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            if !inside(fx, fy) {
                continue;
            }
            let inner = inside(fx - 1.2, fy) && inside(fx + 1.2, fy) && inside(fx, fy - 1.2) && inside(fx, fy + 1.2);
            set(&mut px, x, y, if inner { fill } else { blue });
        }
    }
    px
}

/// OneDrive check badge: `solid` = filled green circle with white check (pinned);
/// `!solid` = white circle with green outline and green check (locally available).
fn make_check_badge(solid: bool) -> [u32; SZ * SZ] {
    let mut px = [0u32; SZ * SZ];
    let green = bgra(16, 137, 62, 255);
    let white = bgra(252, 252, 252, 250);
    let (cx, cy) = (8.0f32, 8.0f32);
    for y in 0..SZ {
        for x in 0..SZ {
            let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
            if d <= 6.8 {
                let c = if solid || d >= 5.2 { green } else { white };
                set(&mut px, x, y, c);
            }
        }
    }
    let check_col = if solid { white } else { green };
    let check: &[(usize, usize)] = &[
        (4, 8), (5, 9), (6, 10), (7, 11),
        (5, 8), (6, 9), (7, 10),
        (8, 9), (9, 8), (10, 7), (11, 6), (12, 5),
        (8, 10), (9, 9), (10, 8), (11, 7),
    ];
    for &(x, y) in check {
        set(&mut px, x, y, check_col);
    }
    px
}

fn bitmap_for(ov: IconOverlay) -> Option<(&'static str, &'static [u32; SZ * SZ])> {
    static LNK: OnceLock<[u32; SZ * SZ]> = OnceLock::new();
    static SYM: OnceLock<[u32; SZ * SZ]> = OnceLock::new();
    static CLOUD: OnceLock<[u32; SZ * SZ]> = OnceLock::new();
    static PIN: OnceLock<[u32; SZ * SZ]> = OnceLock::new();
    static SYNC: OnceLock<[u32; SZ * SZ]> = OnceLock::new();
    match ov {
        IconOverlay::None => None,
        IconOverlay::Shortcut => Some(("ov:lnk", LNK.get_or_init(|| make_arrow_badge(bgra(0, 103, 192, 255))))),
        IconOverlay::Symlink => Some(("ov:sym", SYM.get_or_init(|| make_arrow_badge(bgra(0, 130, 114, 255))))),
        IconOverlay::CloudOnline => Some(("ov:cloud", CLOUD.get_or_init(make_cloud_badge))),
        IconOverlay::CloudPinned => Some(("ov:pin", PIN.get_or_init(|| make_check_badge(true)))),
        IconOverlay::CloudSynced => Some(("ov:sync", SYNC.get_or_init(|| make_check_badge(false)))),
    }
}

/// Draws `ov` in the bottom-left corner of `icon_rect`. Zero allocations.
#[inline]
pub fn draw(g: &mut dyn Canvas, icon_rect: Rect, ov: IconOverlay) {
    let Some((key, px)) = bitmap_for(ov) else { return };
    let side = (icon_rect.w * 0.54).clamp(8.0, 20.0).round();
    let r = Rect::new(icon_rect.x - 1.0, icon_rect.b() - side + 1.0, side, side);
    g.image(key, SZ as u32, SZ as u32, px, r);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tsubame_ui::TestCanvas;

    fn row(name: &str, dir: bool, flags: u16) -> RowBuf {
        let mut r = RowBuf::new();
        r.name[..name.len()].copy_from_slice(name.as_bytes());
        r.name_len = name.len();
        r.kind = if dir { 0 } else { crate::data::kind_of(name, false) };
        r.flags = flags;
        r
    }

    #[test]
    fn classifies_lnk_symlink_and_onedrive_flags() {
        assert_eq!(classify(&row("plain.txt", false, 0)), IconOverlay::None);
        assert_eq!(classify(&row("Folder", true, tsubame_core::FOLDER)), IconOverlay::None);
        assert_eq!(classify(&row("App.lnk", false, 0)), IconOverlay::Shortcut);
        assert_eq!(classify(&row("APP.LNK", false, 0)), IconOverlay::Shortcut);
        assert_eq!(classify(&row("LinkDir.lnk", true, tsubame_core::FOLDER)), IconOverlay::None, "folder named .lnk is not a shortcut file");
        assert_eq!(classify(&row("Junction", true, tsubame_core::FOLDER | SYMLINK)), IconOverlay::Symlink);
        assert_eq!(classify(&row("cloud.docx", false, CLOUD_ONLINE)), IconOverlay::CloudOnline);
        assert_eq!(classify(&row("pinned.docx", false, CLOUD_PINNED)), IconOverlay::CloudPinned);
        assert_eq!(classify(&row("local.docx", false, CLOUD_UNPINNED)), IconOverlay::CloudSynced);
        // Online-only takes precedence over unpinned when both attributes are set
        assert_eq!(classify(&row("both.docx", false, CLOUD_ONLINE | CLOUD_UNPINNED)), IconOverlay::CloudOnline);
    }

    #[test]
    fn cached_bitmaps_draw_under_fixed_keys_and_none_draws_nothing() {
        let mut c = TestCanvas::new(200.0, 200.0);
        let ir = Rect::new(8.0, 3.0, 16.0, 16.0);
        draw(&mut c, ir, IconOverlay::None);
        assert!(c.images().is_empty());

        for (ov, key) in [
            (IconOverlay::Shortcut, "ov:lnk"),
            (IconOverlay::Symlink, "ov:sym"),
            (IconOverlay::CloudOnline, "ov:cloud"),
            (IconOverlay::CloudPinned, "ov:pin"),
            (IconOverlay::CloudSynced, "ov:sync"),
        ] {
            let (k, px) = bitmap_for(ov).unwrap();
            assert_eq!(k, key);
            assert!(px.iter().any(|&p| (p >> 24) > 0), "{key} has opaque pixels");
            c.clear();
            draw(&mut c, ir, ov);
            assert_eq!(c.images(), vec![key]);
        }
    }
}
