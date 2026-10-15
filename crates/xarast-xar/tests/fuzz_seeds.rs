//! The fuzz seed corpus: generated here, committed under `fuzz/corpus/`.
//!
//! **Every seed is synthetic.** The 59 real files are the obvious seeds and
//! they may not be copied into this repository
//! (`docs/11-licensing-and-clean-room.md §3.2`), so instead each structural
//! feature of the format gets a hand-built miniature: a bare header, one
//! empty compressed block, two blocks with a streamed record between them,
//! a truncated deflate stream, a bad CRC, a bad length, both `DOWN`/`UP`
//! imbalances, deep nesting, a record declaring `0xFFFFFFFF`, an unknown
//! atomic tag with a subtree, an unknown essential tag, and one minimal
//! payload for each tag the importer decodes.
//!
//! A developer who has the corpus can seed from it locally through
//! `XARAST_XAR_CORPUS`; CI cannot and does not.
//!
//! Regenerate with `XARAST_WRITE_FUZZ_SEEDS=1 cargo test -p xarast-xar
//! --test fuzz_seeds`. Without that variable this test only checks that
//! every seed still parses without panicking, which is what makes the seed
//! set a regression suite rather than a pile of bytes.

use std::path::{Path, PathBuf};

use xarast_geom::Point;
use xarast_xar::synth::XarBuilder;
use xarast_xar::{DiagSink, ReaderLimits, analyse, decode, has_decoder};

fn fuzz_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz")
}

