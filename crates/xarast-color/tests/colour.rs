//! Colour models, conversions, the built-in table and the derived graph.

use proptest::prelude::*;
use std::sync::Arc;
use xarast_color::{
    BuiltinColour, Colour, ColourDef, ColourError, ColourKind, ColourModel, ColourTable,
    ColourValue, FillEffect, Fixed24, Rgba8, Stop, TranspMode, Transparency, interpolate,
};

#[test]
fn components_are_clamped_and_never_nan() {
    let v = ColourValue::rgbt(2.0, -1.0, f32::NAN, 0.5);
    let ColourValue::Rgbt { r, g, b, t } = v else {
        panic!("built an Rgbt")
    };
    assert_eq!((r, g, b, t), (1.0, 0.0, 0.0, 0.5));
    for c in ColourValue::cmyk(5.0, -5.0, f32::NAN, 0.5).components() {
        assert!(c.is_finite() && (0.0..=1.0).contains(&c));
    }
    for c in ColourValue::hsvt(f32::INFINITY, 2.0, -1.0, f32::NAN).components() {
        assert!(c.is_finite() && (0.0..=1.0).contains(&c));
    }
}

#[test]
fn model_discriminants_match_the_file() {
    assert_eq!(ColourModel::Indexed as u8, 0);
    assert_eq!(ColourModel::Ciet as u8, 1);
    assert_eq!(ColourModel::Rgbt as u8, 2);
    assert_eq!(ColourModel::Cmyk as u8, 3);
    assert_eq!(ColourModel::Hsvt as u8, 4);
    assert_eq!(ColourModel::Greyt as u8, 5);
    assert_eq!(ColourModel::WebRgbt as u8, 6);
    // Unknown model bytes fall back to RGB rather than failing the load.
    assert_eq!(ColourModel::from_byte(99), ColourModel::Rgbt);
    for b in 0u8..=6 {
        assert_eq!(ColourModel::from_byte(b) as u8, b);
    }
}

#[test]
fn cmyk_to_rgb_is_the_naive_conversion() {
    // Deliberately `R = 1 - min(1, C + K)`, matching the original. If this
    // test starts failing because someone "fixed" the formula, every
    // imported document has just shifted colour.
    let c = ColourValue::cmyk(0.5, 0.0, 0.0, 0.25);
    let ColourValue::Rgbt { r, g, b, .. } = c.to_rgbt() else {
        panic!()
    };
    assert!((r - 0.25).abs() < 1e-6, "{r}");
    assert!((g - 0.75).abs() < 1e-6, "{g}");
    assert!((b - 0.75).abs() < 1e-6, "{b}");
    // Saturating: C + K over one clamps to zero, it does not wrap.
    let k = ColourValue::cmyk(0.8, 0.0, 0.0, 0.8);
    let ColourValue::Rgbt { r, .. } = k.to_rgbt() else {
        panic!()
    };
    assert_eq!(r, 0.0);
}

#[test]
fn builtin_colours_cover_the_negative_references() {
    assert_eq!(BuiltinColour::from_ref(-1), Some(BuiltinColour::None));
    assert_eq!(BuiltinColour::from_ref(-10), Some(BuiltinColour::CmykKey));
    // A positive value is a record number and zero means none, so neither is
    // a built-in.
    assert_eq!(BuiltinColour::from_ref(0), None);
    assert_eq!(BuiltinColour::from_ref(1), None);
    assert_eq!(BuiltinColour::from_ref(-11), None);
    assert_eq!(BuiltinColour::from_ref(i32::MIN), None);

    // "None" means no colour, not black and not transparent.
    assert_eq!(BuiltinColour::None.value(), None);
    assert_eq!(
        BuiltinColour::Black.value().unwrap().to_rgba8(),
        Rgba8::BLACK
    );
    assert_eq!(
        BuiltinColour::White.value().unwrap().to_rgba8(),
        Rgba8::WHITE
    );
    assert_eq!(
        BuiltinColour::Red.value().unwrap().to_rgba8(),
        Rgba8::rgb(255, 0, 0)
    );
    assert_eq!(
        BuiltinColour::Cyan.value().unwrap().to_rgba8(),
        Rgba8::rgb(0, 255, 255)
    );
    // The key plate is a CMYK colour, so that it separates onto one plate.
    assert_eq!(
        BuiltinColour::CmykKey.value().unwrap().model(),
        ColourModel::Cmyk
    );
    assert_eq!(
        BuiltinColour::CmykKey.value().unwrap().to_rgba8(),
        Rgba8::BLACK
    );

    for b in BuiltinColour::ALL {
        assert_eq!(BuiltinColour::from_ref(b.to_ref()), Some(b));
    }
}

