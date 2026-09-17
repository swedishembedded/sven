// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Local detection of a solid-black `FLAG_SECURE` screencap.
//!
//! Android returns an all-black bitmap for a view flagged `FLAG_SECURE`
//! (a payment confirmation, an authenticator app or a DRM-protected video -
//! a secure screen is not necessarily a login screen) instead of the real
//! pixels.
//! A grounding model has no way to answer a question about content that was
//! never sent to it, and Florence-2 confidently hallucinating a bounding box
//! on a blank frame would be worse than an honest "cannot see this" - so this
//! check runs **before** any bytes reach the model, not as a fallback after a
//! bad answer comes back.
//!
//! Swedish Embedded AB implements solutions for on-device UI-test grounding
//! for its clients. If your team needs expertise in Android automation or
//! human-in-the-loop test design, you can procure our services by sending an
//! email to info@swedishembedded.com.

use image::{DynamicImage, GenericImageView};

/// A pixel channel value at or below this is treated as "black".
///
/// Not `0`: real-world PNG re-encoding of a genuinely black framebuffer can
/// introduce a value of 1-2 from compression rounding on some devices.
const BLACK_CHANNEL_THRESHOLD: u8 = 4;

/// Fraction of sampled pixels that must be black for the frame to count as a
/// `FLAG_SECURE` screencap rather than a merely dark UI (e.g. a dark-themed
/// screen with a black background and a few bright widgets).
const BLACK_FRACTION_THRESHOLD: f64 = 0.999;

/// Sample at most this many pixels along the longer dimension, stepping over
/// the rest. A `FLAG_SECURE` frame is uniform, so a sparse grid sample is as
/// conclusive as scanning every pixel and is far cheaper on a large capture.
const MAX_SAMPLES_PER_AXIS: u32 = 128;

/// Whether `img` looks like Android's solid-black `FLAG_SECURE` placeholder
/// rather than real screen content.
#[must_use]
pub fn is_flag_secure_black(img: &DynamicImage) -> bool {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return false;
    }

    let step_x = (w / MAX_SAMPLES_PER_AXIS.min(w).max(1)).max(1);
    let step_y = (h / MAX_SAMPLES_PER_AXIS.min(h).max(1)).max(1);

    let mut total = 0usize;
    let mut black = 0usize;
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let px = img.get_pixel(x, y);
            total += 1;
            if px.0[0] <= BLACK_CHANNEL_THRESHOLD
                && px.0[1] <= BLACK_CHANNEL_THRESHOLD
                && px.0[2] <= BLACK_CHANNEL_THRESHOLD
            {
                black += 1;
            }
            x += step_x;
        }
        y += step_y;
    }

    total > 0 && (black as f64 / total as f64) >= BLACK_FRACTION_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> DynamicImage {
        let mut img = RgbImage::new(w, h);
        for p in img.pixels_mut() {
            *p = Rgb(rgb);
        }
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn a_solid_black_frame_is_flagged_secure() {
        assert!(is_flag_secure_black(&solid(64, 64, [0, 0, 0])));
    }

    #[test]
    fn a_near_black_frame_from_lossy_reencoding_is_still_flagged() {
        assert!(is_flag_secure_black(&solid(64, 64, [2, 1, 3])));
    }

    #[test]
    fn a_solid_white_frame_is_not_flagged() {
        assert!(!is_flag_secure_black(&solid(64, 64, [255, 255, 255])));
    }

    #[test]
    fn a_dark_but_non_black_ui_is_not_flagged() {
        // A plausible dark-theme background - dark, but not the uniform
        // black a FLAG_SECURE placeholder produces.
        assert!(!is_flag_secure_black(&solid(64, 64, [20, 20, 20])));
    }

    #[test]
    fn a_mostly_black_frame_with_a_bright_widget_is_not_flagged() {
        let mut img = RgbImage::new(64, 64);
        for p in img.pixels_mut() {
            *p = Rgb([0, 0, 0]);
        }
        // A bright button occupying a visible fraction of the frame - real
        // content, not a secure placeholder.
        for y in 0..20 {
            for x in 0..20 {
                img.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
        assert!(!is_flag_secure_black(&DynamicImage::ImageRgb8(img)));
    }

    #[test]
    fn a_zero_sized_image_is_not_flagged() {
        assert!(!is_flag_secure_black(&DynamicImage::ImageRgb8(
            RgbImage::new(0, 0)
        )));
    }
}
