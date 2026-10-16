//! Colour fidelity across formats (phase 11 T11.5.1, T11.5.5) and the
//! byte reproducibility of the built-in documents (T11.6.4), end to end
//! through the binary.
//!
//! The colour sheet (`xarast_cli::fixtures::colour_sheet`) holds flat
//! patches in RGB, CMYK, HSV, grey and palette colours (a named CMYK
//! colour, a tint of it, a local tint, a spot ink). It is exported to all
//! five formats and read back: every format must carry, at every patch,
//! the sRGB bytes `xarast_color` resolves the colour to — exactly for
//! PNG, WebP, SVG and PDF, within JPEG's quantisation for JPEG — and every
//! report must say that CMYK and spot colours were converted.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use xarast_cli::fixtures::{Patch, colour_patches};
use xarast_color::Rgba8;
use xarast_geom::{Mp, Rect};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xarast-cli"))
        .args(args)
        .env(
            "XARAST_FONT_DIR",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../xarast-text/tests/fonts"),
        )
        .output()
        .expect("spawn xarast-cli")
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("xarast-cli-fidelity-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Exports the fixtures in `format` into `dir` and returns stdout.
fn fixtures(dir: &Path, format: &str, extra: &[&str]) -> String {
    let d = dir.to_string_lossy();
    let mut args = vec![
        "fixtures",
        "--out-dir",
        &d,
        "--format",
        format,
        "--dpi",
        "72",
    ];
    args.extend_from_slice(extra);
    let o = run(&args);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{format}: {out}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(out.contains("3 files: 3 exported, 0 failed"), "{out}");
    out
}

/// The sheet's area: the union of the patches (they have no outline).
fn area(patches: &[Patch]) -> Rect {
    patches.iter().fold(Rect::EMPTY, |r, p| {
        if r.is_empty() {
            p.rect
        } else {
            r.union(p.rect)
        }
    })
}

/// The pixel at the centre of a patch, at 72 dpi (one pixel per point).
fn centre_pixel(p: &Patch, area: Rect) -> (u32, u32) {
    let per_pt = f64::from(Mp::PER_PT);
    let cx = f64::from(p.rect.lo.x.raw() + p.rect.hi.x.raw()) / 2.0;
    let cy = f64::from(p.rect.lo.y.raw() + p.rect.hi.y.raw()) / 2.0;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (
        ((cx - f64::from(area.lo.x.raw())) / per_pt) as u32,
        ((f64::from(area.hi.y.raw()) - cy) / per_pt) as u32,
    )
}

fn rgb(c: Rgba8) -> [u8; 3] {
    [c.r, c.g, c.b]
}

/// Checks a decoded raster against every patch, within `tolerance` per
/// channel, over the 5 x 5 pixels around each centre.
fn check_raster(format: &str, img: &image::RgbaImage, patches: &[Patch], tolerance: u8) {
    let a = area(patches);
    assert_eq!(img.dimensions(), (276, 180), "{format}");
    for p in patches {
        let (x, y) = centre_pixel(p, a);
        for dy in 0..5 {
            for dx in 0..5 {
                let px = img.get_pixel(x + dx - 2, y + dy - 2).0;
                let want = rgb(p.raster);
                for c in 0..3 {
                    assert!(
                        px[c].abs_diff(want[c]) <= tolerance,
                        "{format}: {} at ({x},{y}): {px:?}, want {want:?}",
                        p.name
                    );
                }
                assert_eq!(px[3], 255, "{format}: {} opaque", p.name);
            }
        }
    }
}

/// `#rgb` or `#rrggbb` to bytes.
fn parse_hex(h: &str) -> [u8; 3] {
    let v: Vec<u8> = match h.len() {
        3 => h
            .chars()
            .map(|c| {
                let d = u8::try_from(c.to_digit(16).unwrap()).unwrap();
                d * 17
            })
            .collect(),
        6 => (0..3)
            .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap())
            .collect(),
        _ => panic!("colour `#{h}`"),
    };
    [v[0], v[1], v[2]]
}

#[test]
fn the_expected_colours_are_the_models_conversions() {
    // Independent anchors: the naive CMYK conversion, the grey model, and
    // 8-bit rounding, written out by hand.
    let by = |n: &str| {
        colour_patches()
            .into_iter()
            .find(|p| p.name == n)
            .unwrap()
            .expected
    };
    assert_eq!(rgb(by("cmyk cyan")), [0, 255, 255]);
    assert_eq!(rgb(by("cmyk key")), [0, 0, 0]);
    assert_eq!(rgb(by("cmyk yellow")), [255, 255, 0]);
    // R = 1 - min(1, C + K): 1 - 0.3 = 0.7 -> 179; 1 - 0.4 = 0.6 -> 153; 0.5.
    assert_eq!(rgb(by("cmyk mixed")), [179, 153, 128]);
    assert_eq!(rgb(by("rgb steel")), [51, 102, 153]);
    assert_eq!(rgb(by("grey 25")), [64, 64, 64]);
    assert_eq!(colour_patches().len(), 20);
    // The renderer packs as the original does (XARA-T-0203): a channel
    // just above k + 0.5 rounds up but packs down. 0.9 x 255 = 229.5.
    let raster = |n: &str| {
        colour_patches()
            .into_iter()
            .find(|p| p.name == n)
            .unwrap()
            .raster
    };
    assert_eq!(rgb(by("rgb amber")), [230, 153, 26]);
    assert_eq!(rgb(raster("rgb amber")), [229, 153, 25]);
    assert_eq!(rgb(raster("rgb steel")), [51, 102, 153]);
}

