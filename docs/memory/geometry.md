# geometry

Memory note for **`xarast-geom`** and **`xarast-color`**, the two leaf crates
everything else depends on.
Specification: [`../phases/phase-01-geometry-and-colour.md`](../phases/phase-01-geometry-and-colour.md).
Format facts: [`../research/01-xar-format.md`](../research/01-xar-format.md) §5, §7, §8, §9.
Arbitration: [`../10-architecture.md`](../10-architecture.md) §3.3, §3.4.

## Current state

Phase 1 implemented. Both crates are `#![forbid(unsafe_code)]`,
`#![deny(missing_docs)]`, build with no display, and depend on nothing
outside `kurbo`, `i_overlay`, `bitflags`, `thiserror` and `slotmap`.

| Module | Contents | State |
|---|---|---|
| `geom::mp` | `Mp`, the overflow contract, unit conversions, `Display`/`FromStr` | complete |
| `geom::fixed` | `Fixed16` codec | complete |
| `geom::point` | `Point`, `Vector` | complete |
| `geom::rect` | `Rect`, the empty sentinel | complete |
| `geom::matrix` | `Matrix`, composition, inversion, `quantise_fixed16` | complete |
| `geom::profile` | `BiasGain` and its LUT | complete |
| `geom::path` | `Verb`, `PointFlags`, `Path`, `PathBuilder`, SVG and `kurbo` conversion, canonical form | complete |
| `geom::flatten` | `Tolerance`, `Polyline`, `SegmentTrace`, `flatten`, `flatten_traced` | complete |
| `geom::stroke` | `Cap`, `Join`, `FillRule`, `DashPattern`, `StrokeStyle`, `stroke_to_path`, `dash`, `offset`, `reduced_dash_offset` (public since XARA-T-0231: the renderer's stroker uses it too) | complete |
| `geom::boolean` | `BoolOp`, `boolean`, `self_union`, the refit pipeline | complete |
| `geom::measure` | `arclen`, `point_at_arclen`, `nearest_point`, `fill_contains` (exact point-in-fill), `PathHitIndex` (one big path's edges) | complete |
| `geom::hit` | `HitTolerance`, `hit_fill`, `hit_stroke`, their `_transformed` forms, `HitShape`/`ShapeHit` — picking with a radius (phase 7 W3) | complete |
| `geom::hit_index` | `HitIndex<K>`, `RectMode`, `Candidates` — the object index for picking and marquee (phase 7 W3) | complete |
| `color::model` | `ColourModel`, `ColourValue`, `Rgba8`, every conversion | complete |
| `color::fixed24` | `Fixed24` and the inherit sentinel | complete |
| `color::builtin` | `BuiltinColour`, the negative-reference table | complete |
| `color::table` | `ColourId`, `ColourKind`, `ColourDef`, `ColourTable`, `Colour` | complete |
| `color::interp` | `FillEffect`, `Stop`, `Transparency`, `TranspMode`, `interpolate` | complete |

Dependency versions in use: `kurbo` 0.13.1, `i_overlay` 9.0.0 (with
`i_float` 5.0.0, `i_shape` 5.0.0), `bitflags` 2.13, `thiserror` 2.0,
`slotmap` 1.1, and `tiny-skia` 0.12 **as a test-only dependency**.

## Measured performance

Measured with `criterion` in the `release` profile on the **development
container**, which is *not* the reference machine the phase document's
budgets assume — a shared CPU with no frequency guarantees. Treat these as
ratios and as a regression baseline, not as absolutes, and re-measure on the
reference machine before gating CI on them.

| Operation | Input | Budget | Measured | |
|---|---|---|---|---|
| `Mp` operator vs raw `i32` saturating | 4 096 adds | within 5 % | 4.89 µs vs 5.05 µs (−3 %) | ok |
| `Path::bounds` | 10 000 segments | 30 µs | 15.1 µs | ok |
| `Path::tight_bounds` | 10 000 segments | 400 µs | 422 µs | **+5 %** |
| `flatten` at 0.25 device px | 10 000 segments | 2 ms | 163 µs | ok |
| `flatten_traced` | 10 000 segments | ≤1.3× `flatten` | 161 µs (0.99×) | ok |
| `stroke_to_path`, round join | 10 000 segments | 8 ms | 6.23 ms | ok |
| `dash`, 4-element pattern | 10 000 segments | 6 ms | 2.26 ms | ok |
| `boolean` union | 2 × 1 000 segments | 3 ms | 231 µs | ok |
| `boolean` union | 2 × 10 000 segments | 40 ms | 4.60 ms | ok |
| `boolean` union | 2 × 100 000 segments | 600 ms | 39.0 ms | ok |
| `PathHitIndex::build` (was `HitIndex`) | 10 000 segments | 500 µs | 634 µs | **+27 %** |
| `hit_fill` via `PathHitIndex` | 10 000 segments | 15 µs | 0.117 µs | ok, 128× |
| `fill_contains` exact, for contrast | 10 000 segments | — | 390 µs | — |
| `arclen` at 1e-6 | 1 000 cubics | 200 µs | 49.5 µs | ok |
| `ColourTable::resolve` | depth-3 tint chain | 100 ns | 141 ns | **+41 %** |
| `interpolate` fade | — | 20 ns | 14.3 ns | ok |
| `interpolate` rainbow | — | 20 ns | 38.0 ns | **+90 %** |

Notes.

- The boolean figures are **after** fixing a quadratic scan in
  `RestoreTable::build`, which looked up each run's source segment by a
  linear search of the flat segment list. Before the fix the 10 000-segment
  union took **70.5 ms** and the 100 000-segment one did not finish inside
  the benchmark's budget; grouping the segments by subpath once brought them
  to 4.6 ms and 39 ms. If the boolean ever looks slow again, look here first.
- `Mp` arithmetic compiles to the same work as the raw `i32` loop, so the
  newtype and the whole overflow contract are free. That was the point.
- `hit_fill` via the per-path index is worth **128×** over the exact
  winding test — 117 ns against 390 µs — at 10 000 segments. (That index
  is now `PathHitIndex`; the object index for phase 7 is `HitIndex`, see
  "Picking" below, measured on the reference machine.)
- `HitIndex::build` is 27 % over. It flattens at `Tolerance::EXPORT`, 1 mp,
  which is far finer than a build-time index needs; the obvious fix if the
  budget matters is to build at the render tolerance instead and accept a
  coarser near-boundary answer.
- `tight_bounds` is 5 % over on a shared container and is within the noise
  of the budget; re-measure before treating it as a miss.
- `ColourTable::resolve` and `interpolate` for the rainbow effects are over
  budget by 1.4× and 1.9×. Both are under the 2× the phase document sets as
  the re-basing threshold, so by its own rule the budgets stand and the code
  needs the work rather than the numbers needing adjustment. The cost in
  both cases is the model round trip: a tint resolves through RGB and
  converts back to the definition's model, and a rainbow interpolation goes
  RGB → HSV → HSV → RGB. Caching a resolved palette (which the document
  model will want anyway) removes most of the first.

## Decisions taken (and why)

### The `Mp` overflow contract

Stated in full in `crates/xarast-geom/src/mp.rs`; repeated here because it
will be questioned and the reasoning has to survive.

Three properties are in tension. We must never panic, because a parser that
panics on hostile input is a denial of service. We must never wrap, because a
wrapped coordinate produces mirrored geometry that looks *plausible* and so
escapes review — the worst kind of failure. And we must not force every
expression in the flattening and hit-test inner loops to return `Option`.

1. `Add`, `Sub`, `Neg`, `AddAssign`, `SubAssign` **saturate**, identically in
   debug and release. Saturation is total, deterministic and
   order-preserving.
2. `Mp::MIN` is `i32::MIN + 1`, not `i32::MIN`, so that negation is total.
   `Mp(i32::MIN)` is constructible through the public field but is outside
   the canonical range; every operation normalises it, and a test asserts so.
3. **No `Mul<Mp> for Mp`.** Millipoints times millipoints is an area.
   Scaling goes through `scale` (f64, half away from zero, saturating) or
   `mul_ratio` (exact in `i64` until the final narrowing). `Div<Mp> for Mp`
   yields a dimensionless `f64`.
4. The `checked_*` family returns `Option` and is what every I/O boundary
   uses, so an unrepresentable `.xar` record becomes a diagnostic rather than
   a silently clamped point.
5. **The document extent is `±(2^30 − 1)` mp** (≈ ±14.9 km). Inside it,
   `a + b` and `a − b` are exactly representable and provably never saturate.
   That is what lets the rest of the codebase stop thinking about overflow.
6. `Mp::sum` accumulates in `i64` and saturates once at the end, so a long
   polyline's running total cannot saturate mid-way and recover into a wrong
   answer.
7. **No `From<f32>`, no `to_f32`.** `f32` loses millipoint precision above
   16 777 216 mp. `to_f64` is exact; `from_f64_round` rounds half away from
   zero and saturates; a NaN becomes `ZERO`, because a NaN coordinate has no
   defensible clamped value and `MIN`/`MAX` would put geometry 14 km away.

The extent value turned out to matter twice over: it is **exactly**
`i_overlay`'s 32-bit coordinate limit, so millipoints go into the boolean
engine untouched. See below.

### Boolean engine: `i_overlay` 9, through its **integer** API

Chosen: `i_overlay` 9.0 (`MIT OR Apache-2.0`).

The phase document anticipated the float API with a fixed `grid_size`. The
integer API is strictly better and is what is implemented: `i_overlay`'s
`i32` engine accepts `-2^30..=2^30 - 1`, which is `Mp::EXTENT`. Millipoints
go in as they are, so there is **no scaling step, no grid to choose, and no
dependence on the input's magnitude** — results are bit-reproducible by
construction rather than by configuration. Golden-image tests and "undo then
redo gives the same document" both rest on that, and a test asserts it over
the corpus.

`boolean()` and `self_union()` are the only entry points; no `i_overlay` type
appears in the public API, so an API break in 10.0 is contained.

- **Named fallback:** `flo_curves` 0.8 (Apache-2.0). Not wired up — adding a
  cargo feature for an engine nobody has needed would be dead code. The
  facade is what makes swapping it a contained change.
- **2027 candidate:** `i_curve`, booleans natively on cubics and rational
  arcs, built on `i_overlay`. Re-evaluate at ≥1.0 with six months of release
  history.

### The flatten → boolean → refit pipeline

1. `flatten_traced` both operands at `Tolerance::BOOLEAN`, keeping the trace.
2. Run `i_overlay` on the integer millipoints, with `min_output_area = 1`
   (a contour below a square millipoint is numerical noise, and keeping it
   would make `difference(A, A)` return a sliver instead of nothing).
3. Refit: an output run that came wholly from one untouched input segment,
   endpoints included, has its original cubic restored **verbatim**.
4. Clean: drop repeated vertices (coordinates are integers, so "closer than
   1 mp" and "identical" are the same test), then canonicalise ordering.

`SegmentTrace` is the whole point of step 3 and is a first-class deliverable,
not an optimisation: without it every boolean turns every curve in the
document into a polyline, cumulatively and irreversibly. A test asserts that
a circle unioned with a distant square still has its four cubics, and another
applies twenty successive unions and checks the area holds to 1e-4.

### `Tolerance::BOOLEAN` = 10 mp

0.01 pt, 3.5 µm. Tighter buys no accuracy that survives quantisation back to
integer millipoints, since the engine's own output is on the millipoint
lattice. Looser starts to erode small features. The conservation identities
hold over the corpus at this value with the slack described under
"Invariants" below.

### The path representation

Segment-oriented: parallel `verbs`, `points` and optional `flags` vectors,
one verb per segment. SVG, `kurbo` and every rasteriser want this; `.xar` is
point-oriented. They reconcile through an identity worth keeping:

> **`points.len()` equals the `.xar` point count exactly.** `MoveTo` and
> `LineTo` consume one point each, `CubicTo` three, `Close` none — and in
> `.xar` closure is a bit on an existing point, not a point of its own.

That is what makes `TAG_PATH_FLAGS`, one byte per point, map onto `flags`
with no index arithmetic at all.

Fill and stroke intent live in `xarast-doc`, not here, so that `PartialEq`
on a `Path` means "the same shape" — which is what makes cheap subtree
comparison and resource deduplication possible. `PointFlags` has no
`SELECTED` bit for the same reason.

`flags` is empty when every point is default, keeping the common path (SVG
import, boolean output) one allocation lighter.

### Flattening: **not** `kurbo::flatten`

This is the other decision that changed during implementation, and it is a
correctness one.

`kurbo::flatten` converts cubics to quadratics and estimates the conversion
error. The estimate is excellent for ordinary curves and unreliable near a
cusp: measured over 20 000 random cubics
(`crates/xarast-geom/examples/devsweep.rs`, still in the tree so the
measurement can be repeated) it exceeded the requested tolerance by up to a
factor of **nine** — 462 mp at a requested 50 mp.

Since "the polyline is within `tol` of the curve" is a contract this crate
advertises and the boolean pipeline relies on, flattening uses the rigorous
control-polygon bound on `|B(t) − L(t)|` instead:

```
u = 3p₁ − 2p₀ − p₃ ,  v = 3p₂ − p₀ − 2p₃
error ≤ sqrt( (max(uₓ², vₓ²) + max(u_y², v_y²)) / 16 )
```

and recursive de Casteljau subdivision, capped at 20 levels. Measured worst
case after the change: **1.53 mp at a requested 1 mp**, where the 0.53 is
exactly the quantisation of the output vertices to integer millipoints.

The obvious alternative bound — three quarters of the control points'
perpendicular distance from the chord — **is wrong and was tried**: it
measures deviation from the infinite *line*, so a cubic whose control points
overshoot along the chord direction registers as flat while the curve bulges
far past its own endpoint. It measured 47× over tolerance. Do not go back
to it.

### `Matrix`: `f64` linear part, `Mp` translation

`a..d` stay `f64` through composition and inversion; `quantise_fixed16()` is
the only thing that rounds them and is for the file boundary alone. A test
composes a thousand rotations and asserts the drift stays under 1e-9.

Consequence worth knowing: the **translation** is quantised to a whole
millipoint at every composition, and a subsequent scale multiplies that
rounding by its own factor. Compose the whole chain into one `Matrix` and
apply it once; applying step by step is both slower and less accurate. The
property tests bound the error by `max_scale` rather than by a constant,
which is what made this visible.

### Stroke, dash, offset

- A width of `Mp::ZERO` is a **hairline**, which has no document-space
  outline at all. `stroke_to_path` returns `Err(StrokeError::Hairline)`
  rather than inventing a width, so the renderer cannot ignore the case.
- `kurbo::Stroke` takes a start and an end cap and applies each to its own
  end of every open subpath and every dash, matching `.xar`'s separate
  `TAG_STARTCAP`/`TAG_ENDCAP`, so one pass is enough. **Fixed 2026-09-23:**
  the code used to stroke a second time with the caps swapped and union
  the two, which gave *both* ends the union of the two caps (a round start
  and a square end came out square-and-round at both ends). A test now
  asserts each end's own cap.
- `offset()` delegates to `contour::offset_region` since XARA-T-0330 (see
  "Contours" below). The earlier per-segment version assumed a
  counter-clockwise outline (a clockwise one went the wrong way) and
  turned mitres into bevels.
- Validated against `tiny-skia` (test-only) by rasterising both strokes at
  512×512 over 100 cases: worst disagreement **0.31 %** of pixels, gated at
  0.5 %. Two rasterisers antialiasing the same outline account for most of it.

### Colour

- **CMYK → RGB is naive**, `R = 1 − min(1, C + K)`, as the original does it.
  A fidelity decision: matching Xara's appearance for two decades of
  documents beats being colourimetrically right, and the format carries no
  ICC profiles to be right *with*.
- **RGB → CMYK is the original's** (phase 8, `colour.md` decision 1):
  black generated past a 50 % threshold, pure black on the K plate. It is
  still exactly invertible under the naive reverse. *(Phase 1 used `C = 1 −
  R, K = 0`, believing anything else lost colour; that was wrong.)*
  Real under-colour removal belongs with the ICC path.
- `Fixed24::INHERIT` (`0xF800_0000`, reading as −8.0) means "inherit from the
  parent". `to_f32` returns `Option<f32>` and `ColourDef::components` is
  `[Option<f32>; 4]`, so forgetting the check is a compile error rather than
  a rendering bug.
- `ColourTable::resolve` is depth-limited to `MAX_PARENT_DEPTH = 16` and
  cycle-safe, falling back to the file's `cached_rgb` — which is exactly what
  Xara writes that field for. A corrupt palette entry must not stop a
  document opening. `validate()` reports cycles and depth violations for
  `xar-dump`.
- Components are `f32` in `0.0..=1.0`, clamped on construction, so no
  `ColourValue` can hold a NaN. `Fixed24::to_f32` is the only entry point
  that can see out-of-range input and is where clamping happens.
- Interpolation is written `a(1 − t) + bt`, not `a + (b − a)t`: the second is
  not exact at `t == 1`, which showed up as a gradient's last stop landing
  one 8-bit step off the colour the user picked.

### `lyon_algorithms` evaluated and not taken

`10-architecture.md` lists it beside `kurbo` for this crate. It is not used:
it is `f32`, and everything this crate would want from it — walking,
raycasting, AABBs — `kurbo` already provides in `f64`. Taking it would add a
second curve vocabulary and a precision boundary for nothing. Revisit only if
tessellation lands here, which the architecture assigns to `xarast-render`.

### Regular shapes: `regular::regular_shape_outline`

Added 2026-09-23 for XARA-T-0013. It generates polygons, stars and ellipses
from their parameters, following the facts in `research/01 §4.7.1`. The
work is in `f64`, and each point is quantised once as it is emitted.
Primary points are at `π/n + k·2π/n` in the frame
`cos θ · major − sin θ · minor`. Corner rounding cuts along both edges,
scaled down when two cuts would overlap, and joins them with one cubic
whose controls sit at 0.552 of the way to the corner. A one-cubic edge
template is fitted by a similarity. Anything else is a straight edge.
NaN, infinite or negative parameters degrade rather than panic, and more
than `MAX_REGULAR_SIDES` (4096) sides returns `None`, as a bound on
memory.

## Picking: `hit` and `hit_index` (phase 7 W3, XARA-US-0031)

Added 2026-09-23. Tasks XARA-T-0148 (precise tests), XARA-T-0149
(`HitIndex`), XARA-T-0150 (grid-versus-BVH verdict), XARA-T-0151
(`fuzz_hit_test`); the app side is XARA-T-0152.

### What a hit is

A pick is a **disc** in document space: the pointer plus a radius in
millipoints (the device tolerance times millipoints per pixel,
`HitTolerance::from_device(px, mp_per_px)`). A fill is hit when the disc
meets the filled region under the path's fill rule. A stroke is hit when
the disc meets the **outline the renderer draws** (`kurbo`'s stroker, as
`stroke_to_path` uses), so caps, joins, the mitre limit and dashes all
count and a dash gap does not. A stroke whose document-space width is at
most `min_stroke_width` (one device pixel; a hairline always) is picked as
a band of that width around the dashed centreline. `HitShape::hit` tests
the stroke first, since it is painted on top.

The object's matrix is applied to the control points in `f64` **before**
anything is flattened or measured, so the pick stays a circle on screen
under any transform. A mirroring matrix negates winding numbers, so the
rule is applied to the object-space winding (`Positive` still means what
is drawn).

### How the disc test is exact

1. Only segments whose control box reaches a horizontal band round the
   pick and extends to its right are flattened, and **within** a curve
   only the sub-pieces whose box meets the band are subdivided; the rest
   become their chord. Geometry outside the band contributes zero
   crossings to a +x ray cast from inside it, so this is exact, and a
   huge curve passing through costs O(depth) edges.
2. If the pick point is covered, it is a hit.
3. Otherwise the boundary of the region, if the disc meets it, lies on
   edges within the radius. A cheap pass probes either side of each such
   edge's closest point, which settles NonZero/EvenOdd at the first edge.
   The exact pass splits each near edge wherever another crosses or
   overlaps it and probes both sides of every piece. That is what stops
   an edge between winding −1 and 0 from attracting a `Positive` pick,
   and cancelling coincident edges from attracting any.
4. Probes sit `PROBE` = 1e-3 mp off the edge; flattening is at
   `max(radius / 8, 0.25)` mp. Those are the error bars on "within the
   radius".

For strokes, only the runs of segments whose control **hull** (not just
box: a quarter-ellipse's box contains the centre) is within reach are
stroked. That is exact, not approximate: a run's end is a vertex shared
with a segment too far away to matter, and the spurious cap there lies
within that segment's own reach. An allocation-free pass over
`Path::segments` answers the common miss first.

**Invariant (XARA-T-0191): the stroker's accuracy in `hit_stroke` is a
fixed 0.25 mp (the renderer's), never scaled with the pick radius.** It
used to be `max(r/8, 0.25)`, the same figure as the edge flattening. But
it is an error on the *outline itself*, stacked on the flattening: with
a 1903 mp stroke and r = 406 the outline drifted ~50 mp, which is more
than the slack the oracle (`stroke_matches_the_renderers_outline`) and
a user are entitled to, so a pick inside the drawn stroke missed. The
failing seed (mitre limit 1.0, a self-overlapping cubic) was a red
herring: the join and self-overlap were irrelevant, only wide stroke +
large radius mattered. The oracle was right; the hit-test was wrong.
Regression: `hit.proptest-regressions` and
`regression_mitre_limit_one_self_overlapping_cubic`. (Re-checked under
the polyline oracle below: restoring the radius-scaled accuracy still
fails that seed, so this diagnosis stands.)

**Invariant (XARA-T-0344): the hit oracle counts winding numbers against
a polyline, never against `kurbo` cubics.** The second failing seed of
`stroke_matches_the_renderers_outline` (non-uniform scale, mitre limit
1.0, line + cubic) was the *oracle's* bug, not the hit-test's: the
hit-test and `stroke_to_path` both stroke in object space and transform
afterwards, the same geometry, and the hit-test was right. The "inside"
sample sat 2 160 mp from the centreline of a stroke whose doc-space
half-width is ≤ 1 403 mp. `kurbo`'s `winding` (0.13.1) finds a cubic's
crossing with `solve_monotonic_for_y`, which takes the first root in
[0, 1] from the closed-form `solve_cubic`; one outline cubic had a `y`
polynomial that is quadratic up to rounding noise once scaled
(leading coefficient −1.8e−12 against b ≈ −3 377), the closed form
then returns roots that are not on the curve (t = 0.849, y = 8 112, for
y = 9 667; the true root is t = 0.626), and the crossing lands on the
wrong side of the point. The oracle now flattens the region at 0.05 mp
(`polyline` in `tests/hit.rs`) and counts against lines, which is
exact; 0.05 mp is far inside the 1 mp / 2 mp margins every check keeps
from a boundary. Production picking is unaffected (`hit.rs` flattens
with its own chord bound and counts its own crossings). `fill_contains`
(`measure.rs`) still uses `kurbo`'s winding, but on integer object-space
coordinates, where the leading coefficient is an exact integer; a probe
of near-quadratic integer cubics up to 10^8 mp found no disagreement
with the polyline. **Do not call `kurbo`'s `winding`/`contains` on
transformed (non-integer) cubics** where correctness matters.
Regression: `hit.proptest-regressions` and
`regression_non_uniform_scale_bevelled_line_and_cubic`.

### `tight_bounds` is clamped to the control hull (XARA-T-0355)

Root cause: not a wrong root. `kurbo`'s `bounding_box` evaluates the
curve at each extremum `t` and `Rect::from_kurbo` rounds outwards
(`floor`/`ceil`). A cubic whose last two control points coincide (e.g.
`(0,0) (0,-64471) (0,88035) (0,88035)`) has its extremum at t = 1; the
quadratic solve lands a few ulps away, the evaluated y is 88035.00000001,
and `ceil` makes it 88036: one millipoint outside the hull.
**Invariant: `tight_bounds()` of every segment is intersected with that
segment's control-point hull**, which is mathematically lossless (a
Bezier never leaves its hull) and only removes float noise. Other users
of `bounding_box` (`measure.rs` nearest-point culling) use it as a
conservative distance cull in f64 and are not affected. Regression:
`tight_bounds_stay_inside_hull_for_degenerate_vertical_cubic`; property
`hull_contains_tight_bounds` also run at 30 000 cases.

### Bounds on hostile input

A work limit (2·10^7 edge visits), an edge cap (2^21) and a dash cap
(10 000 dashes; denser is treated as solid, which it is on screen) keep
one test bounded. Past a limit the answer is **hit**: the disc is then
within the radius of the outline, and erring towards a hit is the right
direction for picking. The dash offset is reduced modulo the period
before `kurbo` sees it.

### `HitIndex`: the uniform grid won (the open question is closed)

Benchmarked (`benches/hit_index.rs`) against a static BVH written in the
bench: median split, 4 objects per leaf, max-z per node for best-first
picking, contiguous subtree ranges so an enclosed node emits a slice,
O(log n) refit for moves, full rebuild for insert/remove. 100 000
objects, four scenes (uniform, clustered, mixed sizes with 1 % page-sized,
all stacked on one point), reference machine, `taskset -c 4-7`. Other
agents kept the load average at 10–40, so read the figures as ±30 %:

| 100k objects | grid | BVH |
|---|---|---|
| build | 5.5–12 ms | 8.6–23 ms |
| topmost pick, bounds only | 2.2–9.9 µs | 0.008–6.3 µs |
| marquee, small | 4.5–6.2 µs | 1.3–10 µs |
| marquee, quarter page | 0.24–0.79 ms | 16–380 µs |
| marquee, whole page | 0.78–1.38 ms | 5–37 µs |
| move: nudge / across the page | 69–86 ns / 150–680 ns | 87–92 ns (refit) |
| insert + remove one object | **23–30 ns** | **8–23 ms** (rebuild) |

**Verdict: the grid.** The BVH queries faster, especially marquees (it
emits enclosed subtrees wholesale). But every query of both structures
is two to three orders of magnitude inside the budgets, while a static
BVH pays a whole rebuild, more than a frame, on every create, delete,
paste and undo. A dynamic BVH would fix that at the cost of the build
quality that makes it fast; not worth it at these margins. Revisit only
if marquee over a million objects becomes a requirement.

Design points the numbers forced:

- **Hashed cells**, not a dense array: only occupied cells cost memory,
  and one object 14 km away cannot stretch the grid.
- **Cell side = 2 × median object extent** (floored by half the mean
  spacing). 1× halved pick time (2 µs vs 5 µs) but doubled whole-page
  marquee time (3.3 ms vs 1.6 ms), and marquee is the tighter budget.
- **Objects over 16 cells go on a large list** that every query scans
  (page backgrounds, frames).
- **Bounds are stored inline in each cell's list, and every slot records
  its index in each of its cells.** Without the positions a removal
  searched the cell's list: under 100 000 stacked objects a move cost
  12.9 µs. With them it costs 0.15 µs.
- A query spanning more cells than are occupied walks the occupied cells,
  so a whole-page marquee is O(objects), never O(area). A multi-cell
  entry is reported only from its first overlapping cell, so no per-query
  dedup set is needed.
- The cell size retunes itself when the population doubles or quarters.

### Budgets (phase 7: pick ≤ 2 ms worst case, marquee ≤ 20 ms)

| | Measured | Verdict |
|---|---|---|
| Topmost **precise** pick among 100k (index + `HitShape::hit` top-down) | 5.5–13.8 µs | passes (also the 1 ms asked for in US-0031) |
| Marquee over 100k, whole page | ≤ 1.38 ms | passes (also 5 ms) |
| **Adversarial:** 100k *unfilled* outlines all around the pick point | 17.7–21 ms | **fails** |

The failing row is intrinsic. Every candidate's bounds contain the point
and none of the geometry does, so every one needs a precise rejection:
about 180 ns each after the hull culling, down from 1.2 µs. No bounds
index can help. If a real document ever does this, the fix is on the app
side, not in the index: cache per-object "hollow" facts, or stop after N
rejections and widen the search lazily.

### Integration contract for `xarast-app` (XARA-T-0152)

- **What is indexed.** Every *selectable ink leaf* (not groups, not
  attributes) on a layer that is visible, unlocked and not a guide. That
  is what `edit::selectable_objects` walks today, but recursing into
  groups. Hidden and locked layers are **never** in the index. Toggling a
  layer's visibility or lock calls `retain` (or re-inserts its leaves), or
  simply rebuilds.
- **Key and bounds.** The key is the leaf's `NodeId`. The bounds are the
  document bounds cache (`tree.bounds(id)`), which must already include
  the stroke's reach (half width × the mitre/√2 factor). Empty bounds are
  allowed and never hit.
- **Z.** A `u64` where larger is nearer the viewer: the leaf's rank in
  render order (`walk_render` preorder over the whole document), spaced
  out as `rank << 16`. Inserting between two objects then takes a
  midpoint, and reordering one object is a single `set_z`. When no gap is
  left, rebuild (`HitIndex::from_entries`, 6–12 ms at 100k).
- **Build** once per document load (`from_entries`), and again on
  `Intent::InvalidateAll`.
- **Update** on every committed transaction, not on every drag frame:
  - moved or reshaped leaves → `set_bounds` with the leaf's new bounds
    (the bounds cache invalidation climbs);
  - created leaves → `insert`;
  - deleted leaves → `remove`;
  - z-order edits → `set_z`.

  **As built (XARA-T-0168):** the app keys z per *top-level object*
  (`key + leaf ordinal`, tops `2^24` apart) rather than `rank << 16` per
  leaf, and re-inserts a touched object's leaves rather than calling
  `set_z`/`set_bounds` leaf by leaf; the tree's change journal
  (`Tree::drain_changes`) says what was touched. See `tools.md` decision
  37.

  Group and ungroup change no leaf's bounds or z. A live drag preview
  changes nothing in the index; the selection is already known while
  dragging.
- **Pick** (`HitTester::pick`):
  1. Take `radius = tolerance_px × mp_per_px`.
     `idx.candidates_at(p, Mp(radius))` yields leaves nearest the viewer
     first.
  2. For each, build a `HitShape` from the node's path and **resolved**
     attributes:
     - `fill: Some(rule)` only if the fill is not transparent or none;
     - `stroke: Some(style)` only if the line colour is not none;
     - `transform` for text glyph runs and images, identity for ordinary
       paths (their geometry is already in document space).
  3. Call `hit(p, HitTolerance::from_device(px, mp_per_px))`. The first
     `Some` wins.

  The pick modes:
  - `PickMode::TopGroup`: map the hit leaf to its outermost group below
    the layer (tree ancestors).
  - `Leaf`: the leaf itself.
  - `Under { below }`: skip candidates until the first whose z is below
    `below`'s.

  For images, use a `HitShape` on the image's parallelogram with
  `fill: Some(NonZero)`. Alpha-aware picking then samples the image at the
  inverse-mapped point, on the app side.
- **Marquee** (`pick_all`): `idx.query_rect(rect, RectMode::Touch |
  Enclose, &mut out)` on leaf bounds, then map leaves to top groups. For
  `Enclose` in top-group mode, a group is selected when its own bounds are
  enclosed (check the group's bounds cache once per distinct group found).
  For `Touch`, any touching leaf selects its group.
- **Handles** are not in this index. They are picked in device space with
  a fixed pixel radius (T3.7).

## Path editing: `path_edit` and `fit` (phase 7 W6, XARA-US-0034)

`EditPath` (`path_edit.rs`) is the node view of a `Path`: subpaths of
`EditNode { at, flags, ctrl_in, ctrl_out }`; segment `i` leaves node `i`
and is a cubic exactly when node `i` has `ctrl_out` (then node `i+1` has
`ctrl_in`). Every editing tool works on it and writes back with
`to_path()`.

Decisions:

1. **The round trip is exact** (`from_path(p).to_path() == p`, proptest)
   for both ways of closing: the `.xar` way (the last point repeats the
   first, then Close) folds the repeat into node 0 and remembers
   `explicit_close` + the repeat's flags; the SVG way (Close alone) does
   not. Subpaths with fewer than two nodes are dropped on write.
2. **Point indices ⇄ nodes**: `layout()` gives each written point's role
   (`PointRole::Node`/`Control`); `node_point_index` / `node_at_point_index`
   (and `xarast_app::node_edit::Nodes`, which caches the map) translate
   the session's point selection. The repeated closing point maps to node 0.
3. **Flag model = the original's** (`research/04 §4.11`): `ROTATE` on a
   node = smooth (collinear handles); `SMOOTH` on a handle = auto-placed,
   recomputed by `resmooth_around` after a node moves; dragging a handle
   by hand clears `SMOOTH` on it, its opposite and its node. The doc
   comment on `PointFlags::SMOOTH` ("tangents collinear") predates this and
   is loose.
4. **Flags on write**: a path that came with a flag array keeps it as is;
   a flagless one gains one only when a flag other than `END_POINT`
   appears, and then every on-curve point is marked `END_POINT`. So
   add∘delete on a flagless path is exact.
5. **Split** = de Casteljau, rounded to mp; the new node is `ROTATE` on a
   curve, a plain corner on a line. **Delete** of a node between two
   curves rebuilds one cubic keeping the outer handles' *directions*,
   recovering the split parameter (the handle-length ratio, refined by a
   1-D search for the `t` whose re-split best reproduces the deleted
   node's handles). The original keeps the outer handles unchanged (no
   refit); we differ so that add∘delete is the identity (criterion 15).
   Deviation bound in the proptest: `2 / min(t, 1−t)` mp. A line on
   either side gives a line (the original's rule).
6. **Smooth** on a node whose handles are already collinear (within the
   rounding of the node and both ends: `sin θ ≤ 2/la + 2/lc`) only sets
   `ROTATE`, so smooth → cusp → smooth is the identity on a smooth node;
   otherwise it auto-places the handles (the original always does).
   Straight segments stay straight. **Cusp** clears both flags, handles
   stay.
7. `close` folds coincident ends; `open` drops the closing segment's
   handles, so close∘open keeps the point count. `break_at` opens a
   closed subpath at a node (copies at both ends) or splits an open one;
   it renumbers nodes — callers re-find nodes by position.
8. **Fitter** (`fit.rs`, Schneider 1990 from the paper's description):
   corners where the direction turns > 90° over a window of the
   tolerance, least-squares handle lengths with fixed tangents, 4 Newton
   reparameterisations when within 4× the tolerance, else split at the
   worst sample with a shared tangent. A 90° turn is *not* a corner (the
   original's "more than 90°"). `fit_stroke_indexed` returns each node's
   sample index for incremental fitting. Measured: a 4000-sample stroke at
   the default smoothing fits in well under a frame and stays within the
   tolerance (`xarast-app/tests/freehand.rs`).

### Contours: `geom::contour` (XARA-T-0330, 2026-10-15)

- **`offset_region(region, rule, d, join, mitre_limit, tol)`**: clean the
  region (`self_union` under its rule), build the **band** — everything
  within `|d|` of the flattened outline — and union it with the region
  (`d > 0`) or subtract it (`d < 0`). Output is clean under non-zero.
  `offset_clean` skips the first cleaning.
- **The band is oriented pieces, not a stroke**: per edge a CCW
  rectangle that *holds the edge's two end points* (so it meets the
  join pieces exactly, no crack), per turn the join wedge on the outside
  only (disc sector at `2·acos(1 − tol/d)` per segment, mitre quad while
  `1/cos(θ/2) ≤ limit`, else the bevel triangle). All CCW, so non-zero
  cannot cancel anything. Pieces are unioned in runs of 16, then
  pairwise (`Pieces`).
- `CONTOUR_MITRE_LIMIT` = 751 941 / 65 536 ≈ 11.47 (the original's
  default, `Kernel/paths.h:467`); `MAX_CONTOUR_DISTANCE` = 2²⁶ mp; the
  fitting/flattening tolerance is `max(tol, |d|·1e-4)`; an inset of at
  least half the region's smaller side returns empty without work.
- `step_distances(width, n, profile)`: ring `k` at `width ·
  p((k+1)/n)` with the bias negated (`n ≥ 2`; one ring is the full
  width). `steps(sources, outer, distances, …)`: outer offsets the union
  of the sources, inner insets each source and unions the insets (the
  original contours per object and merges). Collapsed inner rings are
  empty paths, kept so ring indices stay stable. `contour_tolerance`:
  the original's flatness, smaller side / 375, clamped to 10..=375 mp.
- Measured (release, a 1 015-segment curved blob): ±1 000 mp 25 ms,
  ±1e6 mp 33 ms per pair of offsets.
- Output rings are **polylines** (curves flattened at the tolerance):
  the boolean restore only keeps untouched *input* curves. Fitting the
  rings back to cubics (`fit`) is an open TODO.
- Property tests (`tests/contour.rs`): identity at 0, monotone area,
  n round offsets of w ≈ one of n·w (tight bounds within 64n mp, area
  within 1 %), curved insets past the inradius empty, hostile
  distances/rules/limits sane and bounded (≤ 256 × (segments + 4)).

### Blends: `geom::blend` (XARA-T-0333, 2026-10-15)

- **`Outline`**: one subpath in `f64` (`Outline::subpaths`; an explicit
  closing point is kept, a missing one becomes a closing line).
  `align(start_point)`: the original's rules (`Kernel/nodebldr.cpp:5042-
  5110`) — a closed outline rotates to the end point at the recorded
  point index (a control point means the end point before it; the index
  counts the subpath's points as `.xar` does: move 1, line 1, curve 3, so
  it is our `Path` point index), else to the **end point nearest the
  bottom-left corner of the end points' box** (the original's computed
  but unused rule; its real one uses a point `GWinding` in closed CDraw
  returns), then runs anticlockwise (positive area, y up). An open one
  is reversed when the index is its last point, or (no index or a middle
  one) when its start is less bottom-left (`x + y` larger) than its end.
- **`pair_up(m, n)`**: the original's `ListStepper`
  (`Kernel/nodebldr.cpp:163-200`): `max(m, n)` pairs, the longer list one
  by one, the shorter advancing by a Bresenham step. Every subpath takes
  part; nothing is dropped.
- **`correspond(a, b, one_to_one)`**: a line meeting a shape runs there
  and back (`ConvertLineToShape`); a lone move is a zero-length line.
  One-to-one with equal segment counts pairs segments in order (the
  original's `GBlend` 1-to-1). Otherwise **Xarast's rule** (the
  original's `GBlend::BlendMtoN` greedily groups segments in 28-bit
  fixed point): segment lengths (kurbo, 1 mp accuracy) as fractions of
  the total, then a two-pointer walk; breakpoints closer than half the
  shorter of the two current segments are one corner, any other splits
  the other side's segment at that length fraction, used as the curve
  parameter (as the original does). At most `m + n` pieces. A line with a
  curve becomes a curve with controls at its thirds. `at(t)` lerps every
  point and quantises; closed if either end is.
- Property tests (`tests/blend.rs`): the outline at 0 and 1 holds every
  end point of its end, in order, with the area within a split's
  rounding (perimeter × ½ mp); curves within 0.1 %; one-to-one exact; the
  stepper complete and monotone; hostile `i32` input never panics.
- **Not done:** F6 (arc-length distribution along a path, tangential
  rotation; nothing in the corpus); the phase plan's "VM comparison over
  12 pairs" of the matching rule.

### Moulds: `geom::mould` (XARA-T-0334, 2026-10-16)

- **Facts, checked numerically against the original (not translated):**
  an envelope is the **bilinearly blended Coons patch** of its four
  cubic sides (the polynomial in `Kernel/genv.cpp:244-369` equals it to
  1e-15); a perspective is the **projective map** of the unit square
  onto its corners (`Kernel/moldpers.cpp:1840-2080`, via vanishing
  points, equals the homography). Cage order: source bottom-left image
  first, then clockwise; left side up, top right, right down, bottom
  left. A straight side's handles sit at its thirds, rounded towards
  zero (`Kernel/moldenv.cpp:807-866`).
- **Refit:** each source cubic mapped at 0, ⅓, ⅔, 1 → the cubic through
  them; checked at ⅙, ½, ⅚, split at the middle while over the
  threshold (`Kernel/gmould.cpp:189-258`). Our error: max axis
  deviation in mp vs the threshold (the original normalises by cage
  size). **Bounded**: depth ≤ `MAX_DEPTH` 10, ≤ `SPLITS_PER_SEGMENT` 64
  splits per input segment per path. Lines stay lines under a valid
  perspective; under an envelope they are cubics; a closed subpath's
  implied closing line is moulded too.
- **Never a NaN:** an invalid perspective (coincident, flat, crossed or
  non-convex; the original's `WillBeValid` rule) maps **bilinearly**; a
  projective denominator is clamped away from 0; every output clamped to
  the extent; a zero-width source axis maps to 0. `affine()` detects a
  parallelogram (exact integer test) and, for envelopes, straight sides
  with handles within 1 mp of the thirds.
- `MouldTransform`: `is_invertible() == false`, line widths unchanged
  (the original's scalar is 1, `Kernel/tranform.cpp:136`),
  `source_point` = 9×9 grid nearest + ≤ 24 Newton steps.
- 2×2 envelope (24 points): Coons over piecewise (2-cubic) sides — our
  definition; the original's also smooths inner lines and has no tag.
- Vanishing points (XARA-T-0342, the mould tool): `vanishing_points`
  gives `[Sides, Ends]` — where bl–tl meets br–tr and where br–bl meets
  tr–tl — `None` when within 1e-9 rad of parallel or outside the
  extent. `drag_vanishing_point(corners, which, to)`: the corners on the
  side the point lies beyond (parameter `t < 0` along the first line:
  its start) slide along their edge's line, on lines from the fixed far
  corners through `to`; `None` unless the result validates. Facts from
  `Kernel/moldpers.cpp:1840-1980`; the formulation is ours.
- Property tests (`tests/mould.rs`): corners map exactly (envelope:
  bit-exact; perspective: ≤ 1 mp), no NaN or malformed path on hostile
  cages/thresholds, subdivision bounded at threshold 1e-9, outputs pass
  through the images of on-curve points, refit within 4 × threshold,
  perspective collinearity and inversion, cage↔path round trips.

### Bevel cross-sections (XARA-T-0322, 2026-10-15)

`xarast_geom::bevel`: `BevelType` (the original's fifteen styles, record
order), each style's `height(u)` / `slope(u)` across the band (u = 0 at
the low edge, heights in units of the band's width, slope bounded to
±8), `Light::new(angle, tilt)` and `shade()`: a face's brightness against
the flat plane in −1..1. Pure functions; the renderer finds the faces with
a distance transform (`render.md`, "Bevels"). **The curves are Xarast's
own** — the original's are in closed CDraw — and stay unverified until
phase 13 D's empirical determination runs (`render.md` TODO 30).

## Invariants that must not be broken

- `EditPath::from_path(p).to_path() == p` for well-formed paths whose
  subpaths have ≥ 2 nodes (`tests/path_edit.rs`).

1. **The five `Path` invariants**, enforced by `PathBuilder` by construction
   and checked by `Path::validate()` for paths that did not come through it
   (in practice, the `.xar` importer):
   1. `points.len()` equals the total verb arity.
   2. `flags` is empty or exactly as long as `points`.
   3. The first verb is `MoveTo`; every `Close` is followed by a `MoveTo` or
      by the end of the path.
   4. No two consecutive `MoveTo` verbs.
   5. Every coordinate is inside `Mp::EXTENT`.
2. **Y is up.** `Rect::lo` is the bottom-left. The flip to a Y-down frame
   belongs to the consumer — SVG, PDF, the screen — never to this crate.
   Putting it here would mean the document model silently carried two
   conventions and every bug in it would look like a rendering bug.
3. **Points translate, vectors do not.** `.xar` writes the major and minor
   axes of regular shapes *without* the coordinate-origin translation while
   everything else gets it, so `transform_point` and `transform_vector` are
   separate methods on purpose.
4. **FIXED16 quantisation happens exactly once, on write.** Never on an
   intermediate.
5. **Colour components are clamped and never NaN.**
6. **`Path: Eq` means "the same shape".** Nothing editor-side may be stored
   in a `Path`.
7. **`Rect::EMPTY` is the union identity**, so bounding-box loops need no
   `Option<Rect>` and no special case.
8. **The boolean engine's orientation is authoritative.** `i_overlay` emits
   outer contours counter-clockwise and holes clockwise, exactly.
   `Path::normalised()` recomputes orientation from a nesting heuristic and
   is *worse*; on a shape whose subpaths touch it silently flipped a hole
   into an outer contour (a 2.4 M mp² area error, found by the
   inclusion–exclusion test). Boolean output therefore goes through
   `Path::canonically_ordered()`, which canonicalises only the start vertex
   and the subpath order — the two things the engine does not define.
9. **Filling closes open subpaths; `kurbo`'s winding number does not.**
   `BezPath::winding` sums the segments it is given and casts its ray to
   the left, so on a path with an open subpath its answer depends on the
   ray's direction. Anything that asks `kurbo` for a winding number must
   close the subpaths first (`measure::closed_for_fill`); `fill_contains`
   does since 2026-09-23. `kurbo`'s stroke output also has unclosed
   subpaths.
10. **Area identities carry two slack terms, and both are physical.** The
   relative `1e-4` from the phase document, plus `4·sqrt(area)` for the
   millipoint lattice (a one-millipoint band around a shape of area `A` has
   area of order `4·sqrt(A)`), plus — for identities that compare results
   reached by different routes through the pipeline — `2·tol·perimeter`,
   because flattening at tolerance `t` can move a boundary by `t`. Demanding
   better than that of a curved input demands accuracy a polygonal engine
   cannot have.

## Dead ends (do not retry)

- **A contour band made by stroking the closed outline** (`kurbo`'s
  stroker at twice the distance). Its two contours are oppositely
  oriented and where the inner one loops back (an inset past a
  curvature or the inradius) the windings cancel to zero: holes in the
  band, so an inset kept the whole region (a 100 k triangle inset by
  1e6 came back unchanged). Found by the property tests (XARA-T-0330).
- **Unioning all band pieces in one sweep.** Quadratic in overlapping
  pieces: a 1 000-edge outline grown by 1e6 took 100 s; a run-then-pair
  tree takes 33 ms.

- **`kurbo::flatten` for anything whose tolerance is a contract.** Up to 9×
  over on cusp-heavy cubics. See above.
- **The `(3/4)·max(d1, d2)` perpendicular flatness bound.** Ignores
  tangential overshoot; measured 47× over tolerance.
- **`Path::normalised()` on boolean output.** Overrides an orientation the
  engine already had exactly right. Use `canonically_ordered()`.
- **A subpath's first vertex as the nesting probe in `normalised()`.** Start
  vertices are routinely shared between touching subpaths, where containment
  is a coin flip, and rotating the subpath moves the probe, so the result is
  not idempotent. The probe is now the lexicographically smallest vertex
  nudged towards the vertex mean — both invariant under rotation and
  reversal. The *centre* does not work either: for a square with a hole,
  each centre is inside the other's outline and both come out as holes.
- **Reversing open subpaths in `normalised()`.** Direction is meaningful for
  them (stroke direction, arrowhead end) and reversing moves the start point,
  breaking idempotence.
- **A flattened polyline as an arc-length reference.** Over 100 000 vertices
  the millipoint quantisation jitter *adds* length, so the "dense
  approximation" comes out longer than the exact value. Sample the curve in
  `f64` instead.
- **Comparing `kurbo` winding contributions per segment across a
  mirroring transform.** The ray direction flips with the mirror, so
  per-segment numbers legitimately differ; only totals over closed
  contours compare.
- **A control *box* to decide which stroke segments can reach a pick.** A
  quarter-ellipse's box contains the ellipse's centre, so every hollow
  shape went to the stroker (120 ms for the adversarial stack); the
  control *hull* is the right lower bound (18 ms).
- **Asserting that a larger pick radius keeps every hit** (fuzz). False on
  zero-area slivers, where two flattening tolerances disagree.
- **`parry2d`** — pure Apache-2.0, physics-shaped, its shape model does not
  fit Béziers. **`geo`** — polylines only, GIS coordinates. **`bezier-rs`** —
  its own authors moved to `kurbo`. **`geo-booleanop`** — abandoned in 2020.
- **`f32` coordinates anywhere in the model.** **Panicking `Mp` operators.**
  **Wrapping `Mp` operators** — plausible-looking wrong geometry is the worst
  outcome of all.

## Evidence: why we do not use `kurbo::flatten`

Replacing a mature library's flattener needs paying for in measurement.
`examples/kurbo_vs_ours.rs` runs both over the same 20,000 random cubics
spanning ±200,000 mp, with the same deviation metric and sample count:

| Requested tolerance | Ours, worst | Ratio | `kurbo`, worst | Ratio |
|---:|---:|---:|---:|---:|
| 1 | 1.53 | 1.53× | 2.04 | 2.04× |
| 10 | 10.40 | 1.04× | 18.49 | 1.85× |
| 50 | 49.90 | 1.00× | 462.66 | **9.25×** |
| 100 | 99.64 | 1.00× | 311.19 | 3.11× |
| 1000 | 986.21 | 0.99× | 3898.89 | 3.90× |

`kurbo` violates its own tolerance contract by up to 9×. Boolean operations
depend on that bound — a flattening error larger than requested changes
topology, not just smoothness — so the contract has to hold. Our 1.53× at
tolerance 1 is millipoint quantisation, not algorithm error: you cannot place
a vertex to better than 1 mp.

The cost is roughly **2× the vertices** for the same requested tolerance. That
is the honest trade, and it is an open optimisation: nobody has yet measured
ours at tolerance *t* against `kurbo` at tolerance *t/10*, which is the fair
comparison at equal achieved accuracy. If `kurbo` wins that, the custom
flattener should go.

## Open TODOs

- ~~`Mp::PER_MM` is inconsistent with `Mp::PER_INCH`.~~ **Decided: derive it.**
  `PER_MM` is now `PER_INCH as f64 / 25.4`, so millimetre and inch conversions
  are exactly reciprocal. The phase document's `2834.652715` came from the
  original (`Kernel/units.h:120`), is 2.5 ppm high, and matches no definition
  of the inch — and the original contradicted itself anyway, clamping line
  widths with the correct `2834646` mp/metre in `Kernel/linwthop.cpp:123`.
  Millipoints are the storage unit in both file formats and millimetres are
  only an entry and display unit, so nothing round-trips through this constant
  and there was no compatibility to keep, only 2.5 µm per metre of error to
  drop. A test pins the reciprocity.
- ~~`fuzz_path_boolean` and `fuzz_svg_path_parse` are not written.~~ Done
  2026-09-23 (`fuzz/fuzz_targets/`), and run nightly. First ten-minute
  runs: `fuzz_path_boolean` 69 k execs at ~115 exec/s, clean;
  `fuzz_svg_path_parse` found the arc OOM below within a minute, then
  12.9 M execs at ~21 500 exec/s, clean.
  - **SVG numbers are bounded before `kurbo` parses them.** An arc's radii
    are not coordinates, so the post-parse extent check never saw them, and
    `kurbo` sizes its arc approximation from the radius: `A 1 8e77 …`
    between two ordinary points asked for ~10^13 cubics (1.8 GB). Every
    number in the string must now be within twice the document extent.
  - **Flattening a degenerate cubic is expensive.** A cubic whose control
    points sit on its end point is a straight line, yet `flatten_traced`
    gives ~9 000 vertices for an extent-long one at `Tolerance::BOOLEAN`,
    because the chord bound sees the non-uniform parametrisation, not the
    geometry; one overlay of it costs ~35 ms. Correct, not a bug — but it
    is why `fuzz_path_boolean` runs one `BoolOp` × `FillRule` per input
    rather than all sixteen, and a candidate for an early collinearity exit
    in the subdivider.
- **Mixed-run refitting is not implemented.** Step 3 restores an untouched
  cubic verbatim; a run the boolean *cut* stays a polyline rather than going
  through `kurbo::fit_to_bezpath_opt`. Fitting a run that contains a corner
  would round the corner off, so it needs corner detection first. The
  verbatim restore handles the case that actually matters — a curve the
  operation did not touch.
- **The `cavalier_contours` offset spike** has not been run and is not
  needed for contours: `geom::contour` meets the phase-13 properties.
- ~~Whether the uniform grid suffices for `HitIndex` at 100 000.~~
  **Settled 2026-09-23: the grid**, see "Picking" above (XARA-T-0150).
- **`stroke_to_path` passes the dash offset to `kurbo` unreduced.** The hit
  test reduces it modulo the period because `kurbo` walks the offset one
  element at a time; the renderer's path does not, so a hostile offset of
  2^30 with a 1 mp pattern is slow there. Same fix, one line.
- ~~Nearest point on a path for snapping through `HitIndex` with a
  transform (XARA-T-0153).~~ **Done 2026-09-23:**
  `nearest_point_transformed(path, m, p, radius, accuracy)` transforms
  the control points in `f64` first and skips segments whose transformed
  control box is out of reach; `nearest_in_index(index, p, radius,
  measure)` takes the closest over `candidates_at`. `xarast-app`'s object
  snap uses both (`tests/nearest.rs`).
- **Precise marquee touch** (geometry, not bounds, meeting the rectangle)
  is not provided; the marquee is on bounds, as the phase document
  specifies. A `rect_touches` built on the disc test's band machinery
  would be the way if it is ever wanted.
- **Arc segments**, if `i_curve` is adopted.
- **Cross-architecture `f64` determinism** is not gated. No `mul_add` and no
  fast-math anywhere in the geometry path, which is the precondition;
  promote to a gate in phase 12 (XARA-T-0300; not done in XARA-US-0061
  because it cannot be verified without an aarch64 host).
- **`insta` snapshots of SVG path data** for the corpus are not wired up; the
  corpus is exercised by assertions instead.
