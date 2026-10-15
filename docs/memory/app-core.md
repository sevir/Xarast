# app-core

Memory note for the **application core** (`crates/xarast-app`), Phase 5.
Arbitrated decisions in [`../10-architecture.md`](../10-architecture.md) §2, §3.4,
§3.5b, §4, §5. Phase specification:
[`../phases/phase-05-shell-and-ui.md`](../phases/phase-05-shell-and-ui.md).

`xarast-app` sits between the document model and the renderer, and below
both consumers. It has **no UI toolkit dependency and no windowing
dependency**, it builds and tests with no GPU and no compositor, and
`xarast-ui` and `xarast-shell` depend on it without depending on each
other.

---

## The contract the other Phase 5 crates build against

This is the part to read before writing a line of `xarast-ui` or
`xarast-shell`. It is deliberately small.

### 1. `Intent` in, `Changed` out

```rust
let changed: Changed = session.apply(intent)?;     // or app.apply(intent)
```

[`Intent`] (`intent.rs`) is the **only** input vocabulary. The shell
raises them from platform events; the UI raises the same ones from menus,
panels and buttons. Neither crate has to know the other exists, and
neither reaches into `EditState`, the `Viewport` or the arena directly.
It is `#[non_exhaustive]`: Phase 7 adds variants without breaking either
caller.

[`Changed`] is a bitflag answer — `VIEW`, `SELECTION`, `DOCUMENT`, `UI`,
`CACHE` — with `needs_redraw()` and `needs_scene()` on top. A shell that
ignores it repaints every frame; a shell that honours it parks. An
intent that changes nothing returns `Changed::empty()`, and that is
checked by a test, because "did anything happen" is the question an
idle-CPU budget rests on.

**Application-level intents** (XARA-US-0082): `ShowOpenDialog`,
`OpenFile(path)`, `CloseDocument`, `ClearRecent`, `Quit`. `AppState::apply`
handles them itself; a `Session` ignores them. The two only the platform
can do (`ShowOpenDialog`, `Quit`) are queued as `PlatformRequest`s that the
shell drains with `AppState::take_requests()` and carries out — the core
never shows a dialog or ends the process. `Changed::ACTIVE` says a
different document (or none) now has the canvas: re-title, re-size, frame.

**The command table** (`command.rs`, phase-05 W4.7): `AppCommand` names
each menu operation with its `label()`, its `shortcuts()` as
toolkit-neutral `KeyChord`s (first = the one a menu shows),
`needs_document()` and `intent(canvas_centre)`. The menu bar draws it,
the shell binds keys from it; both therefore raise the same intents.

`Modifiers` is the semantic triple `constrain` / `adjust` /
`alternative` plus `snap`, never `Ctrl`/`Shift`/`Alt`. The shell owns
that mapping table. They are sampled continuously, never latched at drag
start.

### 2. `EditState`, `Viewport` and `Session` are public and `Send`

A `const` block in `lib.rs` asserts `Send` for `EditState`, `Viewport`,
`Session`, `AppState` and `Intent`, so a change that breaks it fails to
compile here rather than in the shell. They are `Send`, not `Sync`: the
document lives on one thread and the render thread gets an immutable
scene and resolver snapshot (architecture §5).

### 3. Scene building

```rust
// Per frame, in the session that owns the walker's caches:
session.rebuild_scene(dirty: Option<DeviceRect>) -> Result<SceneStats, _>
session.scene()      -> &Scene
session.resolver()   -> &Resolver
session.view_params() -> ViewParams

// From a shared reference, allocating afresh:
xarast_app::build_scene(&session, dirty: Option<DeviceRect>) -> BuiltScene
```

`build_scene` is the entry point the phase document names. It returns
**`BuiltScene`, not `Scene`**, and that is deliberate: a `Scene` is *not
self-contained*. Its paints hold `RampId`s and `ImageId`s into a
`xarast_render::Resolver`. Hand the scene to a backend without its
resolver and every gradient renders transparent. `BuiltScene` derefs to
`Scene`, so `&*built` is a `&Scene` wherever one is wanted, and it also
carries the `ViewParams` and the `SceneStats`.

Prefer `Session::rebuild_scene` per frame: it reuses the walker's
interned ramps, its image registry, its attribute cache and the scene's
allocation. `build_scene` builds all of that from nothing each call.

### 4. Units and the one place the Y axis flips

Millipoints in, device pixels out. Document space is integer millipoints
with **`y` up**; device space is `f64` pixels with **`y` down**. The flip
belongs to `Viewport` and to nothing else — neither `xarast-doc` nor
`xarast-render` has an opinion about which way up a document is, and
giving a second component one is how drawings come out mirrored. `DPI`
likewise has exactly one owner: the shell computes it from the fractional
scale factor and calls `Intent::SetDpi`.

---

## Current state

| Module | Holds |
|---|---|
| `geometry` | `DevicePoint`, `DeviceSize`, `DocPoint`, `DocPointF`, `DocRect`, the saturating `f64 → Mp` quantiser |
| `edit` | `EditState`, `ControlPoints`, `SelectMode`, `Modifiers`, `ToolId`, `ToolState`, `selectable_objects` |
| `viewport` | `Viewport`, `ZoomTarget`, `page_rect`, `spread_rect`, `drawing_rect` (no pages), `drawing_or_page_rect`, `content_rect`, `nodes_rect` |
| `intent` | `Intent`, `Changed`, `PlatformRequest`, `PointerButton`, `PointerSample` |
| `command` | `AppCommand`, `KeyChord`, `ChordKey`, `ZOOM_STEP` — the command table |
| `recent` | `RecentFiles`, `MAX_RECENT` (10), `default_store_path` (`$XDG_STATE_HOME/xarast/recent`) |
| `paint` (private) | document fill → `xarast_render::Paint`, document transparency → `Transparency` |
| `walker` | `SceneWalker`, `WalkStats` — the arena→`Scene` walk |
| `commands` | `SetLayerVisible`, `SetLayerLocked`, `RenameLayer`, `SetActiveLayer`, `AddLayer`, `DeleteNode` |
| `session` | `Session`, `DocumentId`, `FileKind`, `Dirty`, `SessionError`, `build_scene`, `BuiltScene` |
| `headless` | `render`, `render_to_png`, `convert_to_png`, `HeadlessOptions`, `HeadlessFrame`, `HeadlessResult` (with `WalkStats`, `SceneStats`, zoom and view) |
| `prefs` | `Preferences`, `Unit`, `ThemePref`, `RendererPref` |
| `app` | `AppState` (with `open_replacing`, `with_recent_store`, `take_requests`), `DocumentSessions`, `DiagnosticLog` |
| `render_thread` | `RenderThread`, `RenderRequest`, `FrameJob`, `RenderedFrame`, `FrameReuse`, `RenderStats`, `FrameRenderer`, `CpuFrameRenderer` — the one channel to the render thread, and the worker that reuses pixels |
| `reuse` (private) | the pixel-reuse policy: `plan`, scroll, nearest-neighbour rescale, the zoom-out ring, Final columns |
| `ops` | `EditCommand` (transform, delete, create shape, set shape params), `CommandSink`, `on_locked_layer` — the commands tools emit (phase 7) |
| `tool` | `ToolMachine`, `Tool`, `ToolCtx`, `GestureEvent`, `Preview`, `Infobar`, `InfobarValue`, `Anchor`, `OverlayShape`, `pick` |
| `picking` | `Picker`, `PickMode`, `HitResult`, `HitPart` — precise picking over `xarast_geom::HitIndex` |
| `selector` | the unified selector: dual state, scale/rotate/skew, infobar |
| `shapes` | the rectangle and ellipse tools and quick-shape helpers |
| `tools` | `builtin()`, push, zoom and pending tools |
| `save` | `SaveJob` (snapshot → restore → thumbnail ∥ SVG → atomic write), `SaveWorker` (thread per job, waker), `SaveKind`, `SaveOutcome` |
| `locks` | `HeldLock` (a `DocumentLock` in a process registry), `LockMode`, `release_all` for signals |
| `autosave` | `AutosaveStore` (entries under `$XDG_STATE_HOME/xarast/autosave/<id>/`), `AutosavePolicy`, `Recoverable`, `scan` |
| `crash` | Crash reports (`install_panic_hook`, `CrashReport`, `redact`, `scrub_quoted`, the log ring, `set_context`), `expect_panics`, `FatalWatch` + `run_guarded`, `force_panic_point`, the sentinel (`begin_session`, `Sentinel`, `StartupCheck`, `SafeMode`, `CrashNotice`) — XARA-US-0064 |
| `prompt` | `Prompt`, `PromptChoice`, `PromptAnswer`, `ChoiceRole`: the questions the core asks |
| `thumbnail` | `CpuThumbnails` (the `ThumbnailProvider`), `thumbnail_png` |
| `schedule` | `QualityScheduler` (the Draft → Final policy, clock injected), `Canvas` (it joined to a `RenderThread` and a `Session`), `Backdrop`, `FINAL_AFTER` |
| `structure` | `StructureCommand`/`StructureOp` (group, ungroup, z-order, move-each, duplicate, cut), `ZOrder`, `AlignSpec`, `align_moves`, clipboard fragments (`copy_fragment`, `PasteFragment`, `fragment_svg`, `fragment_from_svg`) |
| `snap` | `SnapSource`, `SnapResolver`, `SnapSettings`, grid/guide/object sources, `GuideCommand` |

