//! Entry point for the Xarast desktop application.

use std::path::PathBuf;
use std::process::ExitCode;

use xarast_shell::probe::{Probe, ProbeKind};
use xarast_shell::{
    APP_ID, RendererPreference, ShellConfig, cold_start_ms, init_tracing, install_panic_hook,
    mark_process_start, platform_summary,
};

const USAGE: &str = "\
Xarast — a vector illustration and photo editor

USAGE:
    xarast [OPTIONS] [FILE.xar|FILE.xarast ...]

Opens FILE (of several, the last one that opens is shown); with none,
shows a start page with Open… and the recent files. File › Open… (Ctrl+O)
uses the desktop's file chooser; dropping a .xar on the window opens it
too. Ctrl+S saves as .xarast (Ctrl+Shift+S under a new name; a .xar is
never overwritten), Ctrl+W closes the document, Ctrl+Q quits — both ask
first about unsaved changes. On the canvas: wheel
scrolls, Shift+wheel scrolls sideways, Ctrl+wheel zooms about the
pointer, middle-drag pans, a trackpad pinch zooms; +/- zoom, 1 is 100 %,
0 or Home fits the page, d fits the drawing (the View menu lists them).

OPTIONS:
    -h, --help              Print this help and exit
    -V, --version           Print the version and exit
        --verbose           With --version, also print the platform summary
        --safe-mode         Start in safe mode: no GPU tiles (the canvas is composited
                            on the CPU), a software graphics adapter where one is
                            installed, default settings. Offered after a crash, and
                            forced after three crashes within ten minutes
        --selftest          Check that the build is internally consistent, without
                            opening a window, and exit
        --selftest-window   Open a window, present one frame, report the graphics
                            adapter and exit. This is the liveness check; run it
                            first when reporting a rendering problem. With no
                            display server it reports that and exits 0, because
                            the absence of a compositor is a fact, not a fault.
        --frames <N>        Exit after presenting N frames
        --screenshot <PNG>  Once the document has rendered, write the window's
                            content to PNG and exit
        --size <WxH>        Initial window size in logical pixels (default 1280x800)
        --synthetic <N>     Open a synthetic document of about N nodes (250000
                            gives about 100 000 paths), for the probes
        --probe <pan|zoom|drag|scale|rotate|rect|ellipse|nodes|pen|freehand|snap|arrange|paste>
                            Once the document has rendered, pan or zoom it;
                            move, scale or rotate the object nearest the centre
                            with the selector; draw a rectangle or an ellipse;
                            drag the path node nearest the centre with the
                            shape editor; drag out a pen point; or draw a
                            freehand stroke; by script, one step a frame;
                            print input-to-present latency and exit. Presents
                            without vsync
        --probe-samples <N> How many frames the probe measures (default 300)

ENVIRONMENT:
    XARAST_LOG              Log filter, e.g. `info`, `xarast_shell=debug`
    XDG_STATE_HOME          The recent files are kept in
                            $XDG_STATE_HOME/xarast/recent
                            (default ~/.local/state/xarast/recent),
                            unsaved work is autosaved to
                            $XDG_STATE_HOME/xarast/autosave/, and a crash
                            leaves a report in $XDG_STATE_HOME/xarast/crashes/
                            (no document content; never sent anywhere)
    XARAST_RENDERER         Canvas renderer: `auto` (default), `gpu` or `hybrid`
                            (GPU tile compositing) or `cpu` (CPU compositing;
                            the GPU only presents). The status bar shows the
                            tier in force
    WGPU_BACKEND            Force a wgpu backend, e.g. `vulkan`, `gl`
    WGPU_ADAPTER_NAME       Prefer the adapter whose name contains this, e.g.
                            `intel`, `nvidia`, `llvmpipe`
    WINIT_UNIX_BACKEND      Force `wayland` or `x11`; X11 loses fractional
                            scaling, trackpad gestures and tablet axes
    XARAST_INJECT_GPU_ERRORS
                            Diagnostic: raise a GPU validation error in each of
                            the first N frames, to exercise the recovery path
    XARAST_INJECT_DEVICE_LOSS
                            Diagnostic: after N frames, act as if the GPU device
                            were lost (the device itself is left alone), to
                            exercise the rebuild
    XARAST_FORCE_PANIC      Diagnostic: panic at `startup`, in a `ui` frame or on
                            the `render` thread, to exercise the crash report,
                            the emergency autosave and the next start's offer
";

