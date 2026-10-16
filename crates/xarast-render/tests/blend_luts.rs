//! The blend families over their whole domain.
//!
//! The phase's criterion 8 compares these against tables extracted from
//! `libCDraw.a` with a harness that calls `GDraw::CalcTransparencyX`
//! directly. That extraction (task R4.5) cannot run here: this container
//! has no x86-64 VM and no copy of the binary, and the clean-room rule
//! keeps `GDraw/*.h` out of reach anyway. What can be checked without the
//! original is checked here, and `docs/memory/render.md` records which four
//! families remain unverified against it.

use xarast_color::Rgba8;
use xarast_render::blend::{blend_level, blend_pixel, composite, imul, mul};
use xarast_render::{ALL_FAMILIES, BlendFamily, BlendLuts, LumaWeights};

fn rgb(r: u8, g: u8, b: u8) -> Rgba8 {
    Rgba8 { r, g, b, a: 255 }
}

#[test]
fn the_lut_path_and_the_pixel_path_agree_over_the_whole_domain() {
    // One generator, two consumers: the CPU compositor calls `blend_pixel`
    // and the GPU samples the LUT. If these ever disagree the backends
    // disagree, which is the bug the parity test would find much later.
    let luts = BlendLuts::default();
    let w = LumaWeights::BT601;
    for family in ALL_FAMILIES {
        if family.is_analytic() || family == BlendFamily::None {
            continue;
        }
        for s in (0..=255).step_by(5) {
            for t in (0..=255).step_by(5) {
                for d in (0..=255).step_by(5) {
                    let src = rgb(s, s, s);
                    let dst = rgb(d, d, d);
                    let via_pixel = blend_pixel(family, &luts, w, src, t, dst).r;
                    let level = blend_level(family, w, src, t, 0);
                    let via_lut = if family == BlendFamily::Mix {
                        imul(t, src.r).saturating_add(luts.get(family).get(t, d))
                    } else {
                        luts.get(family).get(level, d)
                    };
                    assert_eq!(via_pixel, via_lut, "{family:?} s={s} t={t} d={d}");
                }
            }
        }
    }
}

#[test]
fn every_family_is_the_identity_at_full_transparency() {
    let luts = BlendLuts::default();
    let w = LumaWeights::BT601;
    for family in ALL_FAMILIES {
        for d in (0..=255).step_by(3) {
            for s in [0u8, 77, 128, 200, 255] {
                let dst = rgb(d, d / 2, 255 - d);
                let out = blend_pixel(family, &luts, w, rgb(s, s, s), 255, dst);
                let delta = i32::from(out.r) - i32::from(dst.r);
                assert!(delta.abs() <= 1, "{family:?} at t=255 moved {d} by {delta}");
            }
        }
    }
}

#[test]
fn mix_is_exactly_a_lerp() {
    let luts = BlendLuts::default();
    let w = LumaWeights::BT601;
    for t in 0..=255u8 {
        for v in [0u8, 1, 127, 254, 255] {
            let out = blend_pixel(
                BlendFamily::Mix,
                &luts,
                w,
                rgb(v, v, v),
                t,
                rgb(255 - v, 0, 0),
            );
            assert_eq!(out.r, imul(t, v).saturating_add(mul(t, 255 - v)));
        }
    }
}

#[test]
fn none_draws_nothing_whatever_the_coverage() {
    let luts = BlendLuts::default();
    let w = LumaWeights::BT601;
    let d = rgb(1, 2, 3);
    for cov in [0u8, 1, 128, 255] {
        assert_eq!(
            composite(BlendFamily::None, &luts, w, rgb(9, 9, 9), 0, cov, d),
            d
        );
    }
}

#[test]
fn the_analytic_three_are_continuous_in_transparency() {
    // No table backs these, so a discontinuity would be a transcription
    // error rather than quantisation.
    let luts = BlendLuts::default();
    let w = LumaWeights::BT601;
    for family in [
        BlendFamily::Saturation,
        BlendFamily::Luminosity,
        BlendFamily::Hue,
    ] {
        let d = rgb(180, 90, 40);
        let s = rgb(40, 200, 120);
        let mut prev = blend_pixel(family, &luts, w, s, 0, d);
        for t in 1..=255u8 {
            let cur = blend_pixel(family, &luts, w, s, t, d);
            let jump = (i32::from(cur.r) - i32::from(prev.r))
                .abs()
                .max((i32::from(cur.g) - i32::from(prev.g)).abs())
                .max((i32::from(cur.b) - i32::from(prev.b)).abs());
            assert!(jump <= 8, "{family:?} jumps by {jump} at t={t}");
            prev = cur;
        }
    }
}

#[test]
fn the_luminance_weights_are_the_recorded_hypothesis() {
    // R4.4 recovers CDraw's real defaults by least squares against the
    // original. Until it runs, BT.601 is the documented hypothesis, and
    // this test is what will fail loudly when the constant changes.
    assert_eq!(LumaWeights::default(), LumaWeights::BT601);
    assert_eq!(LumaWeights::GREY_CONVERSION, LumaWeights::BT601);
    assert_eq!(xarast_color::RENDER_GREY_WEIGHTS, [0.299, 0.587, 0.114]);
    assert_eq!(LumaWeights::BT601.luma(rgb(255, 255, 255)), 255);
    assert_eq!(LumaWeights::BT601.luma(rgb(0, 0, 0)), 0);
    assert_eq!(LumaWeights::BT601.luma(rgb(255, 0, 0)), 76);
    assert_eq!(LumaWeights::BT601.luma(rgb(0, 255, 0)), 150);
    assert_eq!(LumaWeights::BT601.luma(rgb(0, 0, 255)), 29);
}
