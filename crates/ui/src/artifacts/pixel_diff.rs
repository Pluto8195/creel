//! Pixel diff of two renders of the same artifact at the same viewport
//! ("Highlight changes" in the revision compare view).
//!
//! Pure: per-pixel threshold, dilate (so a glyph-by-glyph text change merges
//! into one region), connected components, then one bounding box per
//! component. The box is tight around the changed pixels, not the dilated
//! mask.

/// Tunables. The defaults ignore anti-aliasing noise and merge nearby edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiffConfig {
    /// A pixel changed when any channel differs by more than this (0-255).
    pub(crate) threshold: u8,
    /// Changed pixels within this many pixels of each other form one region.
    pub(crate) dilate: u32,
    /// Regions with fewer changed pixels than this are dropped.
    pub(crate) min_pixels: u32,
}

impl Default for DiffConfig {
    fn default() -> Self {
        Self {
            threshold: 24,
            dilate: 6,
            min_pixels: 4,
        }
    }
}

/// A changed rectangle in image pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Region {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PixelDiff {
    /// The compared area (the larger of the two images in each axis).
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) regions: Vec<Region>,
    /// The images differ in size (the extra area counts as changed).
    pub(crate) size_mismatch: bool,
}

impl PixelDiff {
    pub(crate) fn is_unchanged(&self) -> bool {
        self.regions.is_empty()
    }

    /// "3 regions changed" / "1 region changed" / "No visual change".
    pub(crate) fn summary(&self) -> String {
        match self.regions.len() {
            0 => "No visual change".to_owned(),
            1 => "1 region changed".to_owned(),
            n => format!("{n} regions changed"),
        }
    }
}

/// Raw RGBA8 pixels, row-major.
pub(crate) struct Rgba<'a> {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) data: &'a [u8],
}

impl Rgba<'_> {
    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0, 0, 0, 0];
        }
        let at = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.data[at],
            self.data[at + 1],
            self.data[at + 2],
            self.data[at + 3],
        ]
    }
}

/// Decode two PNGs and diff them.
pub(crate) fn diff_png(left: &[u8], right: &[u8], config: DiffConfig) -> Result<PixelDiff, String> {
    let decode = |bytes: &[u8]| {
        image::load_from_memory(bytes)
            .map(|image| image.to_rgba8())
            .map_err(|error| error.to_string())
    };
    let left = decode(left)?;
    let right = decode(right)?;
    Ok(diff_rgba(
        &Rgba {
            width: left.width(),
            height: left.height(),
            data: left.as_raw(),
        },
        &Rgba {
            width: right.width(),
            height: right.height(),
            data: right.as_raw(),
        },
        config,
    ))
}

pub(crate) fn diff_rgba(left: &Rgba, right: &Rgba, config: DiffConfig) -> PixelDiff {
    let width = left.width.max(right.width);
    let height = left.height.max(right.height);
    let size_mismatch = (left.width, left.height) != (right.width, right.height);
    let count = width as usize * height as usize;
    let mut changed = vec![false; count];
    for y in 0..height {
        for x in 0..width {
            let a = left.pixel(x, y);
            let b = right.pixel(x, y);
            let differs = a
                .iter()
                .zip(b.iter())
                .any(|(a, b)| a.abs_diff(*b) > config.threshold);
            changed[y as usize * width as usize + x as usize] = differs;
        }
    }
    let mask = dilate(&changed, width, height, config.dilate);
    let regions = components(&changed, &mask, width, height, config.min_pixels);
    PixelDiff {
        width,
        height,
        regions,
        size_mismatch,
    }
}

/// Box-dilate by `radius` (Chebyshev), as two separable passes over prefix
/// sums.
fn dilate(mask: &[bool], width: u32, height: u32, radius: u32) -> Vec<bool> {
    if radius == 0 || mask.is_empty() {
        return mask.to_vec();
    }
    let (w, h, r) = (width as usize, height as usize, radius as usize);
    let mut horizontal = vec![false; mask.len()];
    for y in 0..h {
        let row = &mask[y * w..(y + 1) * w];
        let mut prefix = vec![0u32; w + 1];
        for x in 0..w {
            prefix[x + 1] = prefix[x] + u32::from(row[x]);
        }
        for x in 0..w {
            let from = x.saturating_sub(r);
            let to = (x + r + 1).min(w);
            horizontal[y * w + x] = prefix[to] - prefix[from] > 0;
        }
    }
    let mut out = vec![false; mask.len()];
    for x in 0..w {
        let mut prefix = vec![0u32; h + 1];
        for y in 0..h {
            prefix[y + 1] = prefix[y] + u32::from(horizontal[y * w + x]);
        }
        for y in 0..h {
            let from = y.saturating_sub(r);
            let to = (y + r + 1).min(h);
            out[y * w + x] = prefix[to] - prefix[from] > 0;
        }
    }
    out
}