#[test]
fn fixed24_inherit_sentinel() {
    assert_eq!(Fixed24::INHERIT.0, 0xF800_0000u32 as i32);
    assert!(Fixed24::INHERIT.is_inherit());
    assert_eq!(Fixed24::INHERIT.to_f32(), None);
    // Read literally the sentinel is -8.0, which is why it must never reach
    // a colour component.
    assert!((Fixed24::INHERIT.to_f32_raw() + 8.0).abs() < 1e-6);
    assert_eq!(Fixed24::ONE.to_f32(), Some(1.0));
    assert_eq!(Fixed24::ZERO.to_f32(), Some(0.0));
    // Out-of-range components are clamped here, the only place they can
    // appear.
    assert_eq!(Fixed24(1 << 25).to_f32(), Some(1.0));
    assert_eq!(Fixed24(-(1 << 25)).to_f32(), Some(0.0));
    assert_eq!(Fixed24::from_f32(0.5), Fixed24(1 << 23));
    assert_eq!(Fixed24::from_f32(f32::NAN), Fixed24::ZERO);
    assert!(!Fixed24::from_f32(1.0).is_inherit());
}

#[test]
fn fixed24_round_trips() {
    for i in 0..=1000 {
        let v = i as f32 / 1000.0;
        let back = Fixed24::from_f32(v).to_f32().expect("not the sentinel");
        assert!((back - v).abs() < 1e-6, "{v} -> {back}");
    }
}

#[test]
fn the_worked_examples_from_the_format_research() {
    // record #31: CMYK, type 0, comps [0,0,0,1], cached rgb (0,0,0), "Black"
    let mut t = ColourTable::new();
    let black = t.insert(ColourDef {
        name: Some(Arc::from("Black")),
        model: ColourModel::Cmyk,
        kind: ColourKind::Normal,
        parent: None,
        components: [Some(0.0), Some(0.0), Some(0.0), Some(1.0)],
        cached_rgb: Rgba8::BLACK,
        entry_index: 24,
    });
    assert_eq!(t.resolve_rgba8(black), Rgba8::BLACK);
    assert_eq!(t.by_name("Black"), Some(black));

    // record #45: HSVT, type 0, comps [0,1,1,0], cached rgb (255,0,0), "Red"
    let red = t.insert(ColourDef {
        name: Some(Arc::from("Red")),
        model: ColourModel::Hsvt,
        kind: ColourKind::Normal,
        parent: None,
        components: [Some(0.0), Some(1.0), Some(1.0), Some(0.0)],
        cached_rgb: Rgba8::rgb(255, 0, 0),
        entry_index: 0,
    });
    assert_eq!(t.resolve_rgba8(red), Rgba8::rgb(255, 0, 0));

    // record #69: CMYK, type 2 (tint), parent = Black, comps [0.9,0,0,0],
    // cached rgb (25,25,25), "90% Black".
    let tint = t.insert(ColourDef {
        name: Some(Arc::from("90% Black")),
        model: ColourModel::Cmyk,
        kind: ColourKind::Tint { factor: 0.9 },
        parent: Some(black),
        components: [Some(0.9), Some(0.0), Some(0.0), Some(0.0)],
        cached_rgb: Rgba8::rgb(25, 25, 25),
        entry_index: 25,
    });
    // A 90 % tint of black is 10 % of the way to white: 25.5 out of 255.
    let got = t.resolve_rgba8(tint);
    assert!(got.r.abs_diff(25) <= 1, "{got:?}");
    assert_eq!(got.r, got.g);
    assert_eq!(got.g, got.b);
    assert!(t.validate().is_empty());
    assert_eq!(t.len(), 3);
    assert!(!t.is_empty());
}

