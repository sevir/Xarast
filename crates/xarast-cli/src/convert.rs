//! `xarast-cli convert`: `.xar` documents to `.xarast` packages.
//!
//! The import is `xarast-xar`'s, the save is `xarast-format`'s
//! (`save`: the SVG profile, the resources, `meta.xml`, the container, an
//! atomic write). The CLI owns the framing and the report, which is what
//! the corpus timings in `docs/memory/perf.md` are read from.

use std::path::{Path, PathBuf};
use std::time::Instant;

use xarast_format::svg::Stats;
use xarast_format::{SaveOptions, WriteOptions};

use crate::Exit;
use crate::args::Args;
use crate::inputs::{expand, ms};

/// Usage for `convert`.
pub const USAGE: &str = "\
xarast-cli convert — convert .xar documents to .xarast packages

USAGE:
    xarast-cli convert <IN.xar> -o <OUT.xarast> [OPTIONS]
    xarast-cli convert <IN.xar|DIR>... --out-dir <DIR> [OPTIONS]

OPTIONS
    -o, --output FILE    the package to write (one input only)
    --out-dir DIR        write <stem>.xarast per input into DIR; a directory
                         input stands for every .xar file below it
    --pretty             indent document.svg, one space per level
    --deterministic      fixed timestamps: the same input gives the same bytes
    --quiet              print only failures and the summary

Each line reports the node count, the size of document.svg and of the
package, the time spent importing, serialising the SVG and writing the
package, and what the SVG had to approximate.
";

/// Parsed `convert` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertArgs {
    /// Files or directories.
    pub inputs: Vec<PathBuf>,
    /// `-o`.
    pub output: Option<PathBuf>,
    /// `--out-dir`.
    pub out_dir: Option<PathBuf>,
    /// `--pretty`.
    pub pretty: bool,
    /// `--deterministic`.
    pub deterministic: bool,
    /// `--quiet`.
    pub quiet: bool,
}

/// Parses `convert`'s arguments.
///
/// # Errors
///
/// A message for the user when the command line is wrong.
pub fn parse(argv: &[String]) -> Result<ConvertArgs, String> {
    let mut a = ConvertArgs {
        inputs: Vec::new(),
        output: None,
        out_dir: None,
        pretty: false,
        deterministic: false,
        quiet: false,
    };
    let mut it = Args::new(argv);
    while let Some(arg) = it.next_arg() {
        match arg.as_str() {
            "-o" | "--output" => a.output = Some(PathBuf::from(it.value("--output")?)),
            "--out-dir" => a.out_dir = Some(PathBuf::from(it.value("--out-dir")?)),
            "--pretty" => a.pretty = true,
            "--deterministic" => a.deterministic = true,
            "--quiet" | "-q" => a.quiet = true,
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unknown option {other}"));
            }
            other => a.inputs.push(PathBuf::from(other)),
        }
    }
    if a.inputs.is_empty() {
        return Err("no input given".into());
    }
    match (&a.output, &a.out_dir) {
        (Some(_), Some(_)) => return Err("-o and --out-dir are exclusive".into()),
        (None, None) => return Err("give -o FILE or --out-dir DIR".into()),
        (Some(_), None) if a.inputs.len() != 1 || a.inputs.iter().any(|p| p.is_dir()) => {
            return Err("-o takes exactly one input file".into());
        }
        _ => {}
    }
    Ok(a)
}

/// What converting one document did.
#[derive(Debug, Clone)]
pub struct Converted {
    /// Nodes in the imported tree.
    pub nodes: usize,
    /// Size of `document.svg`.
    pub svg_bytes: usize,
    /// Size of the package.
    pub package_bytes: u64,
    /// Milliseconds reading and importing.
    pub import_ms: f64,
    /// Milliseconds serialising `document.svg` and `meta.xml`.
    pub serialise_ms: f64,
    /// Milliseconds compressing, writing, syncing and renaming.
    pub package_ms: f64,
    /// What the SVG holds.
    pub stats: Stats,
}

