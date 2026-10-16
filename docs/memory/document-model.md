# document-model

Memory note for the **document model** subsystem — the crate `xarast-doc`.
Full research in [`../research/02-document-model.md`](../research/02-document-model.md).
Arbitrated decisions in [`../10-architecture.md`](../10-architecture.md) §3.1, §3.2, §3.5b, §3.6.
Phase specification: [`../phases/phase-02-document-model.md`](../phases/phase-02-document-model.md).

> Crate name: the research note called it `xarast-model`; the architecture
> document settles on **`xarast-doc`**. Use `xarast-doc`.

## Current state

**Phase 2 is implemented.** `xarast-doc` is a working document model with no
graphics and no UI dependency; it builds, tests, fuzz-shaped property-tests and
benchmarks with no GPU and no windowing system present.

Modules, all of them in `crates/xarast-doc/src/`:

| Module | Holds |
|---|---|
| `tree` | `NodeId`, `Tag`, `Links`, `NodeFlags`, `NodeData`, `Tree`, `Attach`, `TreeError` |
| `walk` | `Children`, `Ancestors`, `Preorder`, `Postorder`, `RenderWalk`, `WalkEvent`, `Descend` |
| `kind` | `NodeKind` and its payload structs |
| `bounds` | `Epoch`, `BoundsCache`, `compute_bounds`, `compute_bounds_with` |
| `attr` | `AttrSlot` (46), `AttrValue`, `AttrNode`, `MultiAttr`, `Quality`, `DefaultAttrs` |
| `attr::stack` | `AttrStack`, `ResolvedAttrs` |
| `attr::resolve` | `AttrResolver`, `resolve_uncached` |
| `attr::tags` | the `.xar` tag → slot reconciliation table and its tests |
| `fill` | `FillGeometry<S>`, `Ramp`, `RampStop`, `Perspective`, `Tiling`, `RampMapping`, `Paint`, `TranspPaint` |
| `fill_edit` | phase 8 fill/transparency commands (`SetFillGeometry`, `MoveFillControl`, ramp edits, profile, mapping, effect, tiling, transparency mode), `PaintSlot`, `FillChannel`, `FillHandle`, `StopTarget`, `set_own_attr` — see `colour.md` |
| `palette` | phase 8 palette commands, `ColourUses`, `PaletteResolver`, the palette's share of the digest — see `colour.md` |
| `foreign` | `ForeignBaggage`, `ForeignAttr`, `ForeignChild`, `ForeignChildKind`, `ForeignMarks` — per-node data a `.xarast` reader did not understand (XARA-T-0089) |
| `structure` | `DocumentNode`, `SpreadNode`, `PageNode`, `LayerNode`, `GridNode`, `FrameProps`, `AnimProps` |
| `text` | `TextStoryNode`, `TextLineNode`, `TextItem`, `TextLayout`, `Justification`, `LineSpacing`, `Script`, `TabStop` |
| `live` | `LiveNode`, `LiveRole`, `LiveKind`, `RegenState`, the seven parameter structs, `LiveParts`/`parts`, `controller_of`, `in_generated` |
| `regen` | phase 13 A3/A4: `regenerate`, `RegenKey`/`regen_key`, `LiveOutput`, `GeneratedShape`, `LiveCache`, `RegenQueue`, `FlushReport`, `RegenError` |
| `resources` | `DocumentResources`, `BitmapId`/`DashId`/`ArrowId`, `BitmapResource`, `collect_unused` |
| `history` | `Action`, `Tx`, `Transaction`, `History`, `CoalesceKey`, `Command`, `CommandBus`, `EditError` |
| `snapshot` | `Snapshot`, a dense `Arc<SecondaryMap<NodeId, NodeData>>`, plus `Document::snapshot`/`restore` |
| `document` | `Document`, `DocumentMeta`, `DumpOptions`, `canonical_digest`, `dump`, `update_bounds` |
| `digest` | `CanonicalHasher`, the `Canon` trait and its impls |
| `builder` | `DocumentBuilder`, `BuildLimits`, `Diagnostic`, `DiagCode`, `Severity`, `BuildError` |
| `validate` | `ValidationReport`, `Invariant`, `validate_tree`, `validate_document` |
| `synth` | `SynthSpec`, `synthetic_document` — the 100 000-node benchmark fixture |

`NodeKind` variants implemented: `Document`, `Chapter`, `Spread`, `Page`,
`Layer`, `Grid`, `Path`, `Shape`, `QuickShape`, `Bitmap`, `Guideline`, `Group`,
`Live`, `ClipView`, `TextStory`, `TextLine`, `TextItem`, `Attr`, `Opaque`.
`Live` has its phase-13 infrastructure (decision 41: regeneration
outside the tree) but no generator yet; the text variants are laid out
by Phase 9. `QuickShape` stores all
its parameters. Its `path` is the **generated outline** in document
coordinates, from `xarast_geom::regular_shape_outline` (XARA-T-0013,
2026-09-23). `QuickShape::outline()` regenerates it from the stored
parameters.

`ATTR_SLOT_COUNT` is **46**, unchanged by the reconciliation.

Tests: 82 crate-internal (`src/tests/`), 5 size gates, 5 public-API
integration, 1 compile-fail doctest. Benchmarks: `benches/doc.rs` (the budget
table) and `examples/arena_vs_persistent.rs` (the §3.1 benchmark).

**First real consumer, Phase 3.** The `.xar` importer
(`crates/xarast-xar/src/import.rs`) now builds documents from all 59 corpus
files — 830 533 nodes, zero `validate()` errors — through `DocumentBuilder`
and nothing else. It needed exactly one change to this crate:
`BitmapResource::content_hash` (decision 27 below). It did **not** need the
arena, `Tree`'s mutating methods, or an "amend the node I just made"
builder method: where `.xar` describes a node from its children, the
importer reads ahead instead. Keep it that way. What the importer found
that this crate should grow is listed in `xar-import.md` finding 10 — a
three-point linear fill, an "extra" tiling — and neither is urgent.

## The arena benchmark — verdict

Run: `cargo run --release -p xarast-doc --example arena_vs_persistent -- time`
(and `mem-arena` / `mem-hamt` for D, each in a process of its own because
resident memory does not reliably come back down after a free).

Fixture: the synthetic 100 002-node document of `xarast_doc::synth` — one
spread, six layers, groups nested to depth six, eight children per group, 40 %
attribute nodes.

| M | What | Arena | HAMT (`imbl`) | Ratio |
|---|---|---|---|---|
| A | full render traversal | **1.99 ms** (50.3 M nodes/s) | 22.3 ms (4.5 M nodes/s) | HAMT **11.2×** slower |
| B | 10 000 000 random lookups by `NodeId` | **70.6 ms** (7.06 ns each) | 1 033 ms (103.3 ns each) | HAMT **14.6×** slower |
| C | one attribute edit, then undo | **0.21 µs** | 1.52 µs | HAMT **7.2×** slower |
| D | resident bytes, whole document | **22.13 MB** (221 B/node) | 30.89 MB (308 B/node) | HAMT **1.40×** |

Against the rule pre-registered in `phase-02 §W2.11` — *the arena stands unless
all three hold: the HAMT within 20 % on A and B; the HAMT more than 5× faster
on C; the HAMT's memory within 1.5× on D* —

- A: **fails** (11× slower, not within 20 %).
- B: **fails** (15× slower, not within 20 %).
- C: **fails**, and in the opposite direction: the HAMT is 7× *slower*, not
  5× faster. Its own undo really is a pointer swap, but the edit costs a
  `HashMap` clone plus a node rewrite, and the arena's whole edit-and-undo
  cycle is two `SlotMap` writes and an inverse it already had.
- D: passes (1.40× ≤ 1.5×), and it is the only one.

**Verdict: the arena wins, decisively, and the question is closed.**
`10-architecture.md` §3.1 and §7 question 1 are updated to match, and the stale
"Phase 1 ships a benchmark" sentence in §3.1 is corrected in the same change.
Nothing about the result was surprising except how large the margins were; the
benchmark was worth running mainly because C was the condition that could
plausibly have gone the other way, and it did not come close.

Note the second-order finding this produced: a persistent snapshot is
expensive to *build* (see the checkpoint budget below), which is why
checkpointing is off by default.

## Measured budgets

`cargo bench -p xarast-doc --bench doc -- --quick`, release, on the development
machine. The phase's budget table was written for a "reference machine" we do
not have, so read the overruns as "this is where the cost is", not as failures
of a controlled measurement.

| Operation | Budget | Measured | |
|---|---|---|---|
| Undo of a single-node edit | ≤ 1 ms | **0.27 µs** | ✅ 3 700× under |
| Redo of a single-node edit | ≤ 1 ms | **0.17 µs** | ✅ |
| `walk_render` over 100 000 nodes | ≤ 2.0 ms | **2.01 ms** | ≈ at budget |
| `preorder` over 100 000 nodes | ≤ 1.5 ms | **2.09 ms** | ⚠ 1.4× over |
| Random `Tree::get` | ≤ 5 ns | **6.6 ns** | ⚠ 1.3× over |
| `AttrStack::push` + `pop_scope` | ≤ 20 ns | **17.5 ns** | ✅ |
| `AttrResolver::resolve`, warm | ≤ 30 ns | **30.4 ns** | ≈ at budget |
| `AttrResolver::resolve`, cold | ≤ 2 µs | **1.08 µs** | ✅ |
| `DocumentBuilder`, 100 000 nodes | ≤ 150 ms | **47.4 ms** | ✅ |
| `canonical_digest()`, 100 000 nodes | ≤ 30 ms | **6.2 ms** | ✅ |
| `Document::snapshot()`, 100 000 nodes | ≤ 25 ms | **58.5 ms** | ❌ 2.3× over |
| Resident bytes per node | ≤ 160 B | **221 B** | ❌ but see below |
| `compute_bounds` over 100 000 nodes, cold | ≤ 12 ms | **8.95 ms** | ✅ |