#[test]
fn a_fully_inheriting_link_resolves_to_its_parent() {
    let mut t = ColourTable::new();
    let parent = t.insert(ColourDef::normal(ColourValue::rgb(0.25, 0.5, 0.75)));
    let child = t.insert(ColourDef {
        model: ColourModel::Rgbt,
        kind: ColourKind::Linked,
        parent: Some(parent),
        components: [None, None, None, None],
        ..ColourDef::default()
    });
    assert_eq!(t.resolve(child).to_rgba8(), t.resolve(parent).to_rgba8());

    // Overriding one component keeps the other three inherited.
    let partial = t.insert(ColourDef {
        model: ColourModel::Rgbt,
        kind: ColourKind::Linked,
        parent: Some(parent),
        components: [Some(1.0), None, None, None],
        ..ColourDef::default()
    });
    let ColourValue::Rgbt { r, g, b, .. } = t.resolve(partial).to_rgbt() else {
        panic!()
    };
    assert!((r - 1.0).abs() < 1e-6);
    assert!((g - 0.5).abs() < 1e-3);
    assert!((b - 0.75).abs() < 1e-3);
}

#[test]
fn a_shade_moves_through_the_parents_hsv_space() {
    let mut t = ColourTable::new();
    let parent = t.insert(ColourDef::normal(ColourValue::rgb(1.0, 0.0, 0.0)));
    let shade = t.insert(ColourDef {
        model: ColourModel::Hsvt,
        kind: ColourKind::Shade { x: 0.0, y: -0.5 },
        parent: Some(parent),
        components: [Some(0.0), Some(-0.5), Some(0.0), Some(0.0)],
        ..ColourDef::default()
    });
    // y = -0.5 halves the value: pure red becomes a dark red. x = 0 leaves
    // saturation alone (research/02 §5.10.1).
    let got = t.resolve_rgba8(shade);
    assert!(got.r > 100 && got.r < 160, "{got:?}");
    assert_eq!(got.g, 0);
    assert_eq!(got.b, 0);
}

#[test]
fn a_cycle_resolves_to_the_cached_fallback_without_hanging() {
    let mut t = ColourTable::new();
    let a = t.insert(ColourDef {
        kind: ColourKind::Linked,
        components: [None, None, None, None],
        cached_rgb: Rgba8::rgb(10, 20, 30),
        ..ColourDef::default()
    });
    let b = t.insert(ColourDef {
        kind: ColourKind::Linked,
        parent: Some(a),
        components: [None, None, None, None],
        cached_rgb: Rgba8::rgb(40, 50, 60),
        ..ColourDef::default()
    });
    // Close the two-node loop.
    assert!(t.set_parent(a, Some(b)));

    assert!(matches!(t.try_resolve(a), Err(ColourError::ParentChain(_))));
    assert!(matches!(t.try_resolve(b), Err(ColourError::ParentChain(_))));
    assert_eq!(t.resolve_rgba8(a), Rgba8::rgb(10, 20, 30));
    assert_eq!(t.resolve_rgba8(b), Rgba8::rgb(40, 50, 60));
    assert_eq!(t.validate().len(), 2, "validate must report both");

    // The tightest cycle of all: a colour that is its own parent.
    let mut t2 = ColourTable::new();
    let s = t2.insert(ColourDef {
        kind: ColourKind::Linked,
        cached_rgb: Rgba8::rgb(7, 8, 9),
        ..ColourDef::default()
    });
    assert!(t2.set_parent(s, Some(s)));
    assert!(matches!(
        t2.try_resolve(s),
        Err(ColourError::ParentChain(_))
    ));
    assert_eq!(t2.resolve_rgba8(s), Rgba8::rgb(7, 8, 9));
}

