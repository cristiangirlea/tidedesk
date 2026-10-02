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

/// Blit of a `src_w`×`src_h` 0RGB image into `dst` (`dst_w` wide) at `p`,
/// painting the letterbox bars black. Enlarging by a whole number repeats
/// pixels, which keeps text crisp; any other size blends neighbouring
/// pixels, so thin strokes are neither dropped nor drawn at uneven widths.
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
    if !p.width.is_multiple_of(src_w) || !p.height.is_multiple_of(src_h) {
        return blend(src, sw, sh, dst, dst_w, p);
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

/// Scales into `dst` at `p`: halves the picture (2x2 averages) while it is
/// still over twice the size, then blends the two source pixels around each
/// shown pixel's centre, first down the rows and then along them. Bands of
/// rows go to up to four threads, as this runs for every frame.
fn blend(src: &[u32], sw: usize, sh: usize, dst: &mut [u32], dst_w: usize, p: Placement) {
    let (pw, ph) = (p.width as usize, p.height as usize);
    let (mut sw, mut sh, mut halved) = (sw, sh, None::<Vec<u32>>);
    while pw * 2 <= sw && ph * 2 <= sh {
        let next = halve(halved.as_deref().unwrap_or(src), sw, sh);
        (sw, sh, halved) = (sw.div_ceil(2), sh.div_ceil(2), Some(next));
    }
    let src = halved.as_deref().unwrap_or(src);
    let (xs, ys) = (taps(sw, pw), taps(sh, ph));
    let band = ph.div_ceil(threads());
    let rows = &mut dst[p.y as usize * dst_w..(p.y as usize + ph) * dst_w];
    std::thread::scope(|scope| {
        for (lines, ys) in rows.chunks_mut(band * dst_w).zip(ys.chunks(band)) {
            let xs = &xs;
            scope.spawn(move || {
                let mut row = vec![0; sw];
                for (line, &(sy, weight)) in lines.chunks_mut(dst_w).zip(ys) {
                    let top = &src[sy * sw..][..sw];
                    let bottom = &src[(sy + 1).min(sh - 1) * sw..][..sw];
                    for ((blended, &a), &b) in row.iter_mut().zip(top).zip(bottom) {
                        *blended = mix(a, b, weight);
                    }
                    let shown = &mut line[p.x as usize..][..pw];
                    for (pixel, &(sx, weight)) in shown.iter_mut().zip(xs) {
                        *pixel = mix(row[sx], row[(sx + 1).min(sw - 1)], weight);
                    }
                }
            });
        }
    });
}

/// Threads for work on every frame, worked out once.
fn threads() -> usize {
    static THREADS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *THREADS.get_or_init(|| std::thread::available_parallelism().map_or(1, |n| n.get().min(4)))
}

/// For each of `dst` pixels along one axis, the source pixel at or before
/// its centre and the weight, out of 256, of the one after it.
fn taps(src: usize, dst: usize) -> Vec<(usize, u32)> {
    let step = src as f64 / dst as f64;
    (0..dst)
        .map(|d| {
            let centre = ((d as f64 + 0.5) * step - 0.5).clamp(0.0, (src - 1) as f64);
            let first = centre.floor() as usize;
            (first, ((centre - first as f64) * 256.0).round() as u32)
        })
        .collect()
}

/// `a` and `b` blended, `weight` (0..=256) parts of `b` in 256, two colour
/// channels to a multiplication.
fn mix(a: u32, b: u32, weight: u32) -> u32 {
    let keep = 256 - weight;
    let red_blue = ((a & 0x00ff_00ff) * keep + (b & 0x00ff_00ff) * weight + 0x0080_0080) >> 8;
    let green = ((a & 0x0000_ff00) * keep + (b & 0x0000_ff00) * weight + 0x0000_8000) >> 8;
    (red_blue & 0x00ff_00ff) | (green & 0x0000_ff00)
}

/// The picture at half the size, rounded up, each pixel the average of a
/// 2x2 block (an odd last row or column is paired with itself), in bands of
/// rows like [`blend`].
fn halve(src: &[u32], w: usize, h: usize) -> Vec<u32> {
    let (half_w, half_h) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = vec![0; half_w * half_h];
    let band = half_h.div_ceil(threads());
    std::thread::scope(|scope| {
        for (first, rows) in out.chunks_mut(band * half_w).enumerate() {
            scope.spawn(move || {
                for (i, line) in rows.chunks_mut(half_w).enumerate() {
                    let y = first * band + i;
                    let top = &src[2 * y * w..][..w];
                    let bottom = &src[(2 * y + 1).min(h - 1) * w..][..w];
                    for (x, pixel) in line.iter_mut().enumerate() {
                        let right = (2 * x + 1).min(w - 1);
                        let four = [top[2 * x], top[right], bottom[2 * x], bottom[right]];
                        let red_blue: u32 = four.iter().map(|p| p & 0x00ff_00ff).sum();
                        let green: u32 = four.iter().map(|p| p & 0x0000_ff00).sum();
                        *pixel = ((red_blue + 0x0002_0002) >> 2 & 0x00ff_00ff)
                            | ((green + 0x0000_0200) >> 2 & 0x0000_ff00);
                    }
                }
            });
        }
    });
    out
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

/// The mark an unlicensed company computer's sessions show from 2 hours.
pub const MARK: &str = "UNLICENSED COMPANY COMPUTER";

/// 5x7 letters for [`MARK`], one row per byte, the high bit on the left.
fn glyph(letter: char) -> Option<[u8; 7]> {
    Some(match letter {
        'A' => [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'C' => [0x0e, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0e],
        'D' => [0x1e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1e],
        'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        'I' => [0x0e, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0e],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1f],
        'M' => [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'P' => [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10],
        'R' => [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11],
        'S' => [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e],
        'T' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'Y' => [0x11, 0x11, 0x0a, 0x04, 0x04, 0x04, 0x04],
        _ => return None,
    })
}

/// `color` over `pixel`, `alpha` out of 256.
fn tint(pixel: u32, color: u32, alpha: u32) -> u32 {
    let mix = |shift: u32| {
        let (a, b) = ((pixel >> shift) & 0xff, (color >> shift) & 0xff);
        ((a * (256 - alpha) + b * alpha) >> 8) << shift
    };
    mix(16) | mix(8) | mix(0)
}

/// Draws [`MARK`] half-transparent in the picture's bottom-right corner;
/// nothing when the picture is too small for it.
pub fn draw_mark(dst: &mut [u32], dst_w: u32, p: Placement, scale_factor: f64) {
    let scale = (scale_factor * 2.0).round().clamp(2.0, 8.0) as u32;
    let letters = MARK.chars().count() as u32;
    let (pad, margin) = (3 * scale, 8 * scale);
    let (text_w, text_h) = (letters * 6 * scale - scale, 7 * scale);
    let (box_w, box_h) = (text_w + 2 * pad, text_h + 2 * pad);
    if p.width < box_w + 2 * margin || p.height < box_h + 2 * margin || dst_w == 0 {
        return;
    }
    let left = p.x + p.width - margin - box_w;
    let top = p.y + p.height - margin - box_h;
    let mut put = |x: u32, y: u32, color: u32, alpha: u32| {
        if let Some(out) = dst.get_mut(y as usize * dst_w as usize + x as usize) {
            *out = tint(*out, color, alpha);
        }
    };
    for y in top..top + box_h {
        for x in left..left + box_w {
            put(x, y, 0x0000_0000, 96);
        }
    }
    for (i, letter) in MARK.chars().enumerate() {
        let Some(rows) = glyph(letter) else { continue };
        let x0 = left + pad + i as u32 * 6 * scale;
        for (row, bits) in rows.iter().enumerate() {
            for column in 0..5 {
                if bits & (0x10 >> column) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let (x, y) = (
                            x0 + column * scale + dx,
                            top + pad + row as u32 * scale + dy,
                        );
                        put(x, y, 0x00ff_ffff, 150);
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
    fn the_mark_sits_in_the_corner_and_only_when_it_fits() {
        let (w, h) = (1280u32, 720u32);
        let p = Placement::fit(w, h, w, h);
        let mut pixels = vec![0x0020_4060u32; (w * h) as usize];
        draw_mark(&mut pixels, w, p, 1.0);
        let changed: Vec<usize> = (0..pixels.len())
            .filter(|&i| pixels[i] != 0x0020_4060)
            .collect();
        assert!(!changed.is_empty());
        let (min_x, min_y) = changed.iter().fold((w, h), |(x, y), &i| {
            (x.min(i as u32 % w), y.min(i as u32 / w))
        });
        assert!(
            min_x > w / 2 && min_y > h / 2,
            "bottom-right only: {min_x},{min_y}"
        );
        // Half-transparent: the picture still shows through.
        assert!(changed.iter().all(|&i| pixels[i] != 0x00ff_ffff));

        let small = Placement::fit(200, 100, 200, 100);
        let mut tiny = vec![7u32; 200 * 100];
        draw_mark(&mut tiny, 200, small, 1.0);
        assert!(tiny.iter().all(|&p| p == 7), "too small: nothing drawn");
        assert!(MARK.chars().all(|c| c == ' ' || glyph(c).is_some()));
    }

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

    /// 1-pixel white strokes on black every `every` rows (or columns), as
    /// text is drawn.
    fn strokes(size: usize, every: usize, rows: bool) -> Vec<u32> {
        (0..size * size)
            .map(|i| {
                let at = if rows { i / size } else { i % size };
                if at % every == 0 { 0x00ff_ffff } else { 0 }
            })
            .collect()
    }

    /// Shrinking must leave a trace of every thin stroke, as text needs:
    /// skipping source rows or columns makes strokes vanish.
    #[test]
    fn shrinking_never_drops_a_thin_stroke() {
        // A laptop's screen in a slightly smaller window, and a large
        // screen in a small one (halved first).
        for (src, dst, every) in [(120, 100, 5), (300, 100, 4)] {
            for rows in [true, false] {
                let pixels = strokes(src, every, rows);
                let p = Placement::fit(src as u32, src as u32, dst as u32, dst as u32);
                let mut shown = vec![0; dst * dst];
                blit(&pixels, src as u32, src as u32, &mut shown, dst as u32, p);
                for line in (0..src).step_by(every) {
                    // The shown row (or column) the stroke's centre falls in,
                    // read across the picture, as bands of rows join there.
                    let at = ((line as f64 + 0.5) * dst as f64 / src as f64) as usize;
                    for across in [dst / 10, dst / 2, dst - 1 - dst / 10] {
                        let index = if rows {
                            at * dst + across
                        } else {
                            across * dst + at
                        };
                        let shade = shown[index] & 0xff;
                        assert!(
                            shade >= 24,
                            "{src}->{dst}: the stroke at {} {line} shows {shade} at {across}",
                            if rows { "row" } else { "column" }
                        );
                    }
                }
            }
        }
    }

    /// Enlarging by a fraction must not draw some strokes twice as wide as
    /// others, as repeating pixels does (a 1080p screen shown at 1440p).
    #[test]
    fn enlarging_by_a_fraction_keeps_strokes_even() {
        let (src, dst, every) = (60, 80, 5);
        let pixels = strokes(src, every, false);
        let p = Placement::fit(src as u32, src as u32, dst as u32, dst as u32);
        let mut shown = vec![0; dst * dst];
        blit(&pixels, src as u32, src as u32, &mut shown, dst as u32, p);
        let row = &shown[(dst / 2) * dst..][..dst];
        // Each stroke's brightness, summed over the pixels around it.
        let weights: Vec<u32> = (every..src - every)
            .step_by(every)
            .map(|line| {
                let centre = (line as f64 + 0.5) * dst as f64 / src as f64;
                let first = (centre - 2.5).max(0.0) as usize;
                row[first..(first + 5).min(dst)]
                    .iter()
                    .map(|&c| c & 0xff)
                    .sum()
            })
            .collect();
        let (least, most) = (weights.iter().min().unwrap(), weights.iter().max().unwrap());
        assert!(most * 10 <= least * 13, "strokes weigh {weights:?}");
    }

    #[test]
    fn scaling_keeps_flat_colours_exact() {
        // Shrinking, halving an odd size, and enlarging by a fraction.
        for (src, dst) in [(120, 100), (300, 100), (1000, 999), (301, 100), (60, 80)] {
            let pixels = vec![0x0033_6699; src * src];
            let p = Placement::fit(src as u32, src as u32, dst as u32, dst as u32);
            let mut shown = vec![0; dst * dst];
            blit(&pixels, src as u32, src as u32, &mut shown, dst as u32, p);
            assert!(shown.iter().all(|&c| c == 0x0033_6699), "{src}->{dst}");
        }
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