Three of these need a word.

- **`snapshot()` is 2.3× over.** Building a HAMT of 100 000 `Arc<NodeData>`
  clones is simply that expensive, and each clone deep-copies a boxed payload.
  The consequence was concrete: checkpointing every 64 transactions put about
  **1.5 ms of amortised cost on every single edit** and blew the 1 ms undo
  budget on its own. So automatic checkpointing is now **off by default**
  (`History::set_checkpoint_cadence`), and Phase 6 should drive checkpoints
  from the autosave timer and make them incremental — sharing structure with
  the previous snapshot instead of rebuilding — which is the one thing a HAMT
  is actually good at. *(Superseded 2026-09-23: the HAMT is gone from
  checkpoints and `snapshot()` is 9.7 ms. See "Dead ends".)*
- **221 B/node is not comparable to the 160 B budget.** The budget says
  "excluding payloads"; the measurement is whole-process resident memory and
  therefore includes every `Arc<Path>`, every boxed `NodeKind` payload and the
  allocator's own overhead. `NodeData` itself is exactly 64 bytes. A
  payload-free measurement is still owed.
- **`preorder` and `Tree::get` are within 40 % of budgets set for another
  machine.** Not worth optimising until there is a reference machine to
  measure against; recorded so that a later regression is visible.

`compute_bounds` was 53 ms when first written, and 8.95 ms after two changes
worth remembering: it took a `ResolvedAttrs` snapshot per node (46 `Arc` clones
each — the fix is `AttrStack::stroke_extent()`, which is all a bounding box
actually needs), and it accumulated into a `HashMap` where a
`SecondaryMap<NodeId, Rect>` does.

## Decisions taken (and why)

The decisions carried over from the research and re-confirmed in
implementation:

1. **Arena `slotmap::SlotMap<NodeId, NodeData>` + `enum NodeKind`** — not
   inheritance, not trait objects, not ECS. Confirmed by measurement, above.
2. **Sibling linked list** (`parent/prev/next/first_child/last_child`) rather
   than `Vec<NodeId>`. We keep `last_child`, which the original lacks, because
   the importer appends in bulk.
3. **`NodeHidden` is gone.** Delete = `detach` + a `DETACHED` flag; the node
   stays alive in the arena, retained by the history `Transaction`, and is
   really destroyed only when the history evicts that transaction.
4. **Attributes remain nodes** (`NodeKind::Attr`), with `AttrResolver` as a
   derived cache on top.
5. **`AttrStack`** is a dense table indexed by `AttrSlot`, an unwind log of
   `(slot, previous)` and a vector of scope marks. About sixty lines.
6. **One generic `FillGeometry<S: Stop>`**; `Perspective` is an `Option`.
7. **Undo is an inverse-action log**, not a persistent live store.
8. **Selection lives outside the document**, in `xarast-app`. There is no
   `SELECTED` flag in `NodeFlags` and no `EditState` in this crate.
9. **Control-point selection lives outside `PathData`.** `Path` stays
   comparable with `==`.
10. **Caches are keyed on state**, not on a boolean.
11. **`xarast-doc` has no graphics dependency.**

New in Phase 2:

12. **`Tree`'s mutating API is `pub(crate)`, and that is the mechanism, not a
    convention.** `create`, `attach`, `detach`, `destroy_subtree`, `get_mut`,
    `set_tag`, `set_bounds` and `invalidate_bounds` are all crate private. The
    entire public mutation surface is `DocumentBuilder` during construction and
    `CommandBus` afterwards, and both record inverses. A `compile_fail,E0624`
    doctest in `lib.rs` proves a downstream crate cannot call `tree.detach()` —
    a doctest rather than `trybuild`, because it needs no extra dependency and
    does not depend on matching a compiler's exact stderr. **Do not widen this
    API.** Nothing would fail until a user lost work.
13. **`BoundsCache` is a side table on `Tree`, not a field of `NodeData`.**
    This is the one real deviation from the phase document's struct layout and
    it is forced: five `Option<NodeId>` links cost 40 bytes, `NodeKind` costs
    16, the tag and flags cost 6 — which is 62, and `NodeData` is gated at 64.
    A bounds cache is 20 bytes more. It is derived data, traversal never reads
    it, and in a side table it does not pollute the cache line that traversal
    does read. Access is `Tree::bounds(id)`.
14. **Every `NodeKind` payload larger than a pointer is boxed**, which is what
    keeps `NodeKind` at 16 bytes and `NodeData` at exactly 64. `TextItem` is
    deliberately 8 bytes and stays inline: a text document is mostly those, and
    its glyph metrics are Phase 9's derived cache, not node data.
15. **The default attribute block is `DefaultAttrs`, a dense table — not 46
    attribute nodes under the document root.** The original materialises them;
    doing the same here would duplicate the table, make every resolution scan
    forty-odd nodes before it starts, and put them in the digest and every
    dump. **`TAG_CURRENTATTRIBUTES` does *not* map onto it** (it did until
    XARA-T-0037): that block holds the editor's current attributes, what the
    next object drawn gets, and a file never overrides the factory defaults.
    Mapping it turned every unfilled path into the file's current fill.
16. **An attribute's scope is "its following siblings and their subtrees, **and
    its parent's own ink**".** The second half is not an extra rule: a parent
    paints after its children, which is exactly what makes a `.xar` path's
    fill — stored by the format as the path's *child* — apply to the path.
    `RenderWalk` emits scope events only for nodes that have children, so a
    consumer paints an ink node at `LeaveScope` when it has children and at
    `Visit` when it has none; the two cases never overlap. A property test
    checks the walk, the uncached resolver and the cache all agree.
17. **`Action::inverse(&self, doc: &Document)` takes the document**, so it
    physically cannot read post-apply state.
18. **A transform's inverse is exact or it is a snapshot.** A pure translation
    in millipoints inverts exactly, so its inverse is another `Transform`.
    Anything else would go through a float inverse and requantise, so the
    inverse is a `SetKind` carrying the pre-transform payload. Cheap where it
    can be, exact always — which is what makes the byte-identical undo test
    pass rather than nearly pass.
19. **Undo and redo share one function.** `apply_reversed` applies a list in
    reverse while collecting the inverses in the same convention, so redo is
    undo applied to the inverse list. One implementation, one thing to get
    right.
20. **The history budget is in bytes**, `History::DEFAULT_BUDGET` = 128 MiB.
    Eviction is oldest-first and is what finally destroys the nodes a
    transaction retained.
21. **Coalescing is gesture based** by default: `CommandBus::begin_gesture` /
    `end_gesture`, because it does not depend on wall-clock timing and is
    therefore testable. A `CoalesceKey` a command supplies itself overrides it,
    which is the escape hatch for keyboard nudges.
22. **Automatic checkpointing is off by default.** Measured, see above.
23. **"One spread, one active layer" is maintained by commands, not by the
    arena.** `Tx::keep_one_active_layer` runs inside the same transaction, so
    undo puts the old active layer back too. The arena deliberately does not
    fix it up for itself: an automatic change the undo log never saw is exactly
    how undo stops being exact.
24. **`DocumentBuilder::finish` has exactly two outcomes**: `Err(BuildError)`,
    or a document whose `validate()` has no errors. Illegal nestings are
    created detached and swept; dangling bitmap references are dropped;
    controllers without a source get one; spreads get an active layer. Each
    repair is a `Diagnostic`. `BuildError::Inconsistent` exists so that a
    failure of this guarantee is loud rather than hidden — it would be a bug in
    the builder, never in the input.
25. **Build limits live in `BuildLimits`, not in importers**: depth 256, nodes
    20 000 000, bytes 2 GiB, points per path 16 000 000.
26. **`DiagCode` is the shared vocabulary of every importer**, extended from
    the phase list with `IllegalNesting` and `Repaired`.
27. **Deduplication is by SHA-256 of the decoded payload *and the encoded
    original***; liveness is `collect_unused`, a sweep over everything alive
    in the arena — which automatically includes what the history retains, so
    an undone deletion still finds its resources.
    *Changed in Phase 3.* Hashing only the decoded payload was wrong in two
    ways. The immediate one: the `.xar` importer stores the encoded bytes and
    leaves `pixels` empty until Phase 10 decodes them, so every bitmap in a
    file hashed identically and `insert_bitmap` returned one id for all of
    them. The deeper one: two bitmaps with the same pixels but different
    encodings really are two resources, because `original` is emitted
    verbatim on save and collapsing them throws one encoding away.
