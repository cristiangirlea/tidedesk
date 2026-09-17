//! Fitting the remote screen into the window, and mapping the pointer back.

/// Where the remote image lands inside the window (letterboxed, aspect kept).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Placement {
    pub fn contains(self, x: f64, y: f64) -> bool {
        self.width > 0
            && self.height > 0
            && x >= self.x as f64
            && y >= self.y as f64
            && x < (self.x + self.width) as f64
            && y < (self.y + self.height) as f64
    }

    pub fn window_coords(self, x: u16, y: u16) -> (f64, f64) {
        let map = |v: u16, origin: u32, length: u32| {
            (origin as f64 + v as f64 * length.saturating_sub(1) as f64 / 65535.0).round()
        };
        (map(x, self.x, self.width), map(y, self.y, self.height))
    }

    pub fn fit(src_w: u32, src_h: u32, win_w: u32, win_h: u32) -> Self {
        if src_w == 0 || src_h == 0 || win_w == 0 || win_h == 0 {
            return Self {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            };
        }
        // Compare win_w/src_w with win_h/src_h without floating point.
        let (width, height) = if win_w as u64 * src_h as u64 <= win_h as u64 * src_w as u64 {
            (win_w, (src_h as u64 * win_w as u64 / src_w as u64) as u32)
        } else {
            ((src_w as u64 * win_h as u64 / src_h as u64) as u32, win_h)
        };
        Self {
            x: (win_w - width) / 2,
            y: (win_h - height) / 2,
            width: width.max(1),
            height: height.max(1),
        }
    }

    /// Window pixel → normalised remote coordinate (`0..=65535`), clamped to
    /// the image so dragging past its edge still reaches the remote edge.
    pub fn remote_coords(self, wx: f64, wy: f64) -> (u16, u16) {
        let norm = |v: f64, origin: u32, len: u32| {
            let t = (v - origin as f64) / (len.max(2) - 1) as f64;
            (t.clamp(0.0, 1.0) * 65535.0).round() as u16
        };
        (norm(wx, self.x, self.width), norm(wy, self.y, self.height))
    }
}

/// Nearest-neighbour blit of a `src_w`×`src_h` 0RGB image into `dst`
/// (`dst_w` wide) at `p`, painting the letterbox bars black.
pub fn blit(src: &[u32], src_w: u32, src_h: u32, dst: &mut [u32], dst_w: u32, p: Placement) {
    dst.fill(0);
    if p.width == 0 || src_w == 0 || src_h == 0 {
        return;
    }
    let dst_w = dst_w as usize;
    let (sw, sh) = (src_w as usize, src_h as usize);
    if p.width == src_w && p.height == src_h {
        for (row, src_row) in src.chunks_exact(sw).enumerate() {
            let start = (p.y as usize + row) * dst_w + p.x as usize;
            dst[start..start + sw].copy_from_slice(src_row);
        }
        return;
    }
    let xs: Vec<usize> = (0..p.width as usize)
        .map(|x| (x * sw / p.width as usize).min(sw - 1))
        .collect();
    for y in 0..p.height as usize {
        let sy = (y * sh / p.height as usize).min(sh - 1);
        let src_row = &src[sy * sw..(sy + 1) * sw];
        let start = (p.y as usize + y) * dst_w + p.x as usize;
        for (d, &sx) in dst[start..start + p.width as usize].iter_mut().zip(&xs) {
            *d = src_row[sx];
        }
    }
}

