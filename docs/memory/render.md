# render

Memory note for the **render engine** (`crates/xarast-render`), Phase 4.

## Current state

`xarast-render` exists and renders. What is in it:

- **Scene → display list → bands → pixels.** `Scene` is retained and is built
  through `SceneBuilder`, which is the contract the `xarast-app` walker fills;
  the crate has no dependency on `xarast-doc` and never sees a node.
  `DisplayList` is immutable and per frame, with every transform resolved into
  device space, every paint *and transparency* mapping moved with its shape,
  and every command's device bounds precomputed. Since XARA-US-0016 a
  `DrawCmd` is 32 bytes of indices: the display list **shares the scene's
  op vector** (`Arc<Vec<SceneOp>>`) and keeps side tables for device
  transforms, device-space paints and transparencies, and image
  placements. `DisplayList::item` resolves a command to a `DrawItem` of
  references. A scene is re-recorded, never mutated in place, so an
  in-flight list keeps its own ops (`Scene::clear` swaps in a fresh vector
  when one is still shared).
- **The CPU backend, complete and deterministic.** It uses `vello_cpu` as a
  **coverage rasteriser only**: each primitive is rasterised as opaque white
  into a scratch pixmap the size of its clipped bounds, and its alpha is the
  antialiasing coverage. Paint evaluation and compositing are ours, which is
  what makes conical and diamond gradients, perspective mapping and Xara's
  twelve families possible at all.
- **Paints:** solid; all six gradient shapes (linear, radial, conical,
  diamond, 3-colour and 4-colour mesh) × four repeat modes × affine and
  perspective mappings (meshes honour the repeat mode since XARA-T-0256;
  the walker gives a tiled mesh `Mirror`, see "Meshes tile mirrored"); image fills with four repeat modes, three filters,
  contone and brightness/contrast/gamma/saturation adjustment. Images
  resample through one sampler (`resample.rs`, XARA-US-0052; minification
  in linear light, magnification in encoded sRGB):
  Mitchell–Netravali magnification, a widened tent between 1× and 2×
  minification, a lazily built mip pyramid with trilinear beyond, and an
  aligned fast path; bitmap transparencies use the same sampler and filter.
  See "Resampling quality".
- **Pixel budget:** every `ImageRef`'s base and pyramid levels are held
  under a `PixelBudget`; levels above the per-image proxy are evicted
  LRU and come back byte for byte from a reduction, a spill file or the
  walker's re-decode. See "Pixel memory budget".
- **Ramps:** multi-stop, RGB / HSV-short / HSV-long, 256- and 2048-entry
  tables, the Schlick bias/gain profile, the fixed-point transparency path
  with 22 fractional bits, and an interning cache.
- **Blends:** all twelve families, one 256 × 256 table each (768 KiB), one
  generator shared by both backends, plus the three analytic ones.
  Graduated, three-/four-colour (`TranspSource::Mesh`, per pixel) and
  bitmap-sourced transparency feed the same families.
  Every family × {flat, graduated, bitmap} has an exact golden, and the
  grey weights have one definition, `xarast_color::RENDER_GREY_WEIGHTS`
  (XARA-T-0347, "Transparency families").
- **Cache and incremental redraw:** content-hash keys, √2 scale quantisation,
  cost-weighted LRU under a hard byte budget, an admission policy, dirty-rect
  culling and `scroll_surface` reprojection. `scene_damage` diffs two scenes
  into the device rectangles an edit changed (XARA-T-0221, "Edit damage"),
  and any rectangle drawn over a frame is exactly that frame's pixels.
  Live effects' offscreen results are kept across renders by the
  backend's `EffectCache` (XARA-T-0314, "The effect layer cache"). The
  Phase 4 `RenderCache` (`cache.rs`) is still not used by any frame.
- **Validation:** a 130-scene generated feature corpus with committed goldens,
  the 160-scene fill shape × blend family × {flat, graduated} matrix
  (XARA-T-0222), exact CPU goldens, determinism over 20 runs, the gradient matrix, the blend
  domain, the precision rule (measured *and* grepped), AA level counts and a
  supersampled comparison, and `criterion` benches per budget row.

What is **not** in it, and who owns it:

| Missing | Owner |
|---|---|
| The WGSL compositing pass, ping-pong destination reads, GPU tile planner | **Deferred** by the GPU decision (XARA-US-0011); trigger in XARA-T-0051 |
| Presenting the canvas through `GpuTileCache` | built here (`gpu` feature); **wired** into the shell by XARA-T-0050 (`xarast-shell/src/tiles.rs`, see "The tiles in the viewer") |
| Shadow, bevel, contour, blend, mould | Phase 13 (XARA-US-0069..0071). The shared offscreen pipeline and blur exist and feathers draw through them (XARA-US-0068, "Live effects: the offscreen pipeline"). Shadows and bevels are pixel effects; **contours are geometry** (XARA-T-0330): plain fills and strokes the walker emits per ring, nothing in the renderer (`document-model.md` decision 50), and so are **blends** (XARA-T-0333): steps emitted per pair at its generated node (decision 51), and **moulds** (XARA-T-0334): the moulded copy emitted at the generated node, fills deformed by their control points (decision 52) |
| Fractal (plasma, clouds) generation | Done in phase 13 (XARA-T-0336) outside this crate: `xarast-image::procedural` generates the tile, the walker materialises it into a `Paint::Image` ("Clouds and plasma" below). `Paint::Fractal(ProceduralParams)` still refuses to rasterise: it is the parameter tuple, never what reaches a backend |
| Dither styles, sub-32 bpp output, CMYK separation, UCR/GCR | Deferred (`research/03 §3.9` M8) |
| Glyph rasterisation | Phase 9; this crate renders glyph outlines if handed paths |
| `.xar` corpus rendering end to end | `xarast-cli render` renders all 59 files (timings in `perf.md`); comparing against the original is Phase 11 |

---

## The W0 decision record

### The machine

**There was no reference machine when the phase opened, and there still is
not one that matters.** Everything below was measured on the development
container:

| | |
|---|---|
| CPU | Intel Xeon @ 2.10 GHz, **4 vCPU** |
| RAM | 15 GiB |
| GPU | **none** — no `/dev/dri`, no Vulkan ICD, no software rasteriser |
| OS / toolchain | Linux 6.18, rustc 1.94.1, `--release`, `lto = "thin"` |
| `wgpu` adapters | **0** (`enumerate_adapters` returns empty; `request_adapter` fails with "vulkan drivers/libraries could not be loaded") |

The phase's gates were written against a machine with eight cores and an
integrated GPU. This one has half the cores, slower ones, and no GPU at all.
Read every throughput verdict below as "on this machine", and re-measure
before treating G1 or G2 as settled.

*Update 2026-09-23:* there is a reference machine now
(`docs/memory/perf.md` §Reference machine), and G1 and G2 have been
re-measured and settled on it. See "Re-run on the reference machine" after
the decision. The quality gates G3–G7 below were not re-run: they do not
depend on the machine, apart from G4's SIMD-level caveat.

### The candidates, and what could be measured

| | A `vello_cpu` 0.2 | B `vello` 0.10 | C `vello_hybrid` 0.2 | D `tiny-skia` 0.12 (control) | E `lyon` + own pipeline |
|---|---|---|---|---|---|
| Measured? | **yes** | no adapter | no adapter | **yes** | not attempted |

B, C and E were not measured, and saying so is the honest result rather than
an omission. B and C cannot run without an adapter. E was budgeted as the
escape hatch for a G3 failure; G3 did not fail, so spending a week building a
tessellator to price a branch nobody is taking would have been waste. If the
vello family ever fails G3 on real hardware, that estimate still has to be
produced before the branch is taken.

### The numbers

The throughput half of the harness is now in the repository as
`benches/spike.rs` (see the re-run section). The quality half is not: it drives `tiny-skia` as a
*renderer* rather than as a test oracle, and it carries its own ground-truth
supersampler. Its five scenes survive in two places, which is what matters
for rebuilding it — the `edges`, `hairlines`, `seams` and `aa_*` cases of
`crates/xarast-render/src/corpus.rs`, and the `bulk` generator in
`crates/xarast-render/benches/render.rs`. The painter's-model reference it
established is `golden::downsample`.

**M1 — CPU throughput.** `bulk`: 100 000 filled paths averaging twelve
segments, 1920 × 1080, median of five frames including scene recording.
Two object-size variants, because the phase does not fix the size and the
overdraw factor dominates:

| Configuration | `bulk` r3..28 px (overdraw ×27) | `bulk` r2..8 px (overdraw ×2.6) |
|---|---|---|
| `vello_cpu`, 1 thread, u8 pipeline | **442 ms** (record 273 + raster 170) | **250 ms** (record 183 + raster 66) |
| `vello_cpu`, 4 threads | 194 ms (×2.28) | 91 ms (×2.74) |
| `vello_cpu`, 1 thread, f32 pipeline | 414 ms | 265 ms |
| `vello_cpu`, 1 thread, baseline SIMD | 516 ms | 323 ms |
| `tiny-skia`, 1 thread | 1 438 ms | 757 ms |
| `vello_cpu`, 4 threads, 3840 × 2160 | 187 ms | — |

Two things in that table matter more than the totals. **Recording dominates**:
273 of 442 ms is turning paths into strips, not filling pixels, and
`vello_cpu` has no scene-reuse API, so that cost is paid every frame. That is
the argument for the per-node cache holding *pixels*, which is what the phase
specifies. And **`tiny-skia` is 3.2× slower**, which settles it as a control
rather than a candidate.

**M2 — GPU throughput. Unmeasured on the container** (zero adapters).
Measured on the reference machine in the re-run below.

**M3 — incremental cost.** A 64 × 64 dirty rect of the 100 000-object scene,
with binning done once outside the timed loop:

| | ms |
|---|---|
| `vello_cpu` | **0.814** |
| `tiny-skia` | 3.75 |
| A naive 100 000-object bbox scan, for comparison | 36.5 |

The third row is the finding: the scan costs forty times the render. The
display list must carry precomputed bounds and a tile bin, which it now does.

**M4 — AA fidelity**, 512 × 512, mean and p99 ΔE₀₀:

| Reference | `vello_cpu` u8 | `vello_cpu` f32 | `tiny-skia` |
|---|---|---|---|
| `edges` vs 16×16 supersampling | 0.352 / 7.82 | 0.339 / 7.82 | 1.026 / 10.39 |
| `edges` vs 48×48 supersampling | 0.310 / 7.82 | 0.285 / 7.83 | 1.024 / 10.27 |
| `edges` vs a **painter's-model** 48×48 reference | **0.072 / 0.647** | 0.038 / 0.557 | 0.879 / 7.64 |
| `hairlines` vs painter's-model 48×48 | **0.046 / 0.723** | 0.022 / 0.569 | 0.225 / 3.92 |
| one non-overlapping 0.5° edge vs 16×16 | 0.002 / 0.000 | 0.002 / 0.000 | 0.009 / 0.000 |

**This is the most important row of the spike, and it is about the
*reference*, not the candidate.** Against a supersampled ground truth that
resolves sub-pixel *visibility* between overlapping shapes, p99 is 7.8 and
refining the ground truth from 16×16 to 48×48 does not move it — so it is not
quantisation. It is conflation between separately drawn overlapping shapes,
which **no painter's-algorithm compositor avoids**, CDraw included. Against a
reference that composites exact per-shape coverage in order — the thing a
painter's model can actually reach — the same renderer scores p99 0.65. The
phase's G3 threshold was therefore being applied to a property nothing has.
`golden::downsample` documents the reference the golden harness uses, and the
corrected G3 is stated below.

**M5 — coverage levels** on a 0.5° edge, 1024 × 256:

| | levels |
|---|---|
| `vello_cpu`, u8 and f32 | **256** |
| 16×16 supersampled reference | 143 |
| `tiny-skia` | 17 |
| CDraw normal / high quality (`research/03 §2.3`) | 85 / 132 |

256 against a bar of 132. `tiny-skia`'s 17 on a shallow edge is the clearest
single reason it is not the production rasteriser.

**M6 — conflation**, `seams`, 512 × 512, 400 shared-edge pairs, all one
colour, counting interior pixels that are not exactly the fill colour:

| Case | `vello_cpu` u8 | `vello_cpu` f32 | `tiny-skia` |
|---|---|---|---|
| Two abutting shapes, drawn separately | 5 640 / 76 949 (2.15 % of the frame) | 5 525 | 4 403 |
| One merged nonzero path, **mixed** subpath winding | 4 283 / 56 134 (1.63 %) | 4 283 | **0** |
| One merged nonzero path, **consistent** winding | **0** | 0 | 0 |

The middle row is a real, reproducible property of `vello_cpu` and it is
worth knowing: its nonzero fill accumulates *fractional signed area*, so two
subpaths of opposite winding that share an edge cancel along it and leave a
seam. `tiny-skia`'s exact per-sample winding does not. Forcing the subpaths to
one direction removes it completely. Note that this is not simply a bug to
route around: for a donut the cancellation is exactly what nonzero *means*, so
blanket orientation normalisation would fill the holes. See the invariants.

The first row is inherent: abutting shapes drawn one after another always
conflate in a painter's model.

**M7 — determinism.** 100 renders of a 5 000-object scene, hashed:

| | distinct hashes |
|---|---|
| u8, 1 thread, detected SIMD | **1** |
| u8, 4 threads, detected SIMD | **1** |
| u8, 1 thread, baseline SIMD | **1** |
| f32, 1 and 4 threads | **1** |
| `tiny-skia`, 50 runs | 1 |

One thread and four threads produce **identical** bytes. Detected (AVX2) and
baseline SIMD do not: **3 of 786 432 channel samples differ, by at most
1/255**. Small, but "at most 1/255" is not "zero", and a golden baseline
cannot be "nearly". `CpuConfig::deterministic()` therefore pins the level.

**M8 — interposability. Yes**, in about twenty lines of glue: render a layer
into our own buffer, composite it with our own pass. 200 nested full-canvas
1024² layers with a multiply composite took 948 ms total, ≈ 4.7 ms per layer —
an upper bound, since a real layer is bounded by its content.

**M9 — weight.** Stripped, `lto = "thin"`, against a 341 KiB hello-world:

| | stripped size | delta | cold build |
|---|---|---|---|
| baseline | 341 KiB | — | — |
| `+ vello_cpu` | 1.39 MiB | **+1.05 MiB** | 45.6 s on 4 cores |
| `+ wgpu 30 + vello 0.10` | 3.20 MiB | +2.86 MiB | ≈ 5 min |

No C++ toolchain anywhere.

**M10 — licence.** `vello_cpu`, `vello` and `wgpu` are all
`Apache-2.0 OR MIT`. `tiny-skia` is BSD-3 and is a dev-dependency only.
`cargo deny check licenses` is clean.

### Gate results

| Gate | Verdict | Number |
|---|---|---|
| G1 CPU throughput ≤ 120 ms single, ≤ 25 ms on 8 cores | **fails — settled on the reference machine** | container: 442 / 194 ms on 4 slow cores. Reference machine: **159–164 / 53–58 ms** (see the re-run below) |
| G2 GPU throughput ≤ 8 ms | **fails on the integrated GPU — settled**; at the line on the discrete GPU | container: no adapter. Reference machine: Intel iGPU **72–86 ms**, RTX 4000 SFF Ada 7.8–8.0 ms |
| G3 AA: mean ΔE < 1.0, p99 < 3.0, ≥ 132 levels | **passes**, with the reference corrected | 0.072 / 0.647, 256 levels |
| G4 determinism, byte-identical per architecture | **passes**, with the SIMD level pinned | 1 hash / 100 runs; 3 of 786 432 samples move if it is not |
| G5 conflation ≤ 0.05 % | **passes** for a single path with consistent winding | 0.0000 %; 1.63 % with mixed winding; see the invariant |
| G6 interposability, ≤ 300 lines of glue | **passes** | ≈ 20 lines |
| G7 ≤ 8 MB added, no C++ | **passes** | +1.05 MiB (CPU), +2.86 MiB (with wgpu) |

### The decision

**We build on `vello_cpu` 0.2 as the coverage rasteriser, with our own scene,
display list, paints, blend families and compositor above it — exactly
architecture §3.3 — and we ship Phase 4 CPU-only, with the GPU backend behind
a feature flag.**

Why, with the numbers attached:

1. **Quality.** 256 coverage levels against CDraw's 132 and `tiny-skia`'s 17;
   mean ΔE₀₀ 0.07 against a reachable reference, where `tiny-skia` scores
   0.88. The goal was to equal or beat the original, and this clears it.
2. **Determinism.** Byte-identical over 100 runs and across thread counts,
   which is what the entire export and golden-image strategy rests on.
   `tiny-skia` is also deterministic, and would have been the fallback oracle
   had this failed; it did not.
3. **Interposability.** The compositor stays ours, in twenty lines. That is
   the whole point: nothing else on the list brings Xara's blend families, its
   conical and diamond gradients or its perspective mapping, and none of them
   would let us add them without a fork.
4. **Speed, relatively.** 3.2× faster than the control on the same scenes.
5. **Weight and licence.** 1.05 MiB, `Apache-2.0 OR MIT`, no C++.

And the honest counterweight: **G1 fails on this machine and G2 was never
measured**, so the performance half of the gate is not closed. The phase's own
decision rule for an unmeasurable GPU is "ship CPU-only for Phase 4, move the
GPU backend behind a feature flag, re-evaluate at the Phase 5 gate", and that
is what has been done. Re-run the spike on the real reference machine before
anyone claims the budgets are met.

Do **not** re-open the rasteriser question on intuition. Re-open it if, on
real hardware, `bulk` at 1920 × 1080 cannot reach 25 ms on eight cores, or if
`vello_cpu`'s alpha status produces a correctness regression the golden suite
catches. *(The first condition fired on 2026-09-23. It was reviewed and the
decision stands; see the next section.)*

### Re-run on the reference machine (2026-09-23): G1 and G2 settled

The machine is described in `docs/memory/perf.md` §Reference machine: Intel
Core Ultra 9 285 (8 P + 16 E cores), an Intel Arrow Lake iGPU (Mesa 26.1.6)
and an NVIDIA RTX 4000 SFF Ada (580.173.02), COSMIC Wayland, rustc 1.98.1.
Other agents were fuzzing and compiling throughout. Each figure is the median
of 30–50 frames, and a range spans 3–5 runs at load average 6–25.

**The harness now exists in the repository:** `crates/xarast-render/benches/spike.rs`
is the throughput half of the spike (G1, G2, candidate C). It talks to
`vello_cpu`, `vello` and `vello_hybrid` directly, uses the same `bulk`
generator in both object sizes, times the CPU from recording to the finished
pixmap and the GPU from submit to `poll(wait)`, and checks every GPU frame
against the `vello_cpu` one (mean absolute channel difference). Run it with:

```text
cargo bench -p xarast-render --bench spike                        # G1
cargo bench -p xarast-render --bench spike --features spike-gpu   # + G2, every adapter
SPIKE_THREADS=0,7 taskset -c 0-7 cargo bench -p xarast-render --bench spike   # 8 P-cores
```

**M1 / G1 — `vello_cpu` 0.2, u8 pipeline, detected SIMD (AVX2):**

| `bulk` variant | 1 thread | 8 threads (7 workers + 1) | 24 threads |
|---|---|---|---|
| r3..28 px, overdraw ≈ ×27 (the gate scene) | **159–164 ms** (record 104 + raster 56); 175 pinned to the P-cores | **53–58 ms**; 55.6 pinned to the P-cores | 49–51 ms |
| r2..8 px, overdraw ≈ ×2.6 | 83–88 ms (record 64 + raster 20) | 34–37 ms | 35–39 ms |
| r3..28 px, E-cores only (`taskset -c 8-23`) | 227 ms | 119 ms (15 workers; one noisy run, 68–246) | — |

Against the container this is 2.8× faster single-threaded and 3.5× faster in
parallel, and **G1 still fails on both halves.** Parallel scaling stops at
about 3×, and 8, 16 and 24 threads land within 10 % of each other. The serial
part is the recording thread, which feeds paths to the workers. Recording was
already the dominant cost on one thread (104 of 164 ms). More cores do not
close this gate. Only not re-recording does.

**M2 / G2 — `vello` 0.10, area AA, scene retained, submit → `poll(wait)`:**