/// Whole-file seeds, for `fuzz_xar_records`, `fuzz_xar_tree`,
/// `fuzz_xar_decode` and `fuzz_xar_import`.
fn file_seeds() -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();

    out.push(("header-only".into(), XarBuilder::new().finish()));
    out.push((
        "header-and-eof".into(),
        XarBuilder::new().end_of_file().finish(),
    ));
    out.push((
        "empty-block".into(),
        XarBuilder::new().compressed(|_| {}).end_of_file().finish(),
    ));
    out.push((
        "two-blocks-streamed-record".into(),
        XarBuilder::new()
            .compressed(|b| {
                b.record(40, &[]);
            })
            .record(68, b"\x89PNG\r\n\x1a\nfake")
            .compressed(|b| {
                b.record(43, &[]);
            })
            .end_of_file()
            .finish(),
    ));
    let good = XarBuilder::new()
        .compressed(|b| {
            b.record(40, &[]).down().record(104, &[]).up();
        })
        .end_of_file()
        .finish();
    out.push(("truncated-deflate".into(), good[..good.len() / 2].to_vec()));
    out.push((
        "bad-crc".into(),
        XarBuilder::new()
            .compressed_tweaked(
                |b| {
                    b.record(40, &[]);
                },
                99,
                0xDEAD_BEEF,
                0,
            )
            .end_of_file()
            .finish(),
    ));
    out.push((
        "bad-length".into(),
        XarBuilder::new()
            .compressed_tweaked(
                |b| {
                    b.record(40, &[]);
                },
                99,
                0,
                9,
            )
            .end_of_file()
            .finish(),
    ));
    out.push((
        "unbalanced-down".into(),
        XarBuilder::new().down().down().record(104, &[]).finish(),
    ));
    out.push((
        "unbalanced-up".into(),
        XarBuilder::new()
            .up()
            .up()
            .record(104, &[])
            .end_of_file()
            .finish(),
    ));
    let mut deep = XarBuilder::new();
    for _ in 0..256 {
        deep = deep.record(104, &[]).down();
    }
    for _ in 0..256 {
        deep = deep.up();
    }
    out.push(("nesting-256".into(), deep.end_of_file().finish()));
    let mut lying = XarBuilder::new().finish();
    lying.extend_from_slice(&104u32.to_le_bytes());
    lying.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    out.push(("record-size-4g".into(), lying));
    out.push((
        "atomic-subtree".into(),
        XarBuilder::new()
            .record(10, &9001u32.to_le_bytes())
            .record(9001, &[])
            .down()
            .record(104, &[])
            .record(104, &[])
            .up()
            .record(43, &[])
            .end_of_file()
            .finish(),
    ));
    out.push((
        "essential-tag".into(),
        XarBuilder::new()
            .record(11, &9002u32.to_le_bytes())
            .record(9002, &[])
            .end_of_file()
            .finish(),
    ));
    out.push(("web-and-minimal-type".into(), {
        let mut v = XarBuilder::new().end_of_file().finish();
        // Flip CXN to CXW in place: the type byte is at offset 16 + 2.
        if let Some(b) = v.get_mut(18) {
            *b = b'W';
        }
        v
    }));

    // A ClipView: a keyhole with a winding rule of its own and a second
    // keyhole before the marker, one clipped object after it, and a
    // controller with no marker. Declared atomic, as real files do.
    out.push((
        "clip-view".into(),
        XarBuilder::new()
            .record(10, &[0xF4, 0x0F, 0, 0, 0xF5, 0x0F, 0, 0])
            .record(40, &[])
            .down()
            .record(4084, &[])
            .down()
            .record(116, &minimal_payload(116))
            .down()
            .record(178, &[3])
            .up()
            .record(116, &minimal_payload(116))
            .record(4085, &[])
            .record(116, &minimal_payload(116))
            .up()
            .record(4084, &[])
            .down()
            .record(116, &minimal_payload(116))
            .up()
            .up()
            .end_of_file()
            .finish(),
    ));
    // Shadows: a wall shadow over a path (controller attributes, a
    // `TAG_SHADOW` with its colour and transparency, the object), a floor
    // shadow with no `TAG_SHADOW` and an old 16-byte one, and a stray
    // `TAG_SHADOW` outside any controller. Declared atomic, as real files do.
    out.push(("shadow".into(), {
        let mut wall = vec![1u8];
        for v in [4500i32, 3750, -3750, 785_398, 50, 100, 3000] {
            wall.extend_from_slice(&v.to_le_bytes());
        }
        let mut floor = wall.clone();
        floor[0] = 2;
        let mut shadow = Vec::new();
        for v in [0.2f64, -0.1, 0.75] {
            shadow.extend_from_slice(&v.to_le_bytes());
        }
        XarBuilder::new()
            .record(10, &[0xD2, 0x0F, 0, 0, 0xD3, 0x0F, 0, 0])
            .record(40, &[])
            .down()
            .record(4050, &wall)
            .down()
            .record(152, &500i32.to_le_bytes())
            .record(4051, &shadow)
            .down()
            .record(166, &[0x40, 1])
            .record(150, &(-2i32).to_le_bytes())
            .up()
            .record(116, &minimal_payload(116))
            .up()
            .record(4050, &floor)
            .down()
            .record(4051, &shadow[..16])
            .record(116, &minimal_payload(116))
            .up()
            .record(4051, &shadow)
            .down()
            .record(150, &(-3i32).to_le_bytes())
            .up()
            .up()
            .end_of_file()
            .finish()
    }));
    // Bevels: an inner bevel over a path (controller attributes, the
    // object, then `TAG_BEVELINK` with its own attributes), an outer one
    // whose bevel node comes first and carries a flat fill, an old 20-byte
    // record with no tilt, a style out of range, a short record, a stray
    // `TAG_BEVELINK` and a discarded `TAG_BEVATTR_INDENT`. Declared atomic,
    // as real files do.
    out.push(("bevel".into(), {
        let bevel = |style: i32, outer: i32| {
            let mut v = Vec::new();
            for n in [style, 3_750, 300, outer, 50, 45] {
                v.extend_from_slice(&n.to_le_bytes());
            }
            v
        };
        XarBuilder::new()
            .record(10, &[0xD4, 0x0F, 0, 0, 0xD9, 0x0F, 0, 0])
            .record(40, &[])
            .down()
            .record(4052, &bevel(6, 0))
            .down()
            .record(152, &500i32.to_le_bytes())
            .record(116, &minimal_payload(116))
            .record(4057, &[])
            .down()
            .record(166, &[0, 1])
            .up()
            .up()
            .record(4052, &bevel(8, 1))
            .down()
            .record(4057, &[])
            .down()
            .record(150, &(-2i32).to_le_bytes())
            .up()
            .record(116, &minimal_payload(116))
            .up()
            .record(4052, &bevel(0, 1)[..20])
            .down()
            .record(116, &minimal_payload(116))
            .up()
            .record(4052, &bevel(99, 0))
            .record(4052, &bevel(1, 0)[..7])
            .record(4057, &[])
            .down()
            .record(150, &(-3i32).to_le_bytes())
            .up()
            .record(4053, &750i32.to_le_bytes())
            .up()
            .end_of_file()
            .finish()
    }));
    // A bitmap transparency with levels 115..200 in Bleach, over a PNG
    // definition (record 2).
    out.push(("bitmap-transparency".into(), {
        let mut t = Vec::new();
        for v in [0i32, 0, 1000, 0, 0, 1000] {
            t.extend_from_slice(&v.to_le_bytes());
        }
        t.extend_from_slice(&[115, 200, 3]);
        t.extend_from_slice(&2i32.to_le_bytes());
        t.extend_from_slice(&[0u8; 16]);
        XarBuilder::new()
            .record(68, b"b\0\0\0\x89PNG\r\n\x1a\n")
            .record(40, &[])
            .down()
            .record(116, &minimal_payload(116))
            .down()
            .record(171, &t)
            .up()
            .up()
            .end_of_file()
            .finish()
    }));

    // Contours: an outer contour (negative width) whose contour node comes
    // first with its own fill, an inner one with the node last, a rainbow
    // inset path, a record that stops before its profiles, a colour type
    // and a step count out of range, a short record, a controller with no
    // contour node and a stray `TAG_CONTOUR`. Declared atomic, as real
    // files do.
    out.push(("contour".into(), {
        let contour = |steps: i32, width: i32, kind: u8| {
            let mut v = Vec::new();
            v.extend_from_slice(&steps.to_le_bytes());
            v.extend_from_slice(&width.to_le_bytes());
            v.push(kind);
            for f in [0.25f64, -0.5, 0.0, 0.75] {
                v.extend_from_slice(&f.to_le_bytes());
            }
            v
        };
        XarBuilder::new()
            .record(10, &[0xE2, 0x0F, 0, 0, 0xE3, 0x0F, 0, 0])
            .record(40, &[])
            .down()
            .record(4066, &contour(3, -16_000, 0))
            .down()
            .record(176, &[0])
            .record(152, &500i32.to_le_bytes())
            .record(4067, &[])
            .down()
            .record(150, &(-2i32).to_le_bytes())
            .up()
            .record(116, &minimal_payload(116))
            .up()
            .record(4066, &contour(6, 1_088, 1))
            .down()
            .record(116, &minimal_payload(116))
            .record(4067, &[])
            .up()
            .record(4066, &contour(0, 2_000, 0x82))
            .down()
            .record(116, &minimal_payload(116))
            .record(4067, &[])
            .up()
            .record(4066, &contour(2, -500, 0)[..9])
            .down()
            .record(4067, &[])
            .record(116, &minimal_payload(116))
            .up()
            .record(4066, &contour(i32::MAX, i32::MIN, 0x7f))
            .down()
            .record(116, &minimal_payload(116))
            .up()
            .record(4066, &contour(1, 1, 0)[..5])
            .record(4066, &contour(-4, 0, 3))
            .down()
            .record(116, &minimal_payload(116))
            .up()
            .record(4067, &[])
            .down()
            .record(150, &(-3i32).to_le_bytes())
            .up()
            .up()
            .end_of_file()
            .finish()
    }));

    // Blends: profiles before a three-object blend (two blenders, each
    // with its additional record), a rainbow one-to-one blend, a blend
    // along a path (curve proportions and angles under its blender, a
    // blend path and its filled flag), a blender with nothing after it, a
    // colour type out of range, short records, object indices that
    // disagree, and a stray blender.
    out.push(("blend".into(), {
        let blend = |steps: u16, flags: u8| {
            let mut v = steps.to_le_bytes().to_vec();
            v.push(flags);
            v
        };
        let blender = |s: i32, e: i32| {
            let mut v = s.to_le_bytes().to_vec();
            v.extend_from_slice(&e.to_le_bytes());
            v
        };
        let extra = |curve: i32, s: i32, e: i32| {
            let mut v = Vec::new();
            for x in [curve, -1, s, e] {
                v.extend_from_slice(&x.to_le_bytes());
            }
            v.push(0);
            v
        };
        let doubles = |xs: &[f64]| {
            let mut v = Vec::new();
            for x in xs {
                v.extend_from_slice(&x.to_le_bytes());
            }
            v
        };
        XarBuilder::new()
            .record(40, &[])
            .down()
            .record(4072, &doubles(&[0.25, -0.5, 0.1, 0.0, 0.0, 0.0]))
            .record(105, &blend(5, 0x02))
            .down()
            .record(176, &[0])
            .record(116, &minimal_payload(116))
            .record(106, &blender(3, 0))
            .record(4073, &extra(0, 0, 2))
            .record(116, &minimal_payload(116))
            .record(106, &blender(-1, -1))
            .record(4073, &extra(0, 2, 4))
            .record(116, &minimal_payload(116))
            .up()
            .record(105, &blend(15, 0x13))
            .down()
            .record(116, &minimal_payload(116))
            .record(106, &blender(0, 0))
            .record(4073, &extra(0, 0, 2)[..16])
            .record(116, &minimal_payload(116))
            .up()
            .record(105, &blend(4, 0x02))
            .down()
            .record(116, &minimal_payload(116))
            .record(106, &blender(-1, -1))
            .down()
            .record(4060, &doubles(&[0.0, 1.0]))
            .record(4062, &doubles(&[0.0, 90.0]))
            .up()
            .record(4073, &extra(1, 0, 2))
            .record(116, &minimal_payload(116))
            .record(4061, &minimal_payload(4061))
            .record(4074, &1i32.to_le_bytes())
            .up()
            .record(105, &blend(u16::MAX, 0xF0))
            .down()
            .record(116, &minimal_payload(116))
            .record(106, &blender(7, 7))
            .record(4073, &extra(0, 5, 9))
            .up()
            .record(105, &blend(1, 0)[..2])
            .record(106, &blender(1, 2)[..4])
            .record(106, &blender(0, 0))
            .up()
            .end_of_file()
            .finish()
    }));

    // One file per decodable tag, carrying a plausible minimal payload.
    out.push(("every-tag".into(), {
        let mut b = XarBuilder::new();
        for tag in 0..5000u32 {
            if !has_decoder(tag) || matches!(tag, 0 | 1 | 2 | 3 | 30 | 31) {
                continue;
            }
            b = b.record(tag, &minimal_payload(tag));
        }
        b.end_of_file().finish()
    }));

    out
}

