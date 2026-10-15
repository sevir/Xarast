//! Golden level A: the CPU backend against committed images, **exactly**.
//!
//! Run with `XARAST_UPDATE_GOLDEN=1` to regenerate every golden from the
//! current code. That is a deliberate, reviewable act: the diff shows every
//! image that moved.

mod common;

use common::{artefact_dir, golden_dir, render_case, updating};
use xarast_render::corpus::all_cases;
use xarast_render::golden::{Comparison, compare, diff_heatmap, read_png, write_png};

#[test]
fn the_corpus_matches_its_goldens_exactly() {
    let cases = all_cases();
    assert!(
        cases.len() >= 120,
        "the phase gate is 120 scenes, the corpus has {}",
        cases.len()
    );

    let dir = golden_dir();
    let mut missing = Vec::new();
    let mut failures: Vec<(String, Comparison)> = Vec::new();

    for case in &cases {
        let rendered = render_case(case);
        let path = dir.join(format!("{}.png", case.name));
        if updating() || !path.exists() {
            write_png(&rendered, &path).expect("writing a golden");
            if !updating() {
                missing.push(case.name.clone());
            }
            continue;
        }
        let golden = read_png(&path).expect("reading a golden");
        let c = compare(&rendered, &golden);
        if !c.is_exact() {
            let art = artefact_dir();
            let _ = write_png(&rendered, &art.join(format!("{}.actual.png", case.name)));
            let _ = write_png(&golden, &art.join(format!("{}.expected.png", case.name)));
            let _ = write_png(
                &diff_heatmap(&rendered, &golden),
                &art.join(format!("{}.diff.png", case.name)),
            );
            failures.push((case.name.clone(), c));
        }
    }

    assert!(
        missing.is_empty(),
        "{} goldens did not exist and have just been written; review and commit them: {:?}",
        missing.len(),
        &missing[..missing.len().min(8)]
    );
    assert!(
        failures.is_empty(),
        "{} of {} scenes differ from their golden (artefacts in target/golden-diffs): {:?}",
        failures.len(),
        cases.len(),
        failures
            .iter()
            .take(8)
            .map(|(n, c)| format!(
                "{n}: {} px, max {}/255, dE {:.3}",
                c.differing_pixels, c.max_channel_delta, c.mean_delta_e
            ))
            .collect::<Vec<_>>()
    );
}

/// A wall shadow casts its content's rendered alpha (`render.md`,
/// invariant 24): beside the opaque square it is at the shadow's full
/// darkness, beside the half-transparent disc at half of it.
#[test]
fn a_half_transparent_object_casts_a_half_dark_shadow() {
    let case = all_cases()
        .into_iter()
        .find(|c| c.name == "effect_shadow_transparent")
        .expect("the case exists");
    let s = render_case(&case);
    // How much a black shadow darkens the backdrop's green band (rows
    // 32–63), against the band at its left edge, where no shadow falls.
    let green = |x: i32, y: i32| f64::from(s.pixel(x, y).expect("in the surface")[1]);
    let darkness = |x: i32, y: i32| 1.0 - green(x, y) / green(1, y);
    // Shadow only, well inside each shadow's umbra and off both objects.
    let square = darkness(46, 40);
    let disc = darkness(86, 46);
    assert!(
        (square - 200.0 / 255.0).abs() < 0.02,
        "the opaque square's shadow is at the darkness: {square}"
    );
    let half = disc / square;
    assert!(
        (0.45..=0.55).contains(&half),
        "the half-transparent disc's shadow is half as dark: {disc} of {square}"
    );
}