Tests: 13 viewport, 11 session/selection/command, 8 corpus (the 59 real
`.xar` files, through `XARAST_XAR_CORPUS`, never copied into the
repository). `examples/render_headless.rs` is the whole headless path in
one file.

**Measured against the corpus.** All 59 files import, walk into a
balanced scene and render headless with no GPU: 264 339 primitives in
total, 38 files with ink, 21 blank — and every blank one is blank for a
reason the walker *reports*, which is its own test.

---

## Decisions taken (and why)

1. **`Viewport` is parameterised by zoom and centre, not by an
   accumulated matrix.** A matrix multiplied through a thousand pan and
   zoom events drifts and eventually shears; two numbers cannot. The
   transform is derived on demand and is the only one in the
   application.
2. **Document `y` is up and the flip lives in the viewport.** See the
   contract above.
3. **Zoom is "one document inch covers `dpi` device pixels at 100 %".**
   Clamped to 1 %–25 000 %, as the original's range. Every hostile input
   — `NaN`, infinity, zero, a zero-sized viewport — is ignored rather
   than propagated, and a test asserts the viewport is bit-identical
   afterwards.
4. **The selection is an `IndexSet`, not a `HashSet`.** Selection order
   is user-visible: "align to the last object selected" and the key
   object both depend on it, and an unordered set makes the overlay
   order flicker between frames.
5. **`EditState::prune` runs after every mutation.** It is the whole
   price of keeping the selection out of the arena, and it is linear in
   the *selection*, not in the document. A control-point overlay may
   only exist on a selected node, so deselecting drops it.
6. **The scene id is the node's `Tag`, not its `NodeId`.** `Tag` is
   unique, stable across save and reload, and deterministic; a `NodeId`
   is an arena slot and would make the scene depend on allocation order.
   This is what makes `the_walk_is_stable_across_runs_and_across_walkers`
   pass.
7. **Ink is painted at `LeaveScope` when a node has children and at
   `Visit` when it has none.** A `.xar` path stores its fill as its own
   child and a parent paints after its children, so the child scope must
   still be in force when the parent's ink is emitted. `RenderWalk`
   emits scope events only for nodes that have children, so the two
   cases never overlap — the protocol `xarast_doc::walk` documents.
8. **A transparency ramp borrows its id from the colour ramp cache.**
   `Resolver::transparency_ramps` is a parallel `Vec` indexed by the same
   `RampId` space, so interning one means interning a *colour* ramp whose
   stops are the transparency levels as greys and writing the real table
   at that index. Two transparency ramps with the same levels share an
   id, which is the point; a collision with a real grey gradient is
   harmless, because the two tables are read by different code paths and
   both are correct.
9. **`push_transparency`/`pop_transparency` only wrap a non-opaque
   object.** Wrapping every object would double the op count for
   nothing.
10. **An invisible fill emits no command at all.** "No colour" in the
    model is a flat colour with zero alpha; emitting it would put a
    no-op in every display list and in every tile bin.
11. **The walker takes `&Document` and a test asserts the canonical
    digest is unchanged.** The signature makes mutation impossible by
    assignment; the digest is what would catch it happening through an
    interior-mutability cache. Bounds are read from the cache where it
    is warm and computed on the stack where it is not — filling the
    cache would make rendering an edit.
12. **`commands::SetActiveLayer` uses `Tx::act`, not `Tx::set_kind`.**
    See "Invariants" below; this is the one real friction with
    `xarast-doc` found in this phase.
13. **A guide layer may not become the active layer.** The model's own
    repair never elects one, so letting a command do it would produce a
    document the next repair silently undoes.
14. **`FileKind` is the Phase 6 seam.** `.xar` import and `.xarast` open
    are wired (`.xarast` since W4, 2026-09-23: `Session::open_bytes` →
    `xarast_format::open_reader` over the bytes; container and reader
    diagnostics go into `Session::diagnostics`; a refusal is
    `SessionError::Xarast`). `Session::open`, argv and File › Open all go
    through `open_bytes`; the Open dialog lists "Xarast documents
    (*.xarast)" first. Saving is wired (XARA-US-0084, "Saving" below);
    the session keeps the opened package's bytes (`source_package`) for
    `save_opened`'s raw copies. Writing `.xar` says "a permanent
    non-goal" and a test asserts that wording, because that is a
    decision, not a gap.
15. **Session state is flat and public.** `Session`'s fields are `pub`
    because reading them is the whole point; mutation is still funnelled
    through `dispatch`, `undo`, `redo` and `apply`, which are the only
    things that keep `modified`, the prune and the dirty tracking
    consistent.
16. **`RendererPref` lives here even though `xarast-shell` owns the
    capability ladder.** It is the *persisted user preference*; the
    shell converts. Keeping it here is what lets the headless tools and
    the window agree on it.
17. **Nothing is serialised in this crate.** Preferences have no on-disk
    form yet: Phase 6 owns that, and inventing one here would be a
    format to migrate later.
18. **The render-thread channel is a latest-wins mailbox, not a queue**
    (`render_thread`, XARA-T-0002). One slot each way: a submitted frame
    replaces the waiting one (`RenderStats::superseded`), a finished frame
    replaces an uncollected one (`dropped`). That *is* the backpressure —
    at most one frame waits and one is in flight — and `submit` never
    blocks. Built on `std::sync::{Mutex, Condvar}`; `crossbeam-channel`
    (named in `phase-05 §U2.7`) was not needed and would have been a
    queue to fight.
19. **Generations are allocated by `RenderThread::submit`** from a
    main-thread counter, strictly increasing. `RenderRequest::Cancel {
    up_to_generation }` drops the waiting frame and makes the worker
    discard an in-flight result instead of publishing it; a result is
    never published over a newer one. A `Final` is drawn in columns and
    abandoned between them for newer input (decision 27); a `Draft` is
    never interrupted.
20. **`FrameJob` carries the `Resolver`, which the §U5.5 sketch omits.**
    `Arc<Scene>` + scene epoch + `Arc<Resolver>` + `ViewParams` + ink
    rectangle + pasteboard and page colours — invariant 6 applied across
    threads. (It carried an `Arc<DisplayList>` until XARA-US-0005; see
    decision 24.) `Resolver` and
    `RampCache` derive `Clone` for this; `Session::resolver_snapshot`
    clones once per scene rebuild and hands the same `Arc` to every frame
    of a pan.
