//! Measures the operation journal (F6.6) on real files: what one command
//! costs the interface thread with the journal on and off, what the
//! journal's base costs the autosave thread, how big the files get, and
//! how long a recovery takes.
//!
//! ```text
//! cargo run --release -p xarast-app --example journal_probe -- FILE.xar|FILE.xarast… SCRATCH_DIR
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use xarast_app::autosave::AutosavePolicy;
use xarast_app::{AppState, EditCommand, Intent};
use xarast_geom::{Mp, Vector};

const NUDGES: u32 = 2_000;

fn size(p: &Path) -> u64 {
    std::fs::metadata(p).map_or(0, |m| m.len())
}

fn dir_size(p: &Path) -> u64 {
    std::fs::read_dir(p)
        .map(|r| r.flatten().map(|e| size(&e.path())).sum())
        .unwrap_or(0)
}

/// `NUDGES` one-object moves, each its own undo step; the mean time per
/// command on this thread.
fn nudges(app: &mut AppState) -> Duration {
    let s = app.active_mut().expect("open");
    let Some(n) = xarast_app::edit::selectable_objects(&s.doc).next() else {
        return Duration::ZERO;
    };
    let t = Instant::now();
    for i in 0..NUDGES {
        let d = if i % 2 == 0 { 100 } else { -100 };
        s.apply_edit(EditCommand::translate(
            vec![n],
            Vector::new(Mp::new(d), Mp::new(0)),
        ))
        .expect("moves");
    }
    t.elapsed() / NUDGES
}

/// One move of every object.
fn move_all(app: &mut AppState) -> Duration {
    let s = app.active_mut().expect("open");
    let all: Vec<_> = xarast_app::edit::selectable_objects(&s.doc).collect();
    let t = Instant::now();
    s.apply_edit(EditCommand::translate(
        all,
        Vector::new(Mp::new(1_000), Mp::new(0)),
    ))
    .expect("moves");
    t.elapsed()
}

fn main() {
    let mut args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let Some(scratch) = args.pop().filter(|_| !args.is_empty()) else {
        eprintln!("usage: journal_probe FILE… SCRATCH_DIR");
        std::process::exit(2);
    };
    for file in args {
        let store = scratch.join("autosave");
        let _ = std::fs::remove_dir_all(&store);
        let policy = AutosavePolicy {
            interval: Duration::from_secs(3600),
            idle: Duration::ZERO,
        };
        let mut app = AppState::new().with_autosave(store.clone(), policy);
        app.apply(Intent::OpenFile(file.clone())).expect("opens");
        let name = file.file_name().unwrap_or_default().to_string_lossy();
        let nodes = app.active().map_or(0, |s| s.doc.tree.node_count());
        println!("{name}: {nodes} nodes");

        // Journal off: no base yet, so nothing records.
        let off = nudges(&mut app);
        let all_off = move_all(&mut app);
        // What the base alone costs the autosave thread.
        {
            let s = app.active().expect("open");
            let snap = s.doc.snapshot();
            let dir = scratch.join("base-only");
            let t = Instant::now();
            xarast_app::journal::write_base(
                &dir,
                &snap,
                0,
                "probe",
                &xarast_app::journal::Blobs::new(&dir),
            )
            .expect("base");
            println!(
                "  base alone: {:.0} ms on the autosave thread",
                t.elapsed().as_secs_f64() * 1e3
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
        // The first base: an autosave taken at once.
        let t = Instant::now();
        app.tick(Instant::now());
        let ui = t.elapsed();
        let t = Instant::now();
        app.wait_for_saves();
        let job = t.elapsed();
        let entry = std::fs::read_dir(&store)
            .expect("an entry")
            .flatten()
            .next()
            .expect("an entry")
            .path();
        println!(
            "  base: interface thread {:.1} ms, autosave job {:.0} ms; base {} KiB, snapshot {} KiB, blobs {} KiB",
            ui.as_secs_f64() * 1e3,
            job.as_secs_f64() * 1e3,
            size(&entry.join(xarast_app::journal::BASE)) / 1024,
            size(&entry.join(xarast_app::autosave::SNAPSHOT)) / 1024,
            dir_size(&entry.join(xarast_app::journal::BLOBS)) / 1024,
        );
        let on = nudges(&mut app);
        let t = Instant::now();
        app.sync_journals();
        let drain = t.elapsed();
        let journal = size(&entry.join(xarast_app::journal::JOURNAL));
        println!(
            "  per command: {:.1} µs off, {:.1} µs on (+{:.1} µs); {NUDGES} entries, {} B each, writer drained {:.0} ms after the last",
            off.as_secs_f64() * 1e6,
            on.as_secs_f64() * 1e6,
            (on.as_secs_f64() - off.as_secs_f64()) * 1e6,
            journal / u64::from(NUDGES),
            drain.as_secs_f64() * 1e3,
        );

        // One command that touches everything: select all, move.
        let before = size(&entry.join(xarast_app::journal::JOURNAL));
        let big = move_all(&mut app);
        let t = Instant::now();
        app.sync_journals();
        let drain = t.elapsed();
        println!(
            "  move everything: {:.1} ms off, {:.1} ms on, entry {} KiB, written {:.0} ms",
            all_off.as_secs_f64() * 1e3,
            big.as_secs_f64() * 1e3,
            size(&entry.join(xarast_app::journal::JOURNAL)).saturating_sub(before) / 1024,
            drain.as_secs_f64() * 1e3,
        );

        let t = Instant::now();
        let rec = xarast_app::journal::recover(&entry).expect("recovers");
        let took = t.elapsed();
        let live = app.active().expect("open").doc.canonical_digest();
        println!(
            "  recovery: {} entries replayed in {:.0} ms, digest {}",
            rec.replayed,
            took.as_secs_f64() * 1e3,
            if rec.doc.canonical_digest() == live {
                "equal"
            } else {
                "DIFFERENT"
            }
        );
        drop(app);
    }
}