/// A payload big enough for a tag's fixed fields, with recognisable
/// content so that a mutation of it means something.
fn minimal_payload(tag: u32) -> Vec<u8> {
    match tag {
        // A two-point relative path.
        113..=116 => {
            let mut v = vec![0x06];
            v.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
            v.push(0x03);
            v.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xF0, 0xF0, 0x00, 0x00]);
            v
        }
        111 => vec![0x05, 0x05],
        // A named CMYK colour.
        51 => {
            let mut v = vec![0, 0, 0, 3, 0];
            v.extend_from_slice(&24u32.to_le_bytes());
            v.extend_from_slice(&0i32.to_le_bytes());
            v.extend_from_slice(&[0u8; 12]);
            v.extend_from_slice(&0x0100_0000u32.to_le_bytes());
            for u in "Black".encode_utf16() {
                v.extend_from_slice(&u.to_le_bytes());
            }
            v.extend_from_slice(&0u16.to_le_bytes());
            v
        }
        48 | 49 => {
            let mut v = vec![0x0D];
            for u in "Layer 1".encode_utf16() {
                v.extend_from_slice(&u.to_le_bytes());
            }
            v.extend_from_slice(&0u16.to_le_bytes());
            v
        }
        2201 => {
            let mut v = Vec::new();
            for u in "text".encode_utf16() {
                v.extend_from_slice(&u.to_le_bytes());
            }
            v
        }
        _ => vec![0u8; 72],
    }
}