21. **The page is a backdrop the render thread fills, not scene ink.**
    `Session::frame_job(pasteboard, page)` derives the page's device
    rectangle through the `Viewport`, so the Y flip still has one owner.
22. **The renderer behind the thread is a trait (`FrameRenderer`).**
    Production is `CpuFrameRenderer` (interactive config); the tests use a
    gated renderer to hold a frame in flight deterministically, which is
    the only way to test supersession without sleeps deciding the result.
    It is called once per rectangle drawn (a strip, a column), not once
    per frame, and receives the display list the worker built.
23. **Draft while interacting, one Final after 120 ms idle**
    (XARA-US-0005, `schedule`). `QualityScheduler` is a pure state
    machine: `note(now, changed)` marks interaction (anything that
    `needs_redraw`) and returns the generation of a Final to cancel;
    `next(now, doc)` says `Submit(Draft|Final)`, `Wait` or `Idle`;
    `deadline()` is when the owed Final is due. Time is always passed in,
    so the tests use a synthetic clock and no sleeps. `Canvas` wraps it
    around a `RenderThread`: `note`, `pump` (rebuilds the scene if the
    session says so, submits, returns the deadline), `take_latest`,
    `is_settled`. **The first frame of a document is Final**: nothing is
    on screen to refine, and Draft-then-Final would be two full renders.
24. **The worker builds the display lists, so a job carries the scene.**
    Which list to build depends on what the worker holds (its kept
    frame), which the main thread cannot know without racing it.
    `Session` keeps its scene in an `Arc` and rebuilds in place when it
    is the only owner, else into a fresh one (never clone-then-clear);
    `scene_epoch` counts rebuilds so the worker can tell "same scene".
25. **Pixel reuse lives on the render thread** (`reuse.rs`). The worker
    keeps the last frame it published (and publishes a clone, ~1 ms at
    1080p). A pan by whole pixels scrolls it (`scroll_surface`) and
    draws only the exposed strips, at either quality, but a `Final` only
    reuses pixels that are themselves exact `Final` pixels. A `Draft` pan
    by a fractional offset is **snapped** to whole pixels and the frame
    reports the snapped view; the `Final` redraws exactly. A `Draft` zoom
    resamples the kept frame (nearest) and paints only the backdrop into
    the border a zoom-out uncovers (decision 26). Anything else (new
    scene epoch, resize, dpi, colours, rotation) is a full frame.
    `RenderedFrame::{reuse, exact}` say what happened; the viewer's
    screenshot waits for an exact frame. Scrolled and column-drawn frames
    match a one-call render byte for byte (tests; they were within 1/255
    until the rasteriser's origin was fixed, `render.md` invariant 15).
    A new scene at the same view is decision 40.
26. **A Draft zoom-out's border is left to the Final.** Rasterising it
    cost ~190 ms a frame over 100 000 objects: the border is short, wide
    strips, and the CPU backend runs a strip shorter than a band on one
    core (XARA-T-0034). With the backdrop there instead, a zoom frame is
    ~1.6 ms. Revisit when T-0034 lands.
27. **A Final is drawn in up to four full-height columns** (no narrower
    than 256 px), each its own display list, checking between columns
    for a cancel or a newer job. Columns, not row slabs: the backend's
    parallelism is across bands, and 192-row slabs doubled a 1080p Final.
28. **Strips outside the scene's ink are backdrop, with no list built.**
    `FrameJob::ink` is `viewport::content_rect` (whole-tree bounds, a
    superset of anything drawn, taken at rebuild) in device space plus
    2 px. A `DisplayList::build` scans every scene op however small the
    rect (XARA-T-0033), so not building is the only cheap build.
29. **Draft is a view parameter; the scene stays at the session's
    quality.** Re-walking at Draft quality would shorten ramps and switch
    images to nearest, but the walk is ~100 ms at 250 000 nodes, on the
    first and last frame of every gesture. So Draft frames only scale
    flatness (XARA-T-0035 moves the other two knobs into the view). The
    session's quality is still the ceiling: `Canvas::pump` never submits
    above it.
30. **Headless framing is an enum** (XARA-T-0012): `HeadlessFrame::
    {Session, FitDrawing, Fit(rect), Fixed { zoom, centre_on }}` plus
    `dpi: Option<f64>`, and `HeadlessResult` returns `WalkStats`,
    `SceneStats`, the zoom and the `ViewParams`. `drawing_rect` now means
    the drawing without the pages (the active spread's visible, non-guide
    layers); `drawing_or_page_rect` is what fitting frames;
    `content_rect` keeps the whole-tree bounds for culling.
31. **A fill-mapping value is interpreted per fill family, not at face
    value** (XARA-T-0037, `research/01 §8.3` "How the mapping renders").
    `Tiling` holds the original's values 0–4 (`None`, `Simple`, `Repeat`,
    `RepeatInverted`, `RepeatExtra`). In `paint.rs`:
    * `gradient_repeat`: a linear, radial, conical or diamond fill clamps
      unless the value is `RepeatExtra`, which becomes `Repeat::RepeatHq`;
    * `mesh_repeat`: a three- or four-colour fill clamps only on `Simple`,
      and otherwise tiles, the default included;
    * `repeat_of`: a bitmap fill takes the value as is.
    The original's factory default is `Repeat` (2), and a gradient ignores
    it. Taking it at face value wrapped every gradient into hard bars
    (SimpleSphere, Fill Types, leafgirl's horizon).
32. **A circular fill's minor axis is the major one turned a quarter
    turn** (`paint::radial_frame`). The record has no second axis, and the
    importer repeats the edge point. The frame was degenerate, so every
    circular fill painted nothing.
33. **`drawing_rect` includes half of each object's line width**, from its
    own attribute stack (`viewport::ink_rect`, used when the layer's
    cached box is cold, which is always after import). It is not the
    culling extent, which allows for a full mitre spike and is four times
    as wide. Fitting with a zero extent cut thick strokes off at the image
    border.
34. **A bitmap fill with no decoded image counts in `images_pending`**, as a
    bitmap node does, so a file never looks complete while it is not. So
    does a bitmap *transparency* whose image is missing, and a decode
    failure counts in `images_failed` instead (XARA-T-0129).
    **`register_images` decodes, once per walker** (XARA-T-0129; contract
    and costs in `image.md`, "Walker integration (as built)"). Decoding is
    parallel across a frame's pending bitmaps but registration is in the
    resources' order, so the scene stays byte-identical across runs and
    walkers. Failures are negatively cached until `reset`.
35. **A rendered frame says what it covers and what is new**
    (XARA-T-0050). `RenderedFrame` carries `scene_epoch`, `covered` (the
    rectangle holding picture, not placeholder backdrop), `fresh` (what
    was rasterised: the viewport, a scroll's strips, nothing for a
    rescale) and `base` (the generation a scroll moved pixels from). A
    presenter that already holds `base` uploads only `fresh`; one that
    never saw it (dropped, or not collected) uploads all of `covered`.
    The kept frame carries its generation and cover for this.
36. **A presenter that resamples at input time turns CPU rescale off**
    (`FrameJob::cpu_rescale`, `Canvas::set_cpu_rescale`). The worker then
    skips a Draft whose zoom differs from its kept frame and publishes
    nothing (`RenderStats::skipped`, not counted in `rendered`); the
    Final after the gesture draws the new zoom whole. The viewer always
    sets it off: both canvas tiers composite retained tiles. Decision 26's
    backdrop border is therefore gone from the window; the headless tools
    keep the CPU rescale.

37. **Single-document model for the first usable viewer**
    (XARA-US-0082). `Intent::OpenFile` → `AppState::open_replacing`: the
    new session is opened *first* and the others are closed only once it
    succeeded, so a failed open leaves the current document on screen
    (the error is in the problem list and returned). A path that fails is
    dropped from the recent list. `DocumentSessions` still holds many;
    tabs are later.
38. **Recent files are state, not settings**: `$XDG_STATE_HOME/xarast/recent`
    (a relative or empty `XDG_STATE_HOME` is ignored, as the spec says),
    plain text under a `xarast-recent 1` header, one absolute path per
    line (raw bytes on Unix, so non-UTF-8 paths survive; paths with a line
    break are refused). Parsing never fails: an unknown header is an empty
    list, bad lines are skipped, at most 10 are kept. Written through a
    temporary file and a rename. Pruned of missing files at load. The
    store is opt-in (`with_recent_store`): tests and probes keep the list
    in memory and never touch the user's state directory.

39. **Saving (XARA-US-0084).** See the section below.

40. **An edit repaints its damage, decided on the render thread**
    (XARA-T-0221). `reuse::plan` asks `reuse::repaint` first: when the
    job's transform is bit-identical to the kept frame's, the page rect
    equal and the kept frame fully covered, the job's scene is diffed
    against the kept frame's (`xarast_render::scene_damage`); for a `Final`
    the kept frame's Draft rectangle (`Kept::inexact`) is added. Empty
    damage under a new epoch reuses every pixel; up to four rectangles
    under half the view are `Plan::Repaint` (`FrameReuse::Repainted`,
    `fresh` = the rectangles, `base` = the kept generation); anything
    bigger, or an edit during a pan or zoom, is a full frame.
    * **Why on the render thread and not in `Session::after_mutation`.**
      Only the worker knows which frame is on screen (decision 25 and the
      dead end "Deciding pixel reuse on the main thread"), and a diff of
      the scenes covers every command, undo, redo, preview and palette
      redefinition without per-command extents. `after_mutation` still
      marks the whole view in `Dirty`, which nothing narrower consumes;
      it is an upper bound.
    * **`Kept` holds the scene and resolver it was drawn from**, the page
      rect and `inexact` (a rectangle holding every non-`Final` pixel;
      `final_exact()` is its emptiness). A `Draft` repaint adds its
      rectangles to `inexact`; a `Final` repaint includes it and clears it,
      so the Final after a fill drag redraws the dragged area only.
    * **The session double-buffers its scene** (`spare_scene`): the kept
      frame holds the scene on screen, so after an edit the current scene
      is always shared; the rebuild takes the spare once nobody else holds
      it (`strong_count == 1`, which cannot rise behind our back) instead
      of allocating a whole scene.
    * **The shell keeps its tiles.** A frame whose epoch changed but whose
      `base` is the last frame uploaded keeps the tiles of its level under
      the view, restarts any tile with texels outside the view, forgets
      every other tile and level, and uploads only `fresh`
      (`xarast-shell/src/tiles.rs`, `TileStore::retain`).
    * Measured over the corpus: repainted frames rasterise 4 % of the
      pixels a full frame would (`perf.md`, "An edit repaints its damage").

41. **The render thread never waits for an evicted bitmap base**
    (XARA-T-0281). `CpuFrameRenderer::default()` samples with
    `MissingLevels::Substitute` (`render.md`, "Pixel memory budget"): a
    level that needs its base back from a spill file or a decode is drawn
    from the best resident one instead. After each frame the worker asks
    the job's images which were substituted during it
    (`ImageRef::substituted_since` against `substitution_tick()` read
    before the frame) and sends them to a helper thread
    (`xarast-rematerialise`, started on first use, stopped at shutdown)
    that calls `ImageRef::rematerialise`. For a `Final`, the images'
    `image_damage` is added to `Kept::inexact` and the frame goes out
    with `exact == false`. When the helper reports the batch done
    (`State::rematerialised >= generation`), nothing newer waits and the
    kept frame is still that one, the worker re-runs the kept job under a
    generation of its own: `reuse::plan` turns it into the ordinary
    `Plan::Repaint` of `inexact` (or a full frame when that is over half
    the view), so the shell's tiles take it like any edit repaint.
    * **Generations moved into `Shared`** (`AtomicU64`) so the worker can
      allocate one; `RenderThread::next_generation` is no longer `const`.
      The shell's `frame.generation > shown` rule and the scheduler's
      `presented` both accept the repair as the newer frame.
    * **The repair samples with `Materialise`**
      (`FrameRenderer::set_missing_levels`, a defaulted trait method that
      returns the policy it replaces), so it is exact even if the budget
      evicted a base again between the helper and the repair. That is the
      only place the render thread may still read a base, and only under a
      budget too small to hold one frame's images (a limit of 0 in the
      tests); a real budget finds them resident.
    * **A `Draft` is not repaired** — its `Final` follows — but its bases
      are brought back all the same, so the `Final` usually finds them.
    * `RenderStats::substituted` counts frames that drew substitutes,
      `repaired` the worker's own repairs.
    * **The same path makes deferred images** (XARA-T-0304): the
      session's walker registers a large committed photo chain with
      `ImageRef::deferred`, which a `Substitute` sampler draws from its
      stand-in, so the helper's `rematerialise` is where the chain is
      evaluated and its pyramid built; the repair then shows it. A
      repair is owed to one generation, so after an undo or redo the
      helper's result lands in a store no newer frame draws and no
      stale picture is published (`tests/photo_release.rs`).
    * Tested on the corpus (`tests/pixel_budget.rs`,
      `the_render_thread_never_reads_a_base_back_and_converges`): Groucho2
      and leafgirl, Draft zoomed out then Final, under a budget with every
      base spilled; every base read is the helper's, leafgirl's Final is
      repaired, and the settled frame equals the unlimited budget's.