| Adapter | r3..28 px | r2..8 px | Scene encoding, CPU, once per scene change | Δ vs `vello_cpu` |
|---|---|---|---|---|
| **Intel Arrow Lake iGPU** (the gate's "integrated GPU") | **72–86 ms** (min 48); 197 ms at load 34 | 32–48 ms | 17–25 ms | 0.019 / 0.68 |
| NVIDIA RTX 4000 SFF Ada | **7.8–8.0 ms**; 9.3 ms at load 34 | 5.8–6.0 ms | 17–25 ms | 0.019 / 0.67 |

The Δ column is the mean absolute channel difference against the
`vello_cpu` frame. It is 0.02 where the frame is covered, and it rises on the
light scene's many antialiased edges. `vello` writes straight alpha, so the
comparison is made against the unpremultiplied reference.

**Candidate C, `vello_hybrid` 0.2** (not a gate, recorded for the Phase 5
re-evaluation the decision rule asks for): the GPU half is quick, 5.4–6.0 ms
(light) and 17–18 ms (heavy) on the RTX, but every frame re-records strips on
the CPU at **63–110 ms**, the same cost as `vello_cpu`. It also misbehaved: one
run took 2.5 s per heavy frame on the RTX, and on the Intel iGPU the heavy
frame differs from `vello_cpu` by a mean of 1.77 (0.03 on the RTX).
**Rejected:** it removes nothing that matters.

**Verdicts.**

- **G1 fails, settled.** One thread is 1.3× over and eight P-cores are 2.2×
  over. The machine is no longer the excuse.
- **G2 fails on the integrated GPU, settled**, by 9–11× on the gate scene. On
  the discrete RTX 4000 it sits exactly at the 8 ms line. The integrated GPU
  is also the more fragile of the two: under CPU load its time more than
  doubles.

**What this changes, and what it does not.**

1. **The rasteriser choice stands.** The re-open trigger fired and was
   reviewed. No measured alternative is faster: `tiny-skia` is 3.2× slower,
   and `vello_hybrid` records strips at the same cost. The things `vello_cpu`
   was chosen for (256 levels, determinism, interposability) are unaffected.
2. **A 100k-object full frame is a cold-path cost, not an interactive one.**
   First paint, export and zoom-settle may pay 35–160 ms. A pan or zoom frame
   must not re-record the scene. It must composite cached pixels, per node or
   per tile (the per-node cache the phase already specifies), and re-raster
   only the dirty part: 64 × 64 costs 0.12 ms. This makes the Draft → Final
   scheduler (TODO 8) the thing the 16 ms pan/zoom budget depends on.
3. **The GPU backend does not rescue the integrated-GPU budget on its own.**
   At 72–86 ms for a full `vello` raster, the pan/zoom ≤ 16 ms budget on the
   iGPU also has to come from reusing pixels (transforming cached tiles or
   layers while the gesture runs), not from re-rasterising. On a discrete GPU
   a full re-raster fits in 8 ms.
4. **`vello` 0.10 and `vello_hybrid` 0.2 pin `wgpu` 29**, but the workspace
   and the shell are on `wgpu` 30. The workspace manifest's comment says
   "`vello` 0.10 on `wgpu` 30", and that is wrong. The spike builds against
   `vello::wgpu` behind the bench-only `spike-gpu` feature. Task R5.1 must
   pick one: pin the shell to `wgpu` 29, or wait for a `vello` release on 30.
   Two `wgpu` versions in one
   binary cannot share a device.
5. **Scene encoding for `vello` costs 17–25 ms per 100k objects.** A retained
   `vello::Scene` has to be rebuilt, or appended, when the document changes.
   That is another reason to keep the encoding per node and not per frame.

---

## The GPU decision (XARA-US-0011, 2026-09-23)

**Question.** With the CPU path inside its budgets (100k full frame 19 ms,
Draft pan zoomed 3× 11.1 ms through the scheduler, zoom 1.4 ms) and G1/G2
failed, is a GPU *rasteriser* worth building now, and if not, what should
the GPU do?

**Answer: no GPU rasteriser now. Rasterise on the CPU, keep the pixels on
the GPU as tiles, and let the GPU move them (option a).** The WGSL
paint/blend pass (b) is deferred behind a stated trigger (XARA-T-0051), and
`vello` (c) is rejected until it runs on our `wgpu` *and* fits the iGPU
budget.

### Evidence

Reference machine (`perf.md`), both GPUs selected explicitly through
Vulkan. GPU rows are wall-clock from the first `wgpu` call to `poll(wait)`,
medians of 50–300 frames; a range spans 4 runs at load average 5–25 (other
agents compiling). `cargo bench -p xarast-render --features gpu --bench
tiles`; `WGPU_ADAPTER_NAME=intel|nvidia` selects one adapter.

| | Intel Arrow Lake iGPU (Mesa 26.1.6) | RTX 4000 SFF Ada (580.173) |
|---|---|---|
| empty submit + wait (the floor) | 0.05–0.37 ms | 0.05–0.19 ms |
| **today:** upload a whole 1080p CPU frame | 0.54–1.6 ms | 0.61–0.89 ms |
| (a) pan, tiles resident, 40 × 256² placed | 0.47–1.4 ms | 0.08–0.21 ms |
| (a) pan crossing a tile row: upload 8 tiles + composite | 0.71–2.2 ms | 0.27–0.51 ms |
| (a) Draft zoom 1.3×, 28 tiles | 0.37–1.8 ms | 0.08–0.15 ms |
| (a) Draft zoom-out 0.5×, 144 tiles (today: backdrop border) | 0.71–2.0 ms | 0.15–0.47 ms |
| **today at 4K:** upload a whole 3840 × 2160 frame | **23–27 ms** | 2.6–3.5 ms |
| (a) 4K pan, 144 tiles resident | 1.5–3.0 ms | 0.30–0.54 ms |
| (b) floor: upload 64 MiB of A8 coverage | **27–33 ms** | 5.4–6.9 ms |
| (c) `vello` 0.10 full frame, 100k `bulk` (W0 re-run) | **72–86 ms** | 7.8–8.0 ms |

CPU side, same runs: `scroll_surface` 1080p 0.19–0.73 ms; `compose_cpu`
(the software tier of the same operation) 1.3–3.1 ms at 1080p.

| Criterion | (a) CPU raster + GPU tiles | (b) WGSL paint/blend over CPU coverage | (c) `vello` on `wgpu` 29 |
|---|---|---|---|
| What it speeds up | pan, Draft zoom, 4K presentation; pan latency (the shell can re-composite at input time) | gradient-heavy frames (30 ns/px on the CPU, 1.4 × 10⁸ px per corpus frame) | full re-rasters, on the RTX only |
| iGPU cost | ≤ 3 ms at 4K | ≥ 60 ms of mask upload for the frames it targets | 72–86 ms per frame |
| Parity | **byte-exact** with `compose_cpu`: 0 of 960 composites differ on the iGPU, the RTX and lavapipe | perceptual only (gate B), gradient drift risk K5 | Δ 0.019 mean vs `vello_cpu`, and no Xara paints or families: still needs (b) on top |
| Determinism / goldens | untouched: nothing is re-rasterised on the GPU | export stays CPU; two code paths per paint | as (b) |
| Capability ladder | any tier with a `wgpu` device (lavapipe exact); no device → `compose_cpu` or today's path | per-tier shader validation | tier 0 only (compute) |
| Dependency cost | none: `wgpu` 30 is already in the shell | none | two `wgpu`s: **+4.08 MiB** stripped (probe: 4.27 → 8.35 MB), **+46 crates** (127 → 173 for `xarast-render`), 9 more duplicated crates (`wgpu`, `wgpu-core`, `wgpu-hal`, `wgpu-types`, `naga`, `wgpu-naga-bridge`, `wgpu-core-deps-*`, `bit-set`, `foldhash`); two `wgpu`s cannot share a device. Downgrading the workspace instead means porting the shell back to 29; `vello` alone adds 0.68 MiB over `wgpu`. Licences clean either way |
| Effort | S (done here) + the shell wiring (XARA-T-0050) | L + L + M (R5.2–R5.4) | L, plus (b) |

Reading the table:

1. **The budgets that fail today are presentation budgets, not raster
   budgets.** At 1080p the CPU already meets every interactive row. What
   breaks is a 4K canvas on the integrated GPU: uploading one frame costs
   23–27 ms, over the 16 ms budget before anything is drawn, against 1.5–3
   ms to composite resident tiles. At 1080p on the iGPU the per-frame gain
   is small (composite ≈ upload ≈ 1 ms); the gains there are the CPU work
   removed (scroll, rescale), zoom-out showing content instead of a
   backdrop border, and a pan presented at input time.
2. **(b) is bandwidth-bound on the machine that decides.** Its targets are
   the gradient-heavy files, and feeding their coverage to the iGPU costs
   more than a frame. It becomes interesting only with coverage on the GPU,
   which is (c)'s problem, or after XARA-T-0038 has shown what row-wise SIMD
   leaves on the CPU. Trigger in XARA-T-0051.
3. **(c) loses on every axis that matters here**: slower than the CPU on
   the iGPU, a second `wgpu`, and it would still need (b) for Xara's paints
   and families. Re-open when a `vello` release targets the workspace's
   `wgpu` **and** a full 100k frame fits 16 ms on the iGPU.

### What was built (XARA-T-0040)

- `compose` (always compiled): `TileKey`, `TileGrid` (`covering`,
  `rect`, `tile_view`, `placement`), `TilePlacement` with a `TexelRect`
  valid area, `source_texel` (the one texel rule) and `compose_cpu`.
- `backend::gpu_tiles` (`gpu` feature): `GpuTileCache` — one `Rgba8Unorm`
  2D-array atlas (default 128 × 256², 32 MiB), LRU, `upload(key, surface,
  rect, at)` from any sub-rectangle of a CPU `Surface` to a texel offset,
  `encode` (one instanced draw on the caller's encoder, replace, clear to a
  backdrop) and `compose`; plus `create_target` and `read_back`.
- `tests/parity_tiles.rs`: the 120-scene corpus cut into 32² tiles,
  composited eight ways (identity, whole and fractional pan, zoom ×1.5,
  ×3.7 and ×0.6, a missing tile, a partial tile at an offset beside a tile
  uploaded in halves) on every adapter: **byte-identical** on the Intel
  iGPU, the RTX 4000 Ada and lavapipe (960 composites each). The NVIDIA
  GL adapter fails device creation ("Parent device is lost") and is
  skipped.
- `benches/tiles.rs`: the table above.

**A tile-assembled frame is not a whole frame.** Rasterising the corpus
tile by tile and assembling it gives 105 of 120 cases byte-identical to
the whole-frame render; 12 mesh gradients and `aa_seam` move 1–17 pixels
by 1/255, and two perspective repeating linear gradients move one pixel
on the wrap edge by 230/255 (a sub-ulp difference picks the other side of
the discontinuity). So tiles are for **moving** pixels. A `Final` frame at
rest is still rasterised whole and cut into tiles on upload, which the
texel offsets exist for; the test pins the drift at ≤ 0.2 % of pixels.

### The tiles in the viewer (XARA-T-0050)

The shell (`xarast-shell/src/tiles.rs`) owns the cache. What it needed
from here was one addition, `GpuTileCache::valid(key)`, so that it can
upload only the texels a tile lacks. Facts about the integration that
matter to this crate:

- A **level** is one zoom *and* one pixel grid: the first frame at a zoom
  defines it, and a later frame joins it only if its translation differs
  by whole pixels (1e-3 tolerance, as `reuse.rs`). A Final after a
  fractional pan starts a level. Up to three levels are kept and drawn
  oldest first, so a zoom-out shows older resident content under the
  newest level.
- A tile's valid area is a bounding box, so a piece that would not join
  it into a rectangle restarts the tile (`invalidate` + upload). This
  happens at the trailing edge of a pan, and is most of the per-step
  upload beyond the strips.
- `compose_cpu` is the CPU tier of the same planner, over `256²` tiles in
  memory; a unit test in the shell holds the two tiers byte-identical on
  every adapter it finds, and real-window screenshots of `ProbeX16.xar`
  on the Intel iGPU and the RTX, each tier, have identical canvases.
- An edit keeps the tiles (XARA-T-0221): a frame of a new scene epoch
  whose `base` is the last frame uploaded keeps its level's tiles under
  the view and uploads only its `fresh` damage; tiles holding texels
  outside the view restart, every other tile goes (`TileStore::retain`,
  `GpuTileCache::retain`).
- The composite target is the painter's canvas texture (`Rgba8Unorm`,
  now also a render attachment); the swapchain may be `Bgra8Unorm`, which
  is why the tiles do not draw into it directly.

**Per-tile rasterisation is the wrong grain for large scenes.** A culled
`DisplayList::build` is linear in the scene's ops: 6.5–10 ms for a
900 000-op scene, so a single 256² tile costs 5–10 ms and a 1920 × 256
row 12–18 ms. Rasterise one dirty rectangle per frame (as the render
thread already does) and upload its pieces; do not loop over tiles.

### Edit damage (XARA-T-0221, 2026-09-24)

`damage::scene_damage(old, new, view, max_rects)` returns the device
rectangles whose pixels may differ between a frame of `old` and a frame of
`new`. The render thread calls it with the scene of the frame on screen and
repaints only those rectangles (`app-core.md` decision 40); the shell keeps
its tiles across the repaint.

- **The model.** A scene is a tree: pushes (group, clip, transparency scope,
  layer) are inner nodes, fills, strokes and images are leaves. Children of
  two inner nodes with equal push ops are aligned: common prefix and suffix
  pairwise, then the middle by a cheap key (op kind, node id, path bounds
  bits) plus full op equality, cut to the longest order-preserving run
  (patience/LIS). Matched subtrees recurse; every leaf under an unmatched
  child, on either side, is damage, with its display-list device bounds
  (`device_bounds_of`, `mapping_bounds`, `stroke_pad`: the same functions
  the build uses).
- **Why it is sufficient.** A pixel outside the damage is covered only by
  matched leaves, in the same order, under equal pushes, so it is
  composited from the same inputs. That rests on two renderer properties,
  both pinned: a leaf touches nothing outside its display-list bounds, and
  a clip, group or layer with no leaf on a pixel leaves it alone.
- **Resources are compared by content.** Equal ops can name different
  ramps: an evicted ramp slot is reused by the next intern. A matched op's
  ramp ids (colour table and transparency table) and image ids are checked
  against both resolvers, memoised per id.
- **It is command-agnostic by design.** It covers a group attribute that
  recolours its siblings, a named colour redefinition (every user of the
  colour and nothing else, tested), z-order, undo, redo and previews,
  with no per-command extent to get wrong. `None` only when the scenes'
  qualities differ or one is unbalanced.
- **`damage::coalesce`** merges to at most `max_rects`, cheapest union
  first, and merges below the limit whenever a union costs no area; above
  256 raw rectangles it first chunks them in reading order.
- **Tests.** Unit tests (recolour, move, z-swap, group transform, reused
  ramp slot, LIS, coalescing); `properties::repainting_the_damage_gives_
  the_new_frame` (512 random step sequences × 1–3 random edits including
  layers, clips and blend families, byte-exact); the corpus test in
  `xarast-shell/tests/edit_damage.rs`. Mutation-checked: dropping the leaves
  of an unmatched subtree, or matching on the key alone, fails the property.

**A rectangle drawn over a frame is exactly that frame (invariant 15).**
`vello_cpu` rasterises in `f32` relative to the top left of the pixmap it
is given. Coverage used to start at the draw area's corner (a column, a
column tile, a pan strip, a dirty rectangle), which moved `aa_edge_45` by
1/255 in up to 53 pixels and SimpleSphere's gradients in a few; the old
tests allowed "within 1/255" for scrolls and columns. Coverage now starts at
`max(bounds.x0, 0)` and the band's top (clips at the surface's left edge).
The right edge does not matter (coverage accumulates from the left).
Aligning the origin to 4 px, vello's tile width, was **not** enough:
`aa_edge_45` still moved. One-call renders are unchanged, so every golden
is; `determinism::coverage_does_not_depend_on_the_draw_area` pins five
rectangles × 120 cases × both configurations, and the scroll and column
tests in `render_thread.rs` are now exact. Cost in `perf.md`.

---

## Recovered constants — and the ones that were not recovered

**The luminance weights were not recovered.** Task R4.4 needs the original
running in an x86-64 VM; this container has no VM, no copy of `libCDraw.a`,
and the clean-room rule keeps `GDraw/*.h` out of reach regardless.
`LumaWeights::BT601` (0.299 / 0.587 / 0.114) therefore stands as the stated
hypothesis, with **no measured residual**. `blend_luts::the_luminance_weights_
are_the_recorded_hypothesis` is the test that will fail loudly when the
constant changes, which is the point of writing it down.

**Four families are unverified against the original**, for the same reason:

| Family | Status |
|---|---|
| Mix, Stained Glass, Bleach, Darken, Lighten, Brightness | transcribed from formulas the research recovered instruction by instruction; exact |
| Saturation, Luminosity | reconstructed from the *semantics* the disassembly shows (`aSaturationTable1/2`, `Recip`, `Off`), not from their contents |
| Contrast, Bevel | documented approximations: `aContrastTable` and the bevel pair are static tables in the binary that no header describes |
| Hue | the RGB↔HSV path is exact; the hue byte interpolation follows the described `Mul`/`IMul` |

All twelve have the right endpoints — every one is the identity at t = 255,
which is tested over the whole domain — and all twelve go through one
generator, so the two backends cannot drift apart. Closing the gap is task
R4.5: a small harness that `dlopen`s the original and dumps
`GDraw::CalcTransparencyX` over its full 256 × 256 × t domain.

**Antialiasing, measured.** 256 distinct coverage levels on a 0.5° edge; mean
ΔE₀₀ 0.137 and max channel 24/255 against a 16× supersampled reference for an
ellipse at `Final` quality, 0.746 and 119/255 at `Draft`. Both residuals are
the *flatness tolerance*, not the coverage maths: a chord error of `t` device
pixels moves an edge by up to `t` of a pixel. For a straight edge, where box
averaging of exact sub-areas is exactly the area, the agreement is **bit for
bit at every angle tested**.

**Flatness.** `Final` allows a chord error of **0.1 device pixels** and
`Draft` five times that, 0.5 px. That is the original's rule read the right
way round: it uses half a pixel and divides by five when antialiasing is on
(`research/03 §2.2`), and Xarast is always antialiased, so its antialiased
value is our baseline and Draft lands on its non-antialiased one. Flatness is
**ours**, applied by the backend before the rasteriser sees the path;
otherwise `RenderQuality` would control nothing, which is exactly what it did
until this was wired.

---

## Clouds and plasma (XARA-T-0336, 2026-10-16)

- **Where**: generator `xarast-image::procedural` (from `research/03
  §2.8.1–2.8.2`); tile size and colouring `xarast-format::svg::procedural`
  (`fill_tile`, `tile_pixels`, `tile_rgba`), shared by the walker and the
  `.xarast`/SVG writer so the canvas and the bake are the same bytes; the
  cache `xarast-app/src/procedural.rs`.
- **What reaches the renderer**: a colour fill is a `Paint::Image` of the
  tile **already coloured** (256-entry table of `interpolate(from, to, t,
  effect)`, the colours' alpha kept, the bias/gain profile applied to the
  grey first), no contone; a transparency is a `TranspSource::Image` of
  the grey tile with `bitmap_level_ramp` (start level at black). Mapping
  `bitmap_frame(origin, axis_x, axis_y)`, repeat `bitmap_repeat(None,
  mapping attr)`. A degenerate parallelogram (an axis < 1 mp) is not
  generated: the start colour / the mean level, as before.
- **Cache (H5)**: `TileKey` = `Procedural` (16.16 words, tile size) +
  profile bits + colours; one `ImageId` per key for the walker's life, so a
  redraw generates nothing (`WalkStats::procedural_generated`,
  `tests/procedural_fills.rs`). Beyond 128 keys a whole (unculled) walk
  parks the unused ones on a 1 × 1 placeholder and reuses their slots.
  Tiles are ≤ 256 × 256 RGBA (256 KiB).
- **Moulds**: `mould_fill` moves the three corners (no fourth corner in
  the model: the affine part of a perspective). Follow-up if a corpus
  file needs it: a `persp` on procedural fills, as bitmaps have.

## Transparency families (XARA-T-0347, phase 13 §I, 2026-10-16)

Phase 13 §I asked for twelve families in the model, one LUT source, every
source for every family, the grey weights, the selector and the file
mappings. Most of it was already there; this is what the task found, what
it closed and what it left open.

- **What existed.** The renderer had all twelve `BlendFamily` values, one
  table generator (`build_blend_lut`) that the CPU backend and the GPU
  layer upload (`GpuConfig::weights` → `BlendLuts::build`) both call, and
  `Transparency { family, source: Flat | Gradient | Mesh | Image }` with
  ramps, profiles and level ramps on every source. The document model's
  transparency is `TranspPaint = FillGeometry<Transparency>`, whose stops
  carry the family (`TranspMode`). The gaps were: the model had eleven
  families (no `Bevel`); the `.xarast` writer dropped the family of
  bitmap, fractal and noise transparencies; a bitmap transparency without
  a level pair could not hold a family at all; and the grey weights were
  written out in four places.
- **I1.** `TranspMode::Bevel = 34` and `TranspMode::ALL` (twelve).
  `TranspPaint::family()` and `TranspPaint::source()`
  (`xarast_doc::fill::TranspSource::{Flat, Gradient, Image}`; meshes are
  Gradient, fractal and noise tiles are Image) name the two halves of the
  phase's `Transparency { family, source }` on the existing type rather
  than adding a parallel one. `SetTranspMode` on a bitmap without a level
  pair now writes the implied pair (0 at black, 255 at white) with the new
  family; before, the change was silently lost.
- **I2.** One source: `xarast_color::RENDER_GREY_WEIGHTS`, read by
  `LumaWeights::GREY_CONVERSION` (the default of `CpuConfig`, `GpuConfig`
  and `LumaWeights`), the bevel table, the bitmap-transparency luminance
  and the `.xarast` writer's transparency-mask colour matrix
  (`mask_row`, which still produces the old bytes exactly). The **contone**
  remap (`resample::remap_texel`, `LUMA_MATRIX` in the SVG writer and
  `xarast-image` `photo::luma`) and the PNG grey export keep their own
  BT.601 constants on purpose: they are image operations, not CDraw's grey
  conversion, and they are not part of this hypothesis. The colour
  model's `GREY_MODEL_WEIGHTS` is measured and different (`colour.md`).