/// Payload seeds for `fuzz_xar_path`.
fn path_seeds() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    out.push(("empty".into(), Vec::new()));
    out.push((
        "one-line".into(),
        vec![
            0x06, 0x00, 0x00, 0x01, 0x02, 0xB5, 0xBA, 0xE5, 0xD3, 0x02, 0xFF, 0xFF, 0xFD, 0xFD,
            0x64, 0xD3, 0x08, 0x5C,
        ],
    ));
    out.push(("not-a-multiple-of-nine".into(), vec![0x06; 10]));
    out.push(("cubic".into(), {
        let mut v = vec![0x06];
        v.extend_from_slice(&[0u8; 8]);
        for _ in 0..3 {
            v.push(0x04);
            v.extend_from_slice(&[0xFF; 8]);
        }
        v
    }));
    out.push(("absolute-two-points".into(), {
        let mut v = 2i32.to_le_bytes().to_vec();
        v.extend_from_slice(&[0x06, 0x03]);
        v.extend_from_slice(&[0u8; 16]);
        v
    }));
    out
}

/// Payload seeds for `fuzz_xar_colour`.
fn colour_seeds() -> Vec<(String, Vec<u8>)> {
    vec![
        ("nameless".into(), minimal_payload(51)[..31].to_vec()),
        ("named-cmyk".into(), minimal_payload(51)),
        ("inherit-sentinel".into(), {
            let mut v = minimal_payload(51);
            if let Some(slot) = v.get_mut(13..17) {
                slot.copy_from_slice(&0xF800_0000u32.to_le_bytes());
            }
            v
        }),
        ("self-parent".into(), {
            let mut v = minimal_payload(51);
            if let Some(slot) = v.get_mut(9..13) {
                slot.copy_from_slice(&1i32.to_le_bytes());
            }
            v
        }),
    ]
}