#[test]
fn a_chain_deeper_than_the_limit_falls_back() {
    let mut t = ColourTable::new();
    let mut prev = t.insert(ColourDef::normal(ColourValue::rgb(1.0, 1.0, 1.0)));
    for i in 0..17 {
        prev = t.insert(ColourDef {
            model: ColourModel::Rgbt,
            kind: ColourKind::Linked,
            parent: Some(prev),
            components: [None, None, None, None],
            cached_rgb: Rgba8::rgb(i, i, i),
            ..ColourDef::default()
        });
    }
    assert!(matches!(
        t.try_resolve(prev),
        Err(ColourError::ParentChain(_))
    ));
    // The fallback is the cached triple the file carried.
    assert_eq!(t.resolve_rgba8(prev), Rgba8::rgb(16, 16, 16));
    let errs = t.validate();
    assert!(!errs.is_empty(), "validate must report the depth violation");
}

#[test]
fn an_unknown_handle_resolves_to_black() {
    let t = ColourTable::new();
    let mut other = ColourTable::new();
    let id = other.insert(ColourDef::normal(ColourValue::WHITE));
    assert!(matches!(t.try_resolve(id), Err(ColourError::Unknown(_))));
    assert_eq!(t.resolve(id).to_rgba8(), Rgba8::BLACK);
}

#[test]
fn colour_attribute_resolution() {
    let mut t = ColourTable::new();
    let id = t.insert(ColourDef::normal(ColourValue::rgb(0.0, 0.0, 0.0)).named("K"));
    assert_eq!(
        Colour::Direct(ColourValue::WHITE).resolve(&t),
        ColourValue::WHITE
    );
    let plain = Colour::Indexed { id, tint: None };
    assert_eq!(plain.resolve(&t).to_rgba8(), Rgba8::BLACK);
    let tinted = Colour::Indexed {
        id,
        tint: Some(0.5),
    };
    let got = tinted.resolve(&t).to_rgba8();
    assert!(got.r.abs_diff(128) <= 1, "{got:?}");
}

#[test]
fn transparency_modes_match_the_file_and_default_to_mix() {
    assert_eq!(TranspMode::from_byte(0), TranspMode::None);
    assert_eq!(TranspMode::from_byte(1), TranspMode::Mix);
    assert_eq!(TranspMode::from_byte(2), TranspMode::StainedGlass);
    assert_eq!(TranspMode::from_byte(28), TranspMode::Luminosity);
    // The original's transparency tool writes Hue as 31 (`TT_HUE`).
    assert_eq!(TranspMode::from_byte(31), TranspMode::Hue);
    assert_eq!(TranspMode::Hue as u8, 31);
    // A bevel's lighting fill is `TT_BEVEL`, 34.
    assert_eq!(TranspMode::from_byte(34), TranspMode::Bevel);
    // Every family reads back from its own byte.
    for m in TranspMode::ALL {
        assert_eq!(TranspMode::from_byte(m as u8), m, "{m:?}");
    }
    // The gaps are undocumented render-engine variants; treat them as mix.
    for v in [4u8, 7, 12, 14, 15, 33, 35, 36, 99, 255] {
        assert_eq!(TranspMode::from_byte(v), TranspMode::Mix, "{v}");
    }
    assert_eq!(TranspMode::default(), TranspMode::Mix);
    assert_eq!(Transparency::OPAQUE.alpha(), 1.0);
    assert_eq!(Transparency::mix(255).alpha(), 0.0);
}

