//! Headless command line tools: convert, render and inspect.
//!
//! Not a side project. This is how rendering is verified in CI, how the `.xar`
//! corpus is validated, and how conversions are benchmarked.
//!
//! # Structure
//!
//! `xarast-cli <SUBCOMMAND> [ARGS]`. Each subcommand is one module with a
//! `USAGE` string, a `parse` function over its own arguments and a `run`
//! function that returns an [`Exit`] code. [`run`] only dispatches, so a new
//! subcommand is one module and one match arm.
//!
//! Everything here goes through `xarast-app`'s public headless API
//! (`Session`, `headless::render`); the CLI owns argument parsing, framing
//! and reporting, never rendering.

pub mod args;
pub mod bench;
pub mod convert;
pub mod export;
pub mod fixtures;
pub mod inputs;
pub mod inspect;
pub mod render;
pub mod repair;
pub mod smoke;

/// The process exit codes every subcommand shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Exit {
    /// Everything worked.
    Ok = 0,
    /// The command line was wrong.
    Usage = 1,
    /// At least one input could not be read or imported.
    Import = 2,
    /// At least one input imported but could not be rendered or written.
    Render = 3,
}

impl Exit {
    /// The numeric code.
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }
}

/// The top-level usage text.
pub const USAGE: &str = "\
xarast-cli — headless Xarast tools

USAGE:
    xarast-cli <SUBCOMMAND> [ARGS]
    xarast-cli <SUBCOMMAND> --help

SUBCOMMANDS
    bench        run one performance scenario and print JSON (cargo xtask perf)
    convert      convert .xar documents to .xarast packages
    export       export documents to PNG, JPEG, WebP, PDF or SVG
    fixtures     export the built-in test documents (CI's export checks)
    inspect      report what a document holds (--fills: every fill)
    render       render documents to PNG on the CPU backend
    repair       rebuild a damaged .xarast package (local-header scan)
    smoke-open   import documents, walk the scene and report what is missing
    version      print the version

EXIT CODES
    0  success
    1  bad command line
    2  an input could not be read or imported
    3  an input imported but could not be rendered or written
";

/// Runs the tool over `argv` (without the program name) and returns the
/// exit code.
#[must_use]
pub fn run(argv: &[String]) -> Exit {
    let Some((cmd, rest)) = argv.split_first() else {
        eprint!("{USAGE}");
        return Exit::Usage;
    };
    match cmd.as_str() {
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Exit::Ok
        }
        "-V" | "--version" | "version" => {
            println!("xarast-cli {}", env!("CARGO_PKG_VERSION"));
            Exit::Ok
        }
        "bench" => dispatch(rest, bench::USAGE, bench::parse, bench::run),
        "convert" => dispatch(rest, convert::USAGE, convert::parse, convert::run),
        "export" => dispatch(rest, export::USAGE, export::parse, export::run),
        "fixtures" => dispatch(rest, fixtures::USAGE, fixtures::parse, fixtures::run),
        "inspect" => dispatch(rest, inspect::USAGE, inspect::parse, inspect::run),
        "render" => dispatch(rest, render::USAGE, render::parse, render::run),
        "repair" => dispatch(rest, repair::USAGE, repair::parse, repair::run),
        "smoke-open" => dispatch(rest, smoke::USAGE, smoke::parse, smoke::run),
        other => {
            eprintln!("xarast-cli: unknown subcommand `{other}`\n");
            eprint!("{USAGE}");
            Exit::Usage
        }
    }
}

fn dispatch<A>(
    argv: &[String],
    usage: &str,
    parse: fn(&[String]) -> Result<A, String>,
    run: fn(&A) -> Exit,
) -> Exit {
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        print!("{usage}");
        return Exit::Ok;
    }
    match parse(argv) {
        Ok(a) => run(&a),
        Err(e) => {
            eprintln!("xarast-cli: {e}\n");
            eprint!("{usage}");
            Exit::Usage
        }
    }
}
