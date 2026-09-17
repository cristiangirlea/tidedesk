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

#[cfg(test)]
mod tests {
    use super::*;

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
