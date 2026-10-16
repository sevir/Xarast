//! `xarast-cli repair`: rebuild a damaged `.xarast` package.
//!
//! The work is `xarast_format::repair` (F6.8): scan the local headers,
//! validate every entry, write a new package. The CLI owns the framing and
//! the report. The input is never modified; the output is written atomically.

use std::path::{Path, PathBuf};

use xarast_format::RepairOptions;
use xarast_format::repair::{Action, RepairError, RepairReport};

use crate::Exit;
use crate::args::Args;

/// Usage for `repair`.
pub const USAGE: &str = "\
xarast-cli repair — rebuild a damaged .xarast package

USAGE:
    xarast-cli repair <IN.xarast> <OUT.xarast> [OPTIONS]

OPTIONS
    --quiet    print only the summary line

The central directory and end record are not trusted: every local header is
scanned, each entry is checked against its CRC-32 and the manifest digests,
and a new package is written. document.svg is kept even when it does not
verify (opening it then recovers what it can, read-only); a resource, preview
or thumbnail that does not verify is dropped, and a missing meta.xml is
replaced by a minimal one. The input is never modified.

EXIT CODES
    0  a package was written (the report says what was lost, if anything)
    1  bad command line
    2  the input could not be read or holds no recoverable document
    3  the output could not be written
";

/// Parsed `repair` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairArgs {
    /// The damaged package.
    pub input: PathBuf,
    /// The package to write.
    pub output: PathBuf,
    /// `--quiet`.
    pub quiet: bool,
}

/// Parses `repair`'s arguments.
///
/// # Errors
///
/// A message for the user when the command line is wrong.
pub fn parse(argv: &[String]) -> Result<RepairArgs, String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut quiet = false;
    let mut it = Args::new(argv);
    while let Some(arg) = it.next_arg() {
        match arg.as_str() {
            "--quiet" | "-q" => quiet = true,
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unknown option {other}"));
            }
            other => paths.push(PathBuf::from(other)),
        }
    }
    let [input, output] = <[PathBuf; 2]>::try_from(paths)
        .map_err(|_| "give exactly an input and an output file".to_owned())?;
    if input == output {
        return Err("the output must not be the input".into());
    }
    Ok(RepairArgs {
        input,
        output,
        quiet,
    })
}

/// The lines of a report, for people.
#[must_use]
pub fn describe(r: &RepairReport) -> Vec<String> {
    let mut v = Vec::new();
    if r.was_openable {
        v.push("the container was readable as it was".to_owned());
    }
    if !r.manifest_used {
        v.push("the manifest was unreadable: digests could not be checked".to_owned());
    }
    for e in &r.entries {
        match e.action {
            Action::Dropped => v.push(format!("dropped {} ({:?})", e.name, e.verdict)),
            Action::Salvaged => v.push(format!(
                "kept {} although it does not verify ({:?}); opening it recovers what it can",
                e.name, e.verdict
            )),
            Action::Replaced => v.push(format!("replaced {} with a minimal one", e.name)),
            Action::Kept => {}
        }
    }
    for m in &r.manifest_rows_missing {
        v.push(format!("{m} is listed in the manifest but was not found"));
    }
    v
}

/// Repairs one file.
///
/// # Errors
///
/// The exit code and message when it cannot.
pub fn repair_one(input: &Path, output: &Path) -> Result<RepairReport, (Exit, String)> {
    xarast_format::repair_file(input, output, &RepairOptions::default()).map_err(|e| match e {
        RepairError::Write(_) => (Exit::Render, format!("{}: {e}", output.display())),
        _ => (Exit::Import, format!("{}: {e}", input.display())),
    })
}

/// Runs `repair`.
#[must_use]
pub fn run(a: &RepairArgs) -> Exit {
    match repair_one(&a.input, &a.output) {
        Ok(r) => {
            if !a.quiet {
                for l in describe(&r) {
                    println!("  {l}");
                }
            }
            println!(
                "{} -> {}: {} of {} entries found, {} bytes{}",
                a.input.display(),
                a.output.display(),
                r.entries
                    .iter()
                    .filter(|e| matches!(e.action, Action::Kept | Action::Salvaged))
                    .count(),
                r.entries_found,
                r.bytes_written,
                if r.lossy() { " (with losses)" } else { "" }
            );
            Exit::Ok
        }
        Err((code, msg)) => {
            eprintln!("xarast-cli: {msg}");
            code
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn needs_two_distinct_paths() {
        assert!(parse(&argv(&[])).is_err());
        assert!(parse(&argv(&["a"])).is_err());
        assert!(parse(&argv(&["a", "a"])).is_err());
        assert!(parse(&argv(&["a", "b", "c"])).is_err());
        assert!(parse(&argv(&["a", "b", "--nope"])).is_err());
        let p = parse(&argv(&["a.xarast", "b.xarast", "-q"])).unwrap();
        assert!(p.quiet);
    }
}
