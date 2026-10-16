//! `xarast-cli`'s contract, end to end through the binary.
//!
//! The first group needs nothing but synthetic input: a `.xar` stream
//! built record by record with `xarast_xar::synth`, holding one black
//! square — our own geometry, not a corpus byte. The second group runs
//! over the real 59-file corpus through `XARAST_XAR_CORPUS` and skips with
//! a notice when it is not there, so CI stays green without it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use xarast_xar::synth::XarBuilder;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_xarast-cli")
}

fn run(args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        // Pinned fonts: a test never depends on the machine's.
        .env(
            "XARAST_FONT_DIR",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../xarast-text/tests/fonts"),
        )
        .output()
        .expect("spawn xarast-cli")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A fresh directory per test, so tests running in parallel never share
/// a file. The process id keeps two concurrent `cargo test` runs (say, in
/// two worktrees) from deleting each other's output mid-export.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xarast-cli-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const TAG_DOCUMENT: u32 = 40;
const TAG_CHAPTER: u32 = 41;
const TAG_SPREAD: u32 = 42;
const TAG_LAYER: u32 = 43;
const TAG_PATH_FILLED: u32 = 101;
const TAG_FLATFILL_BLACK: u32 = 191;

/// A closed square path payload: count, verbs, then absolute points.
fn square(x: i32, y: i32, side: i32) -> Vec<u8> {
    let pts = [
        (0x06u8, x, y),
        (0x02, x + side, y),
        (0x02, x + side, y + side),
        (0x02 | 0x01, x, y + side),
    ];
    let mut v = (pts.len() as i32).to_le_bytes().to_vec();
    v.extend(pts.iter().map(|p| p.0));
    for &(_, px, py) in &pts {
        v.extend_from_slice(&px.to_le_bytes());
        v.extend_from_slice(&py.to_le_bytes());
    }
    v
}

/// A document holding one black one-inch square.
fn square_xar() -> Vec<u8> {
    XarBuilder::new()
        .record(TAG_DOCUMENT, &[])
        .down()
        .record(TAG_CHAPTER, &[])
        .down()
        .record(TAG_SPREAD, &[])
        .down()
        .record(TAG_LAYER, &[])
        .down()
        .record(TAG_PATH_FILLED, &square(72_000, 72_000, 72_000))
        .down()
        .record(TAG_FLATFILL_BLACK, &[])
        .up()
        .up()
        .up()
        .up()
        .up()
        .end_of_file()
        .finish()
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn png_size(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    (w, h)
}

#[test]
fn no_arguments_is_a_usage_error_and_help_is_not() {
    assert_eq!(run(&[]).status.code(), Some(1));
    let h = run(&["--help"]);
    assert_eq!(h.status.code(), Some(0));
    assert!(stdout(&h).contains("smoke-open"));
    let h = run(&["render", "--help"]);
    assert_eq!(h.status.code(), Some(0));
    assert!(stdout(&h).contains("--zoom"));
    assert_eq!(run(&["frobnicate"]).status.code(), Some(1));
    assert_eq!(run(&["render", "a.xar"]).status.code(), Some(1));
}

#[test]
fn inspect_fills_lists_the_fill_and_its_histogram() {
    let dir = scratch("inspect-fills");
    let src = write(&dir, "square.xar", &square_xar());
    let src = src.to_str().unwrap();
    let out = run(&["inspect", "--fills", src]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains(": 1 fills"), "{text}");
    assert!(text.contains("     0  fill              flat"), "{text}");
    assert!(text.contains("  histogram\n"), "{text}");
    let summary = stdout(&run(&["inspect", "--fills", "--summary", src]));
    assert!(!summary.contains("     0  fill"), "{summary}");
    assert!(summary.contains("    fill              flat"), "{summary}");
    // What to inspect must be said.
    assert_eq!(run(&["inspect", src]).status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn render_at_100_percent_is_the_drawing_at_96_dpi() {
    let dir = scratch("render100");
    let input = write(&dir, "square.xar", &square_xar());
    let out = dir.join("square.png");
    let o = run(&[
        "render",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    // One inch at 100 % and 96 dpi is 96 pixels. The bounds may include
    // a pixel of antialiasing slack, never more.
    let (w, h) = png_size(&out);
    assert!((96..=98).contains(&w) && (96..=98).contains(&h), "{w}x{h}");
    let text = stdout(&o);
    assert!(text.contains("at 100.0%"), "{text}");
    assert!(
        !text.contains("ink 0.00%"),
        "the square drew nothing: {text}"
    );
}

#[test]
fn render_honours_zoom_dpi_and_explicit_sizes() {
    let dir = scratch("render-sizes");
    let input = write(&dir, "square.xar", &square_xar());
    let i = input.to_str().unwrap();

    let z = dir.join("zoom.png");
    let o = run(&[
        "render",
        i,
        "-o",
        z.to_str().unwrap(),
        "--zoom",
        "50%",
        "--dpi",
        "144",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let (w, _) = png_size(&z);
    assert!((72..=74).contains(&w), "{w}");

    let s = dir.join("sized.png");
    let o = run(&[
        "render",
        i,
        "-o",
        s.to_str().unwrap(),
        "--width=300",
        "--height=200",
        "--quality",
        "draft",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(png_size(&s), (300, 200));
}

#[test]
fn render_a_directory_into_an_out_dir_with_a_summary() {
    let dir = scratch("render-dir");
    let docs = dir.join("docs");
    std::fs::create_dir_all(docs.join("sub")).unwrap();
    write(&docs, "a.xar", &square_xar());
    write(&docs.join("sub"), "a.xar", &square_xar());
    let out = dir.join("out");
    let o = run(&[
        "render",
        docs.to_str().unwrap(),
        "--out-dir",
        out.to_str().unwrap(),
        "--quiet",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(out.join("a.png").is_file());
    assert!(
        out.join("a-2.png").is_file(),
        "same stem must not overwrite"
    );
    assert!(
        stdout(&o).contains("2 files: 2 rendered (2 with ink, 0 blank), 0 failed"),
        "{}",
        stdout(&o)
    );
}

#[test]
fn render_of_a_broken_file_fails_with_the_import_code() {
    let dir = scratch("render-broken");
    let input = write(&dir, "broken.xar", b"not a xar file at all");
    let o = run(&[
        "render",
        input.to_str().unwrap(),
        "-o",
        dir.join("x.png").to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("FAILED"), "{}", stderr(&o));
}

#[test]
fn smoke_open_reports_walk_stats_and_fails_on_a_bad_file() {
    let dir = scratch("smoke");
    let good = write(&dir, "good.xar", &square_xar());
    let o = run(&["smoke-open", good.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let text = stdout(&o);
    assert!(text.contains("good.xar: ok"), "{text}");
    assert!(text.contains("1 primitives, complete"), "{text}");
    assert!(
        text.contains("1 files: 1 opened (1 complete, 1 with primitives), 0 failed"),
        "{text}"
    );

    let bad = write(&dir, "bad.xar", b"XARA garbage");
    let missing = dir.join("missing.xar");
    let o = run(&[
        "smoke-open",
        good.to_str().unwrap(),
        bad.to_str().unwrap(),
        missing.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).contains("3 files: 1 opened (1 complete, 1 with primitives), 2 failed"));
    assert_eq!(stderr(&o).matches("FAILED").count(), 2, "{}", stderr(&o));
}

#[test]
fn repair_rebuilds_a_package_without_its_central_directory() {
    let dir = scratch("repair");
    let xar = write(&dir, "sq.xar", &square_xar());
    let pkg = dir.join("sq.xarast");
    let o = run(&[
        "convert",
        xar.to_str().unwrap(),
        "-o",
        pkg.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let bytes = std::fs::read(&pkg).unwrap();
    let cd = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    let damaged = write(&dir, "damaged.xarast", &bytes[..cd]);
    let fixed = dir.join("fixed.xarast");

    let o = run(&["repair", damaged.to_str().unwrap(), fixed.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).contains("entries found"), "{}", stdout(&o));
    let r = xarast_format::XarastReader::open(std::fs::File::open(&fixed).unwrap())
        .expect("the repaired package opens strictly");
    assert!(r.diagnostics().is_empty(), "{:?}", r.diagnostics());

    // Nothing recoverable, same path twice, and a missing input.
    let junk = write(&dir, "junk.xarast", b"not a package");
    let o = run(&["repair", junk.to_str().unwrap(), dir.join("o.xarast").to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    let o = run(&["repair", fixed.to_str().unwrap(), fixed.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    let o = run(&["repair", dir.join("nope").to_str().unwrap(), dir.join("o2").to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn export_writes_every_format_at_the_requested_resolution() {
    let dir = scratch("export-formats");
    let input = write(&dir, "square.xar", &square_xar());
    let i = input.to_str().unwrap();
    for ext in ["png", "jpg", "webp"] {
        let out = dir.join(format!("sq.{ext}"));
        let o = run(&[
            "export",
            i,
            "-o",
            out.to_str().unwrap(),
            "--dpi",
            "300",
            "--area",
            "72,72,144,144",
        ]);
        assert_eq!(o.status.code(), Some(0), "{ext}: {}", stderr(&o));
        assert!(
            stdout(&o).contains("300x300 px at 300.0 dpi"),
            "{}",
            stdout(&o)
        );
        let img = image_size(&out);
        assert_eq!(img, (300, 300), "{ext}");
    }
    // JPEG over the default transparent background says it flattened.
    let o = run(&["export", i, "-o", dir.join("n.jpg").to_str().unwrap()]);
    assert!(
        stdout(&o).contains("note: transparency flattened onto #ffffff"),
        "{}",
        stdout(&o)
    );
}

#[test]
fn export_writes_a_vector_pdf_page_the_size_of_the_area() {
    let dir = scratch("export-pdf");
    let input = write(&dir, "square.xar", &square_xar());
    let out = dir.join("sq.pdf");
    let o = run(&[
        "export",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--area",
        "72,72,144,180",
        "--no-compress",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).contains("72x108 pt page"), "{}", stdout(&o));
    let bytes = std::fs::read(&out).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.starts_with("%PDF-1.7"));
    assert!(text.contains("/MediaBox [0 0 72 108]"), "{text}");
    assert!(!text.contains("/Subtype /Image"), "a flat square is vector");
    // PDF options on another format, and PNG options on PDF, are refused.
    for args in [
        vec!["--raster-dpi", "150", "-o", "x.png"],
        vec!["--interlace", "-o", "x.pdf"],
        vec!["--raster-dpi", "5", "-o", "x.pdf"],
    ] {
        let mut a = vec!["export", input.to_str().unwrap()];
        a.extend(args);
        let o = run(&a);
        assert_ne!(o.status.code(), Some(0), "{a:?}");
    }
}

#[test]
fn export_writes_a_plain_svg_framing_the_area() {
    let dir = scratch("export-svg");
    let input = write(&dir, "square.xar", &square_xar());
    let out = dir.join("sq.svg");
    let o = run(&[
        "export",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--area",
        "72,72,144,180",
        "--minify",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).contains("72x108 pt page"), "{}", stdout(&o));
    let svg = std::fs::read_to_string(&out).unwrap();
    assert!(svg.starts_with("<?xml"), "{svg}");
    assert!(svg.contains(" width=\"25.4mm\" height=\"38.1mm\""), "{svg}");
    assert!(!svg.contains("xarast"), "{svg}");
    // SVG options on another format, and raster options on SVG, are refused.
    for args in [
        vec!["--minify", "-o", "x.png"],
        vec!["--resources", "sidecar", "-o", "x.pdf"],
        vec!["--quality", "50", "-o", "x.svg"],
        vec!["--resources", "zip", "-o", "x.svg"],
    ] {
        let mut a = vec!["export", input.to_str().unwrap()];
        a.extend(args);
        let o = run(&a);
        assert_ne!(o.status.code(), Some(0), "{a:?}");
    }
}

fn image_size(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).unwrap();
    if bytes.starts_with(b"\x89PNG") {
        return png_size(path);
    }
    if bytes.starts_with(b"RIFF") {
        // VP8L: 14 bits each of width-1 and height-1 after the signature.
        let b = &bytes[21..25];
        let v = u32::from_le_bytes(b.try_into().unwrap());
        return ((v & 0x3fff) + 1, ((v >> 14) & 0x3fff) + 1);
    }
    // JPEG: the first SOF0/SOF2 segment.
    let sof = bytes
        .windows(2)
        .position(|w| w == [0xFF, 0xC0] || w == [0xFF, 0xC2])
        .expect("SOF");
    let h = u32::from(u16::from_be_bytes([bytes[sof + 5], bytes[sof + 6]]));
    let w = u32::from(u16::from_be_bytes([bytes[sof + 7], bytes[sof + 8]]));
    (w, h)
}

#[test]
fn export_refuses_xar_with_the_architecture_reason() {
    let dir = scratch("export-xar");
    let input = write(&dir, "square.xar", &square_xar());
    let out = dir.join("out.xar");
    for args in [
        vec![
            "export",
            "--format",
            "xar",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ],
        vec![
            "export",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ],
    ] {
        let o = run(&args);
        assert_ne!(o.status.code(), Some(0));
        assert!(stderr(&o).contains("§3.5"), "{}", stderr(&o));
    }
    assert!(!out.exists());
}

#[test]
fn export_is_byte_identical_across_processes() {
    let dir = scratch("export-determinism");
    let input = write(&dir, "square.xar", &square_xar());
    for ext in ["png", "jpg", "webp", "pdf", "svg"] {
        let a = dir.join(format!("a.{ext}"));
        let b = dir.join(format!("b.{ext}"));
        for out in [&a, &b] {
            let o = run(&[
                "export",
                input.to_str().unwrap(),
                "-o",
                out.to_str().unwrap(),
                "--dpi",
                "150",
            ]);
            assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
        }
        assert_eq!(
            std::fs::read(&a).unwrap(),
            std::fs::read(&b).unwrap(),
            "{ext}"
        );
    }
}

// ── The corpus ──────────────────────────────────────────────────────────────

fn corpus() -> Option<PathBuf> {
    let root = PathBuf::from(std::env::var_os("XARAST_XAR_CORPUS")?);
    if root.join("testfiles").is_dir() {
        Some(root)
    } else {
        None
    }
}

macro_rules! corpus_or_skip {
    () => {
        match corpus() {
            Some(c) => c,
            None => {
                println!("skipping: no .xar corpus (set XARAST_XAR_CORPUS)");
                return;
            }
        }
    };
}

const CORPUS_DIRS: [&str; 4] = ["testfiles", "Designs", "Templates", "TextDesigns"];

#[test]
fn corpus_every_file_smoke_opens() {
    let root = corpus_or_skip!();
    let dirs: Vec<String> = CORPUS_DIRS
        .iter()
        .map(|d| root.join(d).to_string_lossy().into_owned())
        .collect();
    let mut args = vec!["smoke-open", "--quiet"];
    args.extend(dirs.iter().map(String::as_str));
    let o = run(&args);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let text = stdout(&o);
    assert!(text.contains("59 files: 59 opened"), "{text}");
}

#[test]
fn corpus_renders_small_with_no_failure() {
    let root = corpus_or_skip!();
    let out = scratch("corpus-render");
    let mut args = vec![
        "render".to_owned(),
        "--out-dir".to_owned(),
        out.to_string_lossy().into_owned(),
        "--width=128".to_owned(),
        "--quality=draft".to_owned(),
        "--quiet".to_owned(),
    ];
    args.extend(
        CORPUS_DIRS
            .iter()
            .map(|d| root.join(d).to_string_lossy().into_owned()),
    );
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let o = run(&args);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let text = stdout(&o);
    assert!(text.contains("59 files: 59 rendered"), "{text}");
    println!("{text}");
}

#[test]
fn corpus_exports_to_every_format_with_no_failure() {
    let root = corpus_or_skip!();
    for fmt in ["png", "jpeg", "webp", "pdf", "svg"] {
        let out = scratch(&format!("corpus-export-{fmt}"));
        let mut args = vec![
            "export".to_owned(),
            "--out-dir".to_owned(),
            out.to_string_lossy().into_owned(),
            "--format".to_owned(),
            fmt.to_owned(),
            "--width=160".to_owned(),
            "--quiet".to_owned(),
        ];
        args.extend(
            CORPUS_DIRS
                .iter()
                .map(|d| root.join(d).to_string_lossy().into_owned()),
        );
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let o = run(&args);
        assert_eq!(o.status.code(), Some(0), "{fmt}: {}", stderr(&o));
        let text = stdout(&o);
        assert!(text.contains("59 files: 59 exported, 0 failed"), "{text}");
        if fmt == "svg" {
            // The interchange dialect carries none of the profile's
            // private vocabulary (phase 11 W11.3).
            for f in std::fs::read_dir(&out).unwrap() {
                let svg = std::fs::read_to_string(f.unwrap().path()).unwrap();
                assert!(!svg.contains("xarast:"));
                assert!(!svg.contains("xmlns:xarast"));
            }
        }
    }
}
