//! Fill import against the corpus (phase 8, W8.8: T8.8.1, T8.8.4).
//!
//! Every fill, transparency, line colour and line transparency record of a
//! `.xar` file becomes exactly one attribute of the imported model, of the
//! same shape; every fill effect and fill mapping record becomes one
//! attribute too. [`inspect::census`] counts the model side and
//! [`inspect::tag_census`] the record side; they must agree for all 59
//! files, family by family. Records inside `TAG_CURRENTATTRIBUTES` are the
//! editor's current attributes, not part of the drawing, and are left out
//! of both; so are the fill records of a shadow node (`TAG_SHADOW`), which
//! become the shadow's colour and darkness parameters, and those of a bevel
//! node (`TAG_BEVELINK`), copies of the object's fill.
//!
//! No corpus byte enters the repository: files are read from
//! `XARAST_XAR_CORPUS`, and the test skips when it is absent (fails when
//! `XARAST_CORPUS_REQUIRED=1`).

use std::path::{Path, PathBuf};
use std::process::Command;

use xarast_cli::inspect;
use xarast_xar::ImportOptions;

fn corpus_root() -> Option<PathBuf> {
    let required = std::env::var("XARAST_CORPUS_REQUIRED").as_deref() == Ok("1");
    let root = PathBuf::from(
        std::env::var("XARAST_XAR_CORPUS").unwrap_or_else(|_| "/home/user/xara-xtreme".into()),
    );
    if !root.is_dir() {
        assert!(
            !required,
            "XARAST_CORPUS_REQUIRED=1 but the corpus is absent"
        );
        eprintln!("corpus not found at {}; skipping", root.display());
        return None;
    }
    Some(root)
}

fn corpus_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["testfiles", "Designs", "Templates", "TextDesigns"] {
        collect(&root.join(dir), &mut out);
    }
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("xar")) {
            out.push(p);
        }
    }
}

#[test]
fn every_fill_record_lands_in_the_model_as_one_attribute_of_its_shape() {
    let Some(root) = corpus_root() else { return };
    let files = corpus_files(&root);
    assert_eq!(files.len(), 59, "the corpus is 59 files");
    let mut total_rows = 0usize;
    let mut mismatches = Vec::new();
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let (doc, _) = xarast_xar::import(&bytes, &ImportOptions::default()).unwrap();
        let model = inspect::census(&doc);
        let records = inspect::tag_census(&bytes).unwrap();
        total_rows += model.rows.len();
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        if model.histogram_outside_text() != records.histogram_outside_text() {
            mismatches.push(format!(
                "{name}: shapes: model {:?} vs records {:?}",
                model.histogram_outside_text(),
                records.histogram_outside_text()
            ));
        }
        if model.effects != records.effects {
            mismatches.push(format!(
                "{name}: effects: model {:?} vs records {:?}",
                model.effects, records.effects
            ));
        }
        if model.mappings != records.mappings {
            mismatches.push(format!(
                "{name}: mappings: model {:?} vs records {:?}",
                model.mappings, records.mappings
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    assert!(total_rows > 350_000, "{total_rows} fill attributes");
}

#[test]
fn inspect_fills_lists_every_fill_of_fill_types_simple() {
    let Some(root) = corpus_root() else { return };
    let file = root.join("Designs/Fill Types simple.xar");
    let out = Command::new(env!("CARGO_BIN_EXE_xarast-cli"))
        .args(["inspect", "--fills"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    let rows = text
        .lines()
        .filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
        .count();
    let bytes = std::fs::read(&file).unwrap();
    let records = inspect::tag_census(&bytes).unwrap();
    assert_eq!(rows, records.rows.len(), "{text}");
    // Every graduated family of the file is there, with its extras.
    for shape in [
        "linear",
        "circular",
        "elliptical",
        "conical",
        "diamond",
        "three-colour",
        "four-colour",
        "bitmap",
        "contone",
        "fractal",
        "noise",
    ] {
        assert!(
            text.lines()
                .any(|l| l.contains(&format!(" {shape} ")) || l.ends_with(&format!(" {shape}"))),
            "{shape} missing:\n{text}"
        );
    }
    assert!(text.contains("stops=2"), "{text}");
    assert!(text.contains("effect=alt-rainbow"), "{text}");
    assert!(text.contains("mapping=repeat-extra"), "{text}");
}

/// The corpus's three bitmap transparencies (JagSS100 simple, scope3
/// simple) keep their mode through import and the census says which
/// (XARA-T-0307): all three are Mix.
#[test]
fn bitmap_transparencies_are_listed_with_their_mode() {
    let Some(root) = corpus_root() else { return };
    let mut modes = Vec::new();
    for name in ["Designs/JagSS100 simple.xar", "Designs/scope3 simple.xar"] {
        let out = Command::new(env!("CARGO_BIN_EXE_xarast-cli"))
            .args(["inspect", "--fills"])
            .arg(root.join(name))
            .output()
            .unwrap();
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        modes.extend(
            text.lines()
                .filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
                .filter(|l| l.contains(" transparency ") && l.contains(" bitmap "))
                .map(|l| l.split("mode=").nth(1).unwrap_or("").trim().to_owned()),
        );
    }
    assert_eq!(modes, ["mix", "mix", "mix"]);
}