#[test]
fn the_seed_corpus_is_synthetic_and_survives_the_parser() {
    type SeedSet = (&'static str, Vec<(String, Vec<u8>)>);
    let sets: [SeedSet; 6] = [
        ("fuzz_xar_records", file_seeds()),
        ("fuzz_xar_tree", file_seeds()),
        ("fuzz_xar_decode", file_seeds()),
        ("fuzz_xar_import", file_seeds()),
        ("fuzz_xar_path", path_seeds()),
        ("fuzz_xar_colour", colour_seeds()),
    ];
    let write = std::env::var("XARAST_WRITE_FUZZ_SEEDS").as_deref() == Ok("1");
    for (target, seeds) in sets {
        let dir = fuzz_dir().join("corpus").join(target);
        if write {
            std::fs::create_dir_all(&dir).unwrap();
            for (name, bytes) in &seeds {
                std::fs::write(dir.join(format!("{name}.bin")), bytes).unwrap();
            }
            println!("wrote {} seeds to {}", seeds.len(), dir.display());
        }
        for (name, bytes) in &seeds {
            assert!(
                bytes.len() < 1 << 20,
                "{target}/{name}: a seed should be small"
            );
        }
    }

    // Every file seed goes through the whole pipeline. A panic here is a
    // fuzz finding found without the fuzzer.
    let mut diags = DiagSink::new();
    for (name, bytes) in file_seeds() {
        let Ok(a) = analyse(&bytes, ReaderLimits::default()) else {
            continue;
        };
        a.tree.walk(&mut |node, _| {
            let r = &node.record;
            let _ = decode(r.tag, &r.data, Point::ORIGIN, &mut diags, (r.number, r.tag));
        });
        assert!(a.records_read > 0, "{name}: read nothing");
    }
    for (_, bytes) in path_seeds() {
        let mut cur = xarast_xar::Cur::new(&bytes);
        if let Ok(p) = xarast_xar::decode_relative(&mut cur, Point::ORIGIN, &mut diags, (1, 116)) {
            p.validate().unwrap();
        }
    }
    for (_, bytes) in colour_seeds() {
        let mut cur = xarast_xar::Cur::new(&bytes);
        let _ = xarast_xar::ColourRecord::parse_complex(&mut cur);
    }
}

/// No `.xar` file, and nothing that looks like one, may be committed under
/// `fuzz/`.
#[test]
fn the_committed_seed_corpus_contains_nothing_from_the_real_corpus() {
    let dir = fuzz_dir().join("corpus");
    let Ok(targets) = std::fs::read_dir(&dir) else {
        return;
    };
    let ours: Vec<Vec<u8>> = file_seeds()
        .into_iter()
        .chain(path_seeds())
        .chain(colour_seeds())
        .map(|(_, b)| b)
        .collect();
    let mut checked = 0;
    for t in targets.filter_map(Result::ok) {
        let Ok(files) = std::fs::read_dir(t.path()) else {
            continue;
        };
        for f in files.filter_map(Result::ok) {
            let bytes = std::fs::read(f.path()).unwrap_or_default();
            // Every committed seed must be one this file generates, which
            // makes "synthetic only" mechanical rather than a promise.
            assert!(
                ours.contains(&bytes),
                "{} is not a seed this test generates",
                f.path().display()
            );
            // A real corpus file carries a producer string; ours do not.
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains("Xara X"), "{}", f.path().display());
            assert!(!text.contains("Xara Xtreme"), "{}", f.path().display());
            checked += 1;
        }
    }
    println!("{checked} committed fuzz seeds checked");
}