28. **`canonical_digest()` excludes `NodeId` *and* `Tag`.** Both are allocation
    ordered. Resources are hashed in content order so that the order they were
    inserted in does not change the digest. Colour and bitmap *references* are
    hashed by table slot, which is stable within a document but not across two
    documents built by different routes — good enough for what the digest is
    for (undo round-trip, round-trip tests, "did this refactor change
    anything"), and worth knowing before someone compares digests across
    importers.
29. **`Document::restore` rebuilds the tree and remaps every `NodeId`**, but
    carries `Tag` across unchanged. `slotmap` cannot reinsert at a given key,
    and `NodeId` is documented as never serialised, so this is the honest
    implementation. It invalidates any outstanding `NodeId` and any history.
30. **The gradient profile and ramp mapping live on `Ramp`**, and the bitmap
    and procedural fills carry their own `profile` field. The phase document's
    variant list omitted them; the format carries bias/gain on every gradient,
    so they had to go somewhere.
31. **`imbl` is MPL-2.0 and is used unmodified**, for checkpoints and for the
    benchmark. `deny.toml` carries the exception with its justification.
32. **Foreign baggage is a side table on `Tree`, keyed by `NodeId`**
    (XARA-T-0089, phase 6, 2026-09-23). `SecondaryMap<NodeId,
    Arc<ForeignBaggage>>`, next to the bounds cache: `NodeData` stays at
    exactly 64 bytes and a document with no baggage pays one empty map.
    What `research/06 §8.2` asks per node: unknown attributes as
    `(uri, prefix hint, local, value)`; unknown child elements, unknown SVG
    elements, comments and PIs as **verbatim text** with their position
    (how many elements standing for the node's non-attribute children
    precede them); and the §8.5 marks `DIRTY`, `STALE`,
    `BASE_AUTHORITATIVE`. Keying by `NodeId` is what makes it survive
    edits for free: move, regroup and change layer are relinks, a deleted
    node keeps its baggage while the history retains it (so undo brings it
    back), `destroy_subtree` drops it, `Document::snapshot`/`restore` carry
    it across the id remap. Changes go through `Action::SetForeign`
    (inverse: the previous `Arc`), `Tx::set_foreign`, `Tx::mark_foreign`
    (adds marks, records nothing when the node has no baggage or already
    has them) and `DocumentBuilder::foreign` at import. Setting empty
    baggage removes the entry, so "empty" and "absent" are one state.
    `set_foreign` is not refused on a locked node: keeping or marking data
    we do not understand is never an edit of the user's artwork. The
    canonical digest folds baggage in **only where present**, flagged by
    bit 16 of the per-node flags word (`NodeFlags` uses 16 bits), so every
    document without baggage digests exactly as before. Automatic marking
    of edited nodes (F4.7, W4, 2026-09-23) lives in the commands, never in
    the arena: `Tx::transform` marks the moved subtree's baggage `DIRTY`;
    `Tx::set_attr` and attaching an attribute node mark the owner's
    subtree `DIRTY` (recolour); `Tx::set_kind` marks an ink node `STALE`
    (its geometry changed), any other node `DIRTY`; regroup, change layer
    and delete mark nothing (§8.5 rules 1 and 4). The marks are
    `SetForeign` actions in the same transaction, so undo takes them back.
33. **The builder restores what a reader read** (W4): `DocumentBuilder::tag`
    claims a persistent tag (a node the builder numbered itself gives it up
    and is re-tagged fresh: tags stay unique), `flags` sets `LOCKED` /
    `MAGNETIC` only, `root` replaces the `DocumentNode`, `colour_parent`
    points a palette entry at a later one, `current_scope` names the node
    being appended to.
34. **Every node has a content revision** (`Tree::content_rev`, phase 7,
    XARA-US-0029). A lazy `SecondaryMap<NodeId, u64>` beside the bounds
    cache (so `NodeData` stays at 64 bytes); absent reads as 0. Every
    `Action` that changes a node's kind, geometry, attribute value or flags
    — **undo and redo included** — gives it `next_rev`, a per-tree counter,
    so `(tag, rev)` names one version of one node and is never reused within
    a tree. Relinks (attach, detach) do not touch it: structure is the
    walker's business (it folds attribute-node versions into a scope
    fingerprint). `SetResource` bumps `Tree::resources_rev`. `restore`
    continues the old tree's numbering and re-touches every node, because a
    restored tag may carry content the old tree never had under that
    revision. Per tree, not global, on purpose: two imports of the same file
    must produce identical revisions (the walk-stability corpus test).
    Undo getting a *new* revision rather than the pre-edit one only
    over-invalidates; restoring old revisions would need the inverse to
    carry them, for no measured gain.
35. **`History::undo_label`/`redo_label`, `CommandBus::undo`/`redo`/
    `gesture_open`** exist for the Edit menu ("Undo Move") and for the app's
    per-kind coalescing, which splits a gesture when a command does not
    coalesce with the previous one (`xarast_app::Session::apply_edit`).
    The F4.7 foreign-baggage marks ride in the same `Tx` calls
    (`transform`, `set_attr`, `set_kind`), so every `EditCommand` gets them
    for free; `tests/tools.rs` pins it for a selector move.
36. **The palette is undoable as a whole** (phase 8, XARA-US-0037):
    `Action::SetPalette { new: Arc<ColourTable> }`, inverse = the table as
    it was. Its apply moves the palette epoch strictly forward (undo
    included) and bumps `resources_rev`. `EditError` gained `Palette(
    ColourEditError)` and `FillEdit(&'static str)`. Details and the reasons
    in `colour.md` decisions 7, 8.
37. **`canonical_digest` now includes the colour table** (slot order, ids,
    every field except the epoch and the cached order), because palette
    edits must be provably undoable. Digests of documents with a palette
    changed value once; every comparison in the workspace is within one
    document, so nothing pinned a value.
38. **`DocumentBuilder::finish` breaks palette loops** with
    `ColourTable::repair_cycles` and a `Repaired` warning: a loaded file is
    never rejected for a derivation loop.
39. **Fill edits localise**: `fill_edit::set_own_attr` replaces the node's
    own attribute of the slot or adds one as the first child — the same
    rule `xarast-app`'s private `set_own_attr` follows; the app can switch
    to the public one.