/// Converts one document.
///
/// # Errors
///
/// A message when it cannot be read, imported or saved.
pub fn convert_one(input: &Path, output: &Path, a: &ConvertArgs) -> Result<Converted, String> {
    let t0 = Instant::now();
    let bytes = std::fs::read(input).map_err(|e| format!("{}: {e}", input.display()))?;
    let (doc, _report) = xarast_xar::import(&bytes, &xarast_xar::ImportOptions::default())
        .map_err(|e| format!("{}: {e}", input.display()))?;
    let import_ms = ms(t0.elapsed());
    let opts = SaveOptions {
        write: if a.deterministic {
            WriteOptions::deterministic()
        } else {
            WriteOptions::default()
        },
        svg: xarast_format::svg::SvgOptions {
            pretty: a.pretty,
            // Text placed where Xarast draws it (XARA-T-0172).
            text: Some(xarast_app::svg_text::placer()),
            ..Default::default()
        },
        ..SaveOptions::default()
    };
    let r = xarast_format::save(&doc, output, &opts)
        .map_err(|e| format!("{}: {e}", output.display()))?;
    Ok(Converted {
        nodes: doc.tree.node_count(),
        svg_bytes: r.svg.bytes,
        package_bytes: r.package.bytes_written,
        import_ms,
        serialise_ms: ms(r.serialise),
        package_ms: ms(r.package_time),
        stats: r.svg,
    })
}

/// A one-line summary of what an SVG had to approximate.
#[must_use]
pub fn approximations(s: &Stats) -> String {
    let mut v = Vec::new();
    let mut add = |n: usize, what: &str| {
        if n > 0 {
            v.push(format!("{n} {what}"));
        }
    };
    add(s.fills_approximated, "fills");
    add(s.perspective_approximated, "perspective");
    add(s.blend_modes_approximated, "blend modes");
    add(s.arrows_unbaked, "arrows");
    add(s.strokes_approximated, "strokes");
    add(s.effects_approximated, "feathers");
    add(s.shadows_unbaked, "shadows");
    add(s.bevels_unbaked, "bevels");
    add(s.images_unrenderable, "BMP images");
    add(s.images_missing, "missing images");
    add(s.texts, "text stories");
    add(s.live, "live nodes");
    add(s.clips_unsupported, "clips");
    add(s.quickshapes_without_outline, "shapes without outline");
    v.join(", ")
}

/// Runs `convert`.
#[must_use]
pub fn run(a: &ConvertArgs) -> Exit {
    let files = match expand(&a.inputs) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("xarast-cli: {e}");
            return Exit::Import;
        }
    };
    if let Some(d) = &a.out_dir
        && let Err(e) = std::fs::create_dir_all(d)
    {
        eprintln!("xarast-cli: {}: {e}", d.display());
        return Exit::Render;
    }
    let (mut ok, mut failed) = (0usize, 0usize);
    let (mut t_import, mut t_ser, mut t_pkg) = (0.0f64, 0.0f64, 0.0f64);
    let (mut svg_total, mut pkg_total) = (0usize, 0u64);
    for input in &files {
        let output = match (&a.output, &a.out_dir) {
            (Some(o), _) => o.clone(),
            (None, Some(d)) => {
                let stem = input
                    .file_stem()
                    .map_or_else(|| "out".into(), |s| s.to_string_lossy().into_owned());
                d.join(format!("{stem}.xarast"))
            }
            (None, None) => continue,
        };
        match convert_one(input, &output, a) {
            Ok(c) => {
                ok += 1;
                t_import += c.import_ms;
                t_ser += c.serialise_ms;
                t_pkg += c.package_ms;
                svg_total += c.svg_bytes;
                pkg_total += c.package_bytes;
                if !a.quiet {
                    let approx = approximations(&c.stats);
                    println!(
                        "{}: ok, {} nodes, svg {} B, package {} B, import {:.1} ms, \
                         svg {:.1} ms, package {:.1} ms{}",
                        input.display(),
                        c.nodes,
                        c.svg_bytes,
                        c.package_bytes,
                        c.import_ms,
                        c.serialise_ms,
                        c.package_ms,
                        if approx.is_empty() {
                            String::new()
                        } else {
                            format!("; approximated: {approx}")
                        }
                    );
                }
            }
            Err(e) => {
                failed += 1;
                eprintln!("FAILED: {e}");
            }
        }
    }
    println!(
        "{} files: {ok} converted, {failed} failed; svg {svg_total} B, packages {pkg_total} B; \
         import {t_import:.1} ms, svg {t_ser:.1} ms, package {t_pkg:.1} ms",
        files.len()
    );
    if failed > 0 { Exit::Render } else { Exit::Ok }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn exactly_one_destination_is_required() {
        assert!(parse(&argv(&["a.xar"])).is_err());
        assert!(parse(&argv(&["a.xar", "-o", "a.xarast", "--out-dir", "d"])).is_err());
        assert!(parse(&argv(&["a.xar", "b.xar", "-o", "a.xarast"])).is_err());
        let a = parse(&argv(&["a.xar", "-o", "a.xarast", "--pretty"])).unwrap();
        assert!(a.pretty);
        assert_eq!(a.output.as_deref(), Some(Path::new("a.xarast")));
    }
}