- **I4: the family × source goldens.** `corpus::transparency_family_cases()`:
  12 families × {`flat` at 96, `gradient` a 4-stop linear ramp under
  bias/gain (0.35, −0.25), `image` a tinted 16 × 16 grey ramp through a
  4-stop level ramp under (−0.3, 0.4)}, 48 × 48, compared exactly by
  `tests/golden_transparency_family.rs` against
  `tests/golden/transparency_family/<family>_<source>.png` (36 PNGs,
  148 KB). `every_family_and_every_source_draws_differently` asserts all
  66 family pairs differ per source and all three sources differ per
  family, except `None`, which draws nothing whatever its source. Like the
  fill × blend matrix these are regression locks: the unverified
  families are locked as they are, not judged.
- **I5.** `fill_tool::transp_mode_choice`: the infobar offers the
  original's ten (`TRANSP_MODES`, the order of `TranspInfoBarOp`'s menu in
  `tools/filltool.cpp`) and *shows* any other family in force by appending
  it as the selected entry (choosing it is a no-op). `None` shows as Mix.
  Bevel is never offered: the original gives it only to a bevel's own
  lighting fill (`Kernel/nodebev.cpp:434`).
- **I6.** See `xar-import.md` ("Transparency type byte") and
  `xarast-format.md` ("Transparency families"); `.xarast` round trip of all
  twelve families on flat, linear multi-stop, three-point, bitmap and
  fractal sources is `xarast-format/tests/transparency_families.rs`, with
  a byte-identical second and third save.
- **Corpus effect: none.** The corpus uses Mix, Stained Glass, Bleach,
  Darken, Brightness and Hue; every bitmap, fractal and noise transparency
  in it is Mix, and nothing has type byte 34. Exporting the 59 files to
  SVG and PNG before and after the change gives byte-identical output
  (122 files compared).

### Grey-conversion weights (I3): unverified, and how to measure them

`RENDER_GREY_WEIGHTS` is **BT.601 (0.299, 0.587, 0.114), unverified**.
The original never calls `GColour_SetGreyConversionValues`, so CDraw's
built-in defaults apply and they are in no header; the corpus previews
cannot resolve them (see "Blend families against the original's
previews"). Like XARA-T-0325 for the bevel tables, this needs the
original running. The procedure, for whoever has an x86-64 VM with a
2006-era Xara LX build:

1. Make a document with a white rectangle, and over it a rectangle of
   **flat** colour `c` with a **Darken** transparency at level 0. Darken at
   t = 0 draws `d · Y(c) / 255` (`research/03 §2.7.2`), so over white the
   result is `Y(c)` in every channel, with no ramp or antialiasing
   involved.
2. Repeat for a set of colours: the 255 pure reds `(r, 0, 0)`, greens and
   blues, plus about 50 random colours as a check. One page of small
   squares is enough; export a bitmap at 100 % with antialiasing off (or
   sample square centres only).
3. Each pure ramp gives one weight table directly (`Y(r, 0, 0)` against
   `r`), which also tells whether CDraw's tables are linear (a single
   weight) or not. Fit `Y = round(wr·r + wg·g + wb·b)` (or the `>> 24`
   fixed-point form of `research/03 §2.7.2`) by least squares and check
   the residual on the random colours: it must be 0 for every one, or the
   model is wrong, not the weights.
4. Commit the result as `RENDER_GREY_WEIGHTS` with the residual and the
   procedure's date here, change
   `blend_luts::the_luminance_weights_are_the_recorded_hypothesis`, and
   re-bless the goldens of every luminance family (Darken, Lighten,
   Brightness, Saturation, Luminosity, Hue) in all three matrices.

Lighten (screen by `Y(s)`) at t = 0 over black is the same measurement
from the other side, a cheap cross-check.

### Open questions this task did not settle

- **What `None` draws.** The renderer's `BlendFamily::None` draws nothing,
  but the walker never produces it: a flat `None` transparency is no
  transparency (opaque) and a graduated, mesh or procedural `None` is
  drawn as Mix; a bitmap `None` is opaque. The original's reader keeps
  byte 0 as is and maps it to `T_NONE` when rendering
  (`wxOil/grndrgn.cpp:8846`), whose effect in CDraw is unknown. No corpus
  file has one. `.xarast` writes a non-flat `None` as Mix and a flat one
  as nothing, which is what the canvas draws (except the bitmap case).
- **The flat and graduated variants** (type bytes 14, 15, 17, …) are
  documented as illegal in the document (`Kernel/fillval.h:161-164`) and
  read as Mix; the original would draw them as an off-by-one family
  through `MapTranspTypeToGDraw`. Nothing writes them.

## Decisions taken (and why)

**Export has its own entry and its own band geometry (2026-09-23,
XARA-US-0057).** `export::render_export_strips` maps a document rectangle
exactly onto a `w × h` grid and renders it in strips through
`CpuBackend::render_rows`, which draws device rows `y0..y0+h` of a display
list on the **absolute** band grid with a caller-given band height. The
entry constructs `CpuConfig::deterministic()` itself and takes no backend
parameter: the GPU cannot reach an export. Band height is
`export_band_lines(w, h)` — about 64 bands, 16 rows minimum, 1 MiB maximum —
and a strip is a whole number of bands starting on the grid, so the thread
count and the strip budget never change a byte
(`tests/export.rs`: runs, strip budgets, 1 vs 8 threads, over every
synthetic case). This is deliberately *not* `band_budget_bytes`: the
on-screen deterministic configuration keeps its 1 MiB bands so the goldens
do not move, while export gets enough bands to use the machine
(XARA-T-0038's parallelism half: the gradient files export in 0.36–0.9 s
against 2.0–4.5 s through `render`). See `export.md`.


**Display-list commands index into shared scene ops (2026-09-23,
XARA-US-0016).** Copying path, paint, style and transparency into every
command made the 100k list 38 MB. That was past glibc's mmap threshold, so
each build page-faulted it fresh and each drop walked 200k cold `Arc`
counters: 20.8 ms, superlinear. With indices it is 1.9 ms. Boxing only the
stroke payload, the first idea on the TODO list, would have left the
command over 300 bytes. Consequences: `DrawCmd` is `Copy` and opaque, and
backends go through `DisplayList::item`. `DisplayList::from_commands` is
gone; the immediate-mode facade records into `ListParts` (its own op
vector plus commands).

**Culling entries beside the ops (XARA-T-0033).** `Scene::cull` holds 40
bytes per op: a primitive's padded document bounds. A *culled* build (a
dirty rectangle smaller than the viewport) reads only those for rejected
primitives. Under an axis-aligned transform it first rejects in document
space against the dirty rectangle mapped back and grown by 4 px. A full
build ignores them, because reading both arrays made it slower (3.4 ms
against 1.9). A test compares culled builds with the filtered full build,
command for command.

**Compositing over a destination that is not opaque (2026-09-23,
XARA-T-0231).** The twelve families are defined against an opaque
destination. `blend::composite` now treats a destination of alpha `a_d`
as that opaque colour over `a_d` of the pixel and as *nothing* over the
rest: the family's result on the first part, the source alone at
`α_s = coverage·(1 − t)` on the second, alpha `a_d + (1 − a_d)·α_s`, colour
the premultiplied sum divided by it. For Mix that is exactly source-over.
Every non-Mix family behaves as Mix over nothing (it has nothing to read),
which is the decision the task asked for; the PDF exporter still renders
those families' backdrops over the paper, as the editor shows them. An
opaque destination takes the old code path byte for byte, which is why
119 of the 120 goldens did not move; `layer_nested` did (1 904 px, max
16/255): its inner Mix layer lands on the outer isolated layer's
transparent area, which used to darken it. It was regenerated with
`XARAST_UPDATE_GOLDEN=1 cargo test -p xarast-render --test golden_cpu`.
Surfaces stay premultiplied; `unpremultiply_rgba_in_place` and
`premultiply_rgba` (in `surface`) convert at the image-file boundary (see
`export.md`). A `LayerKind::Plain` pop still composites with coverage 255,
ignoring the layer's own alpha; nothing but the synthetic corpus pushes
one, so it is left alone (its golden did not move).

**Stroke ends and dash phase follow `stroke_to_path` (XARA-T-0231).** The
CPU stroker sets `with_start_cap`/`with_end_cap` separately, and starts
the pattern at `xarast_geom::reduced_dash_offset(offset, resolved)` on
both routes: the whole-path dasher, and `cull_stroke_input`, which now
takes the phase and adds it to each kept run's arc length. Adding a
non-zero phase exposed a latent seam bug in the culled route: when the
run that ends on a closed subpath's start vertex itself *starts* inside a
dash, the dasher holds that first dash back and emits it last, so the
dash ending on the vertex is not the tail's last one and the seam was not
joined (a missing mitre corner). The tail's dash ending on the vertex is
now found and moved last before the join.
`tests/stroke_ends_and_alpha.rs` checks both caps, the offset (shift,
whole-period invariance, parity with a filled `stroke_to_path` outline)
and the offset under culling.

**Strokes are cut to the band before dashing (XARA-T-0022).**
`stroke_cull` clips the centre line, in device space, to the band plus the
stroke's reach, but only when the stroke reaches more than 256 px past the
band. Anything smaller is expanded exactly as before, which is why no
golden moved. Each kept run is dashed from its own arc length, and a closed
subpath cut open is rejoined at its start vertex. The dasher emits the first
dash last, and that dash is merged into the run that ends on the vertex. On
top of that, a **work budget** of 10⁶ flattened segments: the dash count
times the cost of each dash (`4 + 2·√(half-width px / tolerance px)`). Past
it the pattern is dropped and the stroke drawn solid. Culling alone cannot
bound a stroke wide enough to reach the band from anywhere: the fuzzer's
second finding was a 5.6 × 10⁵ mp stroke with 43 mp dashes under a
degenerate view.

**`CpuConfig::deterministic` uses every core.** Bands merge by index and the
determinism test proves thread count cannot change a byte. The band height
and the SIMD level are what must stay pinned, and they do.

**Interactive bands are 512 KiB (16 per 1080p frame) and one-band areas are
cut into column tiles.** The column tiles apply to the interactive
configuration only: at most 32 tiles, each at least 64 px wide, a count that
depends on the area and not on the thread count. A tile edge clips coverage
horizontally, as a dirty-rect edge always did. A determinism test pins tiled
against serial byte for byte. The deterministic configuration never tiles
and keeps 1 MiB bands, so the goldens do not depend on either change.

**Hot-path arithmetic avoids libm and repeats nothing per pixel.** The
baseline x86-64 target has no SSE4.1, so `f64::floor/ceil/round` and `%`
are library calls. `DeviceRect::enclosing`, `apply_repeat`'s `rem_euclid`
and the ramp index use exact integer-cast equivalents, each pinned by a
test against the standard library over edge cases and random bits.
`PaintSampler` and `LevelSampler` invert a gradient's mapping once per
primitive; `eval_paint` is a one-point `PaintSampler`.

**Rounding: half away from zero, no half-pixel compensation.** CDraw truncates
on device conversion and the application adds half a pixel back
(`grndrgn.cpp:5290`). We inherit neither. `precision::round_device` is the one
implementation, and the deliberate one-pixel difference from the original is
part of the golden-image methodology rather than a bug to chase.

**`Rgba8Unorm`, never `Rgba8UnormSrgb`; compositing in non-linear sRGB.**
Every blend family is a 256-entry table defined on encoded sRGB
(`research/03 §2.10`). An sRGB-converting target makes the hardware linearise
behind our back and silently changes Stained Glass and Bleach. The GPU backend
refuses the wrong format rather than producing plausible wrong colours.

**The golden-image gate — this closes architecture §7 question 5.** Both, at
different levels, and each level is chosen by what the comparison can honestly
promise:

| Gate | Compared | Tolerance | Why |
|---|---|---|---|
| A | CPU backend against a committed golden | **exact**, zero differing pixels | the whole point of a deterministic backend is that its output is a fact |
| B | GPU against CPU | ΔRMS < 0.5 %, no channel over 8/255 | GPU results are not reproducible across drivers; an exact gate would be a driver-version detector |
| C | against the original | mean ΔE₀₀ < 1.0, p99 < 3.0, **flat interiors exact** | our coverage is analytic and CDraw's is 17×5 supersampled — better, not equal — but inside a fill only the ramp maths and the blend formula are under test |

Gate A is only honest because the SIMD level is pinned; the spike measured
what happens otherwise (3 samples in 786 432, by 1/255). Phase 3 should not
re-litigate this.

**The AA reference is a painter's-model one**, not full supersampled
visibility. Measured above: the same renderer scores p99 7.8 against the
latter and 0.65 against the former, and the difference is a property no
painter's compositor has.

**Cache admission threshold: 64 primitives**, plus every transparent group and
every live-effect subtree unconditionally. Chosen from the `cache_threshold`
bench sweep, which is close to linear rather than showing a sharp knee: at
512 x 512 a group of 8 costs 4.41 ms, 32 costs 4.52, 64 costs 4.82, 128 costs
5.69 and 512 costs 10.08. 64 is the last size whose own cost stays inside a
tenth of a frame; beyond it the marginal cost per primitive roughly doubles. **The curve is weak evidence and the
number should be re-derived from the `.xar` corpus once it renders.**

**The doc→scene walker lives in `xarast-app`.** The crate table gives
`xarast-render` no dependency on `xarast-doc`, and §4 says the arena is walked
into a display list; both hold only if the walker is not here. `SceneBuilder`
is the contract it fills, and `cargo tree -p xarast-render` shows no
`xarast-doc`, `winit` or `egui`.

**Two deliberate deviations from the phase's API sketch**, both documented at
their definitions:

- `Paint::Gradient`'s ramp is `GradRamp` — a table *or* mesh corner colours —
  rather than a bare `RampId`, because a mesh has no ramp and a meaningless
  field on two of six shapes invites a `todo!()`. The shape enumeration is
  unchanged, so the matrix still has its 6 × 4 × 2 cells.
- `SceneBuilder::finish` returns `Result<SceneStats, SceneError>`. The phase
  asks for unbalanced push/pop to be rejected at build time; a return type is
  how that is said in Rust.

**`Profile` is `xarast-geom`'s `BiasGain`, not a second implementation.** They
are the same Schlick bias and gain; they differ only in the epsilon that keeps
the parameter off its poles, 1e-6 there against the 1e-5 that
`research/03 §2.6.3` records. `tests/profile.rs` measures the worst deviation
over a thousand tabulated triples and asserts it stays below 1e-4 — it is a
few times 1e-5, a hundredth of an 8-bit step, invisible in any table. If
`xarast-geom` ever adopts 1e-5 the test says so.

**Bitmap fill orientation (XARA-T-0171, 2026-09-23).** `Paint::Image` and
`TranspSource::Image` sample `v = 0` at the image's **top** row: decoded
images are top-down, and a placed bitmap's walker frame starts at its
top-left corner. A bitmap *fill's* control points start at the image's
**bottom-left**: start = bottom-left, end = bottom-right, second end =
top-left (and, with perspective, `p2` = top-left, `p3` = top-right). The
original passes start, end, second end as the tile plotter's first three
corners (`wxOil/grndrgn.cpp:3501-3521`), and the plotter maps its first
corner to the first stored row of a bottom-up DIB — the plain bitmap plot
passes a rectangle's low corner first (`wxOil/grndrgn.cpp:4862-4865`);
converting a bitmap object into a fill makes its bottom-left corner the
start point and its top-left the second end (`Kernel/nodebmp.cpp:1210-1212`).
The `.xarast` writer's `<pattern>` agrees (its tile starts at `axis_y`).
The translation lives in `xarast-app::paint::bitmap_frame`, which starts
the renderer's frame at the top edge; the renderer keeps its one
convention. This is not a Y flip between document and device — that stays
the `Viewport`'s — but the image's own row order. Mirror tiling is
symmetric about tile edges, so shifting the frame origin by one tile keeps
the mirrored tiles where the original puts them. Measured against resvg on
the `.xarast` SVG (8 × 8 grey SSIM, before → after): Fill Types simple
0.844 → 0.880, scope3 simple 0.926 → 0.959, JagSS100 simple 0.969 → 0.995,
WATCH 0.980 → 0.991, Spitfire 0.850 → 0.958, TestBitmapFill 0.985 → 0.999.
No golden encoded the bug (none has a bitmap fill); pinned by
`crates/xarast-app/tests/bitmap_orientation.rs`.

**Bitmap transparency levels (XARA-T-0307, 2026-09-24).**
`TranspSource::Image` has a `ramp: Option<RampId>`: a transparency ramp
(the same table `TranspSource::Gradient` uses, in
`Resolver::transparency_ramps`) indexed by `luma / 255`, so black takes the
ramp's start level and white its end — the original's 256-entry bitmap
table (`Kernel/gradtbl.cpp:1421-1458`). `None` reads the luminance as the
level, exactly as before; the walker passes `None` for levels 0..255 with
no profile, so no existing render or golden moved. The ramp is a damage
reference like a gradient's. The family is the mode's (it used to be Mix
for every bitmap transparency). Evidence against the embedded previews is
in `xar-import.md`, finding 18; pinned by
`crates/xarast-app/tests/xar_clips_and_masks.rs`.

### Resampling quality (XARA-US-0052, W10.4, 2026-09-24)