42. **A session's walkers decode each bitmap once between them**
    (XARA-T-0281). `Session` owns a `DecodedImages` (`src/decoded.rs`) and
    every walker it makes shares it: its own, `Session::scene_walker()`
    (`build_scene`, the CLI's `SessionSource`), `headless::render`, and
    the save thread's thumbnail (`SaveJob::with_decoded_images`, set by
    `Session::save_job`; `thumbnail::thumbnail_png_with`), and the bitmap
    gallery's thumbnail thread (`DecodedImages::image_for`, which decodes
    through the walker's `ready_image` on a miss; XARA-T-0293). The key is the
    resource's identity — the addresses of its `pixels` and `original`
    `Arc`s, which the entry holds, plus its declared size and the pixel
    budget — so an edited (copy-on-write) resource misses. Entries whose
    resource left the document are dropped on the next registration.
    Registration stays in document order whatever the cache held, so ids
    and scenes are unchanged. `SceneWalker::reset` keeps the cache (it is
    the document's, not the walker's). Walkers built with
    `SceneWalker::new()` elsewhere (tests, the corpus tools) decode for
    themselves as before.
43. **ClipViews render, in both modes** (XARA-US-0017, 2026-09-24).
    * **No ClipView had ever rendered.** `open` pushes a ClipView's clip
      and then its group, and the frame was closed clip first, so the
      scene builder underflowed (`SceneError::Underflow { kind: "clip" }`)
      and the whole render failed. The corpus has no ClipView
      (`TAG_CLIPVIEW*` 4084/4085: 0 records), so nothing noticed. The
      frame now closes in reverse order.
    * **`ClipViewMode::Outside` is an inside clip of a complement**
      (`walker::outside_clip`): a frame around the other children
      (their zero-extent bounds grown by their larger side, at least an
      inch, clamped to `Mp::EXTENT`) minus the clipping path, resolved by
      `xarast_geom::boolean` under the ClipView's winding rule and pushed
      with `NonZero`. The boolean restores untouched cubics verbatim, so
      the edge stays exact at any zoom; a self-overlapping non-zero path
      keeps its overlap in the hole. The renderer keeps a single clip
      primitive (inside of a path) — no `invert` flag in `SceneOp`, which
      would have touched every backend. The SVG writer already used a
      mask for the same thing.
    * The clipping child is never painted, in either mode (it was painted
      in outside mode while the clip was dropped).
    * `WalkStats::clips_unsupported` now counts only a ClipView whose first
      child has no clip geometry.
    Pinned by `tests/gradients_and_clips.rs` with pixel probes.