#[test]
fn transparency_is_a_stop() {
    let a = Transparency::mix(0);
    let b = Transparency {
        level: 200,
        mode: TranspMode::Bleach,
    };
    let mid = a.lerp(&b, 0.5, FillEffect::Fade);
    assert_eq!(mid.level, 100);
    // The mode is categorical, so it snaps to the nearer end.
    assert_eq!(a.lerp(&b, 0.4, FillEffect::Fade).mode, TranspMode::Mix);
    assert_eq!(a.lerp(&b, 0.6, FillEffect::Fade).mode, TranspMode::Bleach);
    assert_eq!(a.lerp(&b, f32::NAN, FillEffect::Fade).level, 0);
    assert_eq!(a.lerp(&b, 5.0, FillEffect::Fade).level, 200);
}

#[test]
fn fade_interpolates_in_rgb() {
    let mid = interpolate(
        ColourValue::rgb(0.0, 0.0, 0.0),
        ColourValue::rgb(1.0, 1.0, 1.0),
        0.5,
        FillEffect::Fade,
    );
    let got = mid.to_rgba8();
    assert!(got.r.abs_diff(128) <= 1, "{got:?}");
    // The endpoints are exact.
    let a = ColourValue::rgb(0.2, 0.4, 0.6);
    let b = ColourValue::rgb(0.9, 0.1, 0.3);
    assert_eq!(
        interpolate(a, b, 0.0, FillEffect::Fade).to_rgba8(),
        a.to_rgba8()
    );
    assert_eq!(
        interpolate(a, b, 1.0, FillEffect::Fade).to_rgba8(),
        b.to_rgba8()
    );
}

#[test]
fn rainbow_takes_the_short_way_and_alt_the_long_way() {
    // Red (hue 0) to blue (hue 2/3). The short way is 1/3 backwards, through
    // magenta; the long way is 2/3 forwards, through green.
    let red = ColourValue::hsvt(0.0, 1.0, 1.0, 0.0);
    let blue = ColourValue::hsvt(2.0 / 3.0, 1.0, 1.0, 0.0);
    let short = interpolate(red, blue, 0.5, FillEffect::Rainbow);
    let long = interpolate(red, blue, 0.5, FillEffect::AltRainbow);
    let ColourValue::Hsvt { h: hs, .. } = short.to_hsvt() else {
        panic!()
    };
    let ColourValue::Hsvt { h: hl, .. } = long.to_hsvt() else {
        panic!()
    };
    // Short way: halfway from 0 to -1/3 is 5/6 (magenta).
    assert!((hs - 5.0 / 6.0).abs() < 1e-3, "short took hue {hs}");
    // Long way: halfway from 0 to +2/3 is 1/3 (green).
    assert!((hl - 1.0 / 3.0).abs() < 1e-3, "long took hue {hl}");
}

#[test]
fn interpolation_returns_the_first_endpoints_model() {
    let a = ColourValue::cmyk(1.0, 0.0, 0.0, 0.0);
    let b = ColourValue::rgb(1.0, 1.0, 1.0);
    assert_eq!(
        interpolate(a, b, 0.5, FillEffect::Fade).model(),
        ColourModel::Cmyk
    );
    assert_eq!(
        interpolate(b, a, 0.5, FillEffect::Fade).model(),
        ColourModel::Rgbt
    );
}

#[test]
fn colour_stop_lerp_falls_back_for_palette_references() {
    let mut t = ColourTable::new();
    let id = t.insert(ColourDef::normal(ColourValue::BLACK));
    let a = Colour::Indexed { id, tint: None };
    let b = Colour::Direct(ColourValue::WHITE);
    assert_eq!(a.lerp(&b, 0.2, FillEffect::Fade), a);
    assert_eq!(a.lerp(&b, 0.8, FillEffect::Fade), b);
    let d1 = Colour::Direct(ColourValue::BLACK);
    assert!(matches!(
        d1.lerp(&b, 0.5, FillEffect::Fade),
        Colour::Direct(_)
    ));
}

