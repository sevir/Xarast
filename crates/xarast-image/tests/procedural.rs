//! Phase 13 H6: the procedural generators are frozen. The same parameters
//! give the same bytes in every run, on every thread, on every platform
//! and in every release; these hashes are the release half of that
//! promise. A change here changes every document with a clouds or plasma
//! fill: bump the generator name (`procedural::CLOUDS_GENERATOR`,
//! `PLASMA_GENERATOR`) instead of updating a hash.

use xarast_image::procedural::{CloudsParams, PlasmaParams, Procedural, clouds, fixed16, plasma};

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn cloud(seed: i32, graininess: f64, tileable: bool) -> CloudsParams {
    CloudsParams {
        seed,
        graininess: fixed16(graininess),
        gravity: 0,
        squash: 0,
        tileable,
        dim: 256,
    }
}

/// Three seeds and the variants the corpus and the parameters exercise.
fn cases() -> Vec<(&'static str, Procedural)> {
    vec![
        ("clouds seed 1", Procedural::Clouds(cloud(1, 5.0, false))),
        (
            "clouds seed 12345",
            Procedural::Clouds(cloud(12_345, 5.0, false)),
        ),
        ("clouds seed -7", Procedural::Clouds(cloud(-7, 5.0, false))),
        (
            "clouds tileable",
            Procedural::Clouds(cloud(5_781_267, 2.0, true)),
        ),
        (
            "clouds gravity squash",
            Procedural::Clouds(CloudsParams {
                gravity: fixed16(0.25),
                squash: fixed16(-2.0),
                dim: 64,
                ..cloud(99, 10.0, true)
            }),
        ),
        (
            "plasma seed 1",
            Procedural::Plasma(PlasmaParams {
                seed: 1,
                grain: fixed16(30.0),
            }),
        ),
        (
            "plasma seed 3072824",
            Procedural::Plasma(PlasmaParams {
                seed: 3_072_824,
                grain: fixed16(10.0),
            }),
        ),
        (
            "plasma seed -2",
            Procedural::Plasma(PlasmaParams {
                seed: -2,
                grain: fixed16(60.0),
            }),
        ),
    ]
}

/// BLAKE3 of each tile, frozen 2026-10-16 (XARA-T-0336).
const GOLDEN: &[(&str, &str)] = &[
    (
        "clouds seed 1",
        "812ca274aac859a2cf05b3dfa8d50b34f4cf4df1d556df3c067d7f3ce65f9ece",
    ),
    (
        "clouds seed 12345",
        "f779e9776132aa53c42bd3eaa0d9822ec1e7a82aa6ba105ad81473c91a3af492",
    ),
    (
        "clouds seed -7",
        "f43463d43b14338e2f8020b2b13a2db336d91793b927beca97832ce047d742e2",
    ),
    (
        "clouds tileable",
        "48ed31204ca636622696c9332c6f460ab83cd5eeed371fe1b952607059c3ecc4",
    ),
    (
        "clouds gravity squash",
        "8c74ccd0177f070de720c992705ad69a5af91b079c9bca4506bf183ff44b30a1",
    ),
    (
        "plasma seed 1",
        "f230953b6a76521bbb5a4a2ae12bf9232175d1ea0f119261e1026ed15fb3b9b2",
    ),
    (
        "plasma seed 3072824",
        "8b2ee466f2fc8176bd739a581c355c8ec17fbe38eb201c9f56e5d1b3898b9f04",
    ),
    (
        "plasma seed -2",
        "d200d608fef939426c7ad04b8982dbbf90d1ff7234512863b8a2e0e19011601c",
    ),
];

#[test]
fn the_generated_tiles_match_their_golden_hashes() {
    let got: Vec<(&str, String)> = cases()
        .into_iter()
        .map(|(name, p)| (name, hash(&p.generate())))
        .collect();
    assert_eq!(got.len(), GOLDEN.len(), "golden table out of date");
    for ((name, h), (gname, gh)) in got.iter().zip(GOLDEN) {
        assert_eq!(name, gname);
        assert_eq!(h, gh, "{name}: the generator changed");
    }
}

#[test]
fn every_thread_generates_the_same_bytes() {
    let reference: Vec<Vec<u8>> = cases().iter().map(|(_, p)| p.generate()).collect();
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                s.spawn(|| {
                    cases()
                        .iter()
                        .map(|(_, p)| p.generate())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), reference);
        }
    });
}

#[test]
fn the_free_functions_agree_with_the_key() {
    let c = cloud(4, 5.0, false);
    assert_eq!(clouds(&c), Procedural::Clouds(c).generate());
    let p = PlasmaParams {
        seed: 4,
        grain: fixed16(20.0),
    };
    assert_eq!(plasma(&p), Procedural::Plasma(p).generate());
}
