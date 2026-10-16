//! Golden level A for phase 13 §I (I1, I4): every one of the twelve
//! transparency families × every source — flat, a graduated multi-stop
//! ramp under a bias/gain profile, and a bitmap read through a multi-stop
//! level ramp under a profile — rendered by the CPU backend and compared
//! **exactly** with committed images in `tests/golden/transparency_family/`.
//!
//! Run with `XARAST_UPDATE_GOLDEN=1` to regenerate them from the current
//! code: a deliberate, reviewable act. These are regression locks: the
//! families the research could not verify against the original
//! (`docs/memory/render.md`) are locked as they are, not judged.

mod common;

use common::{artefact_dir, golden_dir, render_case, updating};
use xarast_render::Surface;
use xarast_render::corpus::{TRANSPARENCY_SOURCES, transparency_family_cases};
use xarast_render::golden::{Comparison, compare, diff_heatmap, read_png, write_png};

#[test]
fn every_family_and_source_matches_its_golden_exactly() {
    let cases = transparency_family_cases();
    assert_eq!(cases.len(), 36, "12 families × 3 sources");

    let dir = golden_dir().join("transparency_family");
    std::fs::create_dir_all(&dir).expect("creating the golden directory");
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
            let art = artefact_dir().join("transparency_family");
            let _ = std::fs::create_dir_all(&art);
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
        "{} of {} scenes differ from their golden (artefacts in target/golden-diffs/transparency_family): {:?}",
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

/// For every source, the twelve families give twelve different pictures
/// (all 66 pairs differ), and for every family the three sources differ —
/// except `None`, which draws nothing whatever its source. A family or a
/// source that silently fell back to another would pass the golden gate
/// once blessed; it cannot pass this.
#[test]
fn every_family_and_every_source_draws_differently() {
    let cases = transparency_family_cases();
    let rendered: Vec<(String, Surface)> = cases
        .iter()
        .map(|c| (c.name.clone(), render_case(c)))
        .collect();
    let mut same = Vec::new();
    for source in TRANSPARENCY_SOURCES {
        let of: Vec<&(String, Surface)> = rendered
            .iter()
            .filter(|(n, _)| n.ends_with(&format!("_{source}")))
            .collect();
        assert_eq!(of.len(), 12, "{source}");
        for (i, a) in of.iter().enumerate() {
            for b in &of[i + 1..] {
                if compare(&a.1, &b.1).is_exact() {
                    same.push(format!("{} == {}", a.0, b.0));
                }
            }
        }
    }
    for chunk in rendered.chunks(TRANSPARENCY_SOURCES.len()) {
        if chunk[0].0.starts_with("none_") {
            continue;
        }
        for (i, a) in chunk.iter().enumerate() {
            for b in &chunk[i + 1..] {
                if compare(&a.1, &b.1).is_exact() {
                    same.push(format!("{} == {}", a.0, b.0));
                }
            }
        }
    }
    assert!(same.is_empty(), "cells that draw alike: {same:?}");
}