#[test]
fn greyscale_and_luminance() {
    let white = ColourValue::WHITE;
    assert!((white.luminance() - 1.0).abs() < 1e-5);
    assert!(ColourValue::BLACK.luminance().abs() < 1e-6);
    let g = ColourValue::rgb(1.0, 1.0, 1.0).to_greyt();
    let ColourValue::Greyt { v, .. } = g else {
        panic!()
    };
    assert!((v - 1.0).abs() < 1e-5);
    // The colour model's own weights (research/02 §5.10.1): 0.305/0.586/0.109.
    let ColourValue::Greyt { v: gv, .. } = ColourValue::rgb(0.0, 1.0, 0.0).to_greyt() else {
        panic!()
    };
    assert!((gv - 0.586).abs() < 1e-6, "{gv}");
}

#[test]
fn web_rgb_snaps_to_the_palette() {
    let ColourValue::WebRgb { r, g, b } = ColourValue::rgb(0.1, 0.5, 0.9).to_web_rgb() else {
        panic!()
    };
    for c in [r, g, b] {
        assert_eq!(c % 51, 0, "{c} is not web safe");
    }
    assert_eq!(
        ColourValue::WHITE.to_web_rgb(),
        ColourValue::WebRgb {
            r: 255,
            g: 255,
            b: 255
        }
    );
}

#[test]
fn rgba8_round_trips_every_channel_value() {
    for i in 0..=255u8 {
        let c = Rgba8 {
            r: i,
            g: 255 - i,
            b: i / 2,
            a: i,
        };
        assert_eq!(ColourValue::from_rgba8(c).to_rgba8(), c);
    }
}

/// A 64-value-per-channel sweep: 262 144 triples, the per-push subset of the
/// exhaustive 16.7 M sweep that the nightly job runs.
#[test]
fn rgb_conversions_round_trip_over_a_dense_sweep() {
    let step = 255.0 / 63.0;
    let mut worst_cmyk = 0u8;
    let mut worst_hsv = 0u8;
    for ri in 0..64 {
        for gi in 0..64 {
            for bi in 0..64 {
                let c = Rgba8 {
                    r: (ri as f32 * step).round() as u8,
                    g: (gi as f32 * step).round() as u8,
                    b: (bi as f32 * step).round() as u8,
                    a: 255,
                };
                let v = ColourValue::from_rgba8(c);

                let via_cmyk = v.to_cmyk().to_rgbt().to_rgba8();
                worst_cmyk = worst_cmyk
                    .max(via_cmyk.r.abs_diff(c.r))
                    .max(via_cmyk.g.abs_diff(c.g))
                    .max(via_cmyk.b.abs_diff(c.b));

                let via_hsv = v.to_hsvt().to_rgbt().to_rgba8();
                worst_hsv = worst_hsv
                    .max(via_hsv.r.abs_diff(c.r))
                    .max(via_hsv.g.abs_diff(c.g))
                    .max(via_hsv.b.abs_diff(c.b));
            }
        }
    }
    assert!(
        worst_cmyk <= 1,
        "RGB -> CMYK -> RGB was off by {worst_cmyk}"
    );
    assert!(worst_hsv <= 1, "RGB -> HSV -> RGB was off by {worst_hsv}");
}