fn main() -> ExitCode {
    mark_process_start();
    init_tracing();
    install_panic_hook();

    let mut config = ShellConfig::default();
    let mut selftest = false;
    let mut selftest_window = false;
    let mut version = false;
    let mut verbose = false;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut screenshot: Option<PathBuf> = None;
    let mut probe: Option<ProbeKind> = None;
    let mut probe_samples: usize = 300;
    let mut synthetic: Option<usize> = None;
    let mut safe_mode = false;
    config.renderer = RendererPreference::from_env();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => version = true,
            "--verbose" => verbose = true,
            "--safe-mode" => safe_mode = true,
            "--selftest" => selftest = true,
            "--selftest-window" => selftest_window = true,
            "--frames" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => config.exit_after_frames = Some(n),
                None => {
                    eprintln!("--frames needs a number");
                    return ExitCode::FAILURE;
                }
            },
            "--screenshot" => match args.next() {
                Some(p) => screenshot = Some(PathBuf::from(p)),
                None => {
                    eprintln!("--screenshot needs a file name");
                    return ExitCode::FAILURE;
                }
            },
            "--size" => match args.next().as_deref().and_then(parse_size) {
                Some(size) => config.size = size,
                None => {
                    eprintln!("--size needs WIDTHxHEIGHT");
                    return ExitCode::FAILURE;
                }
            },
            "--synthetic" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => synthetic = Some(n),
                None => {
                    eprintln!("--synthetic needs a node count");
                    return ExitCode::FAILURE;
                }
            },
            "--probe" => match args.next().as_deref().and_then(ProbeKind::parse) {
                Some(k) => probe = Some(k),
                None => {
                    eprintln!(
                        "--probe needs pan, zoom, drag, scale, rotate, rect, ellipse, \
                         nodes, pen, freehand, snap, arrange or paste"
                    );
                    return ExitCode::FAILURE;
                }
            },
            "--probe-samples" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => probe_samples = n,
                None => {
                    eprintln!("--probe-samples needs a number");
                    return ExitCode::FAILURE;
                }
            },
            other if other.starts_with('-') => {
                eprintln!("unknown argument: {other}\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
            file => files.push(PathBuf::from(file)),
        }
    }

    if version {
        println!("xarast {}", env!("CARGO_PKG_VERSION"));
        if verbose {
            println!("build: {}", xarast_app::crash::build_id());
            println!("platform: {}", platform_summary());
        }
        return ExitCode::SUCCESS;
    }

    if selftest {
        println!("xarast {}", env!("CARGO_PKG_VERSION"));
        println!("app id: {APP_ID}");
        println!("platform: {}", platform_summary());
        println!("selftest: ok");
        return ExitCode::SUCCESS;
    }

    if selftest_window {
        return match xarast_shell::selftest_window(&config) {
            Ok(report) => {
                println!("adapter: {report}");
                if let Some(ms) = cold_start_ms() {
                    println!("cold start: {ms:.1} ms");
                }
                println!("selftest-window: ok");
                ExitCode::SUCCESS
            }
            // No compositor is not a failing build. CI runs this check on
            // machines that have no display at all, and a red result there
            // would teach everyone to ignore it.
            Err(e) if e.is_missing_display() => {
                println!("selftest-window: skipped ({e})");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("selftest-window: {e}");
                ExitCode::FAILURE
            }
        };
    }

    xarast_app::crash::set_context("platform", platform_summary());
    xarast_app::crash::force_panic_point("startup");
    // A screenshot or a latency probe is a tool run, not a session: it
    // neither leaves a sentinel nor is told about an earlier crash.
    let session = (screenshot.is_none() && probe.is_none())
        .then(xarast_app::crash::state_dir)
        .flatten()
        .map(|dir| xarast_app::crash::begin_session(&dir, safe_mode, std::time::SystemTime::now()));
    let check = session.as_ref().map_or(
        xarast_app::crash::StartupCheck {
            crashed: 0,
            recent_crashes: 0,
            report: None,
            safe_mode: if safe_mode {
                xarast_app::crash::SafeMode::Requested
            } else {
                xarast_app::crash::SafeMode::Off
            },
        },
        |(check, _)| check.clone(),
    );
    if check.after_crash() {
        tracing::warn!(
            crashes = check.recent_crashes,
            safe_mode = ?check.safe_mode,
            "the previous session ended unexpectedly"
        );
    }
    if check.safe_mode.active() {
        config.safe_mode = true;
        config.renderer = RendererPreference::Cpu;
        tracing::warn!(why = ?check.safe_mode, "safe mode");
    }
    xarast_app::crash::set_context("safe_mode", format!("{:?}", check.safe_mode));
    let sentinel = session.and_then(|(_, s)| s);

    // System font enumeration (~40 ms) runs in the background from here, so
    // a document without text never waits for it and one with text rarely
    // does.
    xarast_app::fonts::prewarm();
    let mut viewer = xarast_shell::viewer::Viewer::new(files);
    if let Some(store) = xarast_app::recent::default_store_path() {
        viewer = viewer.with_recent_store(store);
    }
    // Autosave and recovery are for people editing: a screenshot or a
    // latency probe would only leave snapshots of synthetic work behind.
    if screenshot.is_none()
        && probe.is_none()
        && let Some(dir) = xarast_app::autosave::default_dir()
    {
        viewer = viewer.with_autosave(dir);
    }
    let signals = xarast_shell::signals::SignalWatch::install();
    let fatal = xarast_app::crash::FatalWatch::global();
    viewer = viewer
        .with_signals(signals.clone())
        .with_fatal_watch(fatal.clone())
        .with_startup_check(&check);
    if let Some(nodes) = synthetic {
        viewer = viewer.with_synthetic(nodes);
    }
    if let Some(path) = screenshot {
        viewer = viewer.with_screenshot(path);
    }
    if let Some(kind) = probe {
        config.probe = true;
        viewer = viewer.with_probe(Probe::new(kind, probe_samples));
    }
    let result = xarast_shell::run_app(config, viewer);
    // A worker thread died of a panic: the work is autosaved, the report
    // written, and the sentinel stays for the next start to find.
    if fatal.raised() {
        return ExitCode::from(xarast_app::crash::PANIC_EXIT_CODE);
    }
    // Anything else that returns here is a clean exit. (A panic on this
    // thread never gets here: it unwinds past, leaving the sentinel.)
    if let Some(s) = sentinel {
        s.finish();
    }
    match result {
        // Ended by a signal: say so the way a shell expects.
        Ok(()) => signals.signal().map_or(ExitCode::SUCCESS, |sig| {
            ExitCode::from(u8::try_from(128 + sig).unwrap_or(1))
        }),
        Err(e) => {
            eprintln!("xarast: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `1920x1080` → `(1920, 1080)`.
fn parse_size(v: &str) -> Option<(u32, u32)> {
    let (w, h) = v.split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}
