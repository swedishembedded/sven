// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Does a rendered page look like columns of text?
//!
//! A cheap look at the pixels, before any model is asked: the share of dark
//! pixels in each vertical strip of the page. Gutters between columns are runs
//! of strips that are nearly empty while the strips beside them are not.

/// Strips the page width is divided into.
const STRIPS: usize = 240;
/// A strip with at most this share of the typical text strip's ink is empty.
const EMPTY_SHARE: f32 = 0.45;
/// Gutters are at least this many strips (a bit over 1% of the width).
const MIN_GUTTER_STRIPS: usize = 2;
/// A pixel this dark (0..=255 luminance) is ink.
const INK_BELOW: u32 = 140;
/// Rows and columns looked at, at most; a sample is enough for a share.
const MAX_SAMPLES: usize = 600;

/// `(columns, ink)`: the column count and the share of dark pixels over the
/// strips that hold text. Newspaper print is dense everywhere; the sparse rules
/// of a table, a form or a figure page are not.
pub fn column_profile(rgba: &[u8], width: usize, height: usize) -> (usize, f32) {
    if width < STRIPS || height < 32 || rgba.len() < width * height * 4 {
        return (1, 0.0);
    }
    let row_step = (height / MAX_SAMPLES).max(1);
    let col_step = (width / (STRIPS * 4)).max(1);
    let mut ink = vec![0u32; STRIPS];
    let mut samples = vec![0u32; STRIPS];
    for y in (0..height).step_by(row_step) {
        let row = &rgba[y * width * 4..(y + 1) * width * 4];
        for x in (0..width).step_by(col_step) {
            let p = &row[x * 4..x * 4 + 3];
            let luma =
                (u32::from(p[0]) * 299 + u32::from(p[1]) * 587 + u32::from(p[2]) * 114) / 1000;
            let strip = x * STRIPS / width;
            samples[strip] += 1;
            if luma < INK_BELOW {
                ink[strip] += 1;
            }
        }
    }
    let share: Vec<f32> = ink
        .iter()
        .zip(&samples)
        .map(|(i, s)| if *s == 0 { 0.0 } else { *i as f32 / *s as f32 })
        .collect();
    let mut typical: Vec<f32> = share.iter().copied().filter(|s| *s > 0.0).collect();
    if typical.len() < STRIPS / 4 {
        return (1, 0.0);
    }
    let ink_mean = typical.iter().sum::<f32>() / typical.len() as f32;
    typical.sort_by(f32::total_cmp);
    let limit = typical[typical.len() * 3 / 4] * EMPTY_SHARE;
    let empty: Vec<bool> = share.iter().map(|s| *s <= limit).collect();
    let first_ink = empty.iter().position(|e| !e);
    let last_ink = empty.iter().rposition(|e| !e);
    let (Some(first), Some(last)) = (first_ink, last_ink) else {
        return (1, 0.0);
    };
    let mut gutters = 0;
    let mut run = 0;
    for &is_empty in &empty[first..=last] {
        if is_empty {
            run += 1;
        } else {
            if run >= MIN_GUTTER_STRIPS {
                gutters += 1;
            }
            run = 0;
        }
    }
    (gutters + 1, ink_mean)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// White page with `n` columns of dark text-like strips.
    fn page(n: usize) -> (Vec<u8>, usize, usize) {
        let (w, h) = (1200, 1600);
        let mut px = vec![255u8; w * h * 4];
        let margin = 100;
        let gutter = 30;
        let col_w = (w - 2 * margin - gutter * (n - 1)) / n;
        for c in 0..n {
            let x0 = margin + c * (col_w + gutter);
            for y in (150..h - 150).filter(|y| y % 6 < 3) {
                for x in x0..x0 + col_w {
                    let i = (y * w + x) * 4;
                    px[i..i + 3].fill(30);
                }
            }
        }
        (px, w, h)
    }

    #[test]
    fn gutters_between_columns_are_counted() {
        for n in 1..=5 {
            let (px, w, h) = page(n);
            assert_eq!(column_profile(&px, w, h).0, n, "{n} columns");
        }
    }

    #[test]
    fn a_blank_page_has_one_column() {
        let px = vec![255u8; 1000 * 1000 * 4];
        assert_eq!(column_profile(&px, 1000, 1000).0, 1);
    }
}