/// A high-contrast amber host-position arrow, separate from the local OS crosshair.
/// This is an indicator, not a copy of the host application's native cursor shape.
pub fn draw_host_cursor(
    dst: &mut [u32],
    dst_w: u32,
    p: Placement,
    cursor: tidedesk_core::sharing::PointerPosition,
    scale_factor: f64,
) {
    if !cursor.inside || p.width == 0 || p.height == 0 || dst_w == 0 {
        return;
    }
    // Constant logical size across remote resolutions; integer scaling keeps the outline crisp.
    let scale = scale_factor.round().clamp(1.0, 4.0) as i64;
    let (x, y) = p.window_coords(cursor.x, cursor.y);
    // Point inward near the bottom/right edge so the marker remains visible.
    let x_direction = if x as i64 + 13 * scale >= (p.x + p.width) as i64 {
        -1
    } else {
        1
    };
    let y_direction = if y as i64 + 19 * scale >= (p.y + p.height) as i64 {
        -1
    } else {
        1
    };
    let shape = [
        "#               ",
        "##              ",
        "#o#             ",
        "#oo#            ",
        "#ooo#           ",
        "#oooo#          ",
        "#ooooo#         ",
        "#oooooo#        ",
        "#ooooooo#       ",
        "#oooooooo#      ",
        "#ooooooooo#     ",
        "#oooooooooo#    ",
        "#oooooo#####    ",
        "#ooo#oo#        ",
        "#oo# #oo#       ",
        "#o#  #oo#       ",
        "##    #oo#      ",
        "#     #oo#      ",
        "       ##       ",
    ];
    for (row, pixels) in shape.iter().enumerate() {
        for (column, pixel) in pixels.bytes().enumerate() {
            let color = match pixel {
                b'#' => 0x00101010,
                b'o' => 0x00ffbf47,
                _ => continue,
            };
            for dy in 0..scale {
                for dx in 0..scale {
                    let px = x as i64 + (column as i64 * scale + dx) * x_direction;
                    let py = y as i64 + (row as i64 * scale + dy) * y_direction;
                    if p.contains(px as f64, py as f64)
                        && let Some(out) = dst.get_mut(py as usize * dst_w as usize + px as usize)
                    {
                        *out = color;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_cursor_is_visible_at_edges_and_never_draws_in_letterbox_bars() {
        let p = Placement::fit(100, 100, 140, 100);
        for scale in [1.0, 2.0] {
            for (x, y) in [(0, 0), (65535, 65535), (0, 65535), (65535, 0)] {
                let mut dst = vec![0; 140 * 100];
                draw_host_cursor(
                    &mut dst,
                    140,
                    p,
                    tidedesk_core::sharing::PointerPosition {
                        epoch: 1,
                        x,
                        y,
                        inside: true,
                    },
                    scale,
                );
                assert!(dst.contains(&0x00ffbf47));
                for (index, color) in dst.iter().enumerate() {
                    if !p.contains((index % 140) as f64, (index / 140) as f64) {
                        assert_eq!(*color, 0);
                    }
                }
            }
        }
    }

    #[test]
    fn hidden_cursor_and_repainting_do_not_leave_trails() {
        let p = Placement::fit(80, 80, 80, 80);
        let src = vec![0x00123456; 80 * 80];
        let mut dst = src.clone();
        let cursor = tidedesk_core::sharing::PointerPosition {
            epoch: 1,
            x: 1000,
            y: 1000,
            inside: true,
        };
        draw_host_cursor(&mut dst, 80, p, cursor, 1.0);
        assert_ne!(dst, src);
        blit(&src, 80, 80, &mut dst, 80, p);
        draw_host_cursor(
            &mut dst,
            80,
            p,
            tidedesk_core::sharing::PointerPosition {
                inside: false,
                ..cursor
            },
            1.0,
        );
        assert_eq!(dst, src);
    }

    #[test]
    fn fit_letterboxes_and_pillarboxes() {
        assert_eq!(
            Placement::fit(1920, 1080, 1920, 1200),
            Placement {
                x: 0,
                y: 60,
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(
            Placement::fit(1920, 1080, 1000, 1080),
            Placement {
                x: 0,
                y: 259,
                width: 1000,
                height: 562
            }
        );
        assert_eq!(
            Placement::fit(1000, 1000, 1600, 800),
            Placement {
                x: 400,
                y: 0,
                width: 800,
                height: 800
            }
        );
    }

    #[test]
    fn pointer_maps_to_remote_edges() {
        let p = Placement::fit(1920, 1080, 1920, 1200);
        assert_eq!(p.remote_coords(0.0, 60.0), (0, 0));
        assert_eq!(p.remote_coords(1919.0, 1139.0), (65535, 65535));
        assert_eq!(p.remote_coords(-50.0, 5000.0), (0, 65535));
    }

    #[test]
    fn blit_scales_and_fills_bars() {
        let src = [1, 2, 3, 4]; // 2x2
        let mut dst = [9u32; 4 * 5];
        let p = Placement::fit(2, 2, 4, 5);
        blit(&src, 2, 2, &mut dst, 4, p);
        assert_eq!(
            p,
            Placement {
                x: 0,
                y: 0,
                width: 4,
                height: 4
            }
        );
        assert_eq!(&dst[0..4], &[1, 1, 2, 2]);
        assert_eq!(&dst[8..12], &[3, 3, 4, 4]);
        assert_eq!(&dst[16..20], &[0, 0, 0, 0]);
    }
}