/// 8-connected components of the dilated mask; each region is the bounding
/// box of the *undilated* changed pixels inside the component. Ordered top to
/// bottom, then left to right.
fn components(
    changed: &[bool],
    mask: &[bool],
    width: u32,
    height: u32,
    min_pixels: u32,
) -> Vec<Region> {
    let (w, h) = (width as usize, height as usize);
    let mut seen = vec![false; mask.len()];
    let mut regions = Vec::new();
    let mut stack = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (usize::MAX, usize::MAX, 0, 0);
        let mut pixels = 0u32;
        while let Some(at) = stack.pop() {
            let (x, y) = (at % w, at / w);
            if changed[at] {
                pixels += 1;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
            for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let next = ny * w + nx;
                    if mask[next] && !seen[next] {
                        seen[next] = true;
                        stack.push(next);
                    }
                }
            }
        }
        if pixels >= min_pixels.max(1) {
            regions.push(Region {
                x: min_x as u32,
                y: min_y as u32,
                width: (max_x - min_x + 1) as u32,
                height: (max_y - min_y + 1) as u32,
            });
        }
    }
    regions.sort_by_key(|region| (region.y, region.x));
    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        rgba.iter()
            .copied()
            .cycle()
            .take(width as usize * height as usize * 4)
            .collect()
    }

    fn paint(data: &mut [u8], width: u32, rect: (u32, u32, u32, u32), rgba: [u8; 4]) {
        let (x0, y0, w, h) = rect;
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                let at = (y as usize * width as usize + x as usize) * 4;
                data[at..at + 4].copy_from_slice(&rgba);
            }
        }
    }

    fn diff(left: &[u8], right: &[u8], size: (u32, u32)) -> PixelDiff {
        diff_rgba(
            &Rgba {
                width: size.0,
                height: size.1,
                data: left,
            },
            &Rgba {
                width: size.0,
                height: size.1,
                data: right,
            },
            DiffConfig::default(),
        )
    }

    #[test]
    fn identical_images_have_no_regions() {
        let a = solid(64, 48, [255, 255, 255, 255]);
        let result = diff(&a, &a.clone(), (64, 48));
        assert!(result.is_unchanged());
        assert_eq!(result.summary(), "No visual change");
        assert!(!result.size_mismatch);
    }

    #[test]
    fn a_changed_rectangle_is_one_tight_bounding_box() {
        let a = solid(100, 80, [255, 255, 255, 255]);
        let mut b = a.clone();
        paint(&mut b, 100, (20, 30, 40, 10), [200, 0, 0, 255]);
        let result = diff(&a, &b, (100, 80));
        assert_eq!(
            result.regions,
            vec![Region {
                x: 20,
                y: 30,
                width: 40,
                height: 10
            }]
        );
        assert_eq!(result.summary(), "1 region changed");
    }

    #[test]
    fn noise_below_the_threshold_is_ignored() {
        let a = solid(64, 64, [255, 255, 255, 255]);
        let mut b = a.clone();
        // Every other pixel is off by 10 (below the default threshold of 24).
        for y in 0..64 {
            for x in (0..64).step_by(2) {
                paint(&mut b, 64, (x, y, 1, 1), [245, 250, 255, 255]);
            }
        }
        assert!(diff(&a, &b, (64, 64)).is_unchanged());
    }

    #[test]
    fn nearby_edits_merge_and_distant_edits_stay_apart() {
        let a = solid(200, 100, [255, 255, 255, 255]);
        let mut b = a.clone();
        paint(&mut b, 200, (10, 10, 5, 5), [0, 0, 0, 255]);
        paint(&mut b, 200, (18, 10, 5, 5), [0, 0, 0, 255]);
        paint(&mut b, 200, (150, 70, 5, 5), [0, 0, 0, 255]);
        let result = diff(&a, &b, (200, 100));
        assert_eq!(
            result.regions,
            vec![
                Region {
                    x: 10,
                    y: 10,
                    width: 13,
                    height: 5
                },
                Region {
                    x: 150,
                    y: 70,
                    width: 5,
                    height: 5
                },
            ]
        );
        assert_eq!(result.summary(), "2 regions changed");
    }

    #[test]
    fn a_lone_speck_is_dropped() {
        let a = solid(40, 40, [255, 255, 255, 255]);
        let mut b = a.clone();
        paint(&mut b, 40, (5, 5, 1, 1), [0, 0, 0, 255]);
        assert!(diff(&a, &b, (40, 40)).is_unchanged());
    }

    #[test]
    fn size_mismatch_marks_the_extra_area_changed() {
        let a = solid(20, 20, [255, 255, 255, 255]);
        let b = solid(20, 30, [255, 255, 255, 255]);
        let result = diff_rgba(
            &Rgba {
                width: 20,
                height: 20,
                data: &a,
            },
            &Rgba {
                width: 20,
                height: 30,
                data: &b,
            },
            DiffConfig::default(),
        );
        assert!(result.size_mismatch);
        assert_eq!(
            result.regions,
            vec![Region {
                x: 0,
                y: 20,
                width: 20,
                height: 10
            }]
        );
    }

    #[test]
    fn png_round_trip_diffs() {
        let encode = |data: Vec<u8>| {
            let image = image::RgbaImage::from_raw(32, 32, data).unwrap();
            let mut out = std::io::Cursor::new(Vec::new());
            image.write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        let a = solid(32, 32, [255, 255, 255, 255]);
        let mut b = a.clone();
        paint(&mut b, 32, (4, 4, 8, 8), [0, 0, 255, 255]);
        let result = diff_png(&encode(a), &encode(b), DiffConfig::default()).unwrap();
        assert_eq!(result.regions.len(), 1);
        assert!(diff_png(b"nope", b"nope", DiffConfig::default()).is_err());
    }
}