40. **A placed bitmap carries its photo operations**
    (`BitmapNode::photo_ops: PhotoOps`, XARA-US-0054; `image.md`, "Photo
    adjustments"). A field of the node, not an attribute: the chain is
    per object, never inherited, and changing it may move the object
    (crop, orientation), which only a `set_kind` of the whole node records
    as one step (`SetPhotoOps`). The node is boxed, so `NodeData` stays
    within its 64-byte gate. `canonical_digest` adds the chain's hash
    **only when it is non-empty**, so every existing digest is unchanged.
    `bitmap_usage` / `collect_unused` need nothing new: the chain names no
    resource (the master is the node's `image`). `SetPhotoOps` only
    `set_kind`s, so it creates and detaches nothing and the history's
    retention rule (below) does not apply to it; the history property
    tests place bitmaps and apply adjust/orient/crop chains to prove it
    (`Edit::AddBitmap`, `Edit::Photo`; clean at `PROPTEST_CASES=20000`).
41. **Live objects regenerate outside the tree** (phase 13 A1–A4,
    XARA-US-0068, 2026-09-24; `src/regen.rs`). `regenerate(doc,
    controller, dpi)` is a pure function of the document returning a
    `LiveOutput`; `LiveCache` keeps each controller's last output with
    the `RegenKey` it was computed for; `RegenQueue` collects controllers
    (`mark`, `mark_changes` from the tree's change journal: every
    controller at or above a changed node, or every controller when the
    journal overflowed), deduplicates, and `flush`es deepest first (ties
    by tag), skipping any whose key still matches the cache.
    `RegenState::Dirty` entries flush alone with `flush_urgent`
    (export, conversion); `Deferred` ones wait for paint. Why outside:
    the phase's three traps become structural. *Regeneration is not an
    edit*: nothing in `regen` writes the tree or the history, so one
    parameter change is one undo step and undo recomputes (or finds) the
    old output — `one_parameter_change_is_one_undo_step_and_a_flush_
    records_nothing`. *The double invalidation* (old extent and new) is
    the render thread's scene diff, which compares the scene drawn with
    each output (`render.md`, "Edit damage"); there is no per-controller
    bounds bookkeeping to forget. *Deferred beats immediate*: a drag
    marks one entry, flushed once per frame. The alternative — rewrite
    the generated subtree in place without recording it — breaks the
    history property "every undo lands on the recorded digest" the moment
    the flush has not run yet.
42. **`RegenKey`** = SHA-256 of a version string, the controller's
    parameters (the `LiveNode` canon with role/regen/name neutralised),
    its tag and content revision, the tags, content revisions and child
    counts of every non-generated node under it in preorder, the tags and
    revisions of the attribute children that precede the first object at
    every ancestor level (what it inherits), `resources_rev`, and the dpi
    quantised to 1/64. Content revisions move forward on undo too
    (decision 34), so an undo never finds a stale entry — it regenerates.
    Keys are per tree: a `LiveCache` belongs to one document (`clear` on
    replacement, `retain_alive` after the history lets nodes go). An
    attribute placed *after* an object at an ancestor level (the
    `AttrAfterInk` warning) is not in the key; such files exist (267
    warnings in the corpus) but none with a live object today.
43. **`LiveOutput` has two variants.** `Stored` — draw the source and the
    generated subtree a file stored, as they are — is what every kind
    returns until its generator lands (blend/contour XARA-US-0070, mould
    XARA-US-0071), and what pixel effects return for good: shadow,
    feather and bevel lighting are computed by the renderer at the
    resolution they are shown (`render.md`, "Live effects: the offscreen
    pipeline"). `Shapes(Arc<[GeneratedShape]>)` (path + the attributes
    that differ from what the controller inherits, paint order, document
    coordinates) is the contract the geometry generators fill. The
    `match` in `regenerate` lists every `LiveKind` so that a generator
    lands as a one-arm change and a new kind cannot be added silently.
44. **Generated data needs its parent** (A2). `LiveRole::needs_parent()`
    is true for `Generated`; `live::in_generated(tree, id)` is true for a
    generated node or anything inside one, and `Tx::check_permitted`
    now refuses every command on such a node (it refused only the
    generated node itself before). The controller, with its whole
    subtree, is deleted normally. Marquee selection and the clipboard
    need nothing: `xarast-app`'s `selectable_objects` takes a layer's
    direct children, and a generated node is never one. **"Exactly one
    generated subtree" is deliberately not an invariant**: a blend has
    one generated node per pair of objects, a controller built by a
    reader or a command has none until something bakes one, and the
    derived result lives outside the tree anyway. Invariants 4 and 12
    stand as they were.
45. **Blend steps are generated, not materialised** (the phase-13
    decision the story named; F7). They are `LiveOutput::Shapes` in the
    `LiveCache`, never nodes; only an explicit "convert to editable
    shapes" (`BecomeA`, A9) creates nodes, as one undoable command.
46. **`MouldGeometry` is a trait over a closed enum** (the other named
    decision). The trait (`describe`, `validate`, `mould_point`,
    `mould_path`, `invert_point`) is the contract G2 lists; the storage
    is an enum (`Envelope4x4`, `Envelope2x2`, `Perspective`) because the
    set is closed, the params must be `Clone + PartialEq`, digested and
    serialised, and an exhaustive `match` is what makes a new kind
    impossible to forget. To be built by XARA-US-0071; recorded here so
    it is not re-litigated.
47. **`ProceduralSource::cache_key()`** (the third named decision): SHA-256
    of `PROCEDURAL_GENERATOR` (`"xarast-procedural/0"`, bumped whenever
    the pixels change for the same parameters), the fractal flag and the
    `ProceduralParams` canon (floats by their bits: one ulp is another
    bitmap). The generator version in the key is phase 13 H6's "freeze
    it": an old bitmap can never be served under a new algorithm.
    **Still unused** after XARA-T-0336: procedural fills do not become
    `BitmapResource`s. The walker keys its tiles by
    `xarast_image::procedural::Procedural` (the fill's 16.16 words and the
    tile size) plus profile and colours, and the frozen generators are
    named by `CLOUDS_GENERATOR` / `PLASMA_GENERATOR`. Since XARA-T-0336
    `FillGeometry::Fractal`/`Noise` also carry `origin`, `axis_x`,
    `axis_y` (control points, transformed, digested, moulded).
48. **A shadow is a controller whose generated node stays empty**
    (XARA-T-0318). `regenerate` returns `LiveOutput::Stored` for it for
    good: the renderer computes the pixels at the resolution shown, so
    nothing goes through `RegenQueue` (XARA-T-0316's wiring is not needed
    by shadows). `ShadowParams` carries everything the original splits
    between the controller and the shadow node, **including the colour**
    (the shadow node's fill in the original): an attribute on a node with
    no ink would not survive `.xarast`. `glow_width` joined it, all
    fields are kept whatever the kind (switching kinds finds the old
    settings, as in the original), and `Default` is the original's new
    shadow. `silhouette_map` / `extent` give the document-space geometry;
    `bounds::compute_bounds_with` makes a shadow controller's box cover
    its shadow (culling and damage need it). The digest covers the two
    new fields.

49. **A bevel is a controller whose generated node stays empty**
    (XARA-T-0322), as a shadow is (decision 48). `BevelType` is the
    original's fifteen styles in record order and lives in
    `xarast_geom::bevel` beside the cross-sections (re-exported by
    `xarast-doc`, so `AttrValue::BevelType` keeps its name). Angles are
    **degrees** (the file's unit), contrast `0..=1`; `colour:
    Option<Colour>` is the bevel node's own flat fill (`None` = the
    first colour of the source's first object, `BevelParams::paint`). The join is not a
    parameter: it is the controller's join attribute, already in the
    tree. An outer bevel's controller bounds grow by its indent. The
    digest covers the colour.
    *XARA-T-0326*: the walker resolves the join at the generated node
    (`resolve_uncached`: the controller's attributes before it and its
    ancestors'), which is what the original's bevel node finds applied
    when it carries no copy of its own (none in the corpus).
    `BevelParams::flat_paint` is `colour`, else the first object's fill
    when it is `Paint::Flat`: an inner bevel paints its band in it, as the
    original's bevel node does; a non-flat first fill gives `None` (the
    renderer lights the objects' own colours).
50. **A contour regenerates its rings; its generated node keeps the
    blend's start as attributes** (XARA-T-0330). `ContourParams`:
    `steps` (rings = steps + 1, or 1 for `inset`, at most
    `MAX_CONTOUR_RINGS` = 256), `width` (a distance; `outer` gives the
    direction, unlike the file's signed width), `include_line_widths`
    (false from `.xar`), `join: Option<Join>` (`None` = the join
    attribute in force at the generated node, the original's rule),
    `profile` (spacing), `colour_profile`, `colour_effect`, `inset`.
    Flatness is derived (`contour_tolerance`), not kept. The generated
    node's own attribute children are the original `NodeContour`'s
    attributes, the start of the ring blend; it sits before the source
    (outer) or after it (inner), which is the paint order.
    `xarast_doc::contour::generate` → `LiveOutput::Shapes` in paint
    order, every ring (empty ones too), each with the *blended* slots
    only (fill, stroke colour, line width, the two transparencies) on top
    of the generated node's attributes; the start ring has none.
    Blending: flat↔flat, same-family gradients with equal stop counts
    (points, colours, stops, profile), a flat promoted to the other's
    gradient; palette colours resolved first; "no colour" and anything
    else does not blend and keeps the start (the original's failed
    blend). `regenerate` returns them; the walker keys a `LiveCache` on
    `RegenKey` (`LiveCache::current_or_regenerate`), so no queue is
    needed. An outer contour's controller bounds grow by `growth()`:
    the distance × 11.47 when the corners may be mitred (a cheap
    conservative bound, see TODO).
51. **A blend's pairs are generated nodes between its objects; its steps
    are never nodes** (XARA-T-0333, phase 13 F1/F5/F7). The tree mirrors
    the original's: `Controller (Blend)` › attributes, `Source (Blend)` ›
    object, `Generated`, object, `Generated`, object… Generated node `k`
    (direct children of the source, in order) blends the ink sibling
    before it into the one after (`blend::blenders`), with
    `BlendParams::blenders[k]` (`BlenderParams`: recorded start points,
    explicit subpath `pairs`; defaults when missing). `BlendParams` adds
    `attr_profile`, `colour_effect: Option<FillEffect>` (`None` = the
    original's "no colour blend"), `MAX_BLEND_STEPS` 999 (`drawn_steps`).
    `blend::generate` → `LiveOutput::Groups` (one group per generated
    node, `GeneratedShape` now says `filled`/`stroked`): step `k` of `n`
    at `k/(n+1)`, geometry at the object profile (`xarast_geom::blend`),
    attributes slot by slot (colour fills, flat transparencies, line
    widths at the attribute profile with bias **and** gain negated,
    graduated transparencies at the object profile, the rest at the
    attribute profile; colours fade/rainbow/alt-rainbow, no colour as
    white unless both are none; anything else start ≤ ½ < end). A step
    carries **every slot** (a whole blended map, as the original), and
    both ends and every step keep **only what shows**
    (`blend::shown_attrs`): an unfilled/unstroked object's paint is no
    colour, an invisible outline's width/join/caps/dashes/arrows and a
    flat fill's repeat are the defaults. Both rules exist because a
    `.xarast` reload moves a blend's own attributes onto its objects and
    keeps only what shows; anything else breaks the byte fixed point.
    Pairs continuing one object's subpaths make one path; filled/stroked
    when either end is; `reverse` draws the steps from the end. The
    controller's bounds need nothing: a straight blend's steps lie in its
    ends' union.
52. **A mould's cage is a parameter; its copy is generated, never
    stored** (XARA-T-0334, phase 13 G1/G5–G8). `MouldParams` is an enum:
    `Envelope { shape: [Point; 12] }`, `Envelope2x2 { shape: [Point; 24] }`,
    `Perspective { corners: [Point; 4] }`, each with `source: Rect` (the
    rectangle the cage maps from; empty = the source subtree's bounds)
    and `threshold: i32` (refit tolerance, mp; 0 never splits). Cage
    order as the original's mould path: the source's bottom-left image
    first, clockwise. Tree: `Controller (Mould)` › attributes, an
    **empty** `Generated`, `Source` › the originals. `mould::generate`
    (and `generate_with`, which takes the caller's text outliner) →
    `LiveOutput::Shapes` in paint order: each object's outline through
    the mould (`xarast_geom::mould`), carrying **every** slot (so the
    stack only scopes it), `filled`/`stroked` from the object (also
    encoded as no-colour paint). Fills mould **once, on `FillGeometry`**
    (`mould::mould_fill`): control points through the map; a fill under
    a perspective (or already perspective) gains its far corner and a
    `persp`; under an envelope an implied perpendicular axis (linear,
    circular) is made explicit (linear: as an affine `persp`); flat and
    procedural fills unchanged. A bitmap object becomes its
    parallelogram filled with itself (`Tiling::Simple`, not stroked). A
    nested generated node (contour rings, blend pair `k`, inner mould)
    is moulded where it stands; an inner mould's or inset path's source
    is skipped; pixel effects inside a mould follow decision 54. A flat
    cage (no width or height) is the identity: what the importer stores
    for a missing or malformed one. A controller's bounds are the image
    of its source rectangle (an 8×8 sample grid + 2 %), grown by the
    stroke extent. A blend end that is (or holds) a mould controller
    blends from its moulded copy.
53. **Tools build and edit contours and blends through `Tx`**
    (XARA-T-0340). Two narrow additions: `Tx::attach` lets a node made
    in this transaction become the `LastChild` of another made in it,
    even a generated one (a new contour's start attributes; still
    refused when locked), and `Tx::set_live_kind` also rewrites the
    generated nodes directly under the controller's **source** (a
    blend's pairs), so a tool-edited blend carries what a reader builds.
    Previews regenerate under `regen::RegenOverride` (`kind` in place of
    the controller's, `moved` nodes strictly inside it with a matrix):
    `regen_key_with` = `regen_key` when the override is inert, else the
    override's canon and the moved tags and matrix bits;
    `regenerate_with` → `contour::generate_with(params)` and
    `blend::generate_with(params, moved)` (a moved end's outline and
    geometry-linked attributes through the matrix; a mould end ignores
    the move). `LiveCache::regenerated()` counts computed outputs.
54. **The composition order of nested live effects** (phase 13 J4,
    XARA-T-0337). Every live object reads its source **as drawn,
    bottom-up**: an inner controller is computed first because the outer
    generator *calls* it (no stored intermediate, no queue order to get
    wrong; the outer key covers every revision in the inner subtree,
    decision 42, so an inner change regenerates every outer). How each
    outer kind sees what is nested in it:
    | Outer | Reads its source as | Facts |
    |---|---|---|
    | shadow | the rendered alpha of everything drawn (nested effects included) | `render.md`, "Shadows from rendered alpha" |
    | feather, bevel | an outline silhouette (`render.md`, "Nested effects in a silhouette pass") | `Kernel/nodecntr.cpp:1184-1231` |
    | contour | outlines (`contour::inks`): ink nodes, a **mould anywhere below as its moulded copy**; other nested generated output (rings, steps) left out | a mould's path conversion passes back its moulder's paths, `Kernel/nodemold.cpp:997-1015` |
    | blend | each end through `contour::inks` (same rule); nested blends blend their own ends | as above; `Kernel/nodeblnd.cpp:502-620` |
    | mould | outlines of everything (rings, steps, inner moulds moulded where they stand); a **shadow** inside is left out, a **bevel** is its flat unlit band (outer: beneath, inner: over its objects, in its own colour else its first object's fill), a **feather** on an object or group *inside the source* stays on each path from it | `CopyChildrenAsShapes` = `BECOMEA_PASSBACK` (`Kernel/noderend.cpp:4465-4490`); `NodeShadow::DoBecomeA` passes nothing back (`Kernel/nodeshad.cpp:1058-1248`); `NodeBevel` passes its ink path filled, line colour none (`Kernel/nodebev.cpp:2499-2528`); `AttrFeather` is never factored out (`Kernel/fthrattr.cpp:1772-1779`) |
    A feather on the mould controller or above it feathers the mould as
    one unit (the walker's `own_feather`), never each shape again
    (`mould::feather_inside`). **Refused, as the original refuses them**
    (`live_edit`): a contour on or inside a bevel and a bevel on or
    inside a contour (`Kernel/nbevcont.cpp:2346-2351`,
    `Kernel/ncntrcnt.cpp:1246-1249`, whatever the direction); moulding a
    shadow controller directly ("no direct moulding",
    `Kernel/nodecont.cpp:413-418`; a group holding one is moulded), an
    object of a blend (`Kernel/nodeblnd.cpp:1862-1867`) or an object
    inside a mould (`Kernel/nodemldr.cpp:1528-1535`). Through a group
    every ordered pair of the six kinds nests: the 30-pair matrix in
    `xarast-app/tests/live_nesting.rs` renders each completely and undoes
    it exactly. DPI does not propagate: generators are resolution-free
    and pixel effects are computed at the resolution shown.
55. **"Convert to editable shapes" of live objects** (phase 13 A9,
    `live_edit::become_shapes`). A mould, a contour or a blend becomes a
    plain group standing where it stood, holding exactly what it drew:
    a mould's moulded copy (every shape a `Path` with attribute children
    for what it would not inherit there); a contour's rings (negative
    winding, as drawn) and its objects; an inset path's rings only; a
    blend's objects with each pair's steps between them. The controller's
    own attributes are copied onto the group (the original copies its
    mould's non-optimising ones, `Kernel/nodemold.cpp:955-966`). Moved
    objects keep their look (`materialise`). Controllers convert deepest
    first, one undo step for the whole selection. Live objects inside a
    blend stay live (the original keeps controllers in a blend,
    `Kernel/nodeblnd.cpp:560-575`). **Shadows and bevels stay live**: the
    original rasterises them into a path with a bitmap fill
    (`Kernel/nodeshad.cpp:1101-1220`, `Kernel/nodebev.cpp:2149-2498`),
    which needs the renderer's pixels as a `BitmapResource`; not built.
    A moulded perspective fill stays perspective (the original strips
    them, `RemovePerspectiveFills`; Xarast's model can say them, so the
    result looks as the live object did).

## The `.xar` attribute tag reconciliation

Done tag by tag against `research/01 §8` and `§4.12`. The result **did not
change the slot count**: 46, as `research/02 §10.6` proposed. The machine-
readable table is `crates/xarast-doc/src/attr/tags.rs`
(`XAR_ATTRIBUTE_TAGS`, `SLOTS_WITHOUT_ATTRIBUTE_TAG`), and three tests assert
that no tag appears twice, that every slot is either reached by a tag or listed
as tagless, and that the count is still 46. Phase 3 reads that table; it does
not invent its own.

**One Xarast-only slot since XARA-T-0225 (2026-09-24): `TxtFeatures`**
(`AttrValue::FontFeatures(Arc<[FeatureSetting]>)`, OpenType feature settings
sorted by tag, empty = the font's defaults). It is appended *after* the 46
(index 46, `ATTR_SLOT_COUNT` = 47), listed in `SLOTS_WITHOUT_ATTRIBUTE_TAG`
(the `.xar` format has no record for it), and the count test now says "46
reconciled plus Xarast's own". Appending kept every existing slot index, so
nothing indexed by slot moved; the canonical digest of every document changes
(the defaults block hashes all slots), but no digest is pinned anywhere.

| `.xar` tags | Slot |
|---|---|
| 151, 193–195 line colour | `StrokeColour` |
| 173 | `StrokeTransp` |
| 150, 190–192, 153–159, 200, 202, 204, 4010, 4075–4078, 4088, 4121, 4122 | `FillGeometry` |
| 166–172, 201, 203, 205, 4011, 4123 | `TranspFillGeometry` |
| 163–165, 206 | `FillMapping` |
| 180–182, 207 | `TranspFillMapping` |
| 160–162 | `FillEffect` |
| 152 | `LineWidth` |
| 178 | `WindingRule` |
| 176 | `JoinType` |
| 179 | `Quality` |
| 183, 184, 188 | `DashPattern` |
| 174 **and 175** | `StartCap` |
| 185 / 186 | `StartArrow` / `EndArrow` |
| 177 | `MitreLimit` |
| 4086 | `Feather` |
| 3500–3505 | `OverprintLine`, `OverprintFill`, `PrintOnAllPlates` |
| 2900, 2901 | `TxtLineSpace` |
| 2902–2905 | `TxtJustification` |
| 2906 / 2907 | `TxtFontSize` / `TxtFontTypeface` |
| 2908–2913 | `TxtBold`, `TxtItalic`, `TxtUnderline` |
| 2914–2917 | `TxtScript` |
| 2918 / 2919 / 2920 | `TxtTracking` / `TxtAspectRatio` / `TxtBaseline` |
| 4201 / 4202 / 4203 / 4204 | `TxtLeftMargin` / `TxtFirstIndent` / `TxtRightMargin` / `TxtRuler` |
| 189 `USERVALUE` | **multi-applicable** — except with the web-address key, which is `WebAddress` |

Slots no attribute tag reaches, and where they come from instead:
`WebAddress` (tag 189 with the web-address key); `StrokeType`,
`VariableWidth`, `BrushType` (§4.10, brushes and strokes); `BevelIndent`,
`BevelType`, `BevelContrast`, `BevelLightAngle`, `BevelLightTilt`,
`ClipRegion`, `ClipView` (§4.8, containers and effects). Phase 3 wires them up.

Two things Phase 3 was asked to check against the corpus, **now answered**
(`the_two_attribute_questions_phase_two_left_open` in
`crates/xarast-xar/tests/corpus.rs`):

- **`ENDCAP` never disagrees with `STARTCAP`.** 26 257 start/end pairs in the
  corpus, zero of them different. One cap slot is enough, and the test fails
  if a future file proves otherwise rather than silently losing the end cap.
- **The overprint pair 3500–3505 cannot be answered from this corpus**: not
  one of those records occurs in any of the 59 files. Leave it open until a
  file that uses them turns up; nothing renders them yet.

## Round 3 additions (phase 7, 2026-09-23)

- **Change journal.** `Tree` keeps `ChangeLog { changes: Vec<TreeChange
  { node, parent }>, overflowed }`, appended by every `Action::apply`
  (forward, undo, redo, rollback), never by the builder. A new tree starts
  overflowed; > 2^16 entries or a resource change overflow it.
  `drain_changes()` is not an edit (digest unchanged).
- **`resolve_inherited(tree, id, defaults)`**: the attribute state just
  before `id` (without its own children), returned as an open `AttrStack`.
- **`FillGeometry::for_each_value_mut` / `bitmap_mut`** for remapping
  colours and bitmaps between documents.
- **`Tx::move_node` charges `size_of::<Action>()`**, not the subtree: a
  move retains nothing.

## Transparency families in the model (XARA-T-0347, phase 13 I1, 2026-10-16)

- The phase's `Transparency { family, source }` is the existing
  `TranspPaint = FillGeometry<Transparency>`: the family is the
  `TranspMode` its stops carry (now all twelve: `TranspMode::Bevel = 34`
  added, `TranspMode::ALL`), the source is the geometry.
  `TranspPaint::family()` (the first stop's mode; a bitmap without a level
  pair is Mix) and `TranspPaint::source()` → `fill::TranspSource::{Flat,
  Gradient, Image}` (meshes are Gradient, fractal/noise tiles Image) name
  them. No parallel type: two representations would have to be kept in
  step by every command.
- **Fixed:** `SetTranspMode` on a bitmap transparency without a level
  pair did nothing (there was no stop to hold the family). It now writes
  the implied pair (0 at black, 255 at white) in the new family.

## Replacing a bitmap resource (XARA-T-0290, 2026-10-16)

`Action::ReplaceBitmap { id, new: Arc<BitmapResource> }` /
`Tx::replace_bitmap` / command `ReplaceBitmap`: the whole resource behind
a `BitmapId` swaps, inverse = the resource as it was. Invariants: the id
stays valid for every reference (nothing in the tree changes); only the
nodes that name the id get a new content revision (not
`touch_resources`, which would move every node's scope — see
`app-core.md` "Replace and Save a copy"); the deduplication index follows
but `tables_rev` does not move (the journal records the action, not a
table). If the new content equals another bitmap's, both keep their ids
and `bitmap_key` of the later one falls back to hashing (the gallery does
this already). Journalled as `Act::ReplaceBitmap`.

## The operation journal's model half (XARA-T-0217, F6.6, 2026-10-16)

Module `journal` (`mod.rs`, `base.rs`, `blob.rs`, `ndjson.rs`); the I/O
and the application side are `app-core.md` "Operation journal".

- **Level: transactions, not commands.** An entry (`journal::Op::Tx`)
  is what a committed `Tx` did — its `Action`s and node creations, its
  label, its coalescing key and whether the live history merged it —
  not the `EditCommand` that asked for it. Why: (1) exactness — a
  command's effect depends on fonts (text layout), regeneration code
  that changes between versions, and session state (current
  attributes, selection); the actions do not; (2) coverage — ten
  `Action` variants plus `create` cover every one of the 40+ `Command`
  impls in two crates, present and future, through one hook
  (`CommandBus::dispatch`); (3) replay goes through the same `Tx` and
  `History`, so undo/redo after a recovery work. Cost: entries are
  bigger than commands; the clone happens on the interface thread.
- **Nodes are named by `Tag`**, never `NodeId`. Tags are unique per
  document, survive `restore` and the base, and `Step::Create` records
  the tag the live node got; replay forces it (`Tx::create_tagged`,
  refusing a tag in use or 0). `next_tag` divergence is harmless
  (every journalled tag is forced; the digest excludes tags).
- **Recording** lives in `Tx` (`begin_recording`, `commit_recorded`):
  `act_costing` converts each action before applying it (`act_of`; an
  unknown node loses the record → a barrier), `create` clones the
  payload. `Tx::transform` records **one** `Act::TransformTree` instead
  of one `Transform` per node (replay expands it over the same subtree's
  preorder) — before this a nudge of ProbeX16's top group was an 11 MB
  line. The retained cost (`bytes`) is recorded and charged on replay.
- **The bus's `Recorder`** counts the undo steps a replay of the journal
  would have (`past`, `future`, `held`). An undo with `past == 0` or a
  redo with `future == 0` would reach a step the base already holds:
  it records `Op::Barrier` and suspends the recorder until
  `start_journal` (a new base). A commit that merged into a pre-base
  step records `merged: true` and does not count; replay
  (`History::commit_as`) merges exactly where the live history did, or
  pushes a step of its own when there is nothing to merge into.
  `hold_redo`/`restore_held_redo`/`release_held_redo`/`discard_redo`/
  `clear_history` are journalled **only through the bus methods** — a
  caller using `history_mut()` directly bypasses the journal (only tests
  do now; `colour_editor` was moved to the bus methods).
- **Resource tables outside transactions** (`insert_bitmap` before a
  place, `import_bitmaps` before a paste, the gallery's
  `remove_unused_bitmap`, dashes, arrows, fonts) bump
  `DocumentResources::tables_rev`; the recorder emits `Op::Tables` (the
  whole bitmap/dash/arrow/font tables, keys and all) before the next
  journalled operation or on `sync_journal`. Whole tables, because
  slotmap keys cannot be chosen on insert and a deserialised slotmap's
  free list is in a different order than the live one's: replaying an
  insert could hand out a different `BitmapId`. The colour table needs
  nothing: it only changes through `Action::SetPalette`, which carries
  the whole table.
- **The base** (`journal::base`): a `.xarast` round trip is equivalent,
  not identical — measured over the corpus, 58 of 59 documents come back
  with a different canonical digest after their first round trip, and
  all 59 are fixed points after it (attributes are hoisted and
  regrouped). A journal names nodes, so it is replayed onto an exact
  copy instead: `Base` serialises a `Snapshot` (preorder, parent index,
  tag, flags minus `DETACHED`/`MARKED`, kind, foreign baggage; colours
  and tables with their keys; defaults; meta), `BaseDoc::into_document`
  rebuilds it (refusing a parent after its child, a repeated tag, tag 0
  off the root).
- **Serde everywhere** (commit 7d3b83f): every `NodeKind` payload,
  `AttrValue`, fills, live parameters, text types derive
  `Serialize`/`Deserialize`. Hand-written: `Path` (verbs as letters,
  points as one flat millipoint array, structural checks only — extent
  and subpath rules are *not* checked so that any live path replays),
  `ColourTable` (keys + the name index as it stands: a repeated name's
  winner is not derivable), `DefaultAttrs` through `set` per value.
  Large `Arc<[u8]>` (pixels, encoded originals, font data, opaque and
  effect payloads) go through `journal::blob`: under 4 KiB inline hex,
  otherwise a SHA-256 reference into the `BlobStore` installed with
  `with_blobs` (thread-local; checked against the key on the way in).
  `serde_json` has `float_roundtrip`; NaN/infinity serialise as `null`
  and do not read back, so such an entry is refused at write time and
  becomes a barrier.
- **Lines** (`journal::ndjson`): header
  `{"journal":"xarast-journal/1","stream":…,"base":S,"sum":…}`, entries
  `{"seq":N,"ts":"…Z","op":"tx","payload":{…},"sum":"8 hex"}` (`Op` is
  adjacently tagged so the line reads as `research/06 §10.3` spells it;
  `sum` = first 4 bytes of SHA-256 of the line before `,"sum"`).
  `read` never panics, stops at the first torn (no newline), corrupt
  (sum, JSON, unknown op) or out-of-sequence line, and reports
  `sound_len` (where a continuing writer truncates).
- **Tests**: `tests/journal.rs` (a property test of 96 cases × ≤ 80
  operations — edits, undo, redo, gestures, held redo, bitmap inserts and
  sweeps, rebases — through JSON onto a base read back from JSON:
  digest-equal, and undoing the local steps walks the same states; a
  fixed sequence that must produce merged, tables and blob lines; the
  barrier; hostile bases/entries are errors), `ndjson` tests (torn,
  flipped byte, gap, first entry not following the base, junk headers,
  NaN, timestamps, two property tests over hostile and damaged bytes),
  `blob` tests. Fuzz target `fuzz_journal_read` (5 min, 3.4 M runs,
  6 473 edges, no finding).
- **Invariants.** (1) Every change to a document goes through
  `Tx::act_costing` or `Tx::create`, or bumps `tables_rev` — a new `Tx`
  helper that writes `self.doc` directly is invisible to the journal and
  recovery silently diverges. (2) Nothing in a `NodeKind` payload may
  hold a `NodeId` (it would be serialised raw and mean nothing after a
  recovery); cross-references are tags. (3) A new `History` operation
  that changes `past`/`future`/the held branch needs a bus wrapper that
  records it and keeps the `Recorder` counts in step.
- **Dead ends.** Replaying onto the reopened `.xarast` snapshot with a
  NodeId → preorder-position map: the structure differs after a first
  round trip (attribute hoisting), so attribute nodes have no
  counterpart. Recording one `Transform` per node of a moved subtree:
  11 MB per nudge on ProbeX16, 63 s to drain 2 000 of them.

## Invariants that must not be broken

The twelve from `research/02 §10.16`, each with a matching `Invariant` variant
and a unit test that corrupts exactly it:

1. The tree is acyclic. → `Cycle`
2. `next`/`prev` are reciprocal; every child points at the same parent;
   `first_child` has no `prev` and `last_child` has no `next`.
   → `BrokenSiblingLink`, `ChildParentMismatch`
3. `DETACHED` is transitive downward: nothing reachable from the root carries
   the flag. → `DetachedReachable`
4. Every `LiveRole::Generated` node has a `Controller` ancestor of the same
   `LiveKind`. → `GeneratedWithoutController`
5. Attribute nodes come before the first ink node in a child list.
   **Warning, never an error**: real `.xar` files violate it, and
   `DocumentBuilder::finish` accepts them. → `AttrAfterInk`
6. One spread, exactly one active layer. → `NoActiveLayer`
7. `Tag` is unique; `by_tag` is bijective with the arena. → `DuplicateTag`
8. An invalid bounding box propagates upward. → `StaleBounds`
9. Every referenced resource exists. → `MissingResource`
    (`Document::validate` only; `Tree::validate` cannot see the tables).
    A missing bitmap, dash or arrow is an **error**; a missing **colour** is
    a **warning**, because a colour reference resolves through the table's
    fallback and `DocumentBuilder::finish` deliberately keeps such nodes
    (`is_hard_ref`). The two used to disagree — see below.
10. The selection contains only reachable nodes — **not this crate's problem**,
    because the selection is not in this crate. Recorded so the numbering
    matches the research.
11. `TextItem` only under `TextLine`; `TextLine` only under `TextStory`.
    → `TextItemOutsideLine`
12. Every controller has exactly one source subtree.
    → `ControllerWithoutSource`

Plus, from this phase:

13. **`size_of::<NodeData>() <= 64`**, and it is exactly 64 today. When the
    gate fails the fix is to box a variant, never to raise the limit.
14. **`size_of::<Option<NodeId>>() == size_of::<NodeId>()`** — the `Links`
    design assumes the niche.
15. **Mutation only through `DocumentBuilder` and `CommandBus`.**
16. **`Action::inverse` is computed before `apply`.**
17. **Selection, the caret and the insertion point never enter the tree.**
18. **Coordinates are `Mp` everywhere in the model.** The only floats are
    colour components, ramp positions, matrix scale/shear and the handful of
    angle and ratio parameters on live effects.
19. **Invariant 6 is a command-level invariant, not an arena-level one.**
    Raw tree surgery may break it; a `Tx` may not leave it broken.
20. **Foreign baggage exists only on live nodes and is never empty**:
    `destroy_subtree` removes it, and `Tree::set_foreign` stores empty
    baggage as none. Pinned by `tests::foreign`.

## Fixed defects worth remembering

**Invariant repair must run at commit, not per operation.** `Tx::attach`,
`delete`, `move_node` and `set_kind` each used to call
`keep_one_active_layer` immediately. That looks safer and is a trap: moving
the active layer takes two mutations, and the intermediate state necessarily
has zero or two active layers. The per-operation repair saw that intermediate
state, picked the first layer, and silently reverted the caller — so **no pair
of operations could ever move the active layer**, and the failure was silent.
Found by the phase 5 app-core work, which had to route around it.

The repair now runs once in `Tx::commit`, still inside the transaction, so its
own actions are recorded and undo stays exact. Pinned by
`tests::undo::the_active_layer_can_be_moved_within_one_transaction`, which was
confirmed to fail against the old behaviour before being committed.

**`Tx::commit` is O(what changed), not O(document)** (XARA-T-0030,
2026-09-23). `Tx::act` records the spreads each action can affect, and the
commit repairs only those:

- a detach: the old parent, read *before* the action applies;
- an attach: the new parent, plus every spread inside the attached
  subtree, read *after* it applies;
- a `SetKind`: the parent spread when the old or the new kind is a layer,
  and the node itself when it becomes a spread;
- a `Batch` holding any attach, detach or set-kind: a full rescan, because
  its sub-actions' pre-states depend on each other.

Transforms, flags, attributes and resources record nothing. The argument
for correctness is that every document a `Tx` sees starts valid: the
builder guarantees it and every commit preserves it. So a spread nobody
touched is still valid. The subtree walk on attach is not optional. A
spread broken *while detached* is unreachable at that commit and is not
repaired then, so it must be caught when it comes back.
`a_spread_broken_while_detached_is_repaired_when_it_comes_back` pins this.
The walk costs no more than `Tree::attach`'s own height check. The undo
property test now also moves nodes, flips `active` and attaches spreads
that have two active layers. Each tracking path was confirmed to fail it
when disabled. `dispatch/single_node_edit`: 1.96 ms → 101 ns at 100k nodes.

The general rule this establishes: **a transaction is allowed to pass through
an invalid intermediate state — that is what transactions are for.** Repairs
and invariant checks belong at the boundary, never between two mutations that
are meant to be one change.

**`fuzz_doc_builder` broke "valid or nothing" twice in its first minutes**
(2026-09-23), both as `BuildError::Inconsistent` on the builder's own output:

1. *A dangling colour reference.* `repair_missing_resources` keeps nodes
   whose only missing resource is a colour (`is_hard_ref`), but
   `validate_document` called every missing resource an error. Now a
   colour is a warning. Pinned by
   `a_dangling_colour_reference_is_kept_and_is_only_a_warning`.
2. *A sourceless live controller at the depth limit.* `repair_live` gives
   it an empty source child, but the tree refused the attach as too deep,
   and `let _ =` threw the error away. Such a controller is now dropped
   with a `Repaired` diagnostic. Pinned by
   `a_sourceless_controller_at_the_depth_limit_is_dropped`.

The lesson: **a repair that can fail must check its own result.** Every
`let _ = tree.attach(…)` in a repair path is a latent `Inconsistent`.
Clean rerun afterwards: 10 minutes, 3.8 M execs at ~6 300 exec/s, no
findings.

**Dropping the redo branch destroyed nodes inside a subtree the commit had
just deleted** (XARA-T-0295, 2026-09-24). `History::commit` drops the
future *after* the new transaction's actions ran; `reap` destroyed every
retained node that was unreachable. A redo transaction retains the nodes
it moves (an undone Enter retains the paragraph break it moved to the new
line); when the commit deletes their story, those nodes are unreachable
yet still inside the subtree the commit retains for its own undo, and one
undo gave back a story missing its final line. `reap` now destroys only
**detached roots** (unreachable *and* parentless); a nested node goes when
the root it hangs from is reaped. Invariant: a retained node is destroyed
only as (part of) a detached root no live step retains. Pinned by
`text_edit::tests::deleting_a_story_after_undoing_a_break_keeps_it_whole_for_undo`
and `tests/text_tool.rs`
`deleting_a_story_after_undoing_a_break_undoes_to_the_whole_story`.

**The review of that fix found the real cause one level up** (XARA-T-0295
review, 2026-09-24): `History::undo`/`redo` computed a step's `retained`
from the list they *produced* (what redoing/undoing would detach, i.e.
nodes attached now) instead of the list they *applied*, and every step
retained each node it ever detached, moves included. Consequences, all
pinned in `tests::undo`: delete → undo → delete → undo lost the node (the
dropped redo-of-delete destroyed what the new delete retained); move →
undo → delete → undo lost it the same way; add → undo → commit leaked the
added node (the redo step retained nothing); delete → undo → redo → clear
leaked it. Now `retained` is **the detached roots the step's last
application left** (`detached_roots`/`keep_detached_roots`, applied in
`Tx::commit`, `undo`, `redo` and after a coalescing merge), so a detached
root is retained by exactly one step, the one that last detached it.
`reap`'s parent guard is now defence in depth (removing it no longer fails
anything). Invariant, checked after every step by
`tests::props::the_history_neither_leaks_nor_destroys_a_node_it_still_needs`
(random edits, undo, redo, clear, discard/hold/restore/release redo, small
budgets that evict): **arena = reachable ∪ subtrees of live steps'
retained roots**, every retained node exists, and every undo/redo lands on
the digest recorded for that state serial.

## Dead ends (do not retry)

- **`Box<dyn Node>` + `Any`.** Reproduces the original's constant downcasting
  and makes exhaustive `match` impossible.
- **`Rc<RefCell<Node>>`.** `BorrowMut` panics in traversals that go both down
  and up, allows parent/child cycles, and is not `Send`.
- **A per-node attribute map with no attribute nodes.** Loses list scope and
  breaks round-tripping with both `.xar` and SVG.
- **A global `Epoch` for bounds invalidation.** Invalidates everything on every
  edit. Use the per-node flag with upward propagation and early cut-off.
- **Putting the caret, the selection or the insertion point in the tree.**
- **A persistent HAMT as the *live* store.** Measured in this phase: 11× on
  traversal, 15× on lookup, 7× on edit-and-undo, all against it. Closed.
- **`BoundsCache` as a field of `NodeData`.** It does not fit under the 64-byte
  gate alongside five links and a boxed kind, and traversal does not want it in
  the cache line anyway.
- **Automatic checkpointing on the edit path.** Measured at ~1.5 ms amortised
  per edit at a cadence of 64. Checkpoints belong to autosave.
- **Taking a `ResolvedAttrs` snapshot per node in a whole-document pass.** 46
  `Arc` clones per node; `AttrStack::stroke_extent()` is what a bounding box
  needs.
- **Letting `Tree::detach` fix the active layer for itself.** An automatic
  change the undo log never saw breaks byte-identical undo. Policy belongs to
  commands.
- **Building checkpoints as an `imbl::HashMap<NodeId, Arc<NodeData>>` from
  scratch.** Nothing used the structural sharing, and every rebuild paid a
  HAMT insert and an `Arc` allocation per node: 43–80 ms at 100k nodes. A
  dense `SecondaryMap` costs 9.7 ms. A HAMT only pays off once checkpoints
  are incremental, and that needs dirty tracking in the tree first.
- **Walking the whole tree in each `DocumentBuilder::finish` repair.** The
  builder now records its candidates at `node()` time: spreads, hard
  resource references, and whether any text or live node exists. Four
  preorders over 518k nodes cost ~70 ms.
- **Validating twice on import.** Use `finish_with_report()`.

## Open TODOs

- **Mould follow-ups (XARA-T-0334).** G9 is done (XARA-T-0342,
  `tools.md`; `mould::generate_preview` draws a dragged cage), and a
  moulded object is picked by its moulded outline; snapping and
  editing *inside* a mould still see the undeformed source
  (interaction must map the pointer through
  `MouldTransform::source_point`; not started in XARA-T-0337: the shape
  editor, its handles and snapping would all need the map and its
  inverse, and the source rectangle would have to follow an edit made
  inside); the original's 2×2 envelope smooths its inner lines (ours is
  the Coons patch of its piecewise sides; it cannot come from a
  `.xar`). Closed by XARA-T-0337: deep mould blend ends and contours
  around moulds (decision 54), shadows/bevels/feathers inside a mould
  (decision 54), "convert to shapes" (decision 55), fonts (the walker
  clears its `LiveCache` when its fonts change, `app-core.md`). Still
  open from the mould tool: paste any clipboard path as a mould,
  Remove+Constrain copying the cage, the 2×2 envelope preset; the SVG
  single-`transform` shortcut for an affine perspective
  (`research/06 §6.8.5`, a MAY) is not built: the source is already
  written once inside `<xarast:mould-source>` with its ids, so a second
  copy under a `transform` would duplicate ids, and a `<use>` of an
  element inside a foreign element is not known to render in Inkscape.
- **Converting shadows and bevels to shapes** (decision 55): the
  original makes a bitmap-filled path; needs the renderer's offscreen
  result as a `BitmapResource`.
- **Contours of nested blends and contours**: `contour::inks` leaves
  nested rings and steps out of a contour's silhouette; the original's
  conversion passes them back (`Kernel/ncntrcnt.cpp:520-600`).
- **Blend follow-ups (XARA-T-0333).** F6 (along a path: the record
  family is decoded and reported, the path dropped; none in the corpus;
  bounds would then need the path); blends whose ends are shadows or
  other live objects (moulds, at the top of an end or below it, are
  blended from their copy: decision 54) — the original blends what
  those *draw*; text ends (not
  outlined in the document); nested blends blend only their own ends;
  `step_distance` is kept but not used to derive the count; a fill
  family mismatch picks instead of blending the original's way (start's
  fill type, colours blended); the F2 rule in the phase plan ("drop the
  smallest redundant subpaths") is not the original's (it pairs every
  subpath with the stepper and strips only blend-path-derived ones), so
  it was not implemented; the 12-pair VM comparison of the matching rule
  was not run (`geometry.md`, "Blends"). F8, the tool.
- **Contour follow-ups (XARA-T-0330).** Exact controller bounds from the
  regenerated rings (today `growth()` is ×11.47 for mitred corners, which
  over-sizes culling, damage and "fit drawing"); text sources are not
  contoured (their outlines come from the app's layout); a nested live
  object's generated output is left out of the silhouette; the inset
  path hides its source in the walker but not in the SVG bake; the
  contour tool (E6). `RegenQueue` is still unused (the walker keys the
  cache instead): wire it if a consumer needs `flush_urgent`.
- [x] **Per-node foreign-baggage container (XARA-T-0089, phase 6 risk K1).**
      Done 2026-09-23 (8b0834d): decision 32, invariant 20. Automatic
      dirty/stale marking of edited nodes: done in W4 (decision 32);
      deletion accounting for the save warning is XARA-T-0113.
- [x] Benchmark arena vs `imbl` over a 100 000-node traversal. **Done; the
      arena wins; `10-architecture.md` §3.1 and §7 updated.**
- [x] Settle the exact `AttrSlot` set against the `.xar` tags. **Done; 46,
      unchanged; the table is `attr/tags.rs`.**
- [x] `Tree::validate()` plus `proptest` property tests over attach/detach/move
      sequences. **Done, before the importer.**
- [x] Size test `size_of::<NodeData>() <= 64`. **Done; it is exactly 64.**
- [x] Confirm whether `ENDCAP` (175) ever disagrees with `STARTCAP` (174)
      against the corpus. **Done: 26 257 pairs, zero disagreements.**
- [ ] The 3500–3505 overprint on/off pairing: **unanswerable from this
      corpus**, which contains none of those records.
- [x] Decide whether blend intermediate steps are materialised as nodes or
      generated at render time. **Generated** (decision 45).
- [x] `ProceduralSource` and its cache hash. **Done**: `cache_key()`
      (decision 47).
- [x] `MouldGeometry` as trait or enum. **Decided**: a trait over a closed
      enum (decision 46); built by XARA-US-0071.
- [ ] Wire `RegenQueue` + `LiveCache` into the session (flush before
      `rebuild_scene`, `mark_changes` beside the picker's `note_changes`)
      and teach the walker to draw `LiveOutput::Shapes`, with the first
      geometry generator (XARA-T-0316).
- [ ] Incremental checkpoints: share structure with the previous snapshot
      instead of rebuilding, and drive them from the autosave timer (Phase 6).
      Until then `History::set_checkpoint_cadence` is opt-in.
- [ ] `AttrResolver` invalidation tuning. Phase 2 ships the conservative
      policy: **any** change drops the whole cache. Correct, and slow on a
      large document mid-drag (Phase 4).
- [ ] A payload-free "bytes per node" measurement, to judge the 160 B budget
      properly.
- [ ] Persistent history across save and reload (Phase 6).
- [ ] `ndoptmz.cpp`'s normalisation — `make_self_contained`, `strip_redundant`,
      `factor_out`, `localise` — at the four boundaries `research/02 §10.6`
      identifies. Not needed until copy/paste and grouping exist (Phase 7) and
      saving does (Phase 6).
- [x] A `cargo-fuzz` target for `DocumentBuilder`: `fuzz/fuzz_targets/
      fuzz_doc_builder.rs` (2026-09-23), nightly in CI. It drives every node
      kind, unbalanced scopes, and colour/bitmap keys forged with
      `slotmap::KeyData::from_ffi`, and treats `Inconsistent` as a finding.
- [x] **`Tx::commit` is O(document size).** Fixed 2026-09-23 (XARA-T-0030):
      the commit checks only the spreads the transaction touched. See
      "Fixed defects worth remembering".
- [x] **`snapshot()` over budget.** Fixed 2026-09-23: 43–80 ms → 9.7 ms,
      with a dense map instead of a HAMT.
- [ ] Incremental checkpoints would need dirty tracking in `Tree`. Revisit
      the snapshot representation (HAMT or chunked) only then.
- [x] **The default join differs from the original's.** Done
      (XARA-T-0329, 2026-10-15): `default_for(JoinType)` is `Join::Bevel`.
      Evidence: the original registers `BevelledJoin` as the document
      default (`Kernel/attr.cpp:1967-1980`, constructor `:1927-1931`); the
      default attributes are nodes at the head of the tree that the
      exporter skips ("all default attributes are assumed in the format",
      `Kernel/camfiltr.cpp:4221-4255`), so an object that strokes bevelled
      carries no join record. The corpus agrees once the records are read
      as the original's enums (`xar-import.md`, "Caps and joins"): 38 722
      mitre and 27 202 round records, **no bevelled one**. Strokes and the 21 bevel controllers with no join in force now
      join bevelled. Mitre limit: the default is 4000 (`Kernel/attr.cpp:
      1735`, `:1788`), as Xarast's, and no corpus file writes one.
      Measured: no golden moved; preview scores (T-0248 method, 60 dpi)
      move by ≤ 0.02 (the ≤ 128 px previews cannot see a corner: GardenPlan
      4.77 → 4.75, SoftShadow 10.51 → 10.49, ProbeX16 8.96 → 8.97, others
      unchanged); the corpus export check moves ≤ 0.12 (Broken Butt Cap pdf
      0.97 → 0.85) and no limit. `.xarast`: see `xarast-format.md`.
- [x] **The default fill is "no colour"** (XARA-T-0204, 2026-10-16):
      `default_for(FillGeometry)` is a flat `Colour::None`
      (`research/02 §4.4`), not transparent white; it paints the same.
      `Colour::None` is a variant of its own (`colour.md` decision 33) and
      hashes as tag 2 in the canonical digest.
- [x] **The default winding rule differs from the original's too.** Done
      (XARA-T-0331, 2026-10-15): `default_for(WindingRule)` is
      `FillRule::EvenOdd`, and `TAG_WINDINGRULE` is read as the
      original's enum. Evidence: the original registers `EvenOddWinding`
      as the document default (`Kernel/attr.cpp:1856-1858`, constructor
      `:1802`) and the exporter never writes defaults; the record is the
      raw `WindingType` byte (0 non-zero, 1 negative, 2 even-odd,
      3 positive; `Kernel/attr.h:169-175`, `Kernel/lineattr.cpp:3506`,
      `:4276`). Corpus: three records, all 1 (negative), on three
      quadrilaterals of `scope3 simple`; no 2.
      **Negative and positive draw as non-zero**, exactly: every renderer
      of the original hands the rasteriser one flag, even-odd or not
      (`wxOil/grndrgn.cpp:2233-2234`, `:2769`, `:3798`, clip regions
      `:7114`; `wxOil/osrndrgn.cpp:1318-1321`). `FillRule::as_drawn`
      says so; the CPU renderer, the PDF writer (it used to rasterise
      them), picking, the walker's outside clip and the importer's
      keyhole union use it. `FillRule::covers` keeps the strict sign
      meaning for geometry that asks for it (`xarast-geom` hit tests,
      booleans). The model keeps all four values (they round-trip).
      Where the original itself forces a rule (stroke shapes and brushes
      fill their outline non-zero, `Kernel/ppstroke.cpp:292`,
      `ppvecstr.cpp:527`, `ppairbsh.cpp:539`) Xarast has no such outline
      fill yet; whoever adds one must set the rule there, not rely on the
      default. Contour rings: the original fills each under a negative
      rule whatever is in force (`Kernel/nodecntr.cpp:605-609`), so the
      walker's `paint_contour` pushes `Negative` per ring, and
      `contour::silhouette` resolves the source under its rule as drawn.
      Text glyph runs still fill non-zero in the walker, as before.
      Measured against a1bc8ec (contours drawn; export 72 dpi, previews
      by the T-0248 registration): 9 of 59 PNGs change, no contour ring
      does; preview |Δ| SimpleSphere 16.52 → **7.65** (its globe is a
      wire frame, as the preview shows), Girard_simple 14.34 → **4.70**,
      scope3 simple 25.16 → **16.07** (wood table, sky),
      BLUECAR 6.24 → 5.58, GardenPlan 4.73 → 4.72, leafgirl 22.65 →
      22.67, JagSS100 / WATCH2 unchanged; **Watch4 15.66 → 16.55** (its
      wheels and rim open into rings — not attributed: it has 65 blends
      and 126 moulds the walker does not generate yet, Phase 13). No
      render golden moved (the fill-rule goldens draw the sign rules
      non-zero, as before); export check 118 comparisons, 0 failures, no
      limit moved (largest moves: scope3 svg 1.26 → 2.23, Girard pdf
      1.09 → 1.67, WATCH2 pdf 2.37 → 2.69; Watch4 svg 1.13 → 0.75). `.xarast`: see `xarast-format.md`.