#[test]
fn every_format_carries_the_colour_sheet() {
    let patches = colour_patches();
    let dir = scratch("sheet");
    for (format, ext, tolerance) in [("png", "png", 0), ("webp", "webp", 0), ("jpeg", "jpg", 6)] {
        let out = fixtures(&dir, format, &["--background", "paper"]);
        assert!(
            out.contains("CMYK colours written as their sRGB conversion (no output profile): 9"),
            "{out}"
        );
        assert!(
            out.contains("spot colours written as their sRGB conversion"),
            "{out}"
        );
        let path = dir.join(format!("colour-sheet.{ext}"));
        let img = image::open(&path).unwrap().to_rgba8();
        check_raster(format, &img, &patches, tolerance);
    }

    // T11.5.1: the markers each raster format carries.
    let png = std::fs::read(dir.join("colour-sheet.png")).unwrap();
    let srgb = png
        .windows(4)
        .position(|w| w == b"sRGB")
        .expect("sRGB chunk");
    assert!(srgb < png.windows(4).position(|w| w == b"IDAT").unwrap());
    let jpg = std::fs::read(dir.join("colour-sheet.jpg")).unwrap();
    assert!(
        jpg.windows(6).any(|w| w == b"Exif\0\0"),
        "EXIF ColorSpace block"
    );
    let webp = std::fs::read(dir.join("colour-sheet.webp")).unwrap();
    assert!(
        !webp.windows(4).any(|w| w == b"ICCP"),
        "a WebP without ICCP is sRGB"
    );

    // SVG: the fills, in document order, are the expected bytes exactly.
    fixtures(&dir, "svg", &[]);
    let svg = std::fs::read_to_string(dir.join("colour-sheet.svg")).unwrap();
    assert!(!svg.contains("icc-color"), "no profile, so no icc-color()");
    // Every patch is a 36 pt `<rect>`; black is SVG's default fill, so
    // a black patch carries no `fill`.
    let fills: Vec<[u8; 3]> = svg
        .match_indices("<rect ")
        .map(|(i, _)| &svg[i..i + svg[i..].find('>').unwrap()])
        .filter(|tag| tag.contains(" width=\"36\""))
        .map(|tag| match tag.find(" fill=\"#") {
            Some(f) => {
                let rest = &tag[f + 8..];
                parse_hex(&rest[..rest.find('"').unwrap()])
            }
            None => [0, 0, 0],
        })
        .collect();
    let want: Vec<[u8; 3]> = patches.iter().map(|p| rgb(p.expected)).collect();
    assert_eq!(fills, want, "SVG fills");

    // PDF, uncompressed: the `rg` operands, in order, round to the bytes
    // the renderer's scene carries (the PDF writer reads the scene, so it
    // has the packed quantisation; the SVG writer resolves colours itself).
    fixtures(&dir, "pdf", &["--no-compress"]);
    let pdf = std::fs::read(dir.join("colour-sheet.pdf")).unwrap();
    let text = String::from_utf8_lossy(&pdf);
    let rg: Vec<[u8; 3]> = text
        .lines()
        .filter(|l| l.ends_with(" rg"))
        .map(|l| {
            let v: Vec<f64> = l
                .split_whitespace()
                .take(3)
                .map(|x| x.parse().unwrap())
                .collect();
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            [0, 1, 2].map(|i| (v[i] * 255.0).round() as u8)
        })
        .collect();
    let scene: Vec<[u8; 3]> = patches.iter().map(|p| rgb(p.raster)).collect();
    assert_eq!(rg, scene, "PDF fill colours");
    assert!(!text.contains("DeviceCMYK"), "sRGB page, DeviceRGB only");
}

/// Poppler's render of the PDF, when `pdftoppm` is installed (CI installs
/// it; the job that runs this also runs `cargo xtask export-check`).
#[test]
fn poppler_reads_the_colour_sheet_back() {
    if Command::new("pdftoppm").arg("-v").output().is_err() {
        println!("skipping: pdftoppm is not installed");
        return;
    }
    let dir = scratch("poppler");
    fixtures(&dir, "pdf", &[]);
    let stem = dir.join("sheet");
    let o = Command::new("pdftoppm")
        .args([
            "-png",
            "-singlefile",
            "-scale-to-x",
            "276",
            "-scale-to-y",
            "180",
        ])
        .arg(dir.join("colour-sheet.pdf"))
        .arg(&stem)
        .output()
        .unwrap();
    assert!(o.status.success());
    let img = image::open(stem.with_extension("png")).unwrap().to_rgba8();
    check_raster("pdf via Poppler", &img, &colour_patches(), 1);
}

/// T11.6.4 on one machine: two processes write byte-identical files for
/// every built-in document in every format. CI compares the same files
/// across two machines (`.github/workflows/ci.yml`, job `export`).
#[test]
fn the_fixtures_are_byte_identical_across_processes() {
    for format in ["png", "jpeg", "webp", "pdf", "svg"] {
        let (a, b) = (
            scratch(&format!("repro-a-{format}")),
            scratch(&format!("repro-b-{format}")),
        );
        fixtures(&a, format, &["--quiet"]);
        fixtures(&b, format, &["--quiet"]);
        let mut names: Vec<_> = std::fs::read_dir(&a)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names.len(), 3, "{format}");
        for n in names {
            assert_eq!(
                std::fs::read(a.join(&n)).unwrap(),
                std::fs::read(b.join(&n)).unwrap(),
                "{format}: {n:?}"
            );
        }
    }
}
