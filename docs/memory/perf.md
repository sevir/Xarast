# perf

Memory note for **performance**: budgets, the measurements behind them, and
the regressions to watch for.

## Current state

Phase 0 set the budgets in the phase documents. Phase 2 is the first phase to
measure against them at scale, with `criterion` over a synthetic 100 000-node
document (`xarast_doc::synth`).

**The budgets are pinned to the reference machine below** (story
XARA-US-0010, 2026-09-23). Its numbers are the ones that count, and they are
in the next two sections. The Phase 2, 4 and 5 sections further down were
measured on the old 4-vCPU development container with no GPU. They are kept
as history and explain the older decisions, but they are not gate verdicts.

**Since XARA-US-0061 (2026-09-24) the budgets are CI gates**: `cargo xtask
perf` over `xtask/perf-budgets.txt`, per push and nightly. See
[CI gates](#ci-gates-xara-us-0061) for what is gated, the margins and the
noise control.

## Reference machine

Every budget in the phase documents and in `docs/phases/00-roadmap.md` is
judged on this machine. If it changes, re-run everything in
[Reference-machine numbers](#reference-machine-numbers) before comparing.

| | |
|---|---|
| CPU | **Intel Core Ultra 9 285** (Arrow Lake). **8 P-cores** (CPUs 0–7, up to 5.6 GHz) + **16 E-cores** (CPUs 8–23, up to 4.7 GHz), 24 threads, no SMT. L2 40 MiB, L3 36 MiB. AVX2 + AVX-VNNI, **no AVX-512**. `intel_pstate`, `powersave` governor |
| RAM | 93.6 GiB |
| GPU, integrated | **Intel Graphics (Arrow Lake)**, Mesa 26.1.6 ANV, Vulkan 1.4.354 |
| GPU, discrete | **NVIDIA RTX 4000 SFF Ada Generation** (AD104GL, 20 GiB), driver 580.173.02, Vulkan 1.4.312, PCIe 16 GT/s |
| OS | Pop!_OS 24.04 LTS, kernel 7.1.5-76070105-generic |
| Compositor | COSMIC (`cosmic-comp` 0.1), Wayland (`wayland-1`) |
| Toolchain | rustc / cargo 1.98.1; `release` = `lto = "thin"`, `codegen-units = 1` |
| `wgpu` adapters | Intel iGPU and NVIDIA through Vulkan; `llvmpipe` also enumerates and is skipped |

**Two GPUs.** The phase gates and the pan/zoom budget are written against
an *integrated* GPU. That is the Intel adapter, and it is the one that
decides a GPU verdict. The NVIDIA numbers are recorded so that we know what
the software can do when the hardware is not the bottleneck.

**Hybrid cores.** "8 cores" in a gate means the 8 P-cores
(`taskset -c 0-7`). The E-cores are about 1.3× slower single-threaded on
this workload.

**Load during the 2026-09-23 round.** Other agents were fuzzing (3–4 cores,
often P-cores) and compiling throughout, with the load average between 5 and
34. Every figure below is the median of several runs, and a range means the
spread across runs. Runs taken at load ≥ 25 are discarded unless noted. The
integrated GPU is the most sensitive to this, because it shares power and
memory bandwidth with the busy cores.

## CI gates (XARA-US-0061)

**What.** `xarast-cli bench <scenario>` measures (headless, CPU only, one
JSON object per process); `cargo xtask perf [--tier pr|nightly]` runs the
scenarios of every gate in `xtask/perf-budgets.txt` and fails on a breach.
CI: job `perf` of `ci.yml` (pr tier, every push, ubuntu-24.04) and
`perf-nightly.yml` (nightly tier = every gate, 04:30 UTC, with the
corpus). Both upload the results JSON; that is the per-commit history
until a dashboard exists.

```text
cargo xtask perf                          # pr tier; builds xarast-cli first
XARAST_XAR_CORPUS=… cargo xtask perf --tier nightly --out perf/n.json
cargo xtask perf --only open-10k,undo-100k --cli target/release/xarast-cli
xarast-cli bench pan --nodes 250000 --iterations 200 --zoom 3   # one scenario
```

**Noise control** (CI runners are shared, slower and have 4 cores):

- Every time is a median (pan/zoom: p95/p99 of 120–200 frames) of 3–20
  runs. Start-up is 20 separate processes.
- **Calibration, not baselines per runner.** Before every scenario the
  xtask runs `bench calibrate` (a fixed sort + float chain, single-
  threaded `st` and over all cores `mt`, medians of 5) and multiplies
  the scenario's limit by `max(1, measured / reference)`, the reference
  being this machine quiet (16.5 ms / 7.5 ms). A slower or busier machine
  gets proportionally more room; a faster one never gets less. Multi-
  threaded scenarios scale by `mt`, so a 4-core runner gets ~4× on
  render/pan/export (generous: our scaling stops at ~3×).
- **Margins.** A limit is the product budget where the reference
  measurement is at most half of it; otherwise ~2× the reference for
  times, ~1.25× for memory, and more for times of a few ms. Memory is
  never scaled (it does not depend on speed).
- **Two strikes.** A failing scenario is measured again at once; only a
  breach both times fails. The report then prints e.g.
  `REGRESSION: open-10k median_ms = 77.11 > limit 34.38 (+124.3 % over the
  limit, scaled ×1.15 for this machine); reference machine 70.00 (+10.2 %);
  first run 59.85`.
- **Budgets vs. limits.** Each gate carries its phase budget too. Where
  the budget is known to be breached, the limit sits above it (catching
  regressions) and the note names the task; the report lists those as
  "known budget breach" without failing.
- Saves write to `/dev/shm` (`XARAST_BENCH_DIR` overrides): on this
  machine's md RAID an fsync added up to 5 s to a ProbeX16 save
  (6.2 s vs 1.1 s on tmpfs); the gates measure our code, not the disk.
- Peak RSS is `VmHWM`, reset through `/proc/self/clear_refs` after the
  set-up, so it is the measured part's (pan's is not reset: its budget
  is document + first frame + pan pass).

**Not an allocation count.** Phase 12 A1 wants `alloc_bytes` as a
low-noise signal. A counting `#[global_allocator]` is `unsafe`, and
`xarast-cli` is not one of the three crates allowed it (phase 0);
`dhat` as an opt-in feature is the way (XARA-T-0297). Valgrind was also
considered (instruction counts); not installed here, so unverifiable.

**The gates and the local numbers** (2026-09-24, release, this machine,
**load 6–14 falling from 100–230** after other agents' builds: indicative;
"ref" is the median of three nightly runs at load 13–55, which is what
the budgets file records; both tiers passed, 12/12 and 27/27):

| Gate | Tier | Metric | Limit | Budget | Ref | Last run | Scale |
|---|---|---|---|---|---|---|---|
| `startup` (20 processes, spawn → exit) | pr | median ms | 400 | 400 | 4.5 | 4.2 | st |
| `startup-frame` (entry → first frame) | pr | median ms | 25 | — | 3.5 | 3.2 | st |
| `startup-rss` | pr | MiB | 32 | — | 19.2 | 19.2 | — |
| `open-10k` (open + Final first paint) | pr | median ms | 140 | — | 70 | 62–80 | st |
| `open-10k-rss` | pr | MiB | 104 | — | 81 | 78–84 | — |
| `save-10k` (app save job) | pr | median ms | 110 | — | 55 | 53–59 | st |
| `render-10k` (whole-frame raster, `--whole`) | pr | median ms | 28 | — | 13.4 | 12.4–14.6 | mt |
| `render-10k-reuse` (pixel-reuse path, no raster) | pr | median ms | 5 | — | 2.1 | 2.0–3.2 | mt |
| `pan-10k` (Draft, scheduler) | pr | p95 ms | 16 | 16 | 1.0 | 0.86–1.0 | mt |
| `export-10k` (PNG 4000 × 5543) | pr | median ms | 3000 | 3000 | 240 | 239–273 | mt |
| `export-10k-rss` | pr | MiB | 552 | — | 440 | 433–446 | — |
| `undo-100k` (undo or redo via `Session::apply`) | pr | median ms/op | 1 | 1 | 0.17 | 0.17 | st |
| `edit-100k` (the move itself) | pr | median ms/op | 0.65 | — | 0.31 | 0.30–0.32 | st |
| `photo-slider-24mpx` (slider frame, render thread) | pr | median ms | 45 | 33 | 21 | 19–57 | mt |
| `photo-slider-24mpx-p95` | pr | p95 ms | 100 | — | 45 | 42–75 | mt |
| `photo-release-24mpx` (frame after the release) | pr | median ms | 45 | 33 | 21 | 25–34 | mt |
| `photo-release-walk-24mpx` (intent + walk at the release) | pr | max ms | 8 | — | 0.2 | 0.2 | st |
| `photo-converge-24mpx` (release → exact frame) | nightly | median ms | 700 | — | 320 | 314–366 | mt |
| `open-100k` | nightly | median ms | 1750 | — | 865 | 828 | st |
| `open-100k-rss` | nightly | MiB | 672 | — | 535 | 534 | — |
| `save-100k` | nightly | median ms | 1100 | — | 550 | 558 | st |
| `render-100k` (whole-frame raster, `--whole`) | nightly | median ms | 220 | — | 108 | 101–112 | mt |
| `render-100k-reuse` (pixel-reuse path, no raster) | nightly | median ms | 50 | — | 24.8 | 23.7 | mt |
| `pan-100k` (fit) | nightly | p95 / p99 ms | 16 / 24 | 16 / 24 | 4.8 / 6.3 | 4.4 / 4.5 | mt |
| `pan-100k-rss` | nightly | MiB | 1536 | 1536 | 285 | 285 | — |
| `pan-100k-zoomed` (3×, CPU tier) | nightly | p95 ms | 33 | 16 (GPU) | 16.5 | 20.5 | mt |
| `zoom-100k` | nightly | p95 ms | 16 | 16 | 2.0 | 2.4 | mt |
| `leak` (200 cycles, after warm-up) | nightly | growth % | 20 | 5 | 4.2 | 2.0–9.4 | — |
| `leak-first` (phase's definition) | nightly | growth % | 35 | 5 | 18.4 | 15–21 | — |
| `probex16-open` | nightly | median ms | 1500 | 500 | 760 | 752–1031 | st |
| `probex16-open-rss` | nightly | MiB | 640 | — | 508 | 508 | — |
| `probex16-save` (app save job, tmpfs) | nightly | median ms | 2200 | 1000 | 1100 | 1083–1281 | st |
| `spitfire-first` (first text document) | nightly | first ms | 200 | — | 100 | 100–108 | st |
| `render-feathers` (whole Final frames, effect cache warm; corpus) | pr | median ms | 12 | — | 3.5 | 3.3–4.0 | mt |
| `render-groucho2` (same) | nightly | median ms | 16 | — | 6.5 | 5.8–6.7 | mt |

"100k" is `--nodes 250000` (105 852 objects), "10k" `--nodes 25000`
(10 520). The pr tier takes ~20 s of scenarios here; the nightly ~45 s
(2.5 min at load 230).

**Reading it.**

- **`render` without `--whole` measures pixel reuse, not a raster**
  (found 2026-09-24, XARA-T-0314). The scenario gives every job a fresh
  scene epoch to defeat reuse, but since XARA-T-0221 the render thread
  diffs the scenes instead, finds no damage in an unchanged scene and
  reuses every pixel: after the first frame each "frame" is ~0.4–2 ms of
  plumbing. The old `render-10k` (2.1 ms) and `render-100k` (24.8 ms)
  were calibrated in that state, i.e. they were **reuse-path
  measurements**; they are kept as `render-10k-reuse` / `render-100k-reuse`
  (same limits) and `render-10k` / `render-100k` now run
  `render --whole`, which flips one pasteboard level between frames so
  the planner treats each as a full frame (XARA-T-0319). Re-baselined
  2026-10-15 on the dev box (24 threads, load 10-12 from two parallel
  builds, so noisy): median of 5 runs x 7 frames, 10k 12.4-14.6 ms
  (median 13.4), 100k 101-112 ms (median 108). Limits are ~2x the
  measurement (28 and 220 ms), per the margin policy, and scale with `mt`.
  The product budget (25 ms at 100k on 8 cores + iGPU) is not gated: the
  CPU tier is ~4x over it. Re-measure on a quiet reference machine and
  tighten if the numbers drop. The effect gates also use `--whole`.
- **Known breaches, gated on regression only:** ProbeX16 open-to-first-
  paint 0.75 s vs 500 ms (XARA-T-0009), ProbeX16 app save 1.08–1.28 s vs
  1 s (XARA-T-0215: the snapshot restore), the CPU tier's zoomed pan
  (the 16 ms budget is the integrated GPU's and is met there, T-0050),
  and the leak check against the phase's own definition (below).
- **Leak check (B5).** 200 open → walk → render → edit → undo → save →
  close cycles of a 10k-node document in one process: the resident set
  after close grows ~5 MiB over cycles 1–3 (thread arenas, pools, tables)
  and then wanders ±2 MiB with no clear trend (traced to 80 cycles; leaving
  out the walk, the render, the edit or the save moved the level by a few
  MiB but did not isolate a stage that climbs on its own). Against
  the first close that is 15–21 %, so the phase's "within 5 % of after the
  first close" fails on warm-up, not on a leak; the gate measures after
  the warm-up (tenth of the cycles) against the median of the last tenth,
  2–9 %. A per-document leak of even 1 % of the document per cycle would
  be > 50 % here. Tightening it is XARA-T-0296.
- **Undo through the session is 0.17 ms at 100k objects**, not the bus's
  0.3 µs: `Session::apply(Undo)` does the session's bookkeeping too.
  Inside budget; `edit-100k` (0.31 ms) would show XARA-T-0030's O(n)
  commit again.
- **Start-up here is headless**: spawn → first frame of an empty page from
  the render thread is 4 ms. The real window's ≈ 330 ms (surface, adapter,
  first present) needs a display and a GPU: XARA-T-0010, on the reference
  machine or a self-hosted runner (A5).
- **Photo slider frames are a gate, not a test** (2026-09-24). The
  33 ms slider-frame budget used to be a hard median assertion in
  `xarast-app/tests/photo_panel.rs`, which failed on the GitHub runners
  (slow, shared, no GPU) at d00e783. `xarast-cli bench photo` now
  measures it through the render thread — the intent, the walk with its
  proxy evaluation, the frame the window would wait for — and the test
  keeps only what is not a clock: the document is untouched mid-drag and
  every frame draws a proxy. Local runs at load 8–14: median 19–57 ms
  (the render of a 1200 × 800 picture dominates; the walk is 3–7 ms),
  so the refs are indicative. Rule: **no wall-clock assertion in
  `cargo test`**; a budget goes in `xtask/perf-budgets.txt`.
- **The photo release (XARA-T-0304).** Same scenario, the frame after
  the slider's release. Before: the release walk evaluated the 24 Mpx
  chain and built its pyramid, **223–373 ms** (release frame 247–482 ms,
  load 8–14). After: the walk registers a deferred image with the drag's
  last proxy as stand-in, **0.2 ms**; the release frame is **25–34 ms**
  at load 33–36 — a slider frame — and the render thread's helper makes
  the image and its pyramid, the exact frame arriving **314–366 ms**
  after the release (it was the release frame itself before, 247–482
  ms). The release frame is no longer an outlier, so it is gated like a
  slider frame (budget 33 ms), the walk alone at 8 ms (a regression to
  an evaluation on the walk shows as ~250 ms), and the convergence
  nightly. `cargo xtask perf --tier pr` at load 80–105 (2026-09-24):
  16/16 passed; slider median 14.6 ms, p95 16.9, release 15.6, release
  walk 0.11 ms; convergence 235 ms (nightly gate, run alone).
- **Font service (XARA-T-0288).** Spitfire's first open + paint is ~100 ms
  of which ~40 ms is waiting for fontconfig enumeration. The shell already
  starts it on a background thread before the window exists (`main.rs`,
  `fonts::prewarm`), and the window opens documents only once it has a
  surface (~300 ms later), so the app does not pay it. Prewarming in
  `xarast-cli` too was measured and dropped: Spitfire first open 45.9 →
  44.8 ms (medians of 5–7, load ~90), because the import it would overlap
  is only ~5 ms. `spitfire-first` keeps an eye on it.

## Reference-machine numbers

Measured 2026-09-23 (XARA-US-0010). Budgets that are breached are in bold.

### Budget table
| PNG export, A4 at 300 dpi (2480 × 3508), 10 000 objects (`xarast-io` bench `export_a4_300dpi_10k/png`) | ≤ 3 s | **127 ms** (109–141) | passes (2026-09-23, load ~10–15); section "Export" |
| JPEG export, same, q90 | ≤ 2.5 s | **132 ms** | passes |
| WebP lossless export, same | ≤ 4 s | **147 ms** | passes |
| Export dialog: size/DPI recompute per keystroke (`sizing_edit_keystroke`) | ≤ 100 µs | **23 ns** | passes |
| Peak RSS, A4 at 300 dpi PNG / JPEG (`Spitfire.xar`, whole process) | ≤ 300 MB | **197 MB / 124 MB** | passes |
| Peak RSS, 20 000 × 13 694 px PNG (streamed) | ≤ 1.5 GB (20k²) | **372 MB**, 6.1–6.4 s | passes (PNG streams; JPEG/WebP hold the image) |

| Budget | Target | Measured | Verdict |
|---|---|---|---|
| Undo of a single-node edit | ≤ 1 ms | 0.28 µs | passes |
| Redo of a single-node edit | ≤ 1 ms | 0.10 µs | passes |
| Dispatch (commit) of that edit | — | ~~1.04 ms~~ **101 ns** | fixed (XARA-T-0030), see below |
| `walk_render`, 100 000 nodes | ≤ 2.0 ms | 0.87–0.92 ms | passes |
| `preorder`, 100 000 nodes | ≤ 1.5 ms | 1.07–1.08 ms | passes (2.09 ms on the container) |
| Random `Tree::get` | ≤ 5 ns | 5.10–5.67 ns | at budget; the figure includes the bench's own index arithmetic |
| `AttrStack::push` + `pop_scope` | ≤ 20 ns | 12.1–12.9 ns | passes |
| `AttrResolver::resolve`, warm / cold | ≤ 30 ns / ≤ 2 µs | 18.9 ns / 0.87–0.92 µs | passes |
| `DocumentBuilder`, 100 000 nodes | ≤ 150 ms | ~~40–46 ms~~ 17.9 ms | passes (candidates recorded at `node()`, O(n) validate) |
| `Document::canonical_digest()` | ≤ 30 ms | 4.1–5.7 ms | passes |
| `Document::snapshot()` | ≤ 25 ms | ~~43–80 ms~~ **9.7 ms** | passes (dense map, not a HAMT) |
| `compute_bounds`, 100 000 nodes, cold | ≤ 12 ms | 7.6–8.1 ms | passes |
| W0 G1: `vello_cpu`, `bulk` 1080p, 1 thread | ≤ 120 ms | **159–164 ms** | **fails** |
| W0 G1: `vello_cpu`, `bulk` 1080p, 8 P-cores | ≤ 25 ms | **53–58 ms** | **fails** |
| W0 G2: `vello`, `bulk` 1080p, integrated GPU | ≤ 8 ms | **72–86 ms** | **fails, 9–11×** |
| W0 G2: same, discrete GPU (information only) | ≤ 8 ms | 7.8–8.0 ms | at the line |
| CPU full frame, production path, 1080p, 100 000 objects | ≤ 25 ms | 19.0 ms (was 44.7) | passes since 2026-09-23 (XARA-US-0016) |
| Incremental 64 × 64 dirty rect | ≤ 0.3 ms | 0.11 ms | passes |
| `DisplayList::build`, 100 000 commands | ≤ 3 ms | 1.9–2.0 ms (was 20.8) | passes since 2026-09-23 (XARA-US-0016) |
| `DisplayList::build`, 1920 × 40 pan strip, 100 000 ops | — | 0.26 ms (was ≈ 15–19 ms in the app bench) | XARA-T-0033 |
| Render the 1920 × 40 pan strip, 100 000 ops, interactive | — | 1.17 ms (was 9.4) | XARA-T-0034 |
| Ramp build, 2048 entries, 8 stops, with profile | ≤ 40 µs | 34.5 µs | passes (86.6 µs on the container) |
| Blend LUT set, 12 families | ≤ 15 ms | 0.28 ms | passes |
| `.xar` full import, `ProbeX16.xar` (7.4 MB) | ≤ 350 ms | ~~644 ms~~ **329–343 ms** | passes, thin margin (XARA-T-0031) |
| `.xar` full import, whole corpus (59 files, 12.3 MB) | ≤ 3 s | ~~0.87–0.89 s~~ 0.46–0.47 s | passes |
| Pan, 100k objects, CPU, Draft through the scheduler, fit page | ≤ 16 ms | ~~5.3–6.6 ms~~ 1.58 ms | passes (after the render-perf merge, 2026-09-23, load ~5) |
| Pan, 100k objects, CPU, Draft through the scheduler, zoomed 3× | ≤ 16 ms | ~~51–57 ms~~ **11.1 ms** | passes after XARA-T-0033/T-0034 (linear display list, document-space culling, column tiles); re-measured 2026-09-23 at load ~5 |
| Zoom (wheel notch), 100k objects, CPU, Draft through the scheduler | ≤ 16 ms | 1.4 ms | passes; the zoom-out border is left to the Final |
| Pan/zoom, 100k objects, integrated GPU, input → presented, live window | ≤ 16 ms | pan p99 **1.3 ms** (GPU idle p99 5.5 ms), zoom p99 **0.5 ms** (GPU idle 2.8 ms); canvas 1796 × 1338 | **passes** (XARA-T-0050/T-0008, 2026-09-23); table in "Pan and zoom end to end" |
| Pan/zoom, 100k objects, integrated GPU, 4K canvas, offscreen | ≤ 16 ms | pan p99 **9.6 ms**, zoom p99 **5.8 ms** (to GPU idle) | passes; the CPU tier at 4K is 35–41 ms (T-0052's upload cliff) |
| Present one CPU frame, 4K, integrated GPU (the pre-T-0050 full `write_texture`) | — | **23–27 ms** (1080p: 0.54–1.6 ms) | off the interactive path since XARA-T-0050; still paid by the CPU tier and by the Final at rest; XARA-T-0052 investigates the cliff |
| Save a 20 MB `.xarast`, container part (no SVG serialisation) | ≤ 1 s | ~~273 ms~~ **89 ms** in memory, ~~285~~ **98 ms** through `write_atomic` to disk (zlib-rs since XARA-T-0090) | passes; table in "`.xarast` container" |
| Save a `.xarast` end to end (SVG + meta + container, atomic), from `.xar`, real corpus | ≤ 1 s for 20 MB | ProbeX16 (518 k nodes, **52.0 MB SVG**, 9.7 MB package): ~~1.60 s~~ ~~0.79 s~~ **0.83 s** (SVG 490 ms + package 340 ms, medians of 7, after the round-trip twins); the text placer of XARA-T-0172 adds ≈ 30 ms (best of 4 interleaved under load avg 18–50: 0.95 s vs 0.92 s for the build before it); every other corpus file ≤ 150 ms | **passes** since round 3 (XARA-T-0090 + XARA-T-0101 + the path-writer fix, 2026-09-23); table in "`.xarast` save end to end" |
| Re-save a 300 MB photo `.xarast` with unchanged resources | ≤ 1 s | ~~104~~ **91 ms** | passes: raw copies, no rehash, no recompression |
| Open a 20 MB `.xarast`: signature + manifest + thumbnail | ≤ 15 ms | **0.046 ms** | passes (unchanged on zlib-rs) |
| BLAKE3 throughput, one core | ≥ 1 GB/s | **5.4–5.9 GiB/s** | passes |
| Shape + lay out a 1 000-glyph story, cold (no layout cache) | ≤ 8 ms | **0.22 ms** | passes (phase 9 round 1, pinned fonts); section "Text" |
| Shape + lay out a 10 000-glyph story, cold | ≤ 60 ms | **2.28 ms** | passes |
| Glyph outline extraction, cached | ≤ 500 ns | **24 ns** | passes |
| System font enumeration (fontconfig, 2 202 families here) | ≤ 300 ms, off the main thread | **≈ 40 ms** | passes (opt-in `system_fonts` test) |
| Open a 5 MB `.xar` to first paint | ≤ 500 ms | **752–1031 ms** headless (ProbeX16, 7.4 MB, open + Final 1920 × 1080; gate `probex16-open`, 2026-09-24) | **fails**, XARA-T-0009; gated on regression at 1.5 s |
| Cold start to window | ≤ 400 ms | headless part 4 ms (gate `startup`); the window ≈ 330–420 ms (below) | window: XARA-T-0010 |
| Undo/redo through `Session::apply`, 100k objects | ≤ 1 ms | **0.17 ms** (gate `undo-100k`) | passes |
| Export 4000 px wide PNG, 10k objects | ≤ 3 s | **240 ms** (gate `export-10k`) | passes |
| Peak RSS, 100k objects + first frame + pan pass | ≤ 1.5 GB | **285 MiB** (gate `pan-100k-rss`) | passes |
| Image probe (header only), 24 Mpx JPEG / PNG | ≤ 200 µs | **59 ns** / **390 ns** | passes (phase 10; JPEG probe no longer goes through `image`) |
| Decode a 24 Mpx baseline JPEG (7.5 MB, q90) | ≤ 400 ms | **184 ms** (181–190) | passes |
| Decode a 24 Mpx PNG (27.5 MB) | ≤ 600 ms | **227 ms** (209–239) | passes |
| Decode a 4K PNG (10.3 MB) | — | **50 ms** | information |
| BLAKE3 of 100 MB of decoded pixels | ≤ 200 ms | **29 ms** | passes |
| Hit test, 100k objects: topmost **precise** pick (`HitIndex` + `HitShape::hit`) | ≤ 2 ms | 5.5–13.8 µs | passes (XARA-T-0150, 2026-09-23, load 10–40) |
| Hit test, 100k objects, adversarial: 100k unfilled outlines all round the point | ≤ 2 ms | **17.7–21 ms** | **fails**; every candidate needs a precise rejection, no index can help (see "Picking") |
| Marquee over 100k objects, whole page, touch or enclose | ≤ 20 ms | 0.78–1.38 ms | passes |
| `HitIndex` insert + remove / move one object, 100k | — | 23–30 ns / 69–680 ns | the reason the grid beat a static BVH (8–23 ms rebuild) |
### Export (`xarast-io`, phase 11 round 1, 2026-09-23)

Release build, reference machine, load 10–16 (other agents compiling).
`xarast-cli export` over the 59-file corpus, one process per format, the
drawing area, transparent background (JPEG flattens onto white):

| Format | 96 dpi: wall / render / encode | 300 dpi: wall / render / encode | Bytes (96 / 300 dpi) | Peak RSS (96 / 300) |
|---|---|---|---|---|
| PNG | 3.7–3.8 s / 1.75 s / 0.58–0.69 s | 15.0–17.4 s / 11.7–12.1 s / 1.9–3.8 s | 9.5 MB / 52.6 MB | 511 / 749–766 MB |
| JPEG q90 4:2:0 | 3.4–3.5 s / 1.6 s / 0.39–0.46 s | 15.1–15.9 s / 11.5–11.9 s / 1.8–3.1 s | 3.7 MB / 20.4 MB | 499 / 601 MB |
| WebP lossless | 3.4–3.5 s / 1.7 s / 0.36–0.37 s | 15.4–15.6 s / 12.4–12.6 s / 1.6 s | 9.4 MB / 52.9 MB | 500 / 601–666 MB |

Open is ≈ 0.69 s and the scene walk ≈ 0.49 s of every run. Zero errors, and
**every file is byte-identical between the two runs** of each format at
both resolutions. The corpus RSS is dominated by the importer and the
largest files, not by the export (a single A4 at 300 dpi peaks at 197 MB).

- **Bands.** `export_band_lines` gives ~64 bands (≥ 16 rows, ≤ 1 MiB) and
  64 MiB strips. The `*GradFilledShapes*` files export at 96 dpi in
  0.36–0.9 s against 2.0–4.5 s through `xarast-cli render`, whose
  deterministic config still has three 1 MiB bands (XARA-T-0038).
- **PNG DEFLATE was the bottleneck** before `deflate::ChunkedZlib`: on
  `Spitfire` A4 at 300 dpi, level 1 encode took 254 ms, level 6 about
  1 040 ms and level 9 8.5 s against a 120 ms render. Parallel 1 MiB pieces
  plus parallel row filtering: A4 PNG 1.25 s → 0.35 s wall (encode
  180 ms); 20 000 × 13 694 px 24 s → 6.1 s (encode 19.8 s → 2.6 s). The
  pieces cost < 0.2 % in size (9 553 847 → 9 569 926 bytes).
- **Memory.** PNG streams strip by strip: the 274 Mpx export peaks at
  372 MB, most of it a 64 MiB strip, its converted and filtered copies, and
  the 32 MiB DEFLATE batch. JPEG and WebP hold the whole RGBA image.

**PDF (round 2, XARA-US-0059, 2026-09-23).** Release, same machine, other
agents compiling. `xarast-cli export --format pdf --background paper` over
the corpus, rasterised objects at 300 dpi, `BlendFidelity::Exact`: 59/59,
**121 MB**, open 0.64 s, scene 0.49 s, translate+rasterise **6.5 s**,
encode 0.47 s. The time and the size are the ladder's last step: the three
largest files are blend-heavy designs whose Stained Glass/Bleach objects
are rendered with their backdrop (`scope3 simple` 29 MB, `ProbeX16` 24 MB,
`Spitfire` 22 MB, `Watch4` 13 MB). Before graduated transparency became a
soft mask, `10000GradFilledShapes` rasterised 2 000 objects: 298 MB and
66 s for one file; after, 7.2 MB. Interning functions and shadings halved
the corpus (214 → 121 MB). Next steps are XARA-T-0232; the phase budget
rows for PDF are not benched yet.

### Images (`xarast-image`, phase 10)

`taskset -c 0-7 cargo bench -p xarast-image --bench decode`, 2026-09-23, at
load 25–28 (other agents fuzzing and compiling), so the decode figures are
pessimistic; an earlier run at load ~6 gave JPEG 194 ms at its minimum and
PNG 24 Mpx 279 ms. The inputs are synthesised in the bench (gradients +
noise, so the encoders produce realistic sizes). Output is premultiplied
RGBA8, conversion and orientation included. All single-threaded: none of
the decoders uses `rayon`.

- The first JPEG probe went through `image`'s decoder and took **487 µs**:
  it copies the whole input before reading the header. The own segment
  walk takes 59 ns. Counting scans (a linear walk of the entropy data,
  ≈ 3 ms on 7.5 MB) is done only in `decode`.
- The whole 59-file `.xar` corpus decodes in ≈ 120 ms (test profile); 99 ms
  of that is the eight JPEG8BPP records, mostly the palette snap (358 ms
  before its exact-key cache). The walker spreads a frame's decodes over
  the cores, so a file costs about its largest bitmap.
- **Size**: a stripped release binary that decodes through the façade is
  **1.26 MiB larger** than one that does not (1.58 MiB vs 0.32 MiB, thin
  LTO). Measured on the shipped binary once the walker linked it
  (XARA-T-0129, 2026-09-23): stripped `xarast` **21 443 032 → 21 978 840
  bytes (+523 KiB, +2.5 %)**, because `arboard` already brought `image` +
  `png` + `tiff`. Still no libwayland/GL/Vulkan in `ldd`.

#### Bitmaps in the walker (XARA-T-0129, 2026-09-23)

Release `xarast-cli smoke-open` over the corpus, first walk (which now
decodes), before → after. Only files with bitmaps change:

| File | Walk before | Walk after | What decodes |
|---|---|---|---|
| `leafgirl` | 0.6 ms | 37.9 ms | JPEG8BPP 649×430 + palette snap, PNG |
| `scope3 simple` | 6.2 ms | 27.2 ms | two JPEG8BPP, three PNG |
| `Spitfire` | 3.4 ms | 10.0 ms | two JPEG |
| `Groucho2` | 0.9 ms | 5.6 ms | JPEG8BPP, PNG |
| `TextJust`, `Rotated` | 0.0 ms | 5–7 ms | 652×1243 RGBA PNG |
| every other bitmap file | ≤ 3 ms | ≤ 3 ms | |

Superseded for Groucho2, which now imports five more bitmaps: see "First
walk of the bitmap files, re-measured" (XARA-T-0287) below.

The import of the 22 alpha PNGs now also decodes and re-encodes them
(`normalise_xar_png`): `Rotated` opens in 6.7 ms instead of 0.4 ms, every
other file < 5 ms; `the_corpus_imports_within_its_time_budget` still
passes. Real window (`xarast --screenshot`, GPU tiles, three runs each,
`cold_start_ms`): leafgirl 326–508 → 344–370 ms, Spitfire 344–420 →
326–355 ms, scope3 329–341 → 344–369 ms — within noise of the ≈ 330 ms
the shell costs with no document work at all. ProbeX16 (no bitmaps) is
unchanged at 1.2–1.5 s (XARA-T-0009).

#### Resampling (XARA-US-0052, 2026-09-24)

`cargo bench -p xarast-render --bench render -- images`: one 512 × 512
frame covered by one placed noise image, `CpuConfig::interactive()`,
pyramid prebuilt. Machine loaded by other agents' builds, so ±20 %.
Before = the tree at 17e5767 with the same bench.

| Case | Before | After | Why |
|---|---|---|---|
| aligned (1 texel/px), HighQuality | 3.86 ms | **0.83 ms** | sampler readied once per command (was a matrix inversion per pixel), integer texel arithmetic, opaque-replace write |
| Nearest, 3× magnified | 2.10 ms | 1.07 ms | readied once per command |
| Bilinear, 3× magnified | 3.83 ms | 1.57 ms | same |
| HighQuality, 3× magnified | 3.20 ms (bilinear) | 5.80 ms | Mitchell, 4 × 4 taps: 22 ns/px |
| HighQuality, ÷1.5 | 3.12 ms (aliasing) | 3.33 ms | widened tent in linear light, ≤ 4 × 4 taps |
| HighQuality, ÷2.67 | 4.31 ms (aliasing) | 2.65 ms | trilinear in linear light, 8 taps |
| pyramid, 2048² | — | 26 ms | once per image, single-threaded; since XARA-US-0053 on the walker's decode threads, not the render thread |

Single-threaded per output pixel (the harness, `tests/resampling.rs`,
release, noisy): magnification in encoded sRGB bilinear 59 ns, Mitchell
113, Catmull–Rom 114, Lanczos-3 313 (linear light costs 10–20 % more:
the encode is a binary search); the product's minification 55 ns;
kernels widened by the ratio 0.4–3.2 µs (why they are not the product's
minifier beyond 2×).

#### Pixel memory budget (XARA-US-0053, 2026-09-24)

**The proxy rule (final, as the phase asks to record it).** Each image's
proxy is one level of its mip pyramid; that level and every smaller one
are never evicted once built. It starts as the largest level whose long
edge is ≤ **256** texels (an image not yet drawn costs at most ~256 KiB
of resident proxy, plus its smaller levels: +⅓). Each time a primitive
*draws* the image and pins levels `lo..`, the proxy moves up to
`max(lo, cap)` if that is larger, `cap` being the largest level with a
long edge ≤ **2048**. So the proxy tracks the image's on-canvas size (the
level actually sampled, which comes from the device footprint — zoom
included), grows when the image is scaled up or zoomed into, never
shrinks, and never exceeds 2048 on the long edge (16 MiB). The phase's
"rounded up to a power of two" becomes "a pyramid level", which is the
image's own halving sequence. Consequences: an image ≤ 2048 on its long
edge that has been drawn at or above its own size keeps its base
resident for good (nothing to evict); a 24 Mpx photograph shown small
keeps a few hundred KiB; worst case, 200 photographs each zoomed into
once pin 200 × 16 MiB × 4⁄3 ≈ 4.2 GiB of proxies — the phase's 3.2 GB
figure plus the smaller levels — which is the case T10.5.6's stress test
(XARA-T-0282) must look at before this rule is called final for huge
documents.

**The limit.** Evictable bytes only (proxies are reported, not limited):
`XARAST_PIXEL_BUDGET_MB`, else ¼ of `MemTotal` clamped to 512 MiB – 4 GiB
(this machine: 4 GiB). Read at the first use of the global budget.

**Costs** (`cargo bench -p xarast-render --bench render -- images/`,
2048² = 16 MiB base, load ≈ 30–98 from other agents, so pessimistic;
spill numbers are page-cache hot, a cold disk read will be slower):

| Operation | Time |
|---|---|
| Build the whole pyramid (`prepare`), single-threaded | 25.6 ms |
| Evict with a spill write (base + level 1, 20 MiB freed, 16 MiB written) | 2.9 ms |
| Re-materialise the base from its spill file (then dropped again) | 1.24 ms |

**Where the pyramid time went.** Release `xarast-cli smoke-open`, first
walk, three runs with and two without (same load) the walker's
`prepare`: leafgirl 28.7 → 31.8 ms, Groucho2 34.9 → 40.4 ms, scope3
23.4 → 26.4 ms, Spitfire 8.6 → 12.8 ms (+3–5 ms on the walker's decode
threads, in parallel) — in exchange the render thread no longer builds
any pyramid on the first minified frame. (Groucho2's walk was many times
the XARA-T-0129 table's *without* `prepare` too. That was not the load:
see "First walk of the bitmap files, re-measured" below.)

#### First walk of the bitmap files, re-measured (XARA-T-0287, 2026-09-24)

Quiet machine (the only agent running; `uptime` 1-min load 1.1–5.8
throughout, 24 cores). Release `xarast-cli smoke-open` on Groucho2,
leafgirl, scope3 simple, Spitfire and ProbeX16 (no bitmaps, the
control) in one process, 9 runs (5 or 3 per bisect point), medians. The
probe scripts are not in the tree: a loop over that command that parses
the `open … ms, walk … ms` columns. Older commits were built from
`git archive <c> | tar -x -m` into a scratch tree with one shared
`CARGO_TARGET_DIR`.

| Walk, ms | 1fa7fef (XARA-T-0129 table) | 5e03b36 | + cell-grid snap |
|---|---|---|---|
| `Groucho2` | 3.1 | 40.3 | **18.3** |
| `leafgirl` | 27.8 | 30.5 | **12.5** |
| `scope3 simple` | 23.1 | 25.9 | **18.4** |
| `Spitfire` | 8.9 | 13.9 | 12.4 |
| `ProbeX16` (control) | 197 | 196 | 203 |

- **Groucho2's slowdown was real, and it is more work, not slower code.**
  Bisected over the first-parent merges to 6ee21df, whose b213986 keeps
  the definitions found inside a dropped atomic subtree: Groucho2 gained
  five bitmaps (four of them 700–900 px JPEG8BPP photographs, used by
  contone fills that had imported as "no colour"). The walker's first
  frame costs its slowest bitmap, here a 764 × 512 tag-71 photograph
  whose palette snap alone was ≈ 30 ms of a 34 ms decode (125 k cache
  misses × a 256-entry scan). The snap now searches only the colour's
  cell of a lazily built `16^3` grid (`image.md`, "The `.xar`
  wrappings"): that decode is 11.6 ms, and the walk is that plus the
  image's `prepare` (4.5 ms). Output unchanged: the 73 PNGs that
  `xarast-cli export <corpus root> --format png --dpi 150` writes are
  byte-identical before and after.
- The +2.5–4 ms on leafgirl, scope3 and Spitfire since the T-0129 table is
  `ImageRef::prepare` (US-0053), as measured above; not a regression.
- **Spitfire's open went from 4.5 to ≈ 46 ms** at 7f44eac (text stories
  in the walker, which bring the font service). It is a once-per-process
  cost paid by the first document with text (≈ 41 ms, mostly fontconfig):
  the same file opened again in the same process takes 3.8 ms. Not per
  document; left as it is.
- **`build_scene`'s re-decode** (XARA-T-0281) does not show here:
  `smoke-open` walks once with the session's own walker, which decodes
  once. It cost every export or thumbnail one decode pass per call; gone
  since the per-document decoded-image cache (next section).
- Render time is unchanged by the snap. `xarast-cli export … --format png`
  at 96 dpi, medians of 5, before → after, render ms: Groucho2 5.8 → 6.2,
  leafgirl 5.8 → 5.2, scope3 16.6 → 15.7, Spitfire 22.6 → 24.2, ProbeX16
  102 → 105 (noise); scene ms: 40.0 → 18.1, 31.1 → 12.7, 22.9 → 17.9,
  12.3 → 14.4, 233 → 231.

**Corpus under a tiny budget** (`xarast-app/tests/pixel_budget.rs`, 24
files × Final/Draft at 640 × 480, limit 0, everything above 1 × 1
evictable): 297 evictions freeing 197 MiB, 209 base re-materialisations,
69 level rebuilds, 88 spill writes (53 MiB), 0 lost, pixels identical.
Peak resident under that budget: 7.6 MiB spilling, 4.1 MiB re-decoding
(pins in flight only).

#### Export and thumbnails with the decoded-image cache (XARA-T-0281, 2026-09-24)

**Indicative numbers**: other agents were building and testing on the
machine throughout the day (1-min load 57–87 during the first runs); the
table is the one pair taken back to back when it had dropped to **3.7–3.9**
for both. Release, a scratch probe (not in the tree, like T-0287's) that
per file, in one process: opens the session, walks it once with the
session's own walker (`rebuild_scene`, as the window does before any
export), exports PNG at 96 dpi twice through `Registry::export` and
`SessionSource` (what `xarast-cli export` runs), then renders the
thumbnail twice (`thumbnail_png` before, `thumbnail_png_with(doc,
session.decoded_images())` after — what a save now does). Nine runs,
medians, ms. "Scene" is `ExportReport::scene_time` (the export's walk).

| File | Export scene 1st / 2nd, before | after | Export wall 1st / 2nd, before | after | Thumbnail 1st / 2nd, before | after |
|---|---|---|---|---|---|---|
| `Groucho2` | 17.4 / 17.6 | **1.5 / 1.2** | 48.4 / 36.0 | **22.8 / 20.1** | 23.6 / 23.2 | **7.0 / 6.8** |
| `leafgirl` | 11.8 / 12.1 | **0.8 / 0.6** | 33.1 / 32.9 | **20.7 / 19.7** | 15.4 / 15.1 | **3.8 / 3.5** |
| `scope3 simple` | 17.9 / 18.1 | **5.6 / 8.2** | 77.7 / 67.2 | **56.5 / 54.0** | 30.9 / 30.7 | **21.3 / 20.9** |

- Before, every export and every thumbnail decoded (and `prepare`d) every
  bitmap again: the export's scene time *was* the first walk's decode
  cost (compare T-0287's walk column), paid again on the second export.
  After, the first export already hits the session's walk, so there is
  no "first export" penalty either. Render times are unchanged (± noise:
  Groucho2 6.6 → 6.1, leafgirl 5.4 → 4.2, scope3 16.1 → 16.0 on the
  first export).
- What is left of scope3's export scene (5–8 ms) is the walk itself
  (text stories, 2 000+ objects), not bitmaps; ProbeX16-sized documents
  are dominated by it too.
- Under the heavy load the same pair gave scene 17.8 / 12.2 / 18.0 →
  1.5 / 0.9 / 8.1 and thumbnail 25.3 / 17.0 / 31.7 → 7.2 / 4.3 / 20.6
  (loads 60–80 vs 57): the saving is the decode, which is the part that
  does not scale with the other agents' load; export wall times swing by
  10× under load (PNG's parallel DEFLATE competes for cores) and mean
  nothing there.
- Session open and the first walk are unchanged (open 1.1–3.7 ms, walk
  12–18 ms both sides): the cache only removes repeats.

**Draft zoomed out, and the render thread.** No timing here: what changed
is *where* the time goes, which the tests count instead
(`xarast-render/tests/pixel_budget.rs`,
`xarast-app/tests/pixel_budget.rs`). A Draft at 1/8 of four evicted
1200 × 900 photographs used to read four spill files (≈ 4 MiB each,
≈ 0.3 ms each hot, or a decode each without spilling); it now reads none
under either policy, because it samples level 3, the proxy. Under
`Substitute` no frame reads a base on the render thread at all; the
helper's reads are the same count and cost as before, off the thread,
followed by a repaint of the images' damage only (leafgirl's corpus test:
one repaint frame, `FrameReuse::Repainted`).

### Document model

`cargo bench -p xarast-doc --bench doc`, two runs. The figures are in the
table above.

**The undo budget hid a regression in dispatch.** The old
`undo/single_node_edit` bench timed dispatch and undo together, and that
figure went from 0.27 µs on the container to 1.13 ms here. The undo itself is
0.28 µs. The whole millisecond is `Tx::commit` → `keep_one_active_layer`,
which walks the **whole tree** with `preorder` on every commit to find the
spreads. That makes every edit O(document size). The bench is now split into
`dispatch/single_node_edit` and `undo/single_node_edit`, so this cannot hide
again. **Fixed 2026-09-23 (XARA-T-0030):** the commit now checks only the
spreads the transaction touched. `dispatch/single_node_edit` went from
1.96 ms (load ~24) to **101 ns** (100.97–101.23 ns, load ~5). Undo is
61 ns and redo 102 ns in the same run.

Full `doc` bench after this round (2026-09-23, load 5): walk_render 0.86 ms, preorder 1.05 ms, get 5.1 ns, resolve warm/cold 20 ns / 0.87 µs, builder 17.9 ms, digest 3.4 ms, snapshot 9.0 ms, dispatch 107 ns, undo 61 ns, redo 103 ns, compute_bounds cold 8.2 ms.

**`snapshot()` fixed the same day.** The HAMT of `Arc<NodeData>` cost a
HAMT insert plus an allocation per node on top of the payload clone. A
side-by-side scratch measurement at load 19–25 split the cost as follows:
the payload clones alone 11–18 ms, into a `std` map 21–24 ms, into `imbl`
28–42 ms, and the old `snapshot()` 26–67 ms. The dense `SecondaryMap`
took 7–13 ms including the drop. `snapshot/100k` now measures
**9.7 ms** (9.63–9.83 ms, load 31).

### Render

`cargo bench -p xarast-render --bench render` (production path: our display
list and compositor over `vello_cpu`, `CpuConfig::interactive()`, light
`bulk` scene):

| | Before (2026-09-23 morning) | After (XARA-US-0016) |
|---|---|---|
| Full frame 1920 × 1080, 20 000 objects | 9.6 ms (8.7–10.1 re-measured) | **5.2 ms** |
| Full frame 1920 × 1080, 100 000 objects | **44.7 ms** (45–49 re-measured) | **19.0 ms** |
| Full frame 960 × 540, 20 000 objects | 22.7 ms (24–52, noisy) | **4.0 ms** |
| Incremental 64 × 64 dirty rect | 0.12–0.15 ms | 0.11 ms |
| `DisplayList::build`, 20 000 | 1.11 ms (55 ns/cmd) | **0.34 ms** (17 ns/cmd) |
| `DisplayList::build`, 100 000 | **19.5–20.8 ms (208 ns/cmd)** | **1.91 ms** (19 ns/cmd) |
| `strip/build_1920x40_100000` (new) | ≈ 15–19 ms (app bench) | **0.26 ms** |
| `strip/render_1920x40_100000` (new) | 9.4 ms | **1.17 ms** |
| Cache sweep, groups of 8 / 32 / 64 / 128 / 512 | 2.44 / 2.59 / 3.15 / 3.55 / 6.67 ms | not re-run |

Criterion medians. The machine was shared with other agents' builds
(load 4–15). Spreads within one run were under 3 %. Between runs at
different loads they reached 15 %, and the "before" full frames varied
more, so the ranges are quoted.

**Why `DisplayList::build` was superlinear (profiled 2026-09-23).** Each
`DrawCmd` was 384 bytes: it cloned the path, the paint, the stroke style
and the transparency. At 100 000 commands the vector was 38 MB, which is
above glibc's 32 MB mmap ceiling. So every build page-faulted a fresh
mapping, and dropping the list walked 200 000 cold `Arc` counters. At 20k
it fitted in the heap and in cache. Ablations on a 100k scene: a 40-byte
record per op, fresh vector, 3.1 ms; the same plus one `PathRef` clone,
5.7 ms; the real build, 21 ms. Rounding was the next cost:
`DeviceRect::enclosing` made eight libm `floor`/`ceil` calls per command,
because the baseline x86-64 target has no SSE4.1. That was 16 ns of the
remaining 26. The fix, in `render.md`: commands index into the scene's
shared ops, and the rounding is done with exact integer casts.

**Where the full frame went.** Single-threaded, 100k: 274 ms, of which
composite 102 ms, `fill_path` 54 ms, **flatten 47 ms** (of polygons, which
flattening reproduces unchanged) and `vello` render 30 ms. The frame is
parallel over bands, and 1 MiB bands are only 8 per 1080p frame. Fixes:
polylines skip flattening, fully covered opaque pixels are written
directly, the unused per-band binning is gone (2 ms), and interactive
bands are 512 KiB (16 per frame).

The W0 spike and the G1/G2 verdicts are in `docs/memory/render.md`.

### Viewport through the Draft → Final scheduler

`cargo bench -p xarast-app -- viewport` (XARA-US-0006, 2026-09-23). The
synthetic document at `SynthSpec { nodes: 250_000, .. }`: **105 852
objects, each filled and stroked, so 224 218 primitives**, 1920 × 1080,
CPU backend in its interactive configuration (all 24 threads). Each figure
is one canvas frame from the intent to the collected pixels: `apply`,
`Canvas::note`, `Canvas::pump`, the render thread, `take_latest`. Two
views: **fit** (the whole page; the drawing fills the height and the sides
are pasteboard) and **zoomed** (3× that, every pixel over the drawing, so
every exposed strip has ink: the worst case for reuse). Two runs at load
7–15 agree to within 3 %; a third taken while the load rose to 25 read
up to 60 % higher and is discarded.

| Frame | fit | zoomed | What the render thread did |
|---|---|---|---|
| `pan_draft`: 9 px right, 5 px up | **5.3–6.6 ms** | **51–57 ms** | scroll + two strips. Zoomed: 35 ms of `DisplayList::build` (two scans of all 224k ops) + 15 ms raster |
| `zoom_draft`: one notch in or out | **1.5–1.6 ms** | **1.5–1.6 ms** | nearest resample of the kept frame; backdrop in the uncovered border |
| `final_after_idle`: the upgrade | 284–287 ms (now 104 ms) | 340–363 ms (now 102 ms) | four full-height columns: ~105 ms of list builds + 170–235 ms raster |
| `full_draft`: no reuse, for reference | 231–235 ms | 262–270 ms | one full Draft frame |

**Verdict against ≤ 16 ms.** Zoom passes everywhere. Pan passes at fit
page and **fails by ~3.4× when the view is full of ink**. Reuse turns a
230–270 ms full Draft into 54 ms, but what is left is not in `xarast-app`:

- `DisplayList::build` visits every scene op for any dirty rect, 15–19 ms
  per strip at 224k primitives however few commands survive
  (XARA-T-0033). A diagonal pan has two strips.
- The CPU backend parallelises over horizontal bands, so a short, wide
  strip runs on one core (XARA-T-0034). Rasterising a zoom-out's border
  this way cost ~270 ms, which is why the Draft now leaves it to the Final.

The Final is off the interactive path, but new input waits for the column
in flight: about a quarter of 285–360 ms. It is 1.2–1.4× a full Draft
because four column builds cost more than one full build.

Also measured while building it (probe, not in the bench): a full scene
walk of this document is ~100 ms, and a walk culled to a 9 px strip is
30–130 ms, so neither re-walking at Draft quality nor walking strips is a
way round the list-build scan.

### `.xar` import

`XARAST_XAR_CORPUS=… cargo bench -p xarast-xar --bench import`. The median
is over 7 warm runs per file, with the bytes already in memory. Two runs, at
load 6 and 19, agree to 0.1 %.

| File | Bytes | Nodes | Parse | Full import |
|---|---|---|---|---|
| `testfiles/ProbeX16.xar` | 7 369 847 | 518 346 | 129–141 ms | **644 ms** |
| `Designs/Spitfire.xar` | 646 635 | 5 971 | 1.9–2.1 ms | 4.3–4.9 ms |
| `testfiles/20000GradFilledShapes50PCtransparent.xar` | 334 528 | 120 006 | 17.6–17.9 ms | 96–103 ms |
| `testfiles/20000GradFilledShapes.xar` | 331 360 | 80 006 | 11.2–13.0 ms | 57–59 ms |
| **All 59 files** | 12 252 824 | 830 533 | 180–200 ms | 870–890 ms |

After XARA-T-0031 (2026-09-23, load 8, three runs): `ProbeX16.xar`
**329–343 ms** (parse 120–147 ms), `20000GradFilledShapes50PCtransparent`
53 ms, `20000GradFilledShapes` 35 ms, and **all 59 files 458–473 ms**
(parse 168–201 ms). Where the time went, and what is left, is in
`docs/memory/xar-import.md` finding 13. This includes the new
quick-shape outline generation (~12 ms on ProbeX16's 34 672 shapes).

Per file: median 0.10 ms, p90 4.5–5.1 ms. The parse is about 20 % of an
import. The rest is mapping into the document model, at roughly 1.2 µs per
node, so that is where to look first. `ProbeX16` is the only corpus file
above 5 MB, and its import alone exceeds the 500 ms open-to-first-paint
budget.

### `.xarast` container

`cargo bench -p xarast-format --bench container`, pinned with
`taskset -c 4-7`, 2026-09-23. Criterion medians; the spread across
samples is under 0.5 %. Two runs: the first with DEFLATE on `zlib-rs`
(load ~4, two fuzzers running), the second on `miniz_oxide` (load ~8),
which is what ships — see "DEFLATE backend" below.

The 20 MB document is synthetic and generated by the bench: 12 MiB of
SVG-like path text plus eight 1 MiB incompressible "JPEG" resources, a
thumbnail, meta. It deflates to a 9.07 MB package, i.e. the SVG part
compresses about 12:1 — at the optimistic end of the 6–12:1 of
`research/06 §4.3`, so a real `document.svg` will cost somewhat more DEFLATE
time per byte. The SVG serialiser (W3) is not in these numbers.

| Bench | What it does | `miniz_oxide` (shipped) | `zlib-rs` |
|---|---|---|---|
| `save_20mb` | `PackageWriter` → `Vec`: BLAKE3 of meta/document/thumbnail, compression policy, DEFLATE level 6, manifest, ZIP | **273 ms** (73 MiB/s of input) | 92.1 ms (217 MiB/s) |
| `save_20mb_atomic` | the same through `write_atomic` to a file on the local disk: temporary, fsync, rename, directory fsync | **285 ms** | 102.4 ms |
| `open_20mb` | `XarastReader::open` (sniff, end record, central directory, manifest parse, consistency) + `thumbnail()` | **46.5 µs** | 47.9 µs |
| `resave_300mb_raw` | open a 310 MB package (30 × 10 MiB resources), `from_package`, `carry_from`, `finish_with_source` into a `Vec` | **104 ms** (1.50 GiB/s: a `memcpy` of compressed bytes) | 100.9 ms |
| `blake3_64mb` | `Digest::of`, single-threaded | 10.6 ms (5.88 GiB/s) | 11.5 ms (5.43 GiB/s) |

Where the save time goes (not profiled, inferred): the resources are
STORED and already hashed in the index, BLAKE3 of 12 MiB is ~2 ms, so the
rest is DEFLATE of the SVG — which is why the backend alone makes it 3×.

**Round 3 (XARA-T-0090, 2026-09-23): `zlib-rs` everywhere.** Same bench,
`taskset -c 4-7`, load ~2: `save_20mb` **89.4 ms** (89.0–89.9),
`save_20mb_atomic` **98.0 ms**, `open_20mb` 46.2 µs, `resave_300mb_raw`
91.4 ms, `blake3_64mb` 11.5 ms. The history below explains why it took
two rounds.

**`runtime_detection` matters for the `.xar` side.** `cargo bench -p
xarast-xar --bench import`, pinned, same session:

| Build | ProbeX16 parse / import | All 59 parse / import |
|---|---|---|
| `miniz_oxide` (before) | 112 / 293 ms | 156 / 416 ms |
| `zlib-rs`, default features off (scalar) | 176 / 356 ms | 261–273 / 529–530 ms |
| `zlib-rs` + `runtime_detection` (shipped) | 123 / **288 ms** | 189 / **419 ms** |

The scalar row breached ProbeX16's 350 ms import budget. The app itself
never saw it (`png` brings flate2's default features, detection
included, into its build); `xarast-xar`'s own bench and the fuzz targets
did. Inflating many small blocks is still a little slower on zlib-rs
(`20000GradFilledShapes` parse 15.6 vs 10.3 ms); the totals are at parity.

**DEFLATE backend (round 2 history).** `zip` was first wired to `zlib-rs`. flate2 picks one
backend for the whole build, so that also switched the `.xar` reader's
tests in workspace builds, and `crates/xarast-xar/tests/fuzz_seeds.rs`
failed because a committed seed (`fuzz_xar_import/two-blocks-streamed-record.bin`)
is compressed bytes. It would equally make a "deterministic" `.xarast`
differ between binaries that happen to enable different backends. The
workspace now has exactly one backend, `miniz_oxide`, and
`deterministic_bytes_are_pinned` fails if it moves. Going to `zlib-rs`
everywhere is a one-line change plus regenerating that one `.xar` seed; it
buys ~180 ms per 20 MB save and is filed as XARA-T-0090. Until the SVG
serialiser exists there is no pressure: 273 ms is a quarter of the budget.

### `.xarast` save end to end (W3, 2026-09-23)

`xarast-cli convert` in release over the 59-file corpus into a scratch
directory (never the repository): `.xar` import, `write_svg`,
`meta.xml`, `PackageWriter` (DEFLATE 6, `miniz_oxide`) through
`write_atomic` with its fsyncs. Development machine, no pinning, load
from other agents; single runs, so read ±10 %.

| Document | Nodes | `document.svg` | Package | Import | SVG | Package write |
|---|---|---|---|---|---|---|
| `ProbeX16.xar` | 518 346 | 56.2 MB | 9.46 MB | 316 ms | **600 ms** | **935 ms** |
| `20000GradFilledShapes50PCtransparent.xar` | 120 006 | 13.3 MB | 0.60 MB | 42 ms | 140 ms | 66 ms |
| `10000GradFilledShapes.xar` | 50 011 | 11.1 MB | 0.74 MB | 19 ms | 100 ms | 79 ms |
| `amurdove.xar` | 8 799 | 1.40 MB | 0.35 MB | 3.3 ms | 13.7 ms | 36.5 ms |
| `Spitfire.xar` (one JPEG) | 5 971 | 0.83 MB | 0.65 MB | 3.1 ms | 10.9 ms | 37.3 ms |
| all 59 files | — | 103 MB | 14.8 MB | 415 ms | 1 076 ms | 1 592 ms |

Criterion (`cargo bench -p xarast-format --bench container -- svg`),
synthetic 100 000-node document: `svg/write_100k` **34.2 ms**,
`svg/save_100k` (SVG + meta + container into a `Vec`) **114.9 ms**.

Reading it: the budget ("save a 20 MB `.xarast` ≤ 1 s") held for every
real file except ProbeX16, whose package is only 9.5 MB but whose SVG was
56 MB — ~108 bytes per node, every ink element carrying its full resolved
paint — and DEFLATE of those 56 MB with `miniz_oxide` is ~60 MB/s.
Serialisation itself was ~1.2 µs per node.

**Round 3 (2026-09-23): ProbeX16 passes.** `xarast-cli convert`, release,
`taskset -c 4-7`, load 2–5; ProbeX16 over 7 runs, the corpus over 3.

| | Baseline (round 2) | `zlib-rs` only | + passes 4–5 | + path writer on the stack (shipped) |
|---|---|---|---|---|
| ProbeX16 `document.svg` | 56.67 MB | 56.67 MB | 50.78 MB | 50.83 MB (+112 separators, the fix) |
| ProbeX16 package | 9.46 MB | 9.66 MB | 9.48 MB | 9.49 MB |
| ProbeX16 SVG serialisation | 637 ms (636–640) | 658 ms (628–744) | 602 ms (594–626) | **453 ms** (437–482) |
| ProbeX16 package write | 966 ms (964–978) | 355 ms (334–366) | 336 ms (325–339) | **337 ms** (328–351) |
| **ProbeX16 save (SVG + package)** | **1.60 s** | 1.01 s | 0.94 s | **0.79 s** |
| Corpus SVG / packages | 103.1 / 14.84 MB | 103.1 / 15.14 MB | 96.5 / 14.95 MB | 96.5 / 14.95 MB |
| Corpus serialisation + package | 1.06 + 1.61 s | 1.12 + 0.80 s | 1.08 + 0.79 s | **0.80 + 0.79 s** |

Where ProbeX16's serialisation went (timing probes, not committed; `perf`
is not usable on this machine): 561 ms walk = path data 250 ms, paint
115 ms (gradients: 30 k `<radialGradient>`s, each hashed into `<defs>`),
quick-shape sidecars 89 ms, element write 57 ms; plan 3 ms; final
assembly 23 ms. The path writer formatted every number of both spellings
of every segment into its own `String`; formatting into a stack buffer
took path data to ~134 ms and quick shapes to ~55 ms. What is left of the
~450 ms is spread thin (paint ~115 ms, element write ~55 ms, the rest the
walk); the package write is DEFLATE-bound (~150 MB/s at level 6).

Criterion, synthetic 100 000-node document, pinned: `svg/write_100k`
34.2 → **30.6 ms**, `svg/save_100k` 114.9 → **63.0 ms**.

### Text (phase 9, round 1, 2026-09-23)

`cargo bench -p xarast-text --bench layout`, pinned test fonts (never the
host's), load ≈ 8. Details and the layout rules in `docs/memory/text.md`.

| Bench | What | Median |
|---|---|---|
| `paragraph_1k` | 1 000 characters, one style, fully justified 400 pt column | 218.9 µs |
| `story_10k` | 10 000 characters, 20 paragraphs, 130 lines, 9 921 glyphs, Latin + Hebrew fallback, regular/bold alternating every 500 bytes, justified | 2.28 ms |
| `outline_cached` | one `FontDb::glyph_outline` hit | 24.1 ns |

"Cold" means no layout cache (T9.3.11 does not exist yet): every iteration
reshapes every character. Font data and harfrust's shaping plans are warm.
Most of the time is parley's itemisation and shaping; our own breaking,
justification and bidi reordering are a small share. The shaper holds the
`FontDb` lock while it works, so one database serialises layout across
threads.

**Binary size.** Measured on a stripped release probe (`lto = "thin"`,
`codegen-units = 1`) that enumerates, lays out and extracts outlines: the
text stack adds **≈ 1.8 MB** without the ICU4X line-break dictionaries and
**≈ 5.6 MB** with the default `complex-scripts` feature (≈ 3.8 MB of
dictionaries for Thai, Lao, Khmer and Myanmar). `xarast` declares
`xarast-text` through `xarast-app` but calls nothing yet, so today it grows by
1.2 KB (23 170 560 → 23 171 752 bytes, unstripped); the real cost lands
with the first call to `Shaper`.

**Binary size, shipped (round 2, 2026-09-23).** Stripped release `xarast`
(`lto = "thin"`), before the walker called `Shaper` (1fa7fef) → after:
**21 981 784 → 27 689 320 bytes (+5 707 536, +5.44 MiB, +26 %)** with the
default `complex-scripts`; **23 897 192 (+1 915 408, +1.83 MiB)** without
it. The ICU4X Thai/Lao/Khmer/Myanmar dictionaries are therefore
**3 792 128 bytes (3.62 MiB, 14 % of the binary)**. Recommendation:
**keep** `complex-scripts` — without it a Thai, Lao, Khmer or Burmese
column (no spaces between words) can only break by emergency, which is a
correctness loss in a text tool; if the AppImage size budget bites, load
the dictionaries as data at run time (ICU4X `DataProvider`) rather than
drop them. `ldd` still shows no libwayland, GL, EGL, Vulkan or
fontconfig (fontique dlopens it).

**Start-up (window, `cold_start_ms` to first presented frame, 3 runs,
release, this machine, GPU tiles).** Text-free `Designs/BLUECAR.xar`:
368–427 ms before, 349–421 ms after (noise). Text-heavy
`TextDesigns/Rotated.xar`: 383–390 → 393–403 ms (+≈10 ms: layout and
outlines of 7 stories; enumeration runs in the background from
`fonts::prewarm`). `Designs/GardenPlan.xar` (text + 1 700 objects):
373–403 → 384–399 ms. The 400 ms cold-start budget (XARA-T-0010) is
unchanged in its status: at the edge with or without text. Headless, the
first story of a process waits for enumeration when nothing prewarmed
(CLI: `AngledText.xar` first walk 48 ms, of which ≈ 40 ms fontconfig).

### Picking (phase 7 W3, 2026-09-23)

`crates/xarast-geom/benches/hit_index.rs`,
`taskset -c 4-7 cargo bench -p xarast-geom --bench hit_index`. 100 000
objects on an A3 page in four scenes: uniform, clustered, mixed sizes
(1 % page-sized), and all stacked on one point. The load average from
other agents was 10–40 throughout, so the ranges are across runs and
scenes, ±30 %. The full grid-versus-BVH table and the reasoning are in
`geometry.md`, "Picking". The phase document's budgets are 2 ms (pick)
and 20 ms (marquee); US-0031 also asked for 1 ms and 5 ms, and every row
that passes passes both.

| Operation, 100k objects | Grid (shipped) | Static BVH (bench only) |
|---|---|---|
| Build | 5.5–12 ms | 8.6–23 ms |
| Topmost pick, bounds only | 2.2–9.9 µs | 0.008–6.3 µs |
| Precise pick, top-down to the first real hit | 5.5–13.8 µs | — |
| Marquee, whole page | 0.78–1.38 ms | 5–37 µs |
| Insert + remove one object | 23–30 ns | 8–23 ms (rebuild) |
| Adversarial hollow stack, precise | 17.7–21 ms (was 120 ms before hull culling) | — |

**Round 4 (2026-09-23): the round-trip twins** (XARA-T-0108…0111: the
subtree under opaque nodes, palette references on key colours, complete
transparency twins, exact palette components). Same command, `taskset
-c 4-7`, load 5–12 (higher than round 3's 2–5), ProbeX16 over 7 runs,
the corpus over 3:

| | Round 3 | Round 4 |
|---|---|---|
| ProbeX16 `document.svg` / package | 50.83 / 9.49 MB | **52.05 / 9.65 MB** (+2.4 %: its opaque subtrees are written now, key colours name their palette colour) |
| ProbeX16 SVG serialisation | 453 ms | **490 ms** (482–505) |
| ProbeX16 package write | 337 ms | **340 ms** (332–345) |
| **ProbeX16 save (SVG + package)** | 0.79 s | **0.83 s** — within the 1 s budget |
| Corpus SVG / packages | 96.5 / 14.95 MB | 100.0 / 15.29 MB |
| Corpus serialisation + package | 0.80 + 0.79 s | 0.89–1.09 + 0.84–0.94 s (load) |

## Phase 2 — document model (development container, historical)

`cargo bench -p xarast-doc --bench doc -- --quick`

| Operation | Budget | Measured | Verdict |
|---|---|---|---|
| Undo of a single-node edit | ≤ 1 ms | 0.27 µs | well under |
| Redo of a single-node edit | ≤ 1 ms | 0.17 µs | well under |
| `walk_render` over 100 000 nodes | ≤ 2.0 ms | 2.01 ms | at budget (50 M nodes/s) |
| `preorder` over 100 000 nodes | ≤ 1.5 ms | 2.09 ms | 1.4× over |
| Random `Tree::get` by `NodeId` | ≤ 5 ns | 6.6 ns | 1.3× over |
| `AttrStack::push` + `pop_scope` | ≤ 20 ns | 17.5 ns | under |
| `AttrResolver::resolve`, warm | ≤ 30 ns | 30.4 ns | at budget |
| `AttrResolver::resolve`, cold | ≤ 2 µs | 1.08 µs | under |
| `DocumentBuilder`, 100 000 nodes | ≤ 150 ms | 47.4 ms | under |
| `Document::canonical_digest()` | ≤ 30 ms | 6.2 ms | under |
| `Document::snapshot()` | ≤ 25 ms | 58.5 ms | **2.3× over** |
| Resident bytes per node | ≤ 160 B | 221 B (incl. payloads) | not comparable |
| `compute_bounds`, 100 000 nodes, cold | ≤ 12 ms | 8.95 ms | under |

### The arena versus a persistent store

`cargo run --release -p xarast-doc --example arena_vs_persistent`

| M | Arena | `imbl` HAMT | |
|---|---|---|---|
| A full render traversal | 1.99 ms (50.3 M nodes/s) | 22.3 ms (4.5 M nodes/s) | HAMT 11.2× slower |
| B 10 M random lookups | 7.06 ns each | 103.3 ns each | HAMT 14.6× slower |
| C edit + undo | 0.21 µs | 1.52 µs | HAMT 7.2× slower |
| D resident bytes | 22.13 MB (221 B/node) | 30.89 MB (308 B/node) | HAMT 1.40× |

The arena wins against the pre-registered decision rule. Full reasoning in
[`document-model.md`](document-model.md).

## Phase 4 — render engine (development container, historical)

`cargo bench -p xarast-render`, on the same development container the W0 spike
used: **Intel Xeon @ 2.10 GHz, 4 vCPU, 15 GiB, no GPU**. The phase's budget
table assumes eight cores and an integrated GPU, so the overruns below are
partly the machine. `docs/memory/render.md` has the full spike table and the
gate verdicts.

| Budget | Target | Measured | Verdict |
|---|---|---|---|
| CPU full frame, 1920 × 1080, Final, 8 cores | ≤ 25 ms at 100 000 objects | **24.6 ms at 20 000 objects** on 4 cores | ~5× over, machine-adjusted ~1.5× over |
| CPU full frame, 960 × 540, 20 000 objects | — | 38.4 ms | slower than 1080p: four times the overdraw |
| Incremental 64 × 64 dirty rect | ≤ 0.3 ms | **0.258 ms** | **under** |
| Display-list build, warm scene | ≤ 3 ms at 100 000 nodes | 2.12 ms at 20 000 nodes (≈ 10.6 ms extrapolated) | **3.5× over**; `DrawCmd` is large |
| Ramp build, 2048 entries, 8 stops, with profile | ≤ 40 µs | 86.6 µs | 2.2× over (was 126 µs before the RGB fast path) |
| Ramp cache hit | — | 0.52 ns | noise |
| Blend LUT set, 12 families | ≤ 15 ms once at startup | **0.56 ms** | **27× under** |
| Resident LUT memory | ≤ 768 KiB | **768 KiB exactly** | at budget |
| GPU full frame | ≤ 8 ms | **unmeasured** | no adapter on this machine |

Cache-admission sweep, 512 × 512, one group of N primitives per frame — the
measurement behind the threshold of 64:

| Group size | 8 | 32 | 64 | 128 | 512 |
|---|---|---|---|---|---|
| Frame | 4.41 ms | 4.52 ms | 4.82 ms | 5.69 ms | 10.08 ms |

### What the spike says about where the time goes

Recording a scene into the rasteriser costs more than filling the pixels:
273 ms of a 442 ms 100 000-object frame. `vello_cpu` has no scene-reuse API,
so that cost recurs every frame, which is the argument for a per-node cache
that holds **pixels** rather than encodings.

A naive bounding-box scan over 100 000 objects costs 36.5 ms — forty times the
0.81 ms it takes to render a 64 × 64 dirty rect out of them. The display list
therefore carries precomputed bounds and the tile planner bins by them; an
incremental redraw that rediscovers its own geometry is not incremental.

## Phase 5 — user interface (`xarast-ui`, development container)

Measured by `cargo run -p xarast-ui --release --example density` and kept
honest by `cargo bench -p xarast-ui`. Same container as Phase 4: 4 slow
cores, **no GPU and no compositor**, so the GPU and presentation rows are
unmeasured rather than estimated. Full context in `docs/memory/ui.md`.

| Budget | Target | Measured | Note |
|---|---|---|---|
| `build_ui_frame()`, density probe (1,789 controls, 2560×1440) | ≤ 3 ms p50 | **1.63–1.65 ms p50, 2.19–3.40 ms p99** | passes; the p99 spread is this container under three concurrent builds |
| UI build + tessellation, same probe | ≤ 8 ms | **4.71–6.68 ms** (p99 + p99) | events and submit not included |
| egui GPU pass | ≤ 1.5 ms | **unmeasured** | needs an adapter |
| 5,000-row virtualised tree, 600 scrolled frames | no frame over 16 ms | **worst 2.84–2.99 ms** (CPU only) | presentation unmeasured |
| One slider dragged vs. idle frame | within 1 ms | **+0.00 to +0.05 ms** | partial update is a non-issue |
| Resident growth, all panels open | ≤ 50 MB | **9.3 MB** | |
| Frame build at 1× / 1.25× / 1.5× | no scale cliff | **1.77 / 1.79 / 1.78 ms** | rows are 20/25/30 device px |
| AccessKit tree, full probe | tree, fields, toggles present | **2,415 nodes, 1,752 labelled** | list roles are published by us, not by egui |

## Corpus render at 100 % (`xarast-cli render`)

```text
cargo build --release -p xarast-cli
xarast-cli render $XARAST_XAR_CORPUS/{testfiles,Designs,Templates,TextDesigns} \
    --out-dir <scratch>          # never into the repository
```

These are the 59 corpus files on the deterministic CPU config, `Final` quality,
96 dpi, each framed on its own drawing. Measured 2026-09-23 on a different
machine from the Phase 4/5 rows: **Intel Core Ultra 9 285, 24 threads, 93 GiB,
no GPU**. Another agent was building on the same machine at the time, so treat
the numbers as ±20 %.

| Measure | Value |
|---|---|
| Wall time, whole run (one process) | **29.2 s**, peak RSS 505 MiB |
| Open (read + import), all 59 files | 0.94–0.98 s; slowest is `ProbeX16` at 0.65 s |
| Render, all 59 files | 26.9–33.4 s |
| PNG encode + write, all 59 files | 0.69–0.79 s |
| Render per file, median | **≈ 0.7 ms** |
| Render per file, p90 | ≈ 135–180 ms |
| Render, the three `*GradFilledShapes*` files | **9.8 / 10.2 / 10.9 s** (766×739 and 766×880 px, 10 000–20 000 gradient fills); **2.6 / 2.2 / 4.5 s** after XARA-T-0014 |
| Render, the other 56 files together | ≈ 2.5 s; next-slowest are `ProbeX16` 1.07 s and `SimpleSphere` 0.66 s |

Ink versus blank:

- **38** files produce scene primitives and **21** do not: the 8 empty
  templates and 13 text-only `TextDesigns`, since text is Phase 9.
- **35** files paint pixels and **24** are blank. The extra three
  (`RedStar`, `Test00`, `TestBitmapFill`) are quick shapes with zero-area
  bounds, which the display list culls (XARA-T-0013).

`smoke-open` over the same four directories reports "59 opened
(36 complete, 38 with primitives)". In total, 7 777 text nodes and 25 images
are still pending.

**The gradient files are the finding.** 10 000 gradient-filled shapes take
10 s at 0.57 Mpx. Phase 4 measured 20 000 *flat* fills at 24.6 ms at 1080p,
so gradients cost roughly 400× more per frame. `--quality draft` only brings
the first file down to 6.5 s. At 192×192 it takes 0.45 s, so the cost scales
with pixels × gradient objects. Tracked as XARA-T-0014.

**Why (profiled 2026-09-23): not per-object waste but pixels.** Each shape
covers about 14 000 px, against about 100 px for a `bulk` shape, so the
10k file composites **1.4 × 10⁸ pixels**. The "400×" compares unequal
pixel counts. Per pixel, sampling the paint and the graduated
transparency cost about 21 ns and the blend 9 ns. Each sample used to
invert the gradient's 3×3 mapping. The transparency also sat in document
space (a bug, see `render.md`), so these files had rendered with *flat*
transparency until then. Fixes: samplers ready the inverse, table and
image once per primitive. A linear gradient in an affine frame computes
only `u`. `rem_euclid` and `round` stop calling libm. Opaque sampled
pixels are written directly. The deterministic configuration uses every
core, which is byte-safe because bands merge by index. After:
**2.6 / 2.2 / 4.5 s**. What is left: about 30 ns per pixel across only
three bands, because a 766 px wide image at 1 MiB per band is three bands
tall. The next steps are row-wise (SIMD) paint and blend, and more bands
for export; see XARA-T-0038.

### GPU tile compositing (XARA-US-0011)

`cargo bench -p xarast-render --features gpu --bench tiles`, 2026-09-23,
four runs at load 5–25, medians of 50–300 frames; ranges span the runs.
`WGPU_ADAPTER_NAME=<substring>` selects one adapter. GPU rows are wall
clock to `poll(wait)`.

| | Intel Arrow Lake iGPU | RTX 4000 SFF Ada |
|---|---|---|
| empty submit + wait | 0.05–0.37 ms | 0.05–0.19 ms |
| upload a whole 1080p frame (today) | 0.54–1.6 ms | 0.61–0.89 ms |
| pan, 40 resident tiles | 0.47–1.4 ms | 0.08–0.21 ms |
| pan crossing a tile row (8 uploads + composite) | 0.71–2.2 ms | 0.27–0.51 ms |
| Draft zoom 1.3× / 0.5× | 0.37–1.8 / 0.71–2.0 ms | 0.08–0.15 / 0.15–0.47 ms |
| upload a whole 4K frame (today) | **23–27 ms** | 2.6–3.5 ms |
| 4K pan, 144 resident tiles | 1.5–3.0 ms | 0.30–0.54 ms |
| upload 64 MiB of A8 | 27–33 ms | 5.4–6.9 ms |

CPU side of the same bench: `scroll_surface` 1080p 0.19–0.73 ms;
`compose_cpu` 1080p 1.3–3.1 ms; one 256² tile of a 900k-op scene 5–10 ms
and a 1920 × 256 row 12–18 ms, of which the culled `DisplayList::build`
alone is 6.5–10 ms (linear in the scene's ops). The decision these feed
is in `render.md`, "The GPU decision".

**The iGPU's large-upload cliff.** 8 MB uploads in about 0.6 ms, 33 MB in
24 ms: four times the bytes, fifteen to fifty times the time. The RTX scales
linearly. Unexplained; XARA-T-0052.

### Pan and zoom end to end (XARA-T-0050, XARA-T-0008)

Two probes, 2026-09-23, load average 1–3.5, 300 samples each after 10
warm-up frames, one scripted step a frame: pan 23.4 × 7.7 px (fractional,
reversing every 60 steps), zoom 1.05× about the centre (ten in, ten out).
Documents: the synthetic 250 000-node document (~105 000 paths, filled and
stroked; `--synthetic 250000`) and `testfiles/ProbeX16.xar`.

**Live window** — `xarast --probe pan|zoom [--synthetic 250000 | file]`
on the COSMIC session, no vsync, the window closing itself. The sample is
from the intent being applied to `Queue::present` returning, and to the
GPU going idle after it (`device.poll(wait)`), which includes the
interface frame, the tile uploads of whatever the render thread delivered
and the composite. COSMIC tiles the window, so the canvas is 1796 × 1338
(2.4 MP, more than 1080p) whatever `--size` asks.

| p50 / p99 ms, input → presented (→ GPU idle) | synthetic pan | synthetic zoom | ProbeX16 pan | ProbeX16 zoom |
|---|---|---|---|---|
| Intel iGPU, GPU tiles | 0.50 / 1.31 (2.24 / 5.52) | 0.37 / 0.54 (2.13 / 2.75) | 0.37 / 1.06 (1.56 / 4.68) | 0.33 / 0.53 (2.05 / 8.72) |
| Intel iGPU, CPU tier | 3.50 / 4.15 (5.00 / 9.47) | 3.46 / 3.96 (5.13 / 10.66) | 3.36 / 4.27 (5.14 / 12.26) | 2.52 / 3.40 (3.58 / 10.07) |
| RTX 4000 Ada, GPU tiles | 0.44 / 3.19 (0.79 / 3.38) | 0.32 / 2.48 (0.69 / 2.49) | 0.31 / 2.64 (0.76 / 2.79) | 0.30 / 2.52 (0.69 / 2.52) |
| RTX 4000 Ada, CPU tier | 3.28 / 5.02 (3.74 / 5.30) | 2.37 / 3.86 (2.90 / 3.94) | 3.11 / 4.15 (3.63 / 4.76) | 2.36 / 3.20 (2.89 / 3.60) |

Worst single sample of the iGPU GPU-tiles runs: 21.8 ms to GPU idle (one,
synthetic pan); every p99 is inside 16 ms.

**Offscreen at any size** — `cargo run --release -p xarast-shell
--example canvas_probe -- --size WxH --tier gpu|cpu --kind pan|zoom`,
input at 120 Hz. Same session, scheduler, render thread, planner and tile
store, composited into a canvas-sized texture; no interface, no
swapchain. The sample is intent → GPU idle.

| p50 / p99 ms | 1080p pan | 1080p zoom | 4K pan | 4K zoom |
|---|---|---|---|---|
| Intel, GPU tiles, synthetic | 1.79 / 3.98 | 1.58 / 4.15 | 3.71 / 9.62 | 1.98 / 5.83 |
| Intel, GPU tiles, ProbeX16 | 1.94 / 5.45 | 1.59 / 3.16 | 3.44 / 7.69 | 2.86 / 6.92 |
| Intel, CPU tier, synthetic | 3.23 / 7.23 | 2.83 / 5.76 | **34.8 / 40.6** | **31.1 / 35.4** |
| RTX, GPU tiles, synthetic | 0.49 / 1.48 | 0.43 / 0.63 | 0.79 / 4.57 | 0.56 / 0.94 |
| RTX, CPU tier, synthetic | 2.90 / 4.38 | 2.66 / 3.19 | 12.7 / 17.8 | 8.79 / 11.3 |

Reading them:

- **The budget is met on the integrated GPU at 1080p and at 4K** through
  the GPU tile tier; XARA-T-0008's GPU half is closed with this.
- The CPU tier (`XARAST_RENDERER=cpu`, and the fallback) is fine at 1080p
  and fails at 4K on the iGPU: its whole-canvas upload is the 33 MB cliff
  of XARA-T-0052. That is the fallback's cost, not the default path's.
- A pan uploads about 320 KB a step (95 MB over 300 steps at 1080p): the
  exposed strips, plus the partial tiles at the trailing edge that cannot
  grow their valid area into a rectangle and start afresh.
- A zoom uploads nothing until the gesture ends: the render thread skips
  Draft zooms (`FrameJob::cpu_rescale = false`) and the GPU resamples the
  resident level. The Final that follows is a whole-frame upload, at rest
  (at 4K on the iGPU that single frame pays the T-0052 cliff once).
- Max outliers (15–25 ms offscreen at 4K, once per run) coincide with the
  first frames after a direction change, when the render thread's strips
  land in a burst; p99 stays inside budget.

### An edit repaints its damage (XARA-T-0221, 2026-09-24)

Before, every edit was a new scene epoch and so a full frame on the render
thread and a full re-upload of the canvas tiles. Now the render thread
diffs the new scene against the one on screen and rasterises only the
damaged rectangles (`app-core.md` decision 40, `render.md` "Edit damage").

**Measured over the corpus** (`xarast-shell/tests/edit_damage.rs`, all 59
files, 320 × 240, 8 random edits each: fill recolours, moves, deletions,
undo/redo, fill-tool drags with their preview in flight, a third of them
drawn Draft first): **817 frames, 512 repainted, 126 full** (59 first
frames, the rest edits damaging over half the view). The repainted frames
rasterised **1.57 M pixels against 39.3 M** for the same frames drawn whole:
**4.0 %, a 96 % saving**, and the tile planner uploaded exactly those 1.57 M
pixels. Every frame, and the canvas composited from the tiles, equals a
full render byte for byte.

**What the diff costs.** Linear in the scene's ops: a prefix/suffix walk
comparing ops (paths by `Arc` pointer first), plus hashing whatever differs.
It runs on the render thread, before the repaint; bounds are computed only
for unmatched leaves. Each repaint rectangle builds one culled display list
(the XARA-T-0033 scan, 1–2 ms at 100k objects), hence the cap of four.

**What the exactness cost.** Rasterising coverage from the primitive's own
left edge instead of the draw area's (`render.md` invariant 15) makes a
column, a tile or a strip re-rasterise the part of a wide primitive left of
it. Viewport bench, same machine, back to back under concurrent load:

| Frame | before | after |
|---|---|---|
| `fit/pan_draft` | 1.57 ms | 1.59 ms |
| `zoomed/pan_draft` | 10.4 ms | 11.4 ms (two runs) |
| `fit/final_after_idle` | 112.9 ms | 111.0 ms |
| `zoomed/final_after_idle` | 91.1 ms | 95.7–99.0 ms |

About +1 ms on a zoomed pan (still inside 16 ms) and +5–9 % on a Final at
rest; one-call renders (export, goldens) are unchanged.

**Memory.** The render thread keeps the scene of the frame on screen, and
the session double-buffers its own (`Session::spare_scene`): two scenes
are resident, where there was one plus a transient.

### Photo adjustments (XARA-US-0054, 2026-09-24)

Fused point-operation LUT (brightness + contrast + gamma) over a 24 Mpx
straight-RGBA image, single thread, test profile (opt-level 2), this
machine under the concurrent agents' load: **66 ms** (phase budget
≤ 200 ms with rayon; `xarast-image/tests/photo.rs` prints it). Not
parallelised: it is a table lookup per byte and already inside budget.
The walker evaluates once per (master, chain), cached in
`DecodedImages` for every other walker: the session's walker defers an
image of ≥ 1 Mi px to the render thread's helper (XARA-T-0304, numbers
under "CI gates"), any other walker evaluates on its walk. The ≤ 33 ms
slider-latency budget is met by the proxy preview of XARA-T-0301. The materialisation threshold of T10.6.6 (250 ms of
regeneration) is not measured yet (XARA-T-0302).

## Phase 13 — live effects: blur and feathers (XARA-US-0068, 2026-09-24)

`cargo bench -p xarast-render --bench render -- effects`, release, the
reference machine with other agents building:

| Bench | Budget (phase 13) | Measured |
|---|---|---:|
| `effects/disc_r20_1024` (disc blur, r = 20 px, 1024² alpha, CPU) | ≤ 12 ms | **5.9 ms** |
| `effects/gaussian_s10_1024` | — | 6.0 ms |
| `effects/erode_r20_1024` (a feather's erosion) | — | 5.7 ms |
| `effects/feather_40pt_frame_512` (one feathered square, whole frame) | — | 3.6 ms |

Corpus, `xarast-cli render`, 100 %, warm: Groucho2 **50 → 129 ms**
(63 feathers), feathers.xar 5 → 26 ms, Watch4 58 → 60 ms. An effect is
recomputed on every frame and repaint that reaches it — colour and
silhouette passes, an erosion and a blur — until the layer cache lands
(XARA-T-0314).

### The effect layer cache (XARA-T-0314, 2026-09-24)

Design in `render.md`, "The effect layer cache". `xarast-cli bench
render --whole` (whole `Final` frames of the same scene and view on the
render thread, 1920 × 1080, page fit, interactive configuration, 11
runs), release, this machine at **load 13–16**, before (e0d8abe) and
after run interleaved, two rounds each:

| Document | Before median | After median | Before first | After first | Peak RSS before → after |
|---|---:|---:|---:|---:|---:|
| Groucho2 (63 feathers) | 26.7 / 26.8 ms | **6.2 / 6.7 ms** | 32.8 / 35.4 ms | 35.3 / 31.6 ms | 56.5 → 55.4–56.1 MiB |
| feathers.xar (9 feathers) | 42.1 / 46.4 ms | **3.3 / 3.6 ms** | 41.7 / 48.2 ms | 41.2 / 53.4 ms | 40.2–40.9 → 46.9–47.9 MiB |

- **Warm frames: 4.2× (Groucho2), 12× (feathers).** What is left is the
  frame without the effects' offscreen passes: the four columns'
  display-list builds and bands, and the composite of each layer.
- **The first frame is unchanged** (within noise): it misses and stores.
  Nothing is cheaper when an effect's content or the view changes: a
  slider on a feathered object, a zoom, a pan's strips (render.md, "Pans
  are not served from it") recompute as before.
- **Memory:** feathers.xar holds ~7 MiB of layers at 1080p (its feathers
  cover much of the page); Groucho2's is within RSS noise. The ceiling
  is 128 MiB.
- **One-shot renders are unchanged** (`xarast-cli render --zoom 100`: a
  fresh backend per file, so nothing to reuse; Groucho2 + feathers
  94.6 → 99.9 ms, noise), and the PNGs are byte-identical.
- **Gates:** `render-feathers` (pr, limit 12 ms, ref 3.5) and
  `render-groucho2` (nightly, limit 16 ms, ref 6.5), both on
  `--whole`; the pre-cache binary fails both (39.6 and 26.5 ms), so they
  catch a cache that stops hitting. They need the corpus; the pr job
  on GitHub has none and skips them.

**With shadows (XARA-T-0318 merged onto the cache, 2026-09-24).**
Groucho2 now carries 63 feathers **and 81 shadows**, all cached.
`xarast-cli bench render --whole --doc Groucho2.xar --runs 11`, release,
load ~15, two rounds: warm median **8.2 / 9.1 ms** (min 7.0), first frame
92.6 / 70.4 ms, peak RSS 61.1–61.9 MiB, walk 19.7 ms. Before the cache,
shadows alone put a one-shot Groucho2 frame at 108–136 ms (render.md,
"Shadows", cost); warm frames stay inside the `render-groucho2` nightly
limit (16 ms × calibration, ref 6.5). `perf --tier pr`: 17/17 passed
(`render-feathers` 3.92 ms against 13.59).

**With bevels (XARA-T-0322, 2026-10-15).** Groucho2 adds 21 bevels
(its frame, which was stripped before) under the cache. `bench render
--whole --doc Groucho2.xar --runs 11`, release, load ~9, same machine and
hour: cbdc3bd warm median 7.19 ms, first 73.1 ms, RSS 62.0 MiB; with
bevels warm **7.35 ms**, first 109.2 ms, RSS 77.1 MiB, walk 18.8 ms.
`render-groucho2` (nightly 16 ms) unaffected.

## Things that were slow, and why

Worth remembering, because each was a factor of several and each has a shape
that will recur:

- **`Document::snapshot()` on the edit path.** Checkpointing every 64
  transactions cost about **1.5 ms amortised per edit** and blew the 1 ms undo
  budget on its own. Automatic checkpointing is now off by default. A
  persistent snapshot is cheap to *hold* and expensive to *build*; build it
  incrementally, off the edit path.
- **Taking a resolved-attribute snapshot per node.** `ResolvedAttrs` is two
  `Arc` clones to copy but a fresh allocation to make, and a whole-document
  bounds pass made one per node: 53 ms for 100 000 nodes. Asking the
  `AttrStack` for just the stroke extent took it to 17.8 ms.
- **`HashMap` where a `SecondaryMap` belongs.** The same bounds pass went from
  17.8 ms to 8.95 ms by keying its scratch map on the slot index instead of
  hashing the `NodeId`. Anything keyed by `NodeId` over a whole document should
  be a `SecondaryMap`.

## Open TODOs

- [x] Establish a reference machine and pin the budget table to it — done
      2026-09-23 (XARA-US-0010); see [Reference machine](#reference-machine).
- [x] Re-measure `preorder` and `Tree::get` on it — `preorder` passes
      (1.07 ms), `Tree::get` is at budget (5.1 ns including the bench's own
      index arithmetic). It was the container.
- [x] Pan/zoom on the CPU through the scheduler: `cargo bench -p xarast-app
      -- viewport` (XARA-US-0006). Zoom passes, pan fails 3.4× when zoomed
      into the drawing; see the section above.
- [x] Pan/zoom on the integrated GPU (XARA-T-0008) — passes end to end
      through the GPU tile tier, 1080p and 4K (XARA-T-0050, see "Pan and
      zoom end to end").
- [ ] Open-to-first-paint (XARA-T-0009) and cold start (XARA-T-0010).
- [ ] `DisplayList::build` culled builds (XARA-T-0033) and single-core
      short strips (XARA-T-0034): the two things between pan and 16 ms.
- [ ] `Tx::commit` walks the whole tree on every edit
      (`keep_one_active_layer`): about 1 ms per command at 100k nodes.
- [ ] `.xar` import maps at about 1.2 µs per node: `ProbeX16.xar` takes 644 ms
      against 350 ms. Profile the model-mapping half.
- [x] `DisplayList::build` at 100k: 20.8 ms against 3 ms — now 1.9 ms
      (XARA-US-0016, 2026-09-23). The CPU full frame at 100k passes too
      (19 ms against 25).
- [ ] Gradient-heavy rendering: about 30 ns per composited pixel
      (XARA-T-0038). The three-band half is fixed for export (≈ 48 bands,
      0.36–0.9 s per file) but not for the headless `render` path.
- [ ] `Document::snapshot()`: 43–80 ms against 25 ms, still.
- [x] Wire the budgets into CI so that a regression fails the build —
      done 2026-09-24 (XARA-US-0061) with limits scaled by a calibration
      bench; see [CI gates](#ci-gates-xara-us-0061). Still open from phase
      12 A/B/C: a self-hosted perf runner (A5) for the window and GPU
      budgets, a history dashboard (A7), allocation counts (B1), render
      cache and undo byte ceilings (B3/B4), the 60-minute soak (B6), idle
      CPU, and the f64 cross-architecture determinism gate: XARA-T-0296
      (leak limits), T-0297 (B1), T-0298 (A5/A7/A8), T-0299 (B3/B4/B6,
      idle), T-0300 (cross-arch).
- [ ] Measure bytes per node excluding payloads, so the 160 B budget can
      actually be judged.
- [ ] Pixel budget under load: the 40 × 24 Mpx stress test (XARA-T-0282)
      and a cold-disk spill read; decide whether the 2048 proxy cap needs a
      global proxy ceiling for huge documents (see "Pixel memory budget").