44. **Perspective gradient corners are the original's `EndPoint2` and
    `EndPoint3`** (XARA-US-0017; the provisional mapping of the old
    TODO, now confirmed from the source, not from an image). For a
    perspective fill the original hands the rasteriser the quadrilateral
    Start, `EndPoint2`, `EndPoint3`, End for a linear fill and Start,
    End, `EndPoint3`, `EndPoint2` for the others
    (`wxOil/grndrgn.cpp:2540-2560`), and `EndPoint3` is the far corner
    when a fill is made perspective (`Kernel/fillval.cpp:1513`, "line at
    90°" from End through `EndPoint2`). So `Perspective::p2` = `EndPoint2`
    = the image of `(0, 1)` (the second axis) and `p3` = `EndPoint3` = the
    image of `(1, 1)`, which is what `paint::perspective` does. No `.xar`
    fill record carries perspective corners — a perspective fill exists
    only inside a mould (129 perspective moulds, all Phase 13 live
    objects) — so no corpus golden can exist. The pixel test pins that a
    parallelogram "perspective" draws exactly as its affine twin and
    that a trapezoid's far edge is where `p3` says.
45. **A sine fill mapping is eased on the canvas**: see `render.md`,
    "Sine-mapped ramps".
46. **A bitmap transparency composites in its mode, between its levels**
    (XARA-T-0307, 2026-09-24). `TranspPaint::Bitmap::contone` holds the
    start and end levels and the mode (`xar-import.md` finding 18);
    `paint::transparency` draws it in `family_of(mode)` (`None` →
    opaque, as a flat one) and maps luminance through
    `bitmap_level_ramp` (black → start, white → end, the fill's profile
    applied). A transparency with no pair — written before the importer
    kept one — is Mix over 0..255, as before. Imported ClipViews
    (XARA-T-0306) need nothing new here: the importer emits the model's
    shape, a group with the painted keyholes and a ClipView whose first
    child is their union (`xar-import.md` finding 17).
47. **A feather wraps the node that owns it** (XARA-US-0068, 2026-09-24).
    At `EnterScope` (and at a story's visit) the walker looks for the
    node's *own* `Feather` attribute child with a size above zero, or a
    preview's override of the slot, and pushes
    `LayerEffect::Feather { size, profile }` before the node's group or
    clip, popping it last at `LeaveScope`; the node's own ink (a path's
    fill, painted at `LeaveScope`) is inside it. The inherited value is
    ignored: the attribute stack hands a group's feather to every member,
    but the original feathers the owner once, as one offscreen unit.
    Structural nodes (document, chapter, spread, page, layer) never
    feather. With a dirty rectangle, culling inside a feathered node
    widens by the feather's size, so everything within its reach of the
    area is in the scene (production passes no rectangle; the renderer
    widens again for itself). `WalkStats::effects` counts them. Live
    controllers still draw their children as before and count
    `live_pending`; `regen::LiveOutput` is not consumed yet
    (`document-model.md` decision 41 and its TODO).

---

## Saving (XARA-US-0084)

**Modified = the undo state differs from the saved one.**
`xarast_doc::History::state_serial()` names the state the history is at:
every commit (a merge into the last step included) hands out a new
serial, undo/redo move between serials already handed out, a dropped
redo branch's serials are never reused, eviction keeps the evicted
step's serial as the base. `Session` records `clean_serial` at open and at
each save; `is_modified()` is `clean_serial != Some(state_serial)`. So
undoing back to the save clears the marker and redoing sets it again; an
edit during a save keeps the document modified (the job captured the
serial it wrote). A recovered snapshot has `clean_serial = None`.

**A save is a job.** `Session::save_job(kind, path)` takes a
`Document::snapshot()` on the interface thread (ProbeX16, 518 k nodes:
~50 ms) and the source package bytes; `SaveJob::run` (on a `SaveWorker`
thread) restores a document from it (~130 ms), renders `thumbnail.png`
on a scoped thread while `xarast_format::prepare_save`/`prepare_resave`
serialise the SVG, then writes atomically. The SVG is written **with the
text placer** (`svg_text::placer()` in `SvgOptions::text`, XARA-T-0259),
so stories are laid out on the save thread and an app save places text
for browsers exactly as `xarast-cli convert` does; `save_job` itself only
clones the placer's `Arc`. `without_text_placer()` exists for
`emergency_shutdown` alone (no wait on font enumeration on the signal
path). Over the corpus the snapshot path writes **byte-identical**
packages to a direct save of the live document with the same placer,
first save and raw-copy re-save alike (`tests/save.rs`), and to
`xarast-cli convert --deterministic` (`xarast-cli` `tests/app_save.rs`,
59/59; File › Save adds only `thumbnail.png`).
The live document is never touched by a save: a failure is a status line
and a problem-list entry, the document stays modified, the target file is
untouched (`write_atomic`).

**`AppState` owns the flow.** Intents `Save`, `SaveAs`, `SaveTo(path)`,
`SaveDialogClosed`, `AnswerPrompt(answer)`; request `ShowSaveDialog {
title, file_name, directory }`. Save in place only when
`can_save_in_place()` (a `.xarast` path, not read-only); anything else —
untitled, a `.xar`, read-only — asks for a name. `xarast_path` makes the
chosen name a `.xarast` (`.xar` replaced, other extensions appended).
Saving to a new target takes its lock first; a target another session
holds is refused with a status line. `poll_saves()` (every frame; the
worker's waker wakes an idle loop) applies outcomes: mark clean, new
path, the new lock, recent files, delete the autosave, status line
"Saved name (size, ms)", then the action that waited.

**Asking before losing work.** `CloseDocument`, `Quit` and `OpenFile`
over modified documents become a `PendingAction` and a
`Prompt::unsaved` (Save / Discard / Cancel). Save → save (asking for a
name if needed), then re-run the action; Discard → remember the document
as discarded and re-run the action (so a quit asks once per modified
document); Cancel / a closed save dialog / a failed save → drop it. While
a prompt is up, `apply` ignores everything but `AnswerPrompt`, `Resize`
and `SetDpi`. `OpenFile` with nothing modified still returns the open
error, as the viewer and `tests/open.rs` expect. `Quit` closes every
session (releasing locks, deleting autosaves) before it queues
`PlatformRequest::Quit`.

**Locks (T-0086).** A `.xarast` is locked when it opens
(`HeldLock::acquire(path, Normal)` = `steal_if_stale`: a dead holder on
this boot is taken silently). Held by someone else → `Prompt::locked`:
*Open read-only* (no lock, `read_only`, Save asks for a name), *Open a
copy* (no lock, no path, "name (copy)", clean), *Force (risky)*
(`DocumentLock::force`; the old holder's drop does not remove the new
file), Cancel. Unavailable (read-only directory) → open unlocked with an
Info diagnostic. Locks drop with the session. `HeldLock` also lists
itself in a process registry so `locks::release_all()` can remove this
process's lock files from a signal handler (only files that still name
this pid).

**Autosave (T-0087).** Opt-in: `with_autosave(dir, policy)` (the binary
passes `autosave::default_dir()`; tests use scratch dirs). `tick(now)`
(every frame; returns when the next is due) snapshots a modified
document when it has been idle `policy.idle` (2 s), no gesture is in
flight, and `policy.interval` (60 s) has passed since its last snapshot
or since it became modified. Entry: `<id>/snapshot.xarast` (deflate 1,
no thumbnail), `holder` (lock-file format, written *before* the
snapshot so another process never mistakes a live entry for a crash),
`origin` (raw path bytes). Deleted on a real save that leaves the
document clean, on undo back to clean, on close and on quit.
`emergency_shutdown()` (signals) writes a fresh snapshot inline and
closes everything **keeping** the entries. **No journal**: the model has
no serialisable command form (F6.6 open).

**Recovery.** `with_autosave` scans: an entry is offered when its holder
is dead or from another boot/host and its snapshot is strictly newer
than its origin file (a tie is offered, not deleted); older ones are
deleted; a live process's entries are left alone. `offer_recovery()`
asks once (`Prompt::recovery`: Recover / Discard / Later). Recover opens
each snapshot as a modified document whose path is the origin (Save
writes there; a `.xar` origin asks for a name), re-records the holder,
takes the origin's lock (held → read-only) and keeps the entry until a
save or close.

**Crashes (XARA-US-0064, phase 12 §F).** Module `crash`; the shell
half is `ui.md` shell decision 43.

- **Report (F1).** The panic hook writes
  `$XDG_STATE_HOME/xarast/crashes/crash-<unix s>-<pid>.toml` before
  unwinding: time, build (`CARGO_PKG_VERSION` plus `XARAST_BUILD_ID` if
  the build sets one — nothing sets it yet), os/arch, thread, location,
  message, backtrace (200 lines), `[context]` (`platform`, `renderer`,
  `gpu_adapter`, `safe_mode`, set by the shell) and the last 200 log
  lines. Hand-written TOML (no new dependency). The hook takes the
  context and log locks with `try_lock`, so a panic inside them cannot
  deadlock it. A panic inside `expect_panics` (the image decoder and the
  gallery thumbnailer) writes no report. The thread-local marker lives in
  the dependency-free `xarast-unwind` crate (`expect_panics`,
  `panics_expected`); `xarast_app::crash::expect_panics` forwards to it
  and the hook asks `panics_expected()`. So low-level crates mark their
  own caught panics too (`xarast-text`'s font subsetter does) without
  depending on `xarast-app`. Rule: every `catch_unwind` that swallows a
  panic must go through `xarast_unwind::expect_panics`; only
  `run_guarded` (fatal worker panics, must report) and a test in the
  shell's `viewer.rs` use bare `catch_unwind`. XARA-T-0310.
- **Privacy rule.** Every string entering a report goes through
  `redact` (an absolute or `~/` path with two or more slashes becomes
  `…/basename`). A `String` panic payload (built at run time) also goes
  through `scrub_quoted` (`"text"` → `"…"`); a `&'static str` payload is
  source text and is kept. Log lines get both. What is *not* caught: a
  `%` (Display) log field holding document text — code must never log
  document text. Nothing is ever uploaded; the UI shows the path and
  copies it.
- **Fatal worker panic (F3).** `RenderThread::spawn_with` runs the worker
  under `crash::run_guarded("xarast-render", …)`: an escaping panic raises
  `FatalWatch::global()` (flag + waker). The shell then calls
  `emergency_shutdown` — the existing signal path: snapshot of every
  modified document into the autosave store, entries kept — and exits
  101. The recovered file is the autosave entry, **not** a
  `*.xarast.recovered` beside the document as phase-12 F3 words it:
  writing beside the user's file fails in read-only folders, leaves
  clutter, and the autosave entry is what the recovery prompt already
  offers. Other threads are not guarded: a save or import thread that
  dies is reported, not fatal.
- **Sentinel (F4) and crash loop (F6).** `begin_session(state_dir,
  requested_safe, now)` writes `sessions/<pid>.running` (lock-file holder
  format) and counts earlier sentinels whose holder is on this host and
  dead or from another boot (another host's are left alone; an
  unreadable one is a crash). Each crash appends `now` to
  `sessions/crash-history` (kept 24 h). `CRASH_LOOP_COUNT` (3) within
  `CRASH_LOOP_WINDOW` (10 min) → `SafeMode::Forced`; else a crash →
  `Offered`; else `--safe-mode` → `Requested`. Only `Sentinel::finish`
  removes the file — dropping it (unwinding) leaves it. Every I/O
  failure degrades to "no crash found".
- **Next start (F5/F7).** `AppState::with_startup_check(&check)`:
  `offer_recovery` first asks `Prompt::crashed` ("Xarast closed
  unexpectedly", the report path, what safe mode is): *Use safe mode*
  (only when `Offered`), *Copy report path* (→ `SetClipboardText`, the
  prompt stays), *Continue* (default and Escape). Safe mode or Continue
  then asks the usual recovery question. `enter_safe_mode` resets
  `prefs` in memory and queues `PlatformRequest::EnterSafeMode`.
- **F8.** `force_panic_point(site)` panics when `XARAST_FORCE_PANIC` is
  `site` (read once; compiled into release). Sites: `startup` (binary),
  `ui` (`Viewer::on_frame`), `render` (each render-thread job).
- **Tests.** `tests/crash_recovery.rs` (phase-12 criteria 12 and 13):
  the test binary re-runs itself as a child with
  `XARAST_FORCE_PANIC=render` and a real `RenderThread`; the parent checks
  exit 101, the report (render thread, context, `…/work.xarast`, no
  private folder name), the recoverable autosave, the crash prompt, Copy,
  Safe mode, Recover; three children force safe mode. 8 unit tests in
  `crash.rs`.

**Thumbnail (T-0082).** `thumbnail::CpuThumbnails`: the active spread's
first page edge to edge, 256 px on the longer side, page colour beneath,
parallel CPU configuration (ProbeX16: walk 182 ms + raster 167 ms; the
deterministic configuration took 456 ms to raster). 50 of 59 corpus
thumbnails show ink (the others are blank pages or text-only in the
walker's reach).

**Numbers** (`examples/save_probe.rs`, release, tmpfs, 2026-09-23):
ProbeX16 — interface thread 54 ms (snapshot); save thread 1.09 s with
thumbnail, 1.05 s without (restore 126 ms + `xarast_format::save` 856 ms
of which serialise 507 ms, package 327 ms). Groucho2 — 15 ms. On the
99 %-full md RAID of the dev machine the fsync alone added 0.5–7 s:
measure on tmpfs. The ≤ 1 s budget is missed by ~9 % on ProbeX16 — the
restore is the part the app adds (follow-up task filed). With the text
placer (XARA-T-0259, 2026-09-24, ext4, load ≈ 3): ProbeX16 UI thread
30–38 ms, save thread 1.05–1.07 s with thumbnail, 970–984 ms without
(965–991 ms without the placer: within noise); TextCurve +2–3 ms.

### Contour rings in the walker (XARA-T-0330, 2026-10-15)

- `SceneWalker` owns a `xarast_doc::LiveCache` (`live`), pruned with
  `retain_alive` when the epoch moves. At a contour's **generated
  node** the walk skips the subtree and `paint_contour` draws
  `LiveCache::current_or_regenerate(doc, controller, 0.0, regenerate)`:
  rings in paint order, each in the attributes in force there + the
  generated node's own + the ring's blended ones (`paint_geometry`, the
  ink half of `paint` factored out), filled and stroked.
- Scene ids: the generated node's tag | (ring index + 1) << 32 (tags are
  `u32`; previews use bit 63). Content hash: the regen key's first 8
  bytes mixed with the index, plus the attribute scope fold.
- A contour's controller and source paint nothing themselves and are not
  `live_pending`; a failed regeneration counts one pending. An inset
  path's source subtree is skipped.
- This is T-0316's drawing half. The `RegenQueue` stays unused: the key
  catches every change (undo included), so a lazy cache per walker is
  enough; the drag-per-frame and damage tests of T-0316 are still open.

### Blend steps in the walker (XARA-T-0333, 2026-10-15)

- At a blend's **generated node** (one per pair, under the source
  between its two objects) the walk skips the subtree and `paint_blend`
  takes the controller (the source's parent), the pair index
  (`xarast_doc::blend::blenders`) and the group of
  `LiveCache::current_or_regenerate`'s `LiveOutput::Groups`; each step is
  pushed with its whole attribute map and painted with its own
  `filled`/`stroked` (two stroked lines blend to stroked lines). Paint
  order is the original's: object, steps, next object.
- Scene ids: node tag | (step + 1) << 32, as rings; content hash mixes
  the key's first 8 bytes with the pair and step index. Blend
  controllers and sources are not `live_pending`.
- `tests/xar_blends.rs` (+ golden): fading squares, widening stroked
  lines, a three-object rainbow blend.

### Moulds in the walker (XARA-T-0334, 2026-10-16)

- At a mould's **generated node** the walk skips the subtree and
  `paint_mould` draws `LiveCache::current_or_regenerate` with a generator
  that calls `mould::generate_with` and lays text out with the walker's
  fonts (`text::story_outlines`), so moulded text is curves (G8). Each
  shape is pushed with its whole attribute map and painted with its own
  `filled`/`stroked`. A mould's **source** is skipped (drawn only through
  the copy); controller and source are not `live_pending`.
- Scene ids as rings (tag | (index + 1) << 32). The key does not cover
  fonts: a font change does not re-mould text until the source changes.
- `SvgTextPlacer::outlines` (new `TextPlacer` method) gives the writer the
  same outlines. `tests/xar_moulds.rs` (+ golden): a perspective whose
  linear fill deforms with it, an envelope that bulges.

---

## Invariants that must not be broken

1. **No UI toolkit, no windowing, no `unsafe`.** `#![forbid(unsafe_code)]`
   is in `lib.rs`, and the dependency list is `xarast-*` plus `indexmap`,
   `bitflags`, `thiserror` and `kurbo`.
2. **`EditState`, `Viewport`, `Session`, `AppState` and `Intent` stay
   `Send`.** Asserted at compile time in `lib.rs`.
3. **The walker never mutates the document**, and never fills a cache in
   it. Asserted by the canonical digest over the whole corpus.
4. **The walk is output-deterministic.** Same document, edit state and
   viewport → byte-identical scene, from a fresh walker or a reused one.
5. **The attribute stack is pushed and popped exactly once per scope.**
   `EnterScope` pushes, `LeaveScope` pops, an attribute node pushes a
   value. Never open-code it.
6. **A `Scene` is never handed anywhere without its `Resolver`.**
7. **Session state never enters the arena** (architecture §3.5b), and
   never enters the canonical digest or the undo log.
8. **A device rectangle passed as `dirty` prunes the walk and nothing
   else.** The scene is always rebuilt whole; culling may only ever
   remove work, which is a corpus test.
9. **Tools never touch the arena.** Every mutation is a
   `xarast_doc::Command` dispatched through `Session::dispatch`; tools
   emit `EditCommand`s through `ToolCtx::commands` and hold only
   `&Document` (compile-fail doctest in `tool.rs`).
11. **Scroll bounds are refreshed at `rebuild_scene`**, never in
    `after_mutation` (34 ms per undo at 250 000 nodes; `tools.md`).
13. **A save never mutates the live document** and never runs on the
    interface thread beyond `save_job`'s snapshot. `tests/save.rs` pins
    the byte identity of the snapshot path over the corpus.
14. **Nothing closes a modified document without asking** except
    `AppState::close` (the caller's explicit decision) and
    `emergency_shutdown` (which autosaves first; called on a signal, a
    fatal worker panic and a panic unwinding through the viewer). Every intent that closes
    documents goes through `request`.
15. **An autosave entry's `holder` is written before its snapshot.**
18. **A crash report never holds document content or a full path.**
    Everything entering one goes through `crash::redact`; run-time panic
    messages and log lines also through `scrub_quoted`. Never log
    document text with `%`.
19. **The sentinel is removed by `Sentinel::finish` alone**, on a clean
    return from the event loop. Anything else — a fatal panic (exit 101),
    an unwinding panic, `SIGKILL`, the signal fallback's `process::exit` —
    leaves it, and the next start counts a crash.
16. **A repaint is only ever drawn over the frame it was diffed against,
    at its exact view.** `Plan::Repaint` requires the kept frame's
    transform bit for bit, its page rect and full cover; the pixels
    outside the damage are that frame's. Loosening any of these (a
    whole-pixel pan plus an edit, say) needs the damage moved with the
    pixels first. `tests/edit_damage.rs` in the shell checks every frame
    and the tiles against a full render byte for byte.
17. **A frame drawn from substitutes is never published as exact.** The
    worker adds the substituted images' `image_damage` to the kept
    frame's `inexact` before publishing (decision 41); a presenter that
    settles on `exact` would otherwise keep proxy pixels for good.
12. **New intents** (phase 7): `Cancel`, `DeleteSelection`,
    `InfobarEdit` (typed `InfobarValue`), `AutoScroll`,
    `SetCurrentAttribute`; pointer intents now drive the `ToolMachine`,
    and `ChooseTool`/`MomentaryTool` switch its tool. A tool may ask for a
    tool change (`ToolRequests::tool`); a `CreateShape` selects what it
    created. `Session` holds a `Picker` invalidated in `after_mutation`.
13. **New intents (phase 7 round 3):** `Group`, `Ungroup`,
    `Arrange(ZOrder)`, `Align(AlignSpec)`, `Duplicate`, `Copy`, `Cut`,
    `Paste { in_place }`, `PasteText { text, in_place }` (the shell's
    answer), `ToggleSnap`, `ToggleGrid`, `ToggleGuides`, `Guides(GuideOp)`,
    `ShowDialog(Dialog)`. Copy/Cut/Paste/PasteText/ShowDialog are
    `AppState`-level; new `PlatformRequest`s `SetClipboardText`,
    `ReadClipboard`, `ShowDialog`. The session drains
    `doc.tree.drain_changes()` into its `Picker` in `after_mutation`.
10. **No two commands share a key chord** (`no_two_commands_share_a_key`),
    and the core never performs a platform action: it queues a
    `PlatformRequest`.

### The one friction with `xarast-doc`

`Tx::set_kind`, `Tx::attach`, `Tx::delete` and `Tx::move_node` each run
`keep_one_active_layer` afterwards, and that repair fires whenever the
number of active layers in a spread is **not exactly one**. Moving the
active flag necessarily passes through zero or two, so a pair of
`set_kind` calls can never move it:

* clear the old one first → zero active → the repair re-elects the first
  non-guide layer, which is the one just cleared;
* set the new one first → two active → the repair clears the new one.

`Tx::act` applies one `Action` and records its inverse **without** the
repair, so `SetActiveLayer` uses that. It is still correct by the
invariant's own terms: `document-model.md` invariant 19 says a
transaction may not *leave* the invariant broken, and this one does not.
`the_active_layer_stays_unique_and_undoes_with_the_rest` pins it,
including byte-identical undo.

Worth raising with whoever next touches `xarast-doc`: the repair would
be better run once at `Tx::commit` than after every call.

---

## Dead ends (do not retry)

- **Tracking "modified" from `past.len()` and a flag.** A coalesced
  gesture merges into the last step without changing the length, and
  budget eviction shifts every depth; neither is visible from outside the
  history. The state serial lives in `History` for that reason.
- **Rendering the thumbnail before serialising.** On ProbeX16 it doubled
  the save (2.0 s against 1.05 s); render it beside the SVG instead.
- **The deterministic CPU configuration for thumbnails.** Single-threaded;
  2.7× slower than the interactive one for the same pixels budget.
- **`release_all` in a unit test.** Tests run in parallel in one process;
  it removes every other test's locks. Test `release_where` on one file.

- **Row slabs for an interruptible Final.** 192-row slabs cost 730 ms
  against 370 ms for one call at 224 000 primitives: each slab is one or
  two bands, so most cores idle. Use full-height columns.
- **One display list for the view, filtered per column.** Cloning the
  surviving `DrawCmd`s cost 65 ms at 224 000 primitives, more than
  building each column's list from the scene.
- **Re-walking the document with the strip as the dirty rect** to get a
  small scene for a pan strip: 30–130 ms per strip on the synthetic
  document, slower than the `DisplayList::build` scan it was meant to
  avoid. Group bounds there cover most of the page, so the walk culls
  late.
- **Re-walking the scene at `Draft` quality for interaction.** ~100 ms
  at 250 000 nodes, on the first and last frame of a gesture. See
  decision 29.
- **Deciding pixel reuse on the main thread.** It cannot know which
  frame the worker holds (superseded frames are never drawn), so any
  plan it makes can be for the wrong base. The worker decides.

- **`build_scene` returning a bare `Scene`.** It compiles and it renders
  nothing but solid colours, because the ramp and image ids in the
  paints point into a `Resolver` that was dropped with the walker. This
  cost an hour of "why are all the gradients transparent" before the
  type was changed to carry both.
- **Using `NodeId` as the scene id.** It is an arena slot: it is stable
  within a session, but it makes the scene depend on allocation order,
  which breaks the stability test and any cross-run scene diff.
- **A pair of `Tx::set_kind` calls to move the active layer.** See
  above. It silently does nothing, which is the worst kind of nothing.
- **Pruning a subtree because its *attribute* children missed the dirty
  rectangle.** Attribute nodes have no bounding box; they are never
  culled, and a node's own attribute children only ever applied to that
  node. Culling them would change the colours of the siblings.
- **Cloning `AttrValue` per attribute node per frame.** The stack wants
  an `Arc<AttrValue>` and `AttrNode` holds a bare `AttrValue`; the naive
  bridge clones a `Ramp`'s `Vec` on every frame for every gradient
  attribute in the document. The walker caches the `Arc` by `NodeId` and
  drops the cache when `Document::epoch` moves.
- **Comparing "dirty rect the size of the viewport" with "no dirty
  rect" and expecting the same scene.** They differ, correctly: the
  viewport-sized rectangle culls everything off screen. Compare against
  a rectangle larger than the document instead.

---

## Open TODOs

- [x] **Perspective gradients are mapped provisionally.** Confirmed
      from the original's source (decision 44, XARA-US-0017): `p2` is
      the image of `(0,1)` and `p3` of `(1,1)`. The corpus has no
      perspective fill, so a golden against the original stays
      impossible until moulds (Phase 13) produce one.
- [x] **`RampMapping::Sin` is ignored.** Done in `xarast-render`
      (`RampEase`, decision 45, XARA-US-0017).
- [x] **When a gradient becomes `Repeat::RepeatHq`**: exactly when its
      mapping is `Tiling::RepeatExtra`, which is also the only mapping
      that makes a gradient tile (decision 31).
- [ ] **Bitmap fills ignore the fill-mapping attribute** (XARA-T-0054).
      They use the per-fill `tiling` the importer leaves at `None`. The
      original tiles them by the attribute, whose default is repeat.
- [ ] **`FrameJob::ink` / `content_rect` use a zero stroke extent** when
      the cache is cold. A thick stroke reaching outside every page could
      be left out of a pan strip. It was not seen in the corpus, because
      pages cover the drawings. If it shows up, reuse `viewport::ink_rect`
      with the culling extent.
- [x] **`ClipViewMode::Outside` drops the clip.** Done as an inverted
      path (decision 43, XARA-US-0017), which also found that no
      ClipView had ever rendered.
- [x] **Bitmaps render.** `SceneWalker::register_images` decodes the
      importer's encoded originals through `xarast-image` (XARA-T-0129);
      `images_pending` is 0 on every corpus file.
- [ ] **`build_scene(&Session)` re-decodes every bitmap per call** (fresh
      walker). A per-document decoded-image cache, or decoding off the
      frame path, is T10.5.5.
- [ ] **Quick shapes with no cached path draw nothing**
      (`WalkStats::shapes_pending`). Generating the path from the
      parameters is Phase 7; `testfiles/RedStar.xar` and
      `testfiles/Test00.xar` are the corpus files that show it.
- [x] **Text draws** (phase 9 round 2). A story is painted whole at its
      visit (`walker::paint_story`, `text.rs`, `fonts.rs`; contract in
      `text.md`, "Walker integration"). `text_pending` is 0 on the corpus;
      `text_on_path_pending` is 0 since W9.5: a story on a path is fitted
      to its first path child (`text::path_fit`, `lay_story`) and that
      path is painted under the text (`walker::paint_story_path`); only a
      story with no usable path is still drawn straight and counted.
      `WalkStats::text_stories` counts drawn stories.
      `SceneWalker::with_fonts` / `headless::render_with_fonts` pin fonts;
      `viewport::drawing_rect_with` and `Session::scene_ink` include text,
      which has no cached bounds. Font substitutions flow walker →
      `Session::take_font_substitutions` → `AppState::
      collect_font_substitutions` (problem list + status bar).
- [ ] **Live effects draw nothing** (`WalkStats::live_pending`).
      Phase 13.
- [x] **`ContentHash` is per node** (phase 7): tag + `content_rev` + a
      fold of the attribute scope's node versions + the resources'
      revision. An edit re-keys only what it touched and the siblings its
      attribute applies to (`tests/tools.rs`). See `tools.md`.
- [x] **Hit testing is precise** (XARA-T-0152, `picking.rs`): painted
      fill and stroke through `xarast_geom::HitIndex`; see `tools.md`
      decision 25. The index is rebuilt lazily after a change (62 ms at
      100k); incremental updates are open.
- [x] **Render-thread protocol** (XARA-T-0002): `render_thread`, see
      decisions 18–22. The running binary uses it (XARA-T-0003).
- [x] **Draft/Final scheduling (U5.6)** (XARA-US-0005): `schedule`,
      decisions 23–29, wired into `xarast-shell::viewer` (one commit, that
      file only). The status bar still shows a fixed "Final"; showing the
      scheduled quality is a `viewer.rs`/`xarast-ui` change.
- [ ] **Pan misses 16 ms when every strip has ink**: 51–57 ms at 3× zoom
      over 100 000 objects, of which ~35 ms is two `DisplayList::build`
      scans (XARA-T-0033) and ~15 ms raster of strips that run on one
      core (XARA-T-0034). See `perf.md`. Nothing more to gain here
      without those, short of overscan (render a margin round the view so
      most pan frames are a pure crop), which trades a 20–40 % costlier
      Final for occasional re-centring spikes.
- [ ] **The Draft rescale is nearest-neighbour and cumulative**: a long
      continuous zoom resamples resampled pixels. It is blurry by design
      and the Final replaces it, but a bilinear filter from the last exact
      frame would look better.
- [ ] **No surface reuse on the render thread.** Every frame allocates a
      fresh `Surface`; a return path (`recycle(surface)`) would save a
      canvas-sized allocation per frame.
- [ ] **The gradient-heavy corpus files stay slow interactively**
      (XARA-T-0014): 4–5 s from open to first frame in the window for
      `testfiles/*GradFilledShapes*.xar`. Supersession keeps the window
      responsive while they render, but each pan costs a full render.
- [x] **`SimpleSphere.xar` rendered its sphere black** (XARA-T-0025,
      XARA-T-0037). The cause was neither the walker nor the renderer: the
      importer took the file's `TAG_CURRENTATTRIBUTES` (a black current
      fill) for the document defaults, so the unfilled frame drawn last
      covered everything. The gradients also wrapped (decision 31).
      `tests/fidelity.rs` pins both on a synthetic `.xar`. Dead end: the
      first suspicion was the transparency stack.
- [x] **`xarast-cli` is wired** (XARA-T-0004), and since XARA-T-0012 it
      needs no workarounds: it frames through `HeadlessOptions`
      (`HeadlessFrame::Fit`/`Fixed` plus `dpi`) and reads the zoom and the
      `WalkStats` from `HeadlessResult`. The former gaps were no walk
      stats (the CLI walked twice), no fixed zoom/dpi (it set the session
      viewport by hand), and `drawing_rect` including the page nodes (it
      kept its own `ink_rect`).
- [ ] **Quick shapes with a cached path have zero-area bounds**
      (XARA-T-0013). `RedStar`, `Test00` and `TestBitmapFill` walk to one
      primitive, but the display list culls it, so they paint nothing.
      That is why the corpus has 38 files with scene primitives but 35
      with painted pixels.
- [x] **`cargo bench -p xarast-app -- viewport`** (XARA-US-0006,
      acceptance criterion 4): `benches/viewport.rs`, numbers in
      `perf.md`.