`crates/xarast-render/src/resample.rs` is the one image sampler, behind
`Paint::Image` (fills and placed bitmaps) and `TranspSource::Image`
(which now carries a `Filter`, set by the walker from the quality like the
colour fill's). `ImageSampler::new` inverts the mapping, picks a plan and
builds the contone table once per primitive; `draw_image_cmd` used to
rebuild a `PaintSampler` (a matrix inversion) and a `LevelSampler` per
pixel.

| `Filter` | ≤ 1 texel/px | 1–2 texels/px | ≥ 2 texels/px |
|---|---|---|---|
| `Nearest` (Draft) | point on the base | point on the base | point on level ⌊log2 footprint⌋ (since XARA-T-0281) |
| `Bilinear` | bilinear | tent widened by the footprint | trilinear, pyramid |
| `HighQuality` (Final, export) | Mitchell–Netravali (B = C = ⅓) | tent widened by the footprint | trilinear, pyramid |

- **Minification in linear light, magnification in encoded sRGB — a
  deliberate deviation from the phase document**, which asks for linear
  light for every filter (§W10.4, "measurement wins"). Minified samples
  and the pyramid go through a 256-entry decode table into *premultiplied*
  linear `f32`, are weighted there, and come back through `encode`: a
  binary search over the 255 linear values of the half-levels, so it
  rounds to the nearest encoded level, is exact on the table's values and
  makes no libm call per pixel. Magnification (bilinear, Mitchell) weights
  premultiplied *encoded* values (`MAGNIFY_SPACE`): the harness measured
  it better for every kernel on every one of the 16 image × ratio rows
  (bilinear 19.54 vs 19.36 dB mean, Mitchell 19.69 vs 19.53) with less
  ringing (Mitchell 0.109 vs 0.180), and linear light visibly leaked a
  bright neighbour into a dark texel of a tiny bitmap blown up (Mitchell
  gives a neighbour 5.6 % weight: 66/255 encoded in linear light, 28/255
  encoded; `xarast-app/tests/bitmap_orientation.rs` caught it at its
  40/255 tolerance). The seam at 1 texel per pixel between the two spaces
  is at most the difference between a bilinear sample in either, and the
  aligned case (below) removes it where it would show. Compositing is
  untouched (invariant 2).
- **Premultiplied, not straight.** The old bilinear lerped straight RGBA,
  so a transparent texel's stored colour bled into its neighbours
  (`transparent_texels_do_not_bleed_their_colour`).
- **Footprint** = the longer pixel axis of the Jacobian of device →
  texel (`FrameMap::jacobian`, analytic for perspective): once per
  primitive for an affine mapping, per pixel for a perspective one.
- **The pyramid** (`ImageRef::level`): 2 × 2 box in premultiplied linear
  light down to 1 × 1, odd edges repeat their last row/column, stored as
  straight sRGB RGBA8 so every level samples with the same code. Built on
  first minified sample (`OnceLock`), shared by every clone of the
  `ImageRef` (the render thread's resolver snapshot included), never for
  magnification, `Nearest` or 1–2× footprints. +⅓ of the image's bytes.
  `ImageRef` equality ignores it (damage compares images by content).
- **The aligned case** (T10.3.7's fast path): an affine mapping with one
  texel per pixel on each axis (mirroring allowed), within 1.5 × 10⁻⁸
  relative, and texel centres on pixel centres within 10⁻⁶ texel, samples
  every filter as a point, with integer texel arithmetic. That is the
  original's rule too — smoothing only under rotation or scaling
  (`research/03 §2.8`) — and without it Mitchell (not interpolating) would
  soften a bitmap drawn at its own size. Pinned bit for bit to point
  sampling through the frame map (`tests/resampling.rs`).
- **Contone is per texel, before the kernel** — the original remaps inside
  the sampling through its R, G, B tables (`research/03 §2.8`) — through a
  256-entry table built per primitive with the same
  `xarast_color::interpolate` call as before (it ran per *pixel*).
  Brightness/contrast/gamma/saturation stay after the filter.
- **GPU parity (T10.4.5)** holds by construction: `GpuBackend::render`
  composites on the CPU (the WGSL paint pass is deferred, XARA-T-0051),
  and `parity_gpu_cpu` runs over the ten new `resample_*` corpus cases.
  When the WGSL pass is written it must mirror this sampler.

**The kernel choice, by measurement (T10.4.3).** `tests/resampling.rs`,
release, 128² synthetic set: zone-plate chart, 3-channel value-noise
"photo", screenshot (flat blocks, 1-px lines, 1-px checker), logo (thin
antialiased rings). Magnification is a round trip (exact area reduction in
linear light, enlarge back, PSNR vs the original, encoded RGB) at ×1.5, 2,
3, 4; ringing is the unclamped overshoot outside the 2 × 2 source texels,
linear × 255, on screenshot + logo.

| Space | Kernel | Mean PSNR | Mean ringing | Max ringing | ns/px |
|---|---|---|---|---|---|
| linear | bilinear | 19.36 | 0.000 | 0.0 | 72 |
| linear | Mitchell–Netravali | 19.53 | 0.180 | 16.3 | 127 |
| linear | Catmull–Rom | 19.99 | 0.502 | 34.5 | 128 |
| linear | Lanczos-3 | 20.26 | 1.169 | 56.6 | 342 |
| **encoded** | bilinear | 19.54 | 0.000 | 0.0 | 59 |
| **encoded** | **Mitchell–Netravali** | 19.69 | **0.109** | 14.7 | 113 |
| encoded | Catmull–Rom | 20.07 | 0.306 | 31.1 | 114 |
| encoded | Lanczos-3 | 20.28 | 0.743 | 51.0 | 313 |

(Ringing is × 255 in the space the kernel averaged in; ns/px are
single-threaded and noisy on a shared machine.) The phase's kernel
hypothesis holds: Mitchell rings 2.8× less than Catmull–Rom and 6.8×
less than Lanczos on hard edges, which is most of what a vector editor
places, still beats bilinear, and costs a third of Lanczos. Its space
hypothesis (linear light) does not, for magnification: see above. On the
"photo" alone Lanczos leads by ≈ 1 dB; if photographs ever dominate, a
per-bitmap choice is the lever, not a different default.

Minification, PSNR against the exact area average at ÷1.5, 2, 4, 8. The
reference is a box — what the pyramid computes — so ÷2 is 99 dB by
construction and ÷4/÷8 nearly; those rows prove the pyramid correct, not
optimal. **÷1.5 is the honest comparison, and trilinear lost it on every
image** (chart 23.7 vs the widened tent's 27.5, photo 49.7 vs 52.8,
screenshot 29.1 vs 34.5, logo 28.3 vs 35.4), which is why 1–2× footprints
use the tent widened by the footprint on the base (Mitchell widened wins
photo and logo but loses the screenshot and costs 4×). Product overall:
63.8 dB against 45.4 for bilinear with no prefilter; widened kernels
34–36 dB at 0.4–3.2 µs/px.

**Goldens.** The eight `image_1_*`/`image_2_*` goldens moved and were
re-blessed with `XARAST_UPDATE_GOLDEN=1 cargo test -p xarast-render
--test golden_cpu`: `image_1_*` (Bilinear) by at most 1/255 in 75–290 px
(premultiplied `f32` weights instead of per-channel `f64` lerps), the
`image_2_*` (HighQuality) by at most 20/255 in 1 500–1 860 px — Mitchell
instead of bilinear at the checker's edges, which is the point. The
`image_0_*` (Nearest) goldens did not move. Thirteen of the fourteen
`TextDesigns/` digests (`xarast-cli/tests/golden/text_designs.sha256`)
moved too: each file places a bitmap of the original's own rendering,
which is now Mitchell-magnified or prefiltered instead of plain bilinear.
Checked against a render from 17e5767: every changed pixel is inside
those bitmaps' text (SimpleText 7 100 px ≤ 63/255, Rotated 88 568 px ≤
172/255 where a minified bitmap used to alias), the live text is
untouched. Re-blessed with `XARAST_UPDATE_GOLDEN=1 XARAST_XAR_CORPUS=…
cargo test -p xarast-cli --test text_designs`. Ten `resample_*` cases
were added (minify either side of 2× per filter, a perspective plane,
contone in the three effect spaces, the aligned case, a bitmap
transparency): 130 cases.

**Draft/Final and export (T10.4.4)** were already wired and are unchanged:
`RenderQuality::image_filter` gives `Nearest` at Draft and `HighQuality`
at Final; export always builds its scene at Final (`xarast-io` model).
What is **not** wired is the document's smoothing flag
(`TAG_DOCUMENTBITMAPSMOOTHING`, "pixelated" intent), which lives in
`xarast-doc`/`xarast-app`: XARA-T-0273.

**A two-row guard band on every band's coverage.** Without it the band height
changed the picture by 1/255 at band boundaries, because the rasteriser's
strips start at the viewport edge. Antialiasing influence is local to one
pixel, so two rows make a banded render identical to an unbanded one; the
determinism suite asserts it over four band heights.

**Ramp construction contract (phase 8, XARA-US-0040).** `build_ramp` /
`build_transparency_ramp` in `xarast-render/src/ramp.rs` are the only table
builders, used by both backends through `RampCache`; the bias/gain curve
itself is `xarast_geom::BiasGain::map`, the one implementation (the
document's `Ramp::sample` calls it too). The identity short-circuit is in
`build_ramp`; `the_identity_profile_is_exactly_linear_over_2048_entries`
pins it exact. Lengths: 256 entries at `Draft`, 2048 at `Final` — and a
node a gesture previews (`Preview::attrs`) always builds 256, because a new
ramp is made every drag frame; the frame after the release walks it at the
session's quality again. Pixel comparisons of a preview against its commit
are exact only at `Draft`.

**Sine-mapped ramps (XARA-US-0017, 2026-09-24).** `RampEase` (`Linear`,
`Sin`) eases the parameter *before* the profile: entry `i` is the stops
sampled at `profile(ease(i / (len − 1)))`, `Sin` being `(1 − cos πt) / 2`.
That is the order of the document's `Ramp::sample`, which the SVG writer
bakes into stops, so canvas, export and fill tool agree. The API is
additive: `build_ramp_eased`, `build_transparency_ramp_eased` and
`RampCache::intern_eased`; the old names delegate with `RampEase::Linear`,
and the easing is part of `RampKey`. `xarast-app`'s `paint::ease_of` maps
`RampMapping` onto it for colour and transparency ramps. The original
never *draws* a sine mapping (its renderers never read the attribute) and
its `.xar` writer refuses to save one (`Kernel/fillattr.cpp:18639-18660`),
so no imported file carries it; it comes from `.xarast` files and the
fill tool.

**`RampCache` evicts least recently used.** The owner calls `begin_frame`
before a scene build and `evict(budget)` after (the walker, through
`Resolver::begin_frame` / `trim_ramps`, budget `RAMP_CACHE_BUDGET` = 16 MiB).
Only tables not used in the current frame are dropped, so every `RampId` of
the scene just built stays valid; freed slots are reused, and `trim_ramps`
empties the `transparency_ramps` entry of each evicted id (that side table
is indexed by ramp id and rebuilt when its length is wrong). The render
thread's resolver is a snapshot clone, untouched by eviction. A cache that
never calls `begin_frame` (export, corpus tools) never evicts.

### Pixel memory budget (XARA-US-0053, W10.5, 2026-09-24)

`crates/xarast-render/src/pixel_budget.rs` and `spill.rs`. **Built in
`xarast-render`, not `xarast-image`** as the phase table says: the levels
being budgeted are `ImageRef`'s base and mip pyramid, which live here, and
`xarast-image` is a leaf crate the renderer does not depend on. The
walker (`xarast-app`) supplies the one thing only it knows: how to
re-produce a base (`PixelSource`).

- **`ImageRef` is a handle to a shared `ImageStore`**: per level an
  `Option<Arc<Vec<u8>>>`, the proxy index, a spill file, the source. Its
  public surface is unchanged except that `level(i)` returns an owned
  `LevelBuf` (width, height, `Arc` data) instead of a borrow, and
  `level_count` no longer builds anything (it is computed from the
  dimensions). New: `with_budget`, `prepare`, `proxy_level`,
  `resident_levels`; `ImageRegistry::iter`.
- **Samplers pin.** `ImageSampler::new` pins, once per primitive, exactly
  the levels its plan can reach — the base for aligned / nearest /
  magnify / 1–2× widen, `level` and `level + 1` for a fixed
  minification, every level for a perspective plane — and samples from
  its own `Arc`s with no lock. Eviction drops the store's references
  only, so it can never pull pixels from under a band being drawn;
  memory is freed when the last pin goes.
- **Evictable vs proxy.** Levels `≥ proxy` are never evicted once built;
  levels `< proxy` (the base and the larger reductions) are, least
  recently *drawn* store first, whenever the evictable total exceeds the
  limit (`PixelBudget::enforce`, run after every pin, registration,
  `prepare` and `set_limit`). The proxy rule is in `perf.md`.
- **Byte-for-byte re-materialisation.** A reduced level is rebuilt from
  the nearest larger resident level with `resample::reduce_level`, the
  one 2 × 2 step the pyramid is built with (deterministic). A base comes
  back from its spill file, else its `PixelSource`. A source marked
  `is_cheap` (the walker's copy of native document pixels) is simply
  dropped; any other base is **spilled once**, the first time it is
  evicted, and read back from then on (1.2 ms per 16 MiB from the page
  cache vs. a decode). A base whose spill fails and that has no source
  is never dropped (only its larger reductions are), so there is always
  a way back; `BudgetStats::lost` counts the only other failure (a
  damaged spill file with no source: drawn transparent).
- **Draws vs reads.** Only a sampler's pin counts as a draw that may grow
  the proxy; `ImageRef::level` (equality, tests) reads without moving it.
  `ImageRef` equality is `Arc::ptr_eq` or else the base bytes (it
  re-materialises both if needed — rare: damage compares clones).
- **The global budget** (`PixelBudget::global`, what `ImageRef::new`
  registers with) takes `XARAST_PIXEL_BUDGET_MB`, else a quarter of
  `MemTotal` clamped to 512 MiB – 4 GiB (1 GiB if `/proc/meminfo` is
  unreadable). Tests and tools make their own (`PixelBudget::new`,
  `SceneWalker::with_pixel_budget`).
- **The spill directory** (`spill.rs`): `$XARAST_SPILL_DIR`, else
  `$XDG_CACHE_HOME/xarast/spill`, else `~/.cache/xarast/spill` —
  deliberately not `/tmp`, which is usually `tmpfs` (RAM). One
  `session-<pid>-<nanos>/` per budget that spilled, holding an exclusive
  `File::lock` on its `lock` file for life; a `SpillFile` deletes itself
  on drop, a `SpillDir` its directory. The global budget lives in a
  static and is never dropped, so its directory is left at exit; the
  next session's `SpillDir::create` sweeps every `session-*` sibling
  whose lock it can take (owner gone; flock is released by the kernel on
  death). Tested with a fake dead session, a lockless one and a live one.
- **Locks.** Store mutex → ledger mutex is allowed, never the reverse;
  `enforce` picks victims under the ledger lock and evicts with it
  released.

**Proof that pixels never change** — both run in the default gate:

- `xarast-render/tests/pixel_budget.rs`: every image case of the
  130-scene feature corpus, re-wrapped under a budget with
  limit 0 and proxy cap/default 1 (everything above 1 × 1 evictable),
  × {no source → spill, expensive source → spill, cheap source →
  re-copy} × {lazy pyramid, prepared pyramid} × {deterministic,
  interactive (parallel bands)}: byte-identical to the unbudgeted
  render; stats assert evictions, spill reads / source reads, level
  rebuilds and `lost == 0`. Plus proxy, cap, accounting, spill-file
  lifetime and equality tests.
- `xarast-app/tests/pixel_budget.rs`: the **24 corpus files with
  bitmaps** (Groucho2 and leafgirl asserted present), Final and Draft,
  640 × 480 fit, under a tiny spilling budget and a tiny budget with
  spilling off (re-decode from the original): byte-identical to an
  unlimited budget. One run: 297 evictions, 209 re-materialisations (all
  from spill, or all by re-decode), 69 level rebuilds, 88 spill writes
  (53 MiB), 0 lost. 28 s in the test profile.

The phase's 40 × 24 Mpx stress test is XARA-T-0282. The document's own
copies (`BitmapResource::pixels`, the encoded `original`) are outside the
budget: it covers the renderer's decoded levels.

### Drawing without waiting for an evicted base (XARA-T-0281, T10.5.5, 2026-09-24)

Before this, a sampler that needed an evicted level re-materialised it on
the render thread (a spill read, ≈ 1.2 ms per 16 MiB hot, or a decode,
tens to hundreds of ms), and Draft's `Nearest` always read the base, so a
zoomed-out Draft of many evicted photographs read every base back.

- **Draft picks its level by footprint.** `Plan::Nearest { level }`
  point-samples the level trilinear would start from
  (`nearest_level(lod_of(ρ))`: the base below 2 texels per pixel, level
  ⌊log2 ρ⌋ above) and pins only that level; a perspective plane
  (`Plan::NearestPerPixel`) picks it per pixel. **Draft output changes
  only for images minified by 2× or more**: each pixel is a texel of a
  2 × 2-box reduction instead of a texel of the base — less aliasing, and
  the value is a linear-light mean of the texels it covers rather than one
  of them. No golden moved (the `image_0_*` Draft goldens and every
  corpus Draft render are magnified or within 2×); every
  byte-for-byte budget test compares Draft to Draft and still holds.
  **Final and export are unchanged.**
- **`MissingLevels`** (`pixel_budget.rs`), a new `CpuConfig` field.
  `Materialise` (both presets, export, thumbnails, tests) is the old
  behaviour, byte-identical to an unlimited budget. `Substitute`: when
  the lowest level a sampler needs has no resident level at or above it
  in size (so it needs the base back), the sampler gets the best resident
  smaller level (`Pinned::Substitute`) and a `Plan::Fallback` (point for
  `Nearest`, bilinear in encoded sRGB otherwise); the image is stamped
  with a process-wide substitution clock (`substitution_tick`,
  `ImageRef::substituted_since`) and `BudgetStats::substituted` counts
  it. A level that is a reduction of a resident larger level is still
  rebuilt in place — arithmetic, exact, not I/O.
- **A substitute always exists.** `evict` now reduces the base down to
  the proxy before dropping it when no level at or below the proxy is
  resident (an image nobody prepared); levels `≥ proxy` are never evicted
  and the proxy only grows, so once a base is gone some smaller level
  stays. If none does (only a damaged spill with no source), the sampler
  falls back to waiting.
- **Bringing it back and repainting** is the caller's:
  `ImageRef::rematerialise()` (blocking, for a worker thread; counted in
  `BudgetStats::rematerialised` as well as `from_spill`/`from_source`),
  then `image_damage(scene, view, hit, max_rects)` (`damage.rs`): the
  display-list bounds of every leaf that samples a hit image (placed
  image, image paint, bitmap transparency) and of everything under a
  transparency scope or layer whose mask is one — the same bounds
  `scene_damage` uses, so redrawing them over the frame gives the full
  render (invariant 15). The render thread does exactly this
  (`app-core.md` decision 41), repainting through its ordinary
  `Plan::Repaint` path so the shell's tiles update.
- **Tests.** `tests/pixel_budget.rs`,
  `a_zoomed_out_draft_reads_no_base_back_and_the_final_converges`: four
  1200 × 900 photographs, every base spilled (product proxy sizes,
  limit 0). A Draft at 1/8 reads nothing back under either policy
  (level 3 is the proxy); a Draft at 1/4 under `Substitute` reads nothing
  back and substitutes; the Final at 1/4 under `Substitute` differs from
  the unlimited render, and after `rematerialise` + a repaint of
  `image_damage` it is byte-identical; under `Materialise` it is
  identical at once. The corpus version on the render thread is in
  `xarast-app/tests/pixel_budget.rs`. The existing byte-for-byte tests
  (130-case corpus × three sources × two pyramids × two configurations;
  24 corpus files × Final/Draft × spill/re-decode) are unchanged and
  green.

### Photo-adjusted images (XARA-US-0054, W10.6, 2026-09-24)

The renderer does not evaluate photo operations: a bitmap object with a
chain reaches it as an ordinary `Op::Image` naming a **derived**
`ImageRef` the walker made (`image.md`, "Photo adjustments"). What
changed here is one additive method, `ImageRegistry::replace(id, image)`:
the walker parks a 1 × 1 placeholder in the slot of a derived image no
object uses any more and reuses the slot for the next one, so a slider
dragged through many values does not grow the registry. Why it is safe:

- the render thread draws a scene with the resolver snapshot cloned at
  its rebuild (`Session::resolver_snapshot`), never the live registry;
- `scene_damage` checks a matched op's image ids against **both**
  resolvers by content (as it does for reused ramp slots), so an id that
  now names another image is damage exactly where it is drawn.

`Paint::Image::adjust` (`BitmapAdjust`, the sampler-time brightness /
contrast / gamma / saturation) is **not** used for photo adjustments and
stays at its default in the walker: sampling-time adjustment would run
per texel per frame, would not cover crop, orientation or levels, and
would not reach SVG export. A derived image is evaluated once and then
sampled, filtered and budgeted like any other image (an expensive
`PixelSource`, spilled on first eviction).

### Deferred images (XARA-T-0304, 2026-09-24)

`ImageRef::deferred(w, h, budget, source, standin)`: an image whose base
its source produces when first needed — the walker's committed photo
chain on a large picture (`image.md`, "The release, off the walk
thread"). The store starts with **no level resident** and `pending`;
`standin` is an approximation about the size of level `.0`, kept
**outside** `levels`, so nothing but a `Substitute` sampler ever sees it.

- **`Materialise`** (export, thumbnails, headless, tests): the first pin
  produces the base in place and reduces the levels it needs from it —
  the same bytes and the same levels as `with_budget` over the source's
  bytes (tested at four sizes × two filters, and level by level).
- **`Substitute`**: a pin that needs the base and finds no smaller
  resident level gets the stand-in (`Pinned::Substitute`, `Plan::
  Fallback`, which samples a level by its own size, so the stand-in's
  size need not be a pyramid size) and stamps the image, exactly like an
  evicted base; `rematerialise` then produces the base **and every
  level** (a pending store only: the render thread must not pay the
  ≈ 200 ms pyramid of a 24 Mpx image on its repair), drops the stand-in
  and clears `pending`.
- **`rematerialise` works unlocked** (for every store, not only deferred
  ones): it reads the spill handle (now `Arc<SpillFile>`) under the
  lock, produces the base and the pyramid with it released, and installs
  whatever is still missing. Before, it held the store's mutex through a
  decode, so a render thread pinning that image waited for the whole
  decode anyway. Tested with a source that blocks until told: a
  `Substitute` render finishes while it is blocked.
- **Accounting.** A stand-in counts as proxy bytes (never evicted, only
  dropped when the base arrives); a pending store has nothing evictable,
  so `evict` leaves it alone. Once made it is an ordinary image with an
  expensive source: spilled on first eviction.
- **Damage never produces a base.** `ImageRef == ImageRef` compares the
  bases when the stores differ, which for a pending image means running
  the evaluation — on the render thread, inside `scene_damage`, when an
  undo parks another derived image of the same size in the same slot.
  `scene_damage` now uses `ImageRef::eq_without_producing`: same store,
  or both made and equal; a pending image is "different" (a false
  "different" costs one repaint of the object). `PartialEq` itself is
  unchanged and exact.
- **A bitmap transparency is two resources** (T-0307 on top of T-0304):
  `transparency_refs` yields the image *and* its level ramp, so a
  deferred image under changed levels is damage and two pending stores
  are compared without producing either. Pinned by
  `damage::tests::a_deferred_bitmap_transparency_with_a_ramp_compares_without_producing`.

### GPU tests: on by default, serialised machine-wide (2026-09-24)

Tests that open a real `wgpu` device run by default again;
`XARAST_GPU_TESTS=0` skips them with a message. Commit 70272a2 had made
them opt-in after several agents ran `cargo test --workspace` at once and
the parity tests hit the display GPU concurrently (COSMIC Wayland, RTX 4000
+ Intel iGPU): the compositor hung and the desktop blacked out. Opt-in only
hid the tests; the cause was concurrency, so the fix is a lock.

`xarast_render::gpu_test_lock` (`#[doc(hidden)]`, std only, no dependency,
never called by the library or the binary) returns a guard from
`acquire(who)`. The guard holds a process-wide `Mutex` (parallel test
threads) and an exclusive `File::lock` (flock on Linux) on
`$XDG_RUNTIME_DIR/xarast-gpu-tests.lock`, or the same name in
`std::env::temp_dir()` (separate binaries, `cargo` runs and checkouts). The
kernel drops the flock when a process dies, so a killed test never leaves
it stuck. A waiting test prints `waiting for the GPU test lock …` every
30 s and panics after `XARAST_GPU_LOCK_TIMEOUT_SECS` (default 900), so one
hung GPU test fails the others rather than hanging them too. Users today:
`tests/parity_gpu_cpu.rs`, `tests/parity_tiles.rs` (both tests),
`benches/tiles.rs`, `benches/spike.rs` (GPU half), and in `xarast-shell`
`tiles::tests::the_gpu_tier_composites_byte_for_byte_as_the_cpu_tier` and
`gpu_errors::tests::a_validation_error_on_a_real_device_is_counted_not_fatal`.
CI needs no switch and never set `XARAST_GPU_TESTS`: as before 70272a2 the
tests skip on a runner with no adapter (or run on a software one; llvmpipe
passes the tile parity byte for byte here). Checked on the
reference machine by holding the file with `flock(1)`: the test waits and
times out without touching the GPU.

**The one exception: `wgpu`'s no-op backend** (XARA-US-0064,
2026-09-24). An instance created with `Backends::NOOP` and
`BackendOptions { noop: NoopBackendOptions::enabled(), .. }` enumerates
only the no-op adapter, loads no driver and runs nothing, so it takes no
lock and can run in parallel. `xarast-shell` enables `wgpu`'s `noop`
feature as a **dev-dependency only** (no new crate in `Cargo.lock`; the
binary never has it) for the device-loss test
`tiles::tests::a_lost_device_hands_its_picture_to_a_cpu_compositor_on_a_new_device`.
Use it for anything that needs a `wgpu::Device` to exist but never needs
pixels back from it; a no-op device returns nothing meaningful from a
read-back. Device loss itself is always simulated
(`GpuErrorSink::record_lost`); no test destroys or resets a real device.

Note for `cargo test -p xarast-render`: the two parity files are
`#![cfg(feature = "gpu")]`; alone, pass `--features gpu` (in a workspace
run the shell's dependency turns it on).

### The fill × blend golden matrix (XARA-T-0222, T8.5.5, 2026-09-24)

`corpus::fill_blend_cases()` is the phase-8 gate "one PNG per shape ×
{flat, graduated} × 10 blend modes": **160 scenes**, compared exactly
(gate A) by `tests/golden_fill_blend.rs` against
`tests/golden/fill_blend/<shape>_<family>_<flat|graduated>.png`.
Regenerate with `XARAST_UPDATE_GOLDEN=1 cargo test -p xarast-render
--test golden_fill_blend`.

- **Axes.** `FILL_SHAPES` are the eight editable `FILLSHAPE_*` shapes:
  flat, linear, circular, elliptical (a `Radial` with unequal axes),
  conical, diamond, mesh3, mesh4. `EXPOSED_FAMILIES` are the ten the
  transparency tool offers; `Bevel` and `None` are renderer-internal and
  are not in the matrix. "Flat" is `Transparency::flat(family, 128)`;
  "graduated" is a transparency ramp from 0 to 224 (identity profile,
  2048 entries), `Repeat::Simple`.
- **The graduated transparency uses the fill's own shape and frame** for
  the five scalar shapes, so the transparency sampler is tested per shape.
  Flat uses a diagonal linear frame (it has none). The two meshes use a
  `TranspSource::Mesh` of their own kind on the fill's square frame,
  levels 0/224/112 (and 176 at the fourth corner) — since XARA-T-0256,
  which re-blessed those 20 cells; before it they used the diagonal
  linear stand-in. A mesh-*shaped* `TranspSource::Gradient` still
  evaluates to level 0 (`grad_param` has no scalar for a mesh); nothing
  builds one.
- **Mesh repeat is not exercised here** (every cell is `Repeat::Simple`);
  the twelve `gradient_mesh*_{repeat,repeathq,mirror}_*` goldens of
  `all_cases` and `tests/mesh_transparency.rs` cover it.
- **Separate from `all_cases`** on purpose: export, PDF, determinism and
  parity iterate the corpus, and 160 more scenes would multiply their cost
  for no new coverage of their own concerns. 48 × 48, drawn at half a
  pixel per point from the corpus's own geometry (backdrop bands + star):
  ≈ 1.1 KiB a PNG, 183 KB for the lot; the test runs in 0.3 s.
- **Acceptance 14 per cell.** `every_exposed_family_draws_differently_…`
  asserts that, for every shape and variant, the ten families give ten
  different images (all 45 pairs). A family that fell back to another
  would pass once blessed; it cannot pass this.
- **These are regression locks, not correctness claims.** The exotic
  families are re-blessed when TODO 3 corrects them against the extracted
  tables (phase-08 risk table). Visually: Hue at 128 renders strongly
  saturated source hues; Contrast and Saturation are faint over the flat
  fill. None of that is judged here.
- No GPU parity: the GPU backend's compositing pass is deferred (TODO 2),
  so there is nothing on the GPU to compare the families with.

### Meshes tile mirrored (XARA-T-0256, 2026-09-24)

- **The sampler honours the repeat mode.** `paint::mesh_uv` folds both
  frame axes through `apply_repeat`; `mesh3_channel`/`mesh4_channel` are
  the one definition of the two meshes (the PDF exporter calls them
  too). `Simple` is the old clamp, byte for byte.
- **Tiled means mirrored.** The original has two mesh styles, "simple"
  (mapping = do not repeat) and "tiled" (every other mapping, the
  default included; `research/01 §8.3`). Which tiling "tiled" is was not
  documented; the preview `Fill Types simple.xar` embeds settles it.
  Rendered at the preview's 52 × 113 (8× supersampled, box-filtered) and
  scored per cell, mean |Δ| against the preview:

  | Cell | clamp | repeat | **mirror** |
  |---|---:|---:|---:|
  | 3-colour repeating (col 2) | 52.9 | 47.1 | **35.4** |
  | 3-colour repeating (col 4) | 56.9 | **35.5** | 38.7 |
  | 4-colour repeating (col 2) | 64.9 | 51.0 | **25.3** |
  | 4-colour repeating (col 4) | 48.3 | 31.8 | **23.2** |
  | 4-colour squashed (col 5, tiled) | 42.8 | 29.3 | **23.4** |
  | untiled cells (1, 3, 5) | equal | equal | equal |

  Visually the original's cells show diagonal stripes and checkers that
  only the mirrored tiling reproduces. `xarast-app` `mesh_repeat` maps
  tiled onto `Repeat::Mirror`; `Repeat`/`RepeatHq` stay available to the
  API and are sampled as plain tiles.
- **Mesh transparencies are per pixel**: `TranspSource::Mesh { mapping,
  repeat, levels: MeshLevels }`, on the colour mesh's frame and corner
  order, tiled the same way, in the family of `c0.mode` (it was the flat
  mean in Mix, whatever the mode). `tests/mesh_transparency.rs` pins it
  to the colour mesh (white mixed over black is `255 − t`) in all four
  repeat modes, ±1 for halves rounding the other way. Fractal and noise
  transparencies are still flat means, now in their own family.
- **Export follows.** PDF: a mirrored mesh is one sampled function over
  every period the shape covers, exact for four colours (a lattice of
  corner samples *is* the mirrored bilinear mesh); plain repetition and
  mesh transparencies are rasterised. SVG (`xarast-format` `bake.rs`):
  rows mirrored per period plus `spreadMethod="reflect"`, and mesh
  transparencies as luminance masks. Corpus export-check 0 failures;
  SoftShadow.svg 4.1 → 1.20.
- **Tile parity.** Tiled meshes cross many more rounding boundaries:
  assembled-from-tiles frames of `gradient_mesh3_mirror_affine` differ
  from the whole frame in 259 px, all by 1/255. `parity_tiles` now lets
  any frame whose every difference is 1/255 pass; larger ones keep the
  0.2 % budget.

### Blend families against the original's previews (XARA-US-0018, 2026-09-24)

The story's criteria need the original running (an x86-64 VM for the
least-squares fit R4.4 and the `CalcTransparencyX` dump R4.5); there is
none here, so they are **not met**. What *can* be judged is the preview
each `.xar` embeds (tags 60–64: GIF87a, 256 colours, ≤ 128 px), which the
original rendered. Method (scratch scripts, not committed; T-0248 owns a
committed tool):

1. Extract the preview; render our page at 60 dpi; find the crop of our
   render that, box-filtered to the preview's size, matches it best
   (coarse grid, then coordinate descent). Mean |Δ| after registration:
   GardenPlan 5.0, BLUECAR 6.1, Watch4 13.0, scope3 20.0, WATCH2 20.9,
   leafgirl 23.1, WATCH 30.6, Groucho2 44.3, SoftShadow 73.1 (live
   effects, text and bevels dominate the worst).
2. For each (file, mode) re-render with that mode's objects drawn in each
   of the other ten families and in None (an experiment-only build), and
   score every variant over the pixels where any variant moves by > 12.

Which transparency modes the corpus uses at all (`xarast-cli inspect
--fills`): Mix, Stained Glass (8 files), Bleach (7), Darken (Watch4, 2
attributes), Brightness (Watch4, 4). **Contrast, Saturation, Lighten,
Luminosity and Hue occur in no corpus file** — the previews cannot say
anything about them, and those are exactly the families that are
reconstructions.

| Family | Evidence (mean |Δ| over the affected pixels; ours vs best alternative) | Verdict |
|---|---|---|
| Stained Glass | best of 11 in all six files it shows in: Watch4 1519 px 27.9 (Darken 32.7, Mix 33.6); Groucho2 2073 px 51.0 (Mix 52.6); scope3 7716 px 20.8 (Darken 21.0); BLUECAR 795 px 14.2 (Mix 17.0); leafgirl 3344 px 28.6 (Mix 33.7); WATCH 5669 px 26.8 (Mix 27.5) | consistent with the original; Darken is close only where sources are near grey |
| Bleach | best in scope3 (7925 px, 20.6; Lighten ties, Mix 30.1) and BLUECAR (70 px, 21.2; Lighten ties); 2nd in Watch4 (284 px, 30.2 vs Mix 28.0, in a region dominated by the lid's missing engraving); 6th in SoftShadow, which misrenders overall | consistent; the Watch4 and SoftShadow counts are not evidence |
| Darken | Watch4 only, 38 px: 36.7 vs Stained Glass 35.5, Mix 36.6 | inconclusive at preview size |
| Brightness | Watch4 only: no pixel at preview size depends on it | no evidence |
| Contrast, Saturation, Lighten, Luminosity, Hue, Bevel | not in the corpus | unverifiable here |
| Luminance weights (R4.4) | a GIF palette of 256 colours at ≤ 128 px cannot resolve a 3-weight fit | unverifiable here |

Two real mismatches were found on the way and fixed: a Hue transparency
(type byte 31) read as Mix, because `TranspMode` had no Hue; and mesh
transparencies drawn in Mix whatever their mode. Visual note on the
fill × blend matrix: Hue interpolates the hue *byte* linearly, the long
way round when the two hues straddle red, so a graduated Hue shows a
sharp notch where the source's hue wraps (`mesh4_hue_graduated`'s right
arm); whether CDraw does the same is one of the VM questions.

### Live effects: the offscreen pipeline (XARA-US-0068, phase 13 A/B, 2026-09-24)

Phase 13's B1–B4 and the first consumer, feathering. Modules `blur.rs`,
`layer.rs`, `effect.rs`; the effect pass lives in `backend/cpu.rs`.

- **An effect is a scene push, rendered in device space per frame.**
  `SceneBuilder::push_effect(LayerEffect)` / `pop_effect` wrap content
  like a layer. The display list emits `DrawCmd::PushEffect { op, xf,
  content }` / `PopEffect` (`content` = the union of the wrapped
  commands' bounds, patched in at the pop; structural for
  `DrawCmd::bounds`, so the GPU tile planner puts it in every bin like a
  clip). Before its bands, the CPU backend renders every outermost
  effect that reaches the area (`materialise` → `render_effect`): the
  wrapped commands into an offscreen premultiplied buffer over
  `keep ⊕ reach` (keep = content ⊕ growth ∩ area), twice when the effect
  needs a silhouette, applies the effect, and the bands composite the
  result (premultiplied source-over, under the enclosing clips and into
  the enclosing layer) and skip what it wraps. Nested effects are
  rendered by their parent's pass. Nothing is baked into the document:
  zoom in and the feather is recomputed at the new resolution.
- **Why per frame and not a cached bitmap in the scene:** a bitmap made
  at one zoom and resampled at another is the fidelity loss phase 13
  exists to avoid, and the scene is view-independent. The cost was that
  an effect was recomputed on every frame and every repaint that reached
  it; since XARA-T-0314 the backend keeps its results at the exact view
  and reuses them while its content is unchanged ("The effect layer
  cache", below).
- **Exactness under repaint (invariant 23).** A pixel an effect keeps
  depends only on the view and the scene, never on the draw area:
  (1) coverage inside the region starts at a left edge fixed by the view
  and the effect chain (`left = min(parent_left, viewport.x0) − reach`),
  not at the region's corner, exactly as a frame's primitives start at
  x = 0 (invariant 15; `render_band` and `rasterise_clip` take `left`,
  0 for a frame); (2) region rows start on the frame's band grid, and the
  2-row guard already makes band tops invisible; (3) the region reaches
  `LayerEffect::reach_px` beyond the kept pixels in every direction,
  **beyond the viewport too**, and the display list keeps wrapped
  primitives within `clip ⊕ reach` (the clip grows at a push, is
  restored at the pop); (4) `scene_damage` inflates the damage of any
  change under an effect by the sum of the enclosing effects' reach
  (`Work::pad`, `leaf_bounds`' pad stack; `image_damage` too). An
  object crossing the view's edge is therefore *not* feathered at the
  edge (`effect_feather_beyond_view`). Pinned by the corpus-wide
  determinism tests (draw area, band height, threads) over the eight
  `effect_*` cases, and by the damage property, which now pushes
  feathers; removing the damage pad fails it within 2 000 cases
  (mutation-checked).
- **`blur.rs`.** `Kernel::Disc { radius_px }` (what we render with) and
  `Kernel::Gaussian { sigma_px }` (SVG's), `sigma_for_disc_radius` = r/2
  (`research/06 §6.8.1`), `MAX_RADIUS_PX` = 100 (the original's
  ceiling; clamped, the reduced-resolution render above it is
  XARA-T-0315). The disc is `dx² + dy² ≤ r²` **centred on a pixel**: no
  half-pixel shift for any radius (the original uses an integer
  diameter and compensates an even one elsewhere). Disc sums use per-row
  prefix sums and a rounded mean (the original truncates through a
  ≤ 0x800-entry table; ±1 level, invisible). The Gaussian quantises its
  weights to sum to exactly 2¹⁶ with a `u16` intermediate. Both are
  integer arithmetic, rows in parallel, thread-count independent
  (tested). `erode_plane` is a soft minimum filter (a pixel loses one
  level per level of coverage its disc lacks); `profile_table` applies a
  bias/gain to a transparency plane (0 opaque, 255 clear).
- **`layer.rs`** (B1–B3): `LayerTarget { bounds, pixel_width, content,
  pixels }`, `LayerContent::{Colour, Alpha, Silhouette}`,
  `render_subtree_to_layer(backend, scene, group, view, res,
  LayerRequest { pad_px, content, clip }, cancelled)`: the group's op
  range under its ancestors' transforms (not their clips, layers or
  effects), tile-aware (`clip`: a tile of the layer is exactly that part
  of the whole) and cancellable per band. **Silhouette** = every paint
  opaque black under Mix at level 0, images with their own alpha,
  layers composited plainly: the original cuts feathers and shadows from
  the object's *outline*, not from how transparent it is, so a
  half-transparent feathered object keeps its own transparency in the
  middle instead of vanishing (`effect_feather_transparent`). Nested
  effects count as their outlines (XARA-T-0321, "Nested effects in a
  silhouette pass").
- **Feather** (`LayerEffect::Feather { size, profile }`, `size` in
  document units): the original's feather is a **diameter**: it strokes
  the outline `size` wide (white) over the filled silhouette (black) and
  blurs the result by a disc of `size / 2`, so the fade runs from fully
  opaque `size` inside the outline to clear at the outline, and nothing
  grows (`Kernel/fthrattr.cpp`, `CreateSilhouetteBitmap`; facts only).
  Ours: silhouette → `erode_plane(r)` → disc blur `r` → profile → colour
  × mask, with `r = size·scale/2` capped at 100 px; `reach` = 2·⌊r⌋ + 1,
  `growth` 0. Not reproduced: the 0.75 px gap-closing outer contour
  (`DEFAULT_GAP_TOLERANCE_MP`) and the grey outline ramp the original
  uses below two pixels; above the 200 px diameter the original renders
  the silhouette at a lower resolution and scales it, we clamp.
- **Against the embedded previews** (T-0248 method: 60 dpi page render,
  registered crop, mean |Δ|; `feathers.xar` is the feather sampler):
  | File | Before | After |
  |---|---:|---:|
  | feathers (9 feathers: five profiles, a bitmap, a shape, a line, a group) | 19.52 | **3.20** |
  | Groucho2 (63 feathers, but 98 shadows stripped and the bitmap frame missing) | 43.12 | 43.89 |
  | Watch4 (3 small feathers) | 13.36 | 13.38 |
  `feathers.xar` goes from the worst match class to one of the best; in
  the "before" render its five stars were drawn at their unfeathered
  size, visibly larger than the preview's. Groucho2 is dominated by the
  shadows and the frame it cannot draw yet (XARA-US-0069); its feathers
  are soft now, which the preview agrees with locally but not in the
  mean.
- **PDF** rasterises an effect whole, alone (`Target::Effect`, from its
  push to its pop, over transparency), and skips what it wraps:
  `Compromise::Rasterised` "a live effect (feather) has no PDF
  equivalent". **SVG** (both dialects) draws it with a filter since
  XARA-T-0317: disc-like erosion, Gaussian σ = r/2, the profile as an
  alpha table (`export.md`, "Feathers in SVG"; feathers svg 20.15 → 2.00).
- **Cost** (`benches/render.rs`, group `effects`): disc blur r = 20 px over 1024² **5.9 ms** (budget ≤ 12 ms), Gaussian σ = 10 px 6.0 ms, erosion r = 20 px 5.7 ms, a 512² frame with one 40 pt feather 3.6 ms (release, this machine, other agents building). The
  corpus render (release, 100 %, warm): Groucho2 50 → 129 ms, feathers
  5 → 26 ms, Watch4 58 → 60 ms; every effect renders its region twice
  (colour and silhouette) plus an erosion and a blur, every frame. Since
  XARA-T-0314 only a frame whose effect content or view changed pays it.

### The effect layer cache (XARA-T-0314, phase 13 B9, 2026-09-24)

`effect_cache.rs`; `CpuBackend` owns one `EffectCache`
(`effect_cache()`, `effect_cache_mut()`), on by default. Results are
byte for byte what recomputing gives.

- **Key = everything a kept pixel depends on** (invariant 23): the scene
  ops from the effect's push through its pop, compared by **op
  equality** (the effect's parameters are the push op), plus every ramp
  and image those ops name, compared by content with the damage diff's
  own rule (`damage::ramp_tables`, `damage::same_image`, factored out of
  `scene_damage` for this; a pending deferred image is "different",
  never produced); the effect's device transform and the view's
  transform **bit for bit**, viewport, quality and dpi; the pass's
  coverage left edge, band height, SIMD pin and luminance weights. A
  cheap hash (`damage::key` per op) finds the candidate, equality
  decides. Not the walker's `ContentHash` (invariant 16), and no
  quantised scale: a layer from another zoom would be the resampling
  the offscreen pipeline exists to avoid.
- **Invalidation is the key.** A change under an effect changes the key
  exactly when `scene_damage` would call the effect's pixels damage
  (an op, a resource, an ancestor transform); anything else leaves it.
  There is no invalidation call to forget.
- **The draw area is not in the key; valid rectangles are.** A result
  rendered for one area is exact over its `keep` (the effect's content
  ∩ that area), for any later area. A key holds up to 8 layers, each with
  its valid rectangle. A lookup cuts the new `keep` by every layer's edges,
  takes each cell from the first layer that contains it (runs along a row
  merged into one `Piece`), and renders the union of the uncovered cells
  as one more layer. So the four columns of a `Final` each store their
  own layer, and a later repaint that straddles two columns is assembled
  from both. `composite_effect` composites each piece clipped to its own
  rectangle; pieces are disjoint. Clipping even a fresh layer to `keep`
  changes nothing: outside `content ⊕ growth` an effect's output is
  transparent.
- **Only outermost effects are cached.** A nested effect is rendered by
  its parent's pass and lives in the parent's layer.
- **Substituted images are not kept.** Under `MissingLevels::Substitute`
  (the render thread), a result is stored only if the process-wide
  `substitution_tick()` did not move while it was rendered; otherwise the
  repair after `rematerialise` would be served the stand-in. Another
  thread's substitution only loses a reuse. Under `Materialise` the
  clock is not consulted (it made the tests flaky when a parallel test
  substituted).
- **Memory: its own ceiling**, `DEFAULT_EFFECT_CACHE_BYTES` = 128 MiB
  (≈ 32 Mpx, fifteen 1080p frames of layers), `set_limit(0)` turns it
  off. It counts layer pixels, each key's copy of its ops and resource
  tables (not path geometry shared by `Arc`). Not the image
  `PixelBudget`: that budgets decoded image levels with a spill and
  re-materialisation path, which a layer (cheaper to recompute than to
  spill) does not need. LRU by layer, **except that a layer used in the
  current frame (render call) is never evicted to make room**: a frame
  whose effects do not fit keeps what fits and recomputes the rest,
  instead of evicting its own layers in a cycle and hitting nothing.
- **Pans are not served from it (decision).** The transform is in the
  key, so a translated view misses. Reusing a translated layer would
  need the layer's pixels to be translation-covariant, and they are not
  guaranteed to be: invariant 23 fixes the coverage origin by the
  *view* (`left = min(parent, viewport.x0) − reach`) and the rows by the
  frame's band grid, not by the content, so after a pan by (dx, dy) the
  same content is rasterised from a different origin relative to it (a
  primitive clipped at `viewport.x0 − reach` starts there; a vertical
  pan that is not a multiple of the band height moves the band tops; the
  `f64` device geometry of a translated transform can round differently
  into `f32`). Invariant 15's history shows that such shifts move
  antialiased pixels by 1/255. Making it covariant would mean an origin
  fixed by the content, which re-opens invariant 23. And the gain is
  nil: a whole-pixel pan already moves the pixels on screen
  (`scroll_surface`), and the strips it exposes were outside the old
  view, where no kept pixel was ever computed. Zooming back to a view
  it has seen (bit-identical transform) does hit.
- **Tests.** `tests/effect_cache.rs`: the eight `effect_*` corpus cases ×
  {deterministic, interactive}: repeated whole frames byte-identical to
  a cold backend's (hit counts asserted); columns, the determinism
  test's five rectangles and a strip drawn over the frame with one warm
  backend, byte-identical after every rectangle; a colour or feather
  change under the effect misses and a change beside it hits; a reused
  ramp slot misses; a pan misses and stays exact; a zero ceiling stores
  nothing and a one-effect ceiling keeps its first layer; a deferred
  image drawn from its stand-in is not kept and the frame after
  `rematerialise` is exact. `properties::repainting_the_damage_with_a_
  warm_effect_cache_gives_the_new_frame` (512 cases): one backend draws
  the old frame, the new damage, a random rectangle and a whole frame,
  each byte-identical to a cold full render. Mutation-checked: comparing
  only the op count instead of the ops fails the property; compositing a
  piece's whole layer instead of its clip fails the assembly test.
- **Numbers** in `perf.md`, "The effect layer cache".

### Shadows (XARA-T-0318, phase 13 C1–C5 and C9, 2026-09-24)

The second consumer of the pipeline. `LayerEffect::Shadow(ShadowEffect)`
wraps a shadow controller's subtree (the walker's `shadow_of`, inside the
controller's own feather) and draws the shadow **beneath** what it wraps:
silhouette → (glow) `blur::dilate_plane` by the glow width →
resample through the document-space `map` → disc blur of **half the
penumbra** → profile → colour × opacity → content over it.

- **Facts from the original** (`Kernel/nodecont.cpp`, `nodeshad.cpp`;
  facts only): the penumbra is the blur's *diameter*
  (`pixBlurDiameter = penumbra / pixel width`), so r = penumbra/2, like
  the feather; a wall shadow translates the silhouette by the offset; a
  glow contours it outwards by the glow width; a floor maps it through
  translate-to-(centre x, bottom y) · scale(1, height) · shear(tan angle)
  · back (the silhouette's `ApplyShadowTransform`; the bitmap itself is
  CDraw's `CreateShadow(floor, height, angle°, …)`, closed); the shadow is
  painted in the shadow node's fill colour through a bitmap transparency
  whose start level is `round(255·(1 − darkness))` and end 255, i.e.
  opacity `255 − that` where the blurred silhouette is full; the profile
  is applied as the bitmap's palette with the **bias negated**.
- **`map` is document space, applied in device space.** `apply_at(…, xf,
  origin)` (new beside `apply`) gets the view transform and the region's
  device origin; the warp is `xf⁻¹ · map⁻¹ · xf`, bilinear, a function of
  each pixel's absolute device position only (invariant 23 holds:
  `effect_shadow_*` are in the determinism tests with the feathers).
  `displacement` (the walker's bound on how far `map` moves the source's
  bounding box, at its corners) + dilation + blur reach + 2 px slack is
  both `reach_px` and `growth_px`, so the region, the display list's
  clip, the damage pad and the kept rectangle all cover the moved
  shadow.
- **Outside the region is uncovered, always.** The first dilation was
  the complement of `erode_plane`, which makes outside the plane
  *covered*; the region's pixels outside `keep` are composited too, so
  every glow drew a frame at the region's edge. `dilate_plane` is now
  `min(255, Σ disc)` directly (the same formula, outside = 0). Any new
  per-effect filter must keep "nothing appears from outside the plane".
- **Sample positions come from absolute device coordinates only.** The
  first warp subtracted the region's origin inside the float expression;
  an export strip's region starts elsewhere than a frame's, samples
  moved by an ulp and `tests/export.rs` caught a strip-height dependence.
- **`DisplayList::with_commands` adds an effect's growth to the list's
  bounds**, as `build` does at the pop. Without it PDF's effect raster
  (a selection holding only the effect) of a shadow whose object is off
  the image drew nothing (`effect_shadow_beyond_view`, caught by
  `xarast-io/tests/pdf.rs`'s Poppler comparison).
- **Colour and darkness**: the walker resolves the colour parameter and
  multiplies its alpha by `ShadowParams::opacity_level()`; the renderer
  never sees a document colour.
- **Not reproduced**: CDraw's floor bitmap (a progressive blur or
  perspective, if it has one, is unknowable here; ours is the affine map
  plus a uniform blur); the original's half-pixel offset for even
  diameters; radii above 100 px are clamped (XARA-T-0315). (A feather
  around a shadow used to see the shadow's half-transparent result in its
  silhouette and erode it away; fixed by XARA-T-0321, see "Nested effects
  in a silhouette pass".)
- **Against the embedded previews** (T-0248 method: 60 dpi page render,
  registered crop, mean |Δ|, each render registered on its own):
  | File (shadows) | Before | After |
  |---|---:|---:|
  | SoftShadow (3) | 73.89 | **7.75** |
  | Groucho2 (81) | 43.99 | **38.87** |
  | testimp1 (1) | 20.05 | **18.25** |
  | Girard_simple (9) | 15.36 | 15.52 |
  | Watch4 (4) | 13.81 | 13.81 |
  | feathers (control, 0) | 2.92 | 2.92 |
  SoftShadow's background and its green "Software" were inside shadow
  controllers and were stripped; Groucho2 gains its eyes', pencils' and
  hands' depth but still misses the bevelled frame (inside shadow
  controllers too: the bevel is stripped, so those sources are empty).
  Girard's dial now appears where the bevelled rim that covers it in the
  original is still missing; Watch4's four shadows are cast by blends and
  moulds, which are still opaque, so their silhouette is empty.
- **Cost**: Groucho2, release, 100 %, `xarast-cli render`, loaded
  machine: 71–85 → 108–136 ms (81 more effects, each two region passes, a
  resample and a blur, every frame) until the layer cache (XARA-T-0314).

### Shadows under the effect layer cache (merge of T-0318 onto T-0314)

- **The key already holds everything a shadow reads** (invariant 25
  verdict): the `ShadowEffect` (map, displacement, spread, blur, profile,
  resolved colour) is inside the push op, compared by equality; the
  warp `xf⁻¹ · map⁻¹ · xf` reads the effect's device transform, which is
  in the key bit for bit (`Setting::xf`, plus the view's transform and
  viewport). The **region origin is not a dependency**: the warp samples
  from absolute device positions only and the region always spans
  `keep ⊕ reach`, so a layer is valid over its `valid` rectangle wherever
  its region started. `tests/effect_cache.rs`
  (`shadow_layers_are_exact_over_columns_and_strips`,
  `a_shadow_under_another_view_misses_and_stays_exact`) and the damage
  properties (whose generator now pushes wall, glow and floor shadows)
  hold it.
- **An effect's content includes its nested effects' growth.** Found by
  the damage property with shadows: the display list unioned a child
  effect's *content* into its parent's, not how far it draws, so a
  strip that culled a shadowed object (its shadow still in the strip)
  shrank the parent's region and cut the shadow: strip ≠ frame. The
  pop now unions `content ⊕ growth`. An effect whose content is empty
  (all culled) is skipped: `DeviceRect::EMPTY.inflated(g)` is a real
  rectangle round the origin, and it rendered and cached a layer of
  nothing (and grew the list's bounds to the origin).

---

### Bevels (XARA-T-0322, phase 13 D1–D4, D7, D8, 2026-10-15)

The third consumer of the pipeline. `LayerEffect::Bevel(BevelEffect)`
wraps a bevel controller's subtree (the walker's `bevel_of`, inside the
controller's own feather) and needs only the silhouette:

1. **Pieces of outline**: pixels the outline crosses (coverage in
   1..254, or 255 with an empty 4-neighbour). Each piece's inward normal
   is the Sobel gradient of the coverage; where the outline crosses it is
   `centre − (coverage − ½)·n`.
2. **Faces**: an exact separable feature transform (column sweeps, then
   the Felzenszwalb–Huttenlocher lower envelope per row, integer
   arithmetic, `effect.rs` `feature_transform`) gives every pixel within
   `width + 2` px its nearest piece. Inside the piece's strip
   (`|across| ≤ 1 px`) the pixel takes the piece's normal and its
   perpendicular distance (a face, the original's trapezium); beyond it,
   the radial direction and distance (a round join, the original's
   triangle fan). Then the rising directions are averaged over 3 × 3
   (1-2-4 weights): without it a straight slanted edge flickers between
   face and join and a round bevel streaks (the first goldens showed it).
3. **Lighting**: `xarast_geom::bevel::shade(style, u, up, light)`: the
   style's slope at `u` (across the band from its low edge: the outline
   of an inner bevel, the far edge of an outer one), a Lambertian ratio
   against the flat plane, brightening × contrast and darkening ×
   contrast² (the original's `SetBevelLightness(contrast·2.56)` /
   `SetBevelDarkness(contrast²·0.0256)`, `Kernel/nodebev.cpp:1927-1928`),
   packed to an index with **128 exactly flat**.
4. **The `Bevel` family's table** (D4) applies the index: to the
   content's own (unpremultiplied) colours for an inner bevel; to a band
   of `colour` composited **beneath** the content for an outer bevel
   (antialiased at the band's far edge). The table had no integer
   neutral (128 brightened by one level, 127 darkened); it is now exact
   at 128 and the family's transparency fades towards 128
   (`blend_bevel_64/192` goldens moved by that).

- **Facts from the original** (facts only): the bevel node is the
  controller's **last** child for an inner bevel (drawn over the object)
  and the **first** for an outer one (`tools/opbevel.cpp:1110-1127`,
  `:1344`); it carries copies of the first object's fill, transparency,
  mappings, effect and join (`:1318-1341`); a flat-filled bevel is drawn
  as the lighting bitmap with a palette translating the fill colour, a
  non-flat one as its fill under a `TT_BEVEL` bitmap transparency
  (`Kernel/nodebev.cpp:1495-1590`); `GDraw2_SetDIBitmap` gets the style
  and `light angle, 90 − tilt`, with styles swapped and the angle turned
  by 180° for some styles of an inner bevel (`Kernel/beveler.cpp:839-893`).
  The face generator (`GenBevelFaces`), the lighting map and the two
  tables are in the closed CDraw.
- **The light**: the stored angle is the light's **direction of travel**
  (degrees, document frame): the light sits opposite. Fitted against
  Groucho2's preview (angles 300–345, lit from the upper left): +180°
  scored 24.94, +0/+90/+270 25.85–27.35, mirrored readings 25.23–26.97.
  The tilt is the light's elevation (only 45 and 32 occur).
- **Cross-sections are Xarast's own** (`xarast_geom::bevel`, D2's
  empirical determination in a VM is not done): flat = 45° chamfer;
  round = quarter circle; half round; frame = ridge; mesa 1/2 = chamfer
  to a plateau over ½ / ⅓; smooth 1 = cosine S, smooth 2 = quintic;
  point 1/2a/2b = concave/convex ridges; ruffles = cosine waves. The
  inner-bevel style swap and angle flip of the original are not
  reproduced (their meaning is inside CDraw). Corpus styles: flat 13,
  smooth 1 7, point 1 2.
- **Joins and the inner paint**: done in XARA-T-0326 (below). An inner
  band's concave corners stay round and its convex ones mitred whatever
  the join.
- **Width**: `indent` in device pixels, capped at `MAX_BEVEL_PX` (2048);
  reach `ceil(width) + 2` (a mitred outer band: `ceil(10·width) + 2 +
  11`), growth that much for an outer bevel, 0 inner.
- **Against the embedded previews** (re-measured with a scratch
  registration over the drawing frame at 60 dpi; it differs from the
  T-0248 scripts, so compare only within this table):
  | File | Before (cbdc3bd) | Import only | After |
  |---|---:|---:|---:|
  | Groucho2 (21 bevels) | 47.03 | 26.05 | **24.94** |
  | testimp1 (1) | 19.92 | 19.41 | **18.97** |
  | feathers, SoftShadow (controls, 0) | 5.50, 18.90 | — | 5.50, 18.90 |
  Most of Groucho2's gain is the frame itself (its objects were stripped
  with the bevels); the lighting adds the rest.
- **Cost**: `xarast-cli bench render --whole --doc Groucho2.xar --runs
  11`, release, load ~9: warm median 7.19 → **7.35 ms** (the 21 bevels
  hit the effect cache), first frame 73 → 109 ms, peak RSS 62.0 → 77.1
  MiB; at 400 % one Groucho2 frame is ~1.0 s.

### Bevel follow-ups (XARA-T-0326, 2026-10-15)

- **The feature transform got cheaper; nothing was cached.** The cost
  item asked for the lighting map to be cached apart from the content's
  colour. Measured first: in Groucho2's first frame the 27 bevel renders
  were ~36 ms, of which the lighting (`BevelEffect::apply`) was ~13 ms and
  the two offscreen passes over the bevelled content ~18–23 ms; at 400 %
  171 ms, 78 of them lighting. A lighting cache would only serve an edit
  of a bevelled object's colour (a new frame or zoom changes the
  silhouette), so the transform itself was made cheaper instead: the
  column sweep runs in parallel blocks of 64 columns with `i32` rows,
  each band pixel's face is computed inside the row pass (no `nearest`
  plane), and the faces are `[f32; 3]` with a NaN sentinel (17 bytes a
  pixel of scratch instead of 49); rows run in tasks of ≥ 1 Ki pixels
  (16 Ki halved the parallelism of Groucho2's 30–55 Ki-pixel regions). Byte-identical output (goldens; Groucho2 at 100/300 %,
  testimp1 at 100/400 % PNGs). `effects/bevel_{inner,outer}_r16_1024`
  (new): 30.2 / 37.9 → **5.3 / 5.1 ms**; in Groucho2's first frame the
  lighting's sum 16.5 → 14.5 ms at 100 % (latency-bound small regions),
  87.9 → 46.4 ms at 400 % (whole frame 437 → 386 ms). Numbers in `perf.md`, "Bevel
  cost". The passes are the bevelled content itself, drawn twice; the
  peak RSS is the frame's content (its bitmap and noise fills), not the
  lighting (disabling the lighting leaves it at 75 MiB).
- **Joins** (`BevelEffect::join`, the walker's `bevel_of`). Facts from
  the original: the beveler gets the join type applied to the bevel node
  (`Kernel/nodebev.cpp:845-850`, `:624-629`; RoundJoin only if none is
  found), passes it to its face generator with a mitre limit of `10<<16`
  and a line width of twice the indent (`Kernel/beveler.cpp:284-293`);
  the outer path is contoured with the same join (`nodebev.cpp:479-501`).
  The bevel node's own attributes copy the first object's join when it has
  one (`tools/opbevel.cpp:1321-1340`); no corpus bevel node carries one,
  so the join is the one resolved at the generated node (the controller's
  attributes and its ancestors'). **Corpus: 21 of 22 controllers have no
  join attribute in force** and get the document default, `BevelledJoin`
  in the original (`Kernel/attr.cpp:1967-1980`) and in the model since
  XARA-T-0329 (it was `Mitre`); the other (testimp1) has an explicit
  mitre in force.
- **How corners are found** (renderer only, from the silhouette): an
  outer band with a mitre or bevel join takes, for each piece, the pieces
  2–8 px away (`CORNER_RING`), sorts their normals by turn from its own
  and splits them at the widest gap; a gap of ≥ 20° (`CORNER_GAP`) with
  ≥ 2 pieces each side (each edge's pieces within 20° of its outer end,
  at least half of its side: the corner's own rounded pieces drop out)
  and a convex corner (each edge's points behind the other's line) is a
  corner. Each edge is a line fitted through its pieces' outline points
  (principal axis; normals tilt ±7° along a straight antialiased edge,
  the points only ±0.3 px). Past the corner (beyond both edges' ends)
  the band is the far face up to the mitre limit, else the straight cut
  (distance across (d1 + d2) / (1 + cos φ)); beside one edge, its face;
  beside both (fit error inside a convex corner), the far face. A curve's
  normals spread evenly (no gap) and round as before. Concave corners of
  an outer band need nothing: both strips cover them.
- **Dead ends**: (1) the extremes of the ring's spread with a 6° cluster
  tolerance: coverage steps tilt piece normals ±7°, so nearly-axis-aligned
  squares were missed; (2) a 2–4.5 px ring: line fits over 2 px tilted
  7°; (3) testing the strip (`across ≤ 1`) before the corner: the corner
  piece's own normal is the bisector, so the whole diagonal became a face
  and the corner stayed round; (4) splitting the ring by the sign of the
  turn: pieces next to the tip have normals near the piece's own and fell
  on both sides; (5) face beside an edge whenever behind the other's line
  (no tangent test): at a sharp tip that region reaches past the vertex,
  so the edges' bands crossed beyond it.
- **Invariant 23 again**: positions in the corner code *and* the piece's
  outline point are relative to the piece's pixel (integer differences +
  fractions). Region-local absolute positions rounded differently per
  strip and moved six pixels by one level in the strip export test.
  The mitre's reach adds the ring and its stencils (`+ CORNER_RING + 3`).
- **Inner paint** (`BevelEffect::paint_inner`, `BevelParams::flat_paint`):
  when the bevel's own colour or its first object's fill is flat, the
  inner band is that colour, lit, painted over the content (antialiased
  at the outline and at the band's inner edge), as the original's bevel
  node (last child, a copy of the first object's flat fill, drawn as the
  lighting bitmap through a palette of that colour) does; otherwise the
  content's own colours are lit, which matches the original's non-flat
  bevel node (the fill under a `TT_BEVEL` transparency) for one object.
  The first object's transparency, also copied onto the bevel node, is
  applied since XARA-T-0328 (below). Corpus: no change (Groucho2's inner bevels are bitmap
  filled). Golden `effect_bevel_smooth_inner_painted`.
- **Corpus effect**: Groucho2 at 100 % byte-identical; at 300 % and
  testimp1 changed: testimp1's frame-like outer bevel now has mitred
  corners (a picture frame) instead of round ones. Not re-scored against
  the previews (the T-0322 scratch registration is not in the tree).

### Bevel leftovers (XARA-T-0328, 2026-10-16)

- **A painted inner band takes its object's transparency.** The original's
  bevel node copies the first object's fill *and* its transparency
  (`tools/opbevel.cpp:1318-1341`, `AttrTranspFillGeometry`).
  `BevelParams::flat_paint_opacity` is that object's flat **mix**
  transparency as an opacity (`255 − level`); the walker's `bevel_of` folds
  it into the painted colour's alpha (`BevelEffect::colour.a`, which the
  band already multiplied), and the SVG bake into the band mask's opacity.
  Any other mode (stained glass, bleach, ...), a gradient transparency and
  no transparency leave the band opaque, as before: those need the bevel
  node composited through its own transparency, and no file has one.
  Tests: `walker::tests::a_painted_inner_band_is_as_transparent_as_the_
  object_it_copies`, `svg_read::a_painted_inner_band_takes_its_object_s_
  flat_transparency`. Not applied to an *unpainted* inner band (the
  content's own colours are lit, and the content already carries its
  transparency).
- **Inner bands' concave corners still round: documented, not changed.**
  What exactly differs: `BevelEffect::apply` forces `Join::Round` for an
  inner band, so the corner finder (`Corners::find`, which looks for
  *convex* corners of the silhouette, where an outer band's strips leave a
  wedge) never runs for it. At a *convex* corner of the object an inner
  band is mitred by construction (the two strips cross), whatever the join,
  so the join is moot there. At a *concave* corner the original
  turns the band's inner edge with the join (a mitre reaches up to ten
  widths into the content, a bevel cuts it); we draw the distance
  transform's arc. The visible difference is the corner's wedge: a
  quarter-disc against a point (mitre) or a chord (bevel, the original's
  *default*, `BevelledJoin`) of a band-width radius, at most 0.29 widths
  deep for the bevelled join and 0.41 for a 90° mitre. Doing it
  needs a concave-corner finder (the same normals analysis on the inverted
  coverage, with the face directions of the *inner* band) and, for a mitre,
  a search window up to ten widths deep over the content (estimates: days
  of work, and a search ~20 times wider at `MAX_BEVEL_PX`). Not worth it
  yet: all 19 corpus inner bevels (Groucho2's frame bands) carry the
  default bevelled join, where the difference is the 0.29-width sliver of a
  chord against an arc at each concave corner.
- **Corners from 2–8 px of outline** (the "very small shapes still round"
  item): unchanged, same reasoning; text-sized bevels are a few pixels wide
  anyway.
- **The damage property pushes bevels** (`tests/properties.rs`,
  `Step::PushBevel`): inner and outer, mitre/bevel/round joins, flat and
  round sections, painted inner bands, a band of 0.5–10 pt, restyled by the
  `Restyle` edit. Mutation-checked: dividing `reach_px` by four for a bevel
  fails `repainting_the_damage_gives_the_new_frame` and its warm-cache twin
  within the 2 000 cases.
- **The SVG bake is resolution independent and follows the join and the
  inner paint** — see `xarast-format.md`, "Bevels". testimp1 svg 0.69 →
  0.56 (72 dpi) and 0.36 → 0.28 (144), Groucho2 6.81 → 5.72 and 4.89 → 3.94.

### Nested effects in a silhouette pass (XARA-T-0321, phase 13, 2026-10-15)

An effect that needs a silhouette renders its content twice; the second
pass draws every paint opaque. Until now an effect nested in that content
was composited there with its **colour result's alpha**, so a feather
around a shadow saw the shadow at its darkness (half transparent) and its
erosion removed most of it (`effect_shadow_feathered`).

- **Facts from the original** (facts only):
  - A feather's silhouette is a *path*: the outer contour of its node's
    outlines (`Kernel/fthrattr.cpp:1407-1443`, `GenerateOuterContour` →
    `NodeContour::GenerateContourPathForNode`, `Kernel/nodecntr.cpp:1184-1231`),
    filled black and stroked white (`CreateSilhouetteBitmap`). The
    conversion is a `PathBecomeA` with `SetDoSilhouette` and
    **`SetDoShadowSilhouettes`** on (`nodecntr.cpp:1215-1216`; the
    `fIncludeShadows` parameter is ignored, it is always on).
  - With shadow silhouettes on, a shadow controller converts its shadowed
    node to outlines, applies **`ApplyShadowTransform`** to a copy (wall:
    translate by the offset; glow: contour out by the glow width; floor:
    the floor matrix) and passes back both, the copy first; a glow omits
    the object, which its contour encloses (`Kernel/nodecont.cpp:1076-1113`).
    The transform's own comment: the result does *not* take the penumbra
    into account. So the shadow counts **opaque, unblurred, its darkness
    ignored**.
  - Without shadow silhouettes, the shadow node itself passes back
    nothing (`Kernel/nodeshad.cpp:1058-1248`, `NodeShadow::DoBecomeA`:
    only `BECOMEA_REPLACE` builds a path), its siblings their outlines.
    A bevel's silhouette is such a conversion (`ContourBecomeA`,
    `Kernel/nodebev.cpp:500`, `:854`; no shadow flag).
  - A bevel node passes back its own ink path, filled
    (`Kernel/nodebev.cpp:2499-2528`): the band, outside the object for an
    outer bevel, inside it for an inner one. A feather is an attribute;
    a conversion ignores it. A live effect under a silhouette conversion
    converts its children (`Kernel/nodeliveeffect.cpp:1507`).
  - A **shadow's** silhouette is different: a *render* of its subtree into
    a 32-bit offscreen bitmap (`nodeshad.cpp:3773-3778`,
    `CreateOffscreenRR`; `RenderTreeAtomic`, `:3999-4011`), whose alpha
    byte the blur reads (`Kernel/bshadow.cpp:245`, `:277`, byte 3 of each
    pixel). A glow contours that bitmap (`ContourBitmap`, threshold 0xFE,
    `NEW_GLOW_SILHOUETTE` is defined at `nodeshad.cpp:193`). Nested
    effects therefore count as drawn, alpha included.
- **The rule** (`LayerEffect::nested_silhouette`, `NestedSilhouette`):
  | Outer effect | Sees a nested effect as | Nested shadow |
  |---|---|---|
  | feather | its outline (`Outline { shadows: true }`) | content ∪ outline moved by `map` / grown by `spread`, opaque, unblurred |
  | bevel | its outline (`Outline { shadows: false }`) | its content only |
  | shadow | its rendered result (wall, floor: the colour pass's alpha, XARA-T-0327; glow: the opaque pass, `Rendered`) | as drawn |
  What an effect contributes as an outline (`LayerEffect::outline_at`): a
  feather or an inner bevel its content's silhouette; an outer bevel its
  content plus its band (the bevel's own `apply` with an opaque colour, so
  the band is exactly what it paints); a shadow as in the table. What a
  shadow shadows is seen with shadows off (`NestedSilhouette::inside`:
  the original starts a fresh conversion for the shadowed node).
  Outlines composite source-over (two opaque shapes into one plane), not
  as a geometric union.
- **Where it lives**: `backend/cpu.rs` `silhouette_plane` (the
  silhouette pass of `render_fresh`), `materialise_outlines` and
  `render_outline`: a nested outline is rendered over the same region,
  band grid and coverage edge `render_fresh` would use for the nested
  effect, so invariant 23 holds; never cached (the outermost effect's
  result is, and its key covers every op). `LayerContent::Silhouette`
  (`render_subtree_to_layer`) follows the feather's rule.
- **Goldens**: `effect_shadow_feathered` (2133 px moved: the shadow now
  survives whole and fades at its own outer edge) and
  `effect_feather_nested` (1968 px: the inner feather's whole outline is
  in the outer silhouette) moved; new `effect_shadow_in_bevel` (the bevel
  lights the square only, the shadow is drawn whole) and
  `effect_bevel_outer_feathered` (the feather fades the band's edge).
  Mutation-checked: a bevel seeing nested effects `Rendered`, or with
  `shadows: true`, fails `effect_shadow_in_bevel`. Unit tests in
  `effect.rs` pin `outline_at` per kind.
- **Corpus impact: none.** An instrumented render of all 59 corpus files
  rendered no nested outline (no file puts a feather or a bevel around
  another effect; Groucho2's bevels sit *inside* shadows, which keep the
  rendered rule).
- **Inner shadows (C5): nothing needed now (decision).** The original's
  shadow types are none, wall, floor, glow and an unimplemented
  `SHADOWTYPE_FEATHER` (`Kernel/nodecont.h:111-118`; the feather case is
  `ERROR3("Unimplemented!")` at `nodeshad.cpp:2271`), so no `.xar` can hold an
  inner shadow, the model has no `ShadowKind::Inner`, and no tool makes
  one. It is a Xarast-native extension (`research/06 §6.8.1`,
  `xarast:type="inner"`). When it lands (with C7's tool or the native
  format, whichever needs it first) its rules are fixed here: drawn
  *over* the content, the moved, blurred silhouette's complement
  intersected with the silhouette ("out", then "in", against the source
  alpha); reach = displacement + blur radius + slack, **growth 0**; as a
  nested effect in an outline silhouette it contributes its content only
  (it draws nothing outside it), so `outline_at` treats it like an inner
  bevel; under an enclosing shadow it counts as rendered.

### Shadows from rendered alpha (XARA-T-0327, 2026-10-16)

TODO 33 closed: a wall or floor shadow is cast from its content's
**rendered alpha**, no longer from an opaque silhouette pass.

- **Facts from the original, verified** (facts only): `NEW_GLOW_SILHOUETTE`
  is defined (`Kernel/nodeshad.cpp:193`), so the live
  `RenderShadowedNodeOffscreen` is the one at `:3946-4148`. It renders
  the controller's applied attributes (offscreen ones excluded) and then
  the shadowed node (`RenderTreeAtomic`, `:3999-4011`) into a **32-bit**
  offscreen region (`CreateOffscreenRR`, `:3773-3778`). The no-transparency
  attributes it declares at `:3990-3991` are **never rendered** in that
  version (the older one, `#ifndef NEW_GLOW_SILHOUETTE`, set them for a
  glow at `:3878-3879`). `CBitmapShadow::MakeWallShadow` copies byte 3 of
  each pixel into an 8-bit plane and blurs it (`Kernel/bshadow.cpp:245`
  for blurs under the minimum, `:277`/`:296` otherwise). So a wall or
  floor shadow's density is the subtree's rendered alpha, transparency
  and nested effects included. A glow contours that bitmap with
  `ContourBitmap` at threshold 0xFE (`nodeshad.cpp` glow branch after
  `:4031`): any pixel not fully clear counts as solid, so a glow ignores
  partial transparency.
- **The rule** (`LayerEffect::casts_rendered_alpha`, used by
  `render_fresh`): a shadow whose `spread` is not above 0 (wall, floor;
  a NaN spread too) takes its silhouette from the colour pass's alpha; a
  glow keeps the opaque pass (`silhouette_plane`, nested effects
  `Rendered`). The opaque pass and the 0xFE threshold differ only for an
  object at most 1/255 opaque (the original casts no glow for it, we do),
  and in a glow's edge antialiasing. Feathers and bevels are untouched.
- **Invariants kept**: the alpha is the same `render_region` output over
  the same region, band grid and coverage edge as before, so invariant 23
  holds; the effect cache's key already covers every op in the push … pop
  range, which is all the alpha depends on (invariant 25). The
  determinism, strip-independence and warm-cache tests pass unchanged.
  Bonus: a wall or floor shadow renders one offscreen pass instead of
  two.
- **Evidence, against the embedded previews** (scratch `score.py`, 60 dpi
  page render, registered, mean |Δ|; opaque pass → rendered alpha):
  | File (shadows) | Pixels changed | Opaque | Alpha |
  |---|---:|---:|---:|
  | Groucho2 (81) | 680 | 12.44 | **12.42** |
  | SoftShadow (3) | 0 | 10.49 | 10.49 |
  | Girard_simple (9) | 0 | 5.22 | 5.22 |
  | Watch4 (4) | 0 | 4.64 | 4.64 |
  | testimp1 (1) | 0 | 6.55 | 6.55 |
  Only Groucho2 has shadowed content that is not opaque (the red
  lipstick's highlight): restricted to the preview pixels that changed
  (178 px of a 113 × 125 preview), opaque 21.18 → alpha **19.08**. The
  evidence agrees with the code reading, so the original's semantics
  win. The non-corpus `filters/SVGFilter/samples/sample2.xar` and
  `sample3.xar` (15 and 9 shadows) change a lot
  (mean |Δ| 1.43 and 0.28) but carry no preview to judge by.
- **Goldens**: none of the existing ones moved (every shadowed golden's
  content is opaque, where coverage and alpha agree byte for byte). New
  `effect_shadow_transparent` (an opaque square and a half-transparent
  disc under one wall shadow) and
  `golden_cpu.rs::a_half_transparent_object_casts_a_half_dark_shadow`
  (darkness 200/255 beside the square, half of it beside the disc);
  mutation-checked (the opaque pass fails both). Unit test
  `a_wall_or_floor_casts_rendered_alpha_and_a_glow_the_opaque_pass`.
- **Export**: SVG's shadow filter already read `SourceAlpha`, so it now
  agrees with the raster: Groucho2 svg 6.87 → **6.81**; pdf (rasterised)
  4.77, unchanged; the other shadowed files unchanged.
- **SVG, feathered controllers (TODO 34, partly closed)**: a wall or glow
  controller that owns a feather is now one filter
  (`svg/effect.rs::feathered_shadow`) whose feather is cut from the
  outline rule above; see `export.md`, "Shadows in SVG". A floor
  controller's feather, and a feather on a group *around* a shadow
  controller, still read the shadow as drawn.

### Nesting live effects (XARA-T-0337, phase 13 J4, 2026-10-16)

The composition order is `document-model.md` decision 54 (facts and
table there). What it means for the renderer: nothing new in the offscreen
pipeline. A shadow around anything casts from its rendered alpha; a
feather or a bevel around anything reads the outline silhouette of the
previous section; geometry generators (contour, blend, mould) call the
inner generators themselves. The one renderer-facing addition: a moulded
shape carrying a feather is drawn inside its own `LayerEffect::Feather`
(per path, as the original applies the attribute to each passed-back
path), so a feathered object inside a mould stays feathered. A shadow
inside a mould is not drawn (the original's moulded copy has none); a
bevel inside one is a flat unlit band. Tests: `xarast-app/tests/
live_nesting.rs` (30 ordered pairs complete, a shadow on a blend on a
moulded group with pixel probes, a feathered object inside a mould, the
refusals, conversion to shapes within mean |Δ| 0.5). **Corpus impact:
none.** The 59 PNG and SVG exports (72 dpi, paper) are byte-identical
before (08609f7) and after: no corpus mould holds a shadow, bevel or
feather, and no corpus contour or blend end holds a mould below its top.
Corpus export check: 118 comparisons, 0 failures; `svg-check
--interchange` 59/59.

### Phase 13 conformance audit (XARA-T-0348, 2026-10-16)

The 17 acceptance criteria of `docs/phases/phase-13-live-effects.md`
against what is in the repository. **Met** = a test or gate in CI proves
it; **partial** = proven in a weaker form (the gap is named); **external**
= needs tooling this repository does not have (the VM running the
original, Chromium and Inkscape, lavapipe, an aarch64 runner, mdbook, the
i18n tool). Numbers: 69 new parameter goldens, 30 nesting goldens, 5
keyboard tests, 2 round-trip tests, 6 new perf gates.

| AC | Status | Evidence and gap |
|---|---|---|
| 1 | met | `cargo clippy --workspace --all-targets --all-features -D warnings` is a CI step; the six `LiveKind` variants are handled in every `match` (it would not compile otherwise). |
| 2 | met | `xarast-app/tests/effect_sweep.rs`: 23 sweeps x min/typ/max = **69 goldens** (`tests/golden/effect_sweep/`, 104 x 72 px, exact match, and the three must differ so a parameter that stopped reaching the renderer fails): shadow offset, blur, darkness, profile, glow width, floor scale and tilt; bevel indent, contrast, light angle, light tilt, type, direction; contour steps, width, direction; blend steps, profile, attribute profile; mould taper and skew; feather size and profile. On top of them `xarast-render/tests/golden/effect_*` (25: feather, shadow, bevel scenes), `transparency_family/` (36 = 12 families x 3 sources) and `fill_blend/` (160). Rendered by the CPU backend (the oracle); the criterion's "vello_cpu" is that. Not swept: the colour of a shadow or bevel (the colour bar sets it, `live_tools.rs`), the mould's envelope kinds (presets are in `mould_tool.rs`). |
| 3 | external | `parity_gpu_cpu.rs` and `parity_tiles.rs` compare the backends on a GPU; the 12-family bit-exact parity on lavapipe needs that runner. GPU tests are opt-in and serial (`XARAST_GPU_TESTS`). |
| 4 | met | `live_nesting.rs::every_ordered_pair_of_effects_nests_and_renders`: the 30 ordered pairs build, validate, render complete, ink the page, undo exactly, **and match `tests/golden/nesting/<outer>_over_<inner>.png` exactly** (added here). |
| 5 | met | `xarast-app/tests/effects_roundtrip.rs`: a document per effect kind (feather, three shadow kinds, bevel, contour, blend, mould) and the shadow-on-blend-on-mould nesting are saved, reopened, saved again (**byte-identical**) and render identically; the corpus half is `xarast_roundtrip.rs` (59 files). Lives in `xarast-app` because it needs `Session::open_bytes`, so the criterion's `xarast-format roundtrip::effects` name does not exist. |
| 6 | external | Mean dE00 against headless Chromium and Inkscape needs both and `cargo xtask check-external-render` (not written). `svg-check --interchange` 59/59 and resvg reads (`xarast-format/tests/svg_corpus.rs`) are the in-tree stand-in. |
| 7 | external | The reference VM running the original. In tree: the corpus imports with the effect tags handled (`xar_shadows.rs`, `xar_bevels.rs`, `xar_contours.rs`, `xar_blends.rs`, `xar_moulds.rs`, `xar_feathers.rs`, goldens `xar_*.png`); `xar-dump --unhandled` numbers are in `xar-import.md`. Whether the render matches the original within dE00 2.0 has never been measured. |
| 8 | met | One undo step per parameter: `live_tools.rs::every_shadow_parameter_is_typed_one_exact_step_each`, `every_bevel_parameter_...`, `contour_blend_tools.rs::every_contour_parameter_...`, `every_blend_parameter_...`, `mould_tool.rs`, `xarast-doc/src/tests/regen.rs::one_parameter_change_is_one_undo_step_...`, and the keyboard tests below. Undo is exact by the canonical digest. |
| 9 | partial | `live_nesting.rs::converting_live_objects_to_shapes_keeps_how_they_look`: each kind converts to plain shapes, draws within **mean absolute channel difference < 0.5 of 255** of the live render, and one undo restores the digest. The criterion's measure is mean dE00 < 1.0; no dE00 comparison is made (a mean channel difference under 0.5 levels is far inside it, but it is not that number). |
| 10 | met (nightly) | `xarast-geom/tests/contour.rs`: 96 cases on every push; `XARAST_CONTOUR_CASES=10000` runs the same properties at 10 000 (identity at 0, no NaN or unclosed output, clean output, monotone area, inset past the inradius empty, hostile input never panics, many small offsets = one big one). 10 000 cases take **59 s in release** and pass; the nightly job `contour-proptest` in `.github/workflows/fuzz.yml` runs them. The first 10 000 run exposed a harness limit, not a bug: two properties `prop_assume!` a non-empty region and tripped proptest's 1 024 global rejects, so `max_global_rejects` is now 1 000 000. |
| 11 | partial | `xarast-image/tests/procedural.rs::the_generated_tiles_match_their_golden_hashes` pins the hashes of the clouds and plasma tiles, and the generators are integer or 16.16 fixed-point maths, so both architectures should agree; no aarch64 runner has checked it. (There is no separate "fractal" generator in the tree: clouds and plasma are the fractals; the criterion's `fractal::determinism` name does not exist.) |
| 12 | met | `xarast_color::RENDER_GREY_WEIGHTS` is the single constant; the procedure is in "Transparency families" above; the 36 `transparency_family` goldens goldens are regenerated against it (invariant 4); the `fill_blend` matrix uses the same constant. |
| 13 | partial | Generation costs are gated: `fx-contour-24`, `fx-blend-50`, `fx-mould-5000`, `fx-bevel-400`, `fx-blur-disc-20`, `fx-clouds-256` (perf.md, "Phase 13 conformance"), all within their budgets with 1-2 orders of margin except the disc blur (5.6 ms of 12). Not gated yet: the frame budgets of the table (shadow, feather, blend and mould **Draft frames** on a rendered scene, p95 16 ms), the GPU disc blur (2 ms, needs a GPU runner), and the exact 512 x 512 fractal (the generator's tile is at most 256 x 256). `render-feathers` and `render-groucho2` gate cached whole frames of two corpus scenes. The criterion's command (`cargo xtask bench --check-budgets --suite effects`) is `cargo xtask perf`; there is no `--suite`. |
| 14 | met | `cargo run --release -p xtask -- perf --tier pr` exits 0 with every phase 12 gate (24 of 24 here); the nightly tier runs the rest. |
| 15 | met | `xarast-app/tests/keyboard_only_effects.rs`: for each of the five tools, **every** infobar parameter (the set is pinned by name) is set through the one `Intent::InfobarEdit` the shell's focusable widgets raise, changes the document and makes exactly one undo step; the three tool-state fields (mould point, mesh, detach) change no document and make no step. No pointer event is sent. The model has no pointer-only parameter. Limits: the criterion's `xarast-ui keyboard_only::effects` lives in `xarast-app` (the model is there); whether the egui widgets of the shell take focus and Tab order is checked by `xarast-ui/tests/accessibility.rs` for the existing panels only. Colours go through the colour bar and editor, which have their own keyboard tests. |
| 16 | external | No `cargo xtask i18n` exists in the tree and the UI strings are English literals; there is nothing to check yet. |
| 17 | external | No `docs/manual` or mdbook exists; the manual chapters and the generated keyboard reference are not written. |

**Dead ends and invariants from the audit.**

- The sweeps need a **non-degenerate** value at each end: blur 4.5 pt
  against 0 renders the same 104 x 72 picture at 0.4 px per point (the
  guard test caught it; the sweep uses 0 / 15 pt / 60 pt), and the
  contour's `colour_effect` changes nothing for a flat fill (the
  direction sweep uses the inset flag instead). A new sweep that cannot
  tell its three values apart fails by design: pick wider values, never
  drop the assertion.
- A golden sweep writes its images and then **fails** ("check them in")
  unless `XARAST_UPDATE_GOLDEN=1`; generate with that and commit.
- A sweep of the infobar is only as good as its pinned field list: adding
  a parameter to a tool's infobar fails `keyboard_only_effects.rs` until
  the field is listed, which is the point.

## Invariants that must not be broken

1. **Never put absolute millipoints into an `f32`.** A 5 m document is
   3.6 × 10⁸ mp and `f32` loses 0.04 pt of it. Every `f64 → f32` conversion of
   a coordinate happens relative to a tile origin, through `Tile::localise`.
   Enforced twice: a measured test at 4000 % zoom, and a grep test requiring
   every other `as f32` in the crate to carry an `// f32-ok:` justification.
2. **Never composite in linear light**, and never let a render target be
   `Rgba8UnormSrgb`.
3. **The CPU backend is the oracle and stays byte-reproducible.** Pin the SIMD
   level (`CpuConfig::deterministic`), merge parallel bands by index, and let
   no floating-point accumulation cross a band boundary.
4. **Exactly one blend-LUT generator**, `build_blend_lut`, shared by both
   backends. Two would drift, and the drift would be invisible until a user
   saw it. Likewise exactly one grey-conversion constant,
   `xarast_color::RENDER_GREY_WEIGHTS`: never write 0.299/0.587/0.114 for
   a blend family or a transparency again (contone and photo luminance are
   separate, see "Transparency families").
5. **Xara's transparency is 0 = opaque, 255 = fully transparent**, the inverse
   of ordinary alpha. Getting it backwards produces plausible wrong output.
6. **`xarast-render` must not depend on `xarast-doc`, `winit` or `egui`**, and
   must build and test with no GPU and no windowing system.
7. **A nonzero path whose subpaths abut with opposite winding will show a seam
   along the shared edge**, because `vello_cpu` accumulates fractional signed
   area. Do **not** "fix" this by normalising every subpath's direction: for a
   donut the cancellation is what nonzero means. If it ever matters for real
   documents, split the offending subpaths into separate fills.
8. **A command whose family reads the destination is a barrier** in its tile.
   The display list is fully ordered before tiling and must stay that way.
9. **Every position-dependent paint input moves to device space in
   `DisplayList::build`: paints, image placements and transparency
   mappings.** The backends evaluate at device pixel centres. A mapping left
   in document units looks like the gradient maths failing: the whole ramp
   sits thousands of pixels away. Transparency was missed until 2026-09-23,
   and the four `transparency_graduated_*` goldens had been blessed from
   that bug.
10. **`Scene::ops` and `Scene::cull` have the same length.** Every op goes
    through `SceneBuilder::push`.
11. **A fast path must be pinned to the slow one by a test**, bit for bit:
    the opaque-replace write, the fast floor/ceil, `rem_euclid_pow2`,
    `ramp_index`, `FrameMap::param`, and culled against full builds.
12. **The tile texel rule lives in two places and they must stay one
    expression**: `compose::source_texel` and `fs_main` in
    `backend/gpu_tiles.rs` compute `floor((p + 0.5 - origin) * inv_scale)`
    in `f32`, subtraction then multiplication, and the shader reads with
    `textureLoad`, never a sampler. That is what makes the GPU composite
    byte-identical to `compose_cpu`; `tests/parity_tiles.rs` fails on any
    device where it is not. Do not "simplify" it to `p * inv - origin *
    inv` (an FMA candidate) or to filtered sampling.
13. **Composition replaces and never blends.** A tile is an opaque piece of
    the canvas, background included. Anything that needs blending belongs
    in the rasteriser, where the families and the goldens are.
14. **An image paint's `v = 0` is the image's top row**; a bitmap fill's
    start point is its bottom row. Only `paint::bitmap_frame` in
    `xarast-app` translates between the two (see "Bitmap fill
    orientation").
15. **Coverage never depends on the draw area.** A primitive is rasterised
    from `max(bounds.x0, 0)` and its band's top minus the guard, a clip
    from the surface's left edge; the draw area (column, tile, strip,
    dirty rectangle) only limits what is composited. Otherwise an edit's
    repaint is not the frame a full render gives, and the damage tests
    fail by 1/255 (XARA-T-0221, "Edit damage").
16. **`scene_damage` compares ops by equality, never by `ContentHash`.**
    The walker's hash is per node version and scope and does not see
    viewport-dependent output or a reused ramp slot; op equality plus the
    resource check does.
17. **Every real-GPU test takes the machine-wide GPU lock.** Any test or
    bench that creates a `wgpu` instance, enumerates adapters or opens a
    device first binds `let Some(_gpu) = gpu_test_lock::acquire("…") else
    { return };` and keeps the guard alive, bound first so that it drops
    after every device, queue and resource. A helper that returns a device
    does not take the lock itself; its caller does. An instance limited
    to `Backends::NOOP` is exempt (it touches no GPU; see "GPU tests").

18. **Images are filtered premultiplied; minified in linear light,
    magnified in encoded sRGB; composited in encoded sRGB.** Only
    `resample.rs` converts, and only for minified samples and the
    pyramid; `encode` must stay the exact inverse of `to_linear` on its
    256 values.
19. **An aligned mapping samples as a point under every filter.** Do not
    "improve" a 1:1 bitmap with the magnification kernel; the fast path
    and the golden `resample_aligned_hq` depend on it.
20. **The pixel budget never changes a rendered pixel** under
    `MissingLevels::Materialise`. Sampling code reads image levels only
    through a sampler's pins (`ImageSampler`) or `ImageRef::level`, never
    by keeping a borrow across a pin; a level is rebuilt only with
    `reduce_level`; a `PixelSource` must return the exact bytes first
    registered; a base with no way back is never dropped.
    `tests/pixel_budget.rs` here and in `xarast-app` hold it.
21. **A render under `MissingLevels::Substitute` is not a picture until
    it is repaired.** Whoever asks for it must read `substitution_tick()`
    before, find the stamped images after, bring their bases back and
    repaint their `image_damage` (or re-render under `Materialise`).
    Export, thumbnails, goldens and every byte-for-byte test use
    `Materialise`; only the interactive render thread substitutes, and it
    never publishes such a frame as exact (`app-core.md` invariant 17).
22. **A deferred image's stand-in is never a level.** It lives outside
    `levels`, only a `Substitute` pin may return it, and producing the
    base drops it. Seeding a level with an approximation would make a
    later `Materialise` render (export) sample it and break byte
    identity. Nothing that runs on the render thread outside a sampler
    may produce a pending base either: compare with
    `eq_without_producing`, never `==`.
23. **An effect's pixels never depend on the draw area.** Its region
    renders from a left edge fixed by the view and the effect chain, on
    the frame's band grid, `reach` beyond what it keeps (beyond the
    viewport too); the display list keeps wrapped primitives within
    `clip ⊕ reach`; `scene_damage` pads changes under an effect by its
    reach; a nested effect counts towards its parent's content with its
    growth. Clamping the region to the viewport feathers every object at
    the view's edge; starting coverage at the region's corner moves
    pixels by 1/255 between a repaint and a frame.
24. **Feathers, bevels and glows are cut from the silhouette; a wall or
    floor shadow is cast from rendered alpha.** A feather of a
    transparent object is shaped by its outline; using its alpha makes a
    half-transparent object feather to nothing. In a feather's or a
    bevel's silhouette a **nested effect counts as its outline**
    (`LayerEffect::outline_at`), never as its colour result: a nested
    shadow opaque and unblurred (feather) or not at all (bevel). A glow's
    silhouette is the opaque pass with nested effects as rendered. A
    **wall or floor shadow casts its content's rendered alpha**
    (`LayerEffect::casts_rendered_alpha`: the colour pass's own alpha,
    transparency and nested effects included), as the original does: a
    half-transparent object casts a half-dark shadow ("Shadows from
    rendered alpha"). Never give a wall shadow the opaque pass back, and
    never give a feather or a bevel the alpha.
25. **An effect's result depends on its key and nothing else.** Anything
    new an effect's pixels come to depend on (a resource kind, a view
    field, a configuration field, an input outside its push … pop range)
    must go into `effect_cache::Probe` too, or the cache serves a stale
    layer; and a cached layer is trusted only over its valid rectangle.
    `tests/effect_cache.rs` and the warm-cache damage property hold it.
    A new effect kind needs nothing: it is a `LayerEffect` variant inside
    the push op, compared by equality like any other parameter.
---

## Dead ends (do not retry)

- **`tiny-skia` as the production rasteriser.** 3.2× slower on `bulk`, 17
  coverage levels on a 0.5° edge against `vello_cpu`'s 256, mean ΔE₀₀ 0.88
  against 0.07. It stays wired into the test harness as a second opinion, and
  as the fallback oracle if `vello_cpu`'s alpha status ever bites, which is
  the only reason to keep it.
- **A 16×16 supersampled "ground truth" as the antialiasing gate.** It
  resolves sub-pixel visibility between overlapping shapes, which no
  painter's-algorithm compositor reproduces; refining it to 48×48 moved p99 by
  0.00. Use the painter's-model reference (`golden::downsample`).
- **Trusting the rasteriser's internal flattening.** It made `Draft` and
  `Final` produce identical pixels and capped curve fidelity at whatever the
  default happened to be. Flatness is ours.
- **Clipping a primitive's coverage exactly to its band.** Costs 1/255 at
  every band boundary and makes the band height visible in the output. Use the
  guard band.
- **Writing paint mappings in device pixels.** They live in document space,
  like the geometry they follow. Getting this wrong makes a gradient frame a
  thousandth of a pixel across, which renders as a single flat colour and
  looks like the gradient maths failing. Cost: an hour.
- **Rendering each primitive into a band-sized scratch buffer.** A full-width
  clear per primitive is 123 KiB of memset per command; size the scratch to
  the primitive's clipped bounds instead.
- **Boxing part of `DrawCmd` to shrink it.** A per-command allocation, and
  the paint and transparency alone are 208 bytes. Index into shared ops.
- **Reading the compact culling entries on full builds too.** Two streams
  instead of one: 3.4 ms against 1.9 ms at 100k.
- **Inverting the view to cull strokes in document space.** A degenerate
  view (zero x scale) has no inverse, so nothing was culled and the fuzzer
  ran out of memory. Test the centre line in device space.
- **Shrinking the deterministic band to parallelise export.** Band height
  is not quite invisible. The 120 golden cases are band-invariant, but the
  100k `bulk` scene differs in 8 of 8.3 M bytes (by at most 2) between 1 MiB
  and 256 KiB bands. Tile the interactive path instead, or re-bless
  deliberately (XARA-T-0038).
- **Hunting libm and divisions in the gradient samplers.** Replacing
  `fmod`/`round` and skipping the `u` division moved the gradient files by
  under 5 %. The time is the scalar per-pixel pipeline as a whole (≈ 30 ns),
  not one call.
- **A GPU rasteriser to meet the interactive budgets (XARA-US-0011).** The
  CPU meets them at 1080p; what fails is presentation (a 4K upload on the
  iGPU). `vello` is 72–86 ms on the iGPU and needs a second `wgpu`; a WGSL
  pass over CPU coverage pays 27–33 ms per 64 MiB of masks on the iGPU.
  Re-open only on the triggers in "The GPU decision".
- **Checking bitmap orientation with the `.xarast` round trip.** Both sides
  go through our renderer, so an upside-down bitmap fill round-trips
  pixel-identical (XARA-T-0171 survived it). Compare against resvg on the
  exported SVG, or pin it with a synthetic asymmetric bitmap.
- **Snapping the coverage origin to vello's 4 px tile grid** to keep a
  dirty rectangle's pixels exact. Translating by a multiple of 4 still
  changes the `f32` geometry: `aa_edge_45` moved in 35 pixels. Start at the
  primitive's left edge (invariant 15).
- **Using the walker's `ContentHash` for edit damage.** See invariant 16.
- **Rasterising tile by tile.** Each call pays a culled display-list build
  linear in the scene (6.5–10 ms at 900k ops), and the assembled frame is
  not byte-identical to a whole one. Rasterise one dirty rectangle and
  upload its pieces.
- **Magnifying in linear light** (the phase document's literal rule).
  Worse on every harness row than encoded sRGB, more ringing, and a
  visible light leak into dark texels of a magnified tiny bitmap.
- **Trilinear between the base and level 1 for 1–2× minification.**
  Measured worse than the tent widened by the footprint on all four test
  images at ÷1.5 (by 3–7 dB), and it builds the pyramid for no gain.
- **Filtering straight (non-premultiplied) RGBA.** A transparent texel's
  colour bleeds into the edge; filter premultiplied.
- **Judging minification only at power-of-two ratios against a box
  reference.** The pyramid wins those by construction (99 dB at ÷2).
- **Unserialised GPU tests running in parallel.** Several `cargo test`
  processes (or parallel threads) opening devices on the display GPU at
  once hung the Wayland compositor and blacked out the desktop. Making the
  tests opt-in only hid them; take the GPU lock (invariant 17) instead.

---

## Open TODOs

| # | Item | Owner |
|---|---|---|
| 1 | ~~Re-run the W0 spike on a real reference machine and settle G1 and G2~~. **Done 2026-09-23**: both fail, settled. See "Re-run on the reference machine" above | done (XARA-US-0010) |
| 2 | The WGSL compositing pass: paint evaluation, family dispatch, LUT sampling, ping-pong destination reads (R5.3, R5.4). **Deferred** by the GPU decision; re-open on its trigger | XARA-T-0051 |
| 15 | ~~Present the canvas through `GpuTileCache` (shell/app wiring, capability ladder)~~. **Done 2026-09-23**; see "The tiles in the viewer" | done (XARA-T-0050) |
| 16 | The iGPU `write_texture` cliff: 1080p 0.6 ms, 4K 24 ms | XARA-T-0052 |
| 3 | Recover CDraw's luminance weights by least squares (R4.4; the procedure is under "Grey-conversion weights (I3)") and extract the twelve tables via `GDraw::CalcTransparencyX` (R4.5) | needs an x86-64 VM (XARA-US-0018); the corpus previews cannot do it, see "Blend families against the original's previews" |
| 4 | Verify Contrast, Bevel, Saturation, Luminosity, Hue and Lighten against those tables | after 3; no corpus file uses any of them |
| 5 | Render the `.xar` corpus end to end and compare against the original at 25 %, 100 % and 400 % | our side done (`xarast-cli render --zoom`); the original cannot run here (closed CDraw, 2006 binaries), so XARA-T-0248 compares with the previews each `.xar` embeds and keeps the zoom comparison VM-gated |
| 6 | Re-derive the cache admission threshold from corpus data | after 5 |
| 7 | ~~`DisplayList::build` at 100k: 20.8 ms against 3 ms~~. **Done 2026-09-23**: 1.9 ms; the cause and the fix are under "Decisions taken". The 100k CPU full frame is 19 ms against 25 | done (XARA-US-0016) |
| 8 | Deferred `Draft → Final` upgrade with the 120 ms idle timer and cancellation (R6.8) — the quality levels exist and differ, the scheduler does not. After the G1/G2 re-run this is **what the 16 ms pan/zoom budget depends on**: a 100k full frame costs 35–160 ms on the CPU and 72–86 ms on the iGPU | Phase 5, which owns the idle timer |
| 9 | ~~Fuzz targets `fuzz_display_list` and `fuzz_ramp`~~ — done 2026-09-23, nightly in CI; see below | — |
| 10 | Dither styles, sub-32 bpp output, CMYK separation, UCR/GCR | deferred, no phase |
| 11 | ~~Strokes dashed whole before clipping~~. **Done 2026-09-23** (`stroke_cull`, XARA-T-0022); `fuzz_display_list` now spans the whole extent with unlimited dash patterns | done |
| 13 | Gradient-heavy export: ≈ 30 ns per composited pixel, and only three 1 MiB bands on a 766 px image. The three `*GradFilledShapes*` files take 2.2–4.5 s | XARA-T-0038 |
| 14 | `SimpleSphere.xar` is still black. The renderer is right; the walker fills an unfilled 12 pt frame opaque black over the whole drawing. The gradient repeat default is also suspect | XARA-T-0037 (app/doc) |
| 17 | ~~Dirty region for a fill edit (T8.5.4)~~. **Done 2026-09-24**, for every edit: the render thread diffs scenes (`scene_damage`) and repaints only the damage; see "Edit damage" | done (XARA-T-0221) |
| 18 | ~~Golden images: every fill shape × every exposed blend mode × {flat, graduated} (T8.5.5)~~. **Done 2026-09-24**: 160 exact goldens; see "The fill × blend golden matrix" | done (XARA-T-0222) |
| 19 | Document/per-bitmap smoothing flag → `Filter` (T10.4.4's remaining half) | XARA-T-0273 |
| 20 | Bitmap-fill tile seams in resvg on exported SVG (the renderer has none) | XARA-T-0274 |
| 21 | ~~Mesh fills ignore `Repeat`; mesh transparencies are flat means~~. **Done 2026-09-24**: see "Meshes tile mirrored" | done (XARA-T-0256) |
| 22 | ~~The pyramid is built on the render thread on first minified frame~~. **Done 2026-09-24**: the walker's decode threads call `ImageRef::prepare`; see "Pixel memory budget" | done (XARA-T-0278) |
| 23 | ~~Re-materialisation of an evicted base is synchronous on the render thread~~. **Done 2026-09-24** (XARA-T-0281), and since XARA-T-0304 the helper no longer holds the image's lock while it decodes | done |
| 25 | ~~Offscreen effect cache (B9)~~. **Done 2026-09-24**: keyed by op equality + resources + exact view, not a content hash or a quantised scale, with its own 128 MiB ceiling rather than the Phase 4 budget; see "The effect layer cache" | done (XARA-T-0314) |
| 26 | Blur radius above the 100 px ceiling: render the silhouette at a reduced resolution and scale up, as the original does for feathers (B7) | XARA-T-0315 |
| 27 | GPU blur (B6): not needed while the GPU only composites CPU tiles; lands with the WGSL pass | XARA-T-0051 |
| 28 | ~~Inner shadows (C5)~~. **Decided 2026-10-15: nothing now**: no `.xar` shadow type is inner, the model has no `Inner` kind and no tool makes one; the rules it will follow are fixed under "Nested effects in a silhouette pass" | deferred to C7 / the native format (XARA-US-0069) |
| 29 | ~~A nested effect in a silhouette pass composites with its alpha~~. **Done 2026-10-15**: feathers and bevels see nested effects as outlines, a shadow as rendered; see "Nested effects in a silhouette pass" | done (XARA-T-0321) |
| 30 | Bevel cross-sections, lighting curve and the inner-bevel style swap are Xarast's own (`xarast_geom::bevel`): determine them empirically against the original in a VM (phase 13 D's gate) | XARA-US-0069 |
| 31 | ~~Outer bevel joins: always round~~. **Done (XARA-T-0326)**: mitre (limit 10) and bevel joins from the applied join attribute; an inner band's concave corners still round | done |
| 32 | ~~Bevel cost~~. **Done (XARA-T-0326)**: the feature transform is ~6× cheaper and uses 17 instead of 49 bytes a pixel; no lighting cache (it would only serve colour edits). What is left of the first frame is the bevelled content's two offscreen passes | done |
| 33 | ~~A shadow's own silhouette ignores transparency~~. **Done 2026-10-16 (XARA-T-0327)**: a wall or floor shadow casts its content's rendered alpha, as the original does; a glow keeps the opaque pass (its 0xFE threshold); no golden moved, Groucho2 matches its preview better; see "Shadows from rendered alpha" | done |
| 34 | SVG: a feather around a shadow reads `SourceAlpha` (the shadow as drawn), not the outline rule. **Partly done 2026-10-16 (XARA-T-0327)**: a wall or glow controller that owns a feather is one filter cut from the outlines (synthetic document: 2.69 → 0.68). Left: a floor controller's feather (its shadow is a derived copy) and a feather on a group around a shadow controller; no corpus file has either (`export.md`, "Shadows in SVG") | XARA-US-0069 |
| 35 | Bevels the renderer still does differently: an inner band's concave corners always round (a mitre there needs a reach of ten widths inside the content; analysed in "Bevel leftovers", XARA-T-0328); corners are found in the silhouette, so a mitre's point and a cut's position are as good as a line fit over 2–8 px (corners under ~16 px of edge on a side, e.g. small text, round). The painted inner band's transparency copy is applied since XARA-T-0328 (flat mix only) | XARA-US-0069 |
| 36 | ~~The SVG bevel bake is resolution-dependent~~. **Done 2026-10-16 (XARA-T-0328)**: a user-unit chain (ramped height field, offset difference, the renderer's own shading), the outer join on rectilinear sources, the inner flat paint; `xarast-format.md`, "Bevels". Left in the bake: non-rectilinear mitre/bevel joins round, corner shading is the blur's, the cross-section is a chamfer's | done |
| 24 | ~~`RampMapping::Sin` ignored; `ClipViewMode::Outside` dropped~~. **Done 2026-09-24**: `RampEase` (above) and the walker's outside clip (`app-core.md` decision 34); the renderer still clips only to a path's inside, on purpose | done (XARA-US-0017) |
| 12 | ~~Reconcile `wgpu` versions~~. **Decided 2026-09-23**: no `vello` in the product until it targets the workspace's `wgpu` (two `wgpu`s cost +4.08 MiB and 46 crates, and cannot share a device); the spike keeps building against `vello::wgpu` behind `spike-gpu` | done (XARA-US-0011) |

### Vector export (PDF, XARA-US-0059, 2026-09-23)

- **Export is CPU-only, including vector export's fallback.** PDF's
  fidelity ladder rasterises through `export::ListRasteriser`, which names
  `CpuBackend` with `CpuConfig::deterministic()`; `DisplayList::with_commands`
  reuses a list's side tables with a chosen command stream and
  `DisplayList::op_of` identifies an object across lists built from one
  scene for different views. Guarded by
  `tests/export.rs::a_list_rasterised_through_its_own_commands_matches_the_export`.
- **The PDF fidelity matrix** (native / workaround / rasterise, per
  feature) and the T11.4.1 crate spike are in `export.md`, section "PDF".
  **Per-family blend decision: not measured yet** (T11.4.6, XARA-T-0227);
  until it is, every family except Mix is rasterised with its backdrop by
  default, and `PreferNative` maps only Stained Glass → Multiply and
  Bleach → Screen. Darken, Lighten, Hue, Saturation and Luminosity are
  deliberately *not* mapped to PDF's same-named modes.
- **Fixed (XARA-T-0231, 2026-09-23):** partly transparent Mix over a
  transparent destination darkened (≈ `c·a²`), and the CPU stroker
  ignored `StrokeStyle::cap_end` and `DashPattern::offset`. See the
  decision "Compositing over a destination that is not opaque" above.

### Fuzzing, first runs (2026-09-23)

- `fuzz_ramp`: NaN stop offsets — possible, `Stop` and `TranspStop` have
  public fields — made the sort's comparator inconsistent, and the
  standard library's sort **panics** when it detects that. Both ramp
  builders now map a NaN offset to 0 (as `Stop::new` does) and sort by
  `total_cmp`. Never sort floats with `partial_cmp().unwrap_or(Equal)`.
  Clean rerun: 4.66 M execs at ~7 760 exec/s.
- `fuzz_display_list`: renders every accepted scene twice on the
  deterministic backend and compares bytes; asserts the display list is
  balanced and inside the viewport. Found only TODO 11. Clean rerun with
  the bounds in place: 565 k execs at ~940 exec/s.
- `fuzz_display_list` with the bounds lifted (2026-09-23), three runs:
  1. An OOM after 121 k execs: the thick-stroke work-budget case under a
     degenerate view.
  2. A panic: `i64` overflow on `x0 + 1` in bilinear image sampling at a
     saturated coordinate. Now `saturating_add`.
  3. Clean: **317 k execs in 600 s** (~530 exec/s), peak RSS 813 MB.