/// The exhaustive sweep of all 16 777 216 triples. Ignored by default; run
/// with `cargo test -- --ignored` in the nightly job.
#[test]
#[ignore = "exhaustive: 16.7 M triples, for the nightly job"]
fn rgb_conversions_round_trip_exhaustively() {
    for packed in 0u32..0x0100_0000 {
        let c = Rgba8 {
            r: (packed >> 16) as u8,
            g: (packed >> 8) as u8,
            b: packed as u8,
            a: 255,
        };
        let v = ColourValue::from_rgba8(c);
        for got in [
            v.to_cmyk().to_rgbt().to_rgba8(),
            v.to_hsvt().to_rgbt().to_rgba8(),
        ] {
            assert!(got.r.abs_diff(c.r) <= 1, "{c:?} -> {got:?}");
            assert!(got.g.abs_diff(c.g) <= 1, "{c:?} -> {got:?}");
            assert!(got.b.abs_diff(c.b) <= 1, "{c:?} -> {got:?}");
        }
    }
}

fn any_rgba8() -> impl Strategy<Value = Rgba8> {
    (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(r, g, b)| Rgba8 { r, g, b, a: 255 })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20_000))]

    /// Every conversion round trip lands within one 8-bit step.
    #[test]
    fn conversions_round_trip(c in any_rgba8()) {
        let v = ColourValue::from_rgba8(c);
        for model in [ColourModel::Cmyk, ColourModel::Hsvt, ColourModel::Ciet] {
            let back = v.to_model(model).to_rgbt().to_rgba8();
            prop_assert!(back.r.abs_diff(c.r) <= 1, "{:?} via {:?} -> {:?}", c, model, back);
            prop_assert!(back.g.abs_diff(c.g) <= 1, "{:?} via {:?} -> {:?}", c, model, back);
            prop_assert!(back.b.abs_diff(c.b) <= 1, "{:?} via {:?} -> {:?}", c, model, back);
        }
    }

    /// No conversion ever produces a NaN or an out-of-range component.
    #[test]
    fn conversions_stay_in_range(c in any_rgba8(), m in 0u8..=6) {
        let v = ColourValue::from_rgba8(c).to_model(ColourModel::from_byte(m));
        for x in v.components() {
            prop_assert!(x.is_finite() && (0.0..=1.0).contains(&x), "{:?}", v);
        }
        prop_assert!(v.luminance().is_finite());
    }

    /// `Fixed24` round-trips every component within its own resolution.
    #[test]
    fn fixed24_round_trips_any_component(v in 0.0f32..=1.0) {
        let back = Fixed24::from_f32(v).to_f32().unwrap();
        prop_assert!((back - v).abs() <= 1e-6, "{} -> {}", v, back);
    }

    /// Interpolation is bounded by its endpoints and exact at them.
    #[test]
    fn interpolation_is_well_behaved(a in any_rgba8(), b in any_rgba8(), t in 0.0f32..=1.0) {
        let (ca, cb) = (ColourValue::from_rgba8(a), ColourValue::from_rgba8(b));
        for effect in [FillEffect::Fade, FillEffect::Rainbow, FillEffect::AltRainbow] {
            let m = interpolate(ca, cb, t, effect);
            for x in m.components() {
                prop_assert!(x.is_finite() && (0.0..=1.0).contains(&x));
            }
            prop_assert_eq!(interpolate(ca, cb, 0.0, effect).to_rgba8(), a);
            prop_assert_eq!(interpolate(ca, cb, 1.0, effect).to_rgba8(), b);
        }
    }

    /// A tint of 1.0 is the parent and a tint of 0.0 is white.
    #[test]
    fn tint_endpoints(c in any_rgba8()) {
        let mut t = ColourTable::new();
        let parent = t.insert(ColourDef::normal(ColourValue::from_rgba8(c)));
        let full = t.insert(ColourDef {
            kind: ColourKind::Tint { factor: 1.0 },
            parent: Some(parent),
            ..ColourDef::default()
        });
        let none = t.insert(ColourDef {
            kind: ColourKind::Tint { factor: 0.0 },
            parent: Some(parent),
            ..ColourDef::default()
        });
        prop_assert_eq!(t.resolve_rgba8(full), c);
        prop_assert_eq!(t.resolve_rgba8(none), Rgba8::WHITE);
    }
}
