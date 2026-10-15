# colour

Memory note for **colour models, the palette and palette/fill editing** —
`xarast-color` (models, conversions, `ColourTable` editing) and the phase-8
command modules of `xarast-doc` (`palette`, `fill_edit`). Phase spec:
[`../phases/phase-08-colour-fills-transparency.md`](../phases/phase-08-colour-fills-transparency.md)
§W8.1, §W8.2. Facts about the original: `research/02 §5.10.1` (added in this
phase). Phase-1 colour decisions (naive CMYK, the inherit sentinel, the
depth limit) stay in [`geometry.md`](geometry.md).

## Current state (2026-09-24, XARA-US-0037 / 0038 / 0041 / 0042)

| Piece | Where | State |
|---|---|---|
| Conversions matching the original (CMYK black generation, grey weights, HSV grey threshold) | `xarast-color/src/model.rs` | done |
| `pack_component` / `to_rgba8_packed` — the original's 8-bit packing | `model.rs` | done |
| `tinted` (per model), `shaded` (signed, in HSV) | `model.rs` | done |
| `ColourKind::from_raw` (signed shade), used by the `.xar` reader | `table.rs`, `xarast-xar/src/colour.rs` | done |
| `ColourContext` (`srgb_of`, `convert`, `resolve`) | `context.rs` | done, holds nothing yet |
| `ColourTable` editing: `redefine`, `rename`, `reparent`, `remove(OnDelete)`, `refresh_from`, `refresh_all`, `repair_cycles`, `resolve_order`, `descendants_of`, `children`, `epoch`, `advance_epoch_past` | `table/edit.rs` | done |
| `Action::SetPalette`, `EditError::{Palette, FillEdit}` | `xarast-doc/src/history.rs` | done |
| Palette commands `CreateColour`, `RedefineColour`, `RenameColour`, `ReparentColour`, `DeleteColour` | `xarast-doc/src/palette.rs` | done |
| `ColourUses` (rebuild-only), `PaletteResolver`, `palette_refs`, `for_each_colour`, `map_colours`, `changed_between`, `Document::palette_epoch` | `palette.rs` | done |
| Palette in `canonical_digest` | `document.rs` → `palette::digest_palette` | done |
| Palette loops broken at build (`repair_cycles` + `Repaired` diagnostic) | `builder.rs` | done |
| Fill commands `SetFillGeometry`, `MoveFillControl`, `InsertStop`, `MoveStop`, `RemoveStop`, `SetStopValue`, `SetFillProfile`, `SetRampMapping`, `SetFillEffect`, `SetTiling`, `SetTranspMode` | `xarast-doc/src/fill_edit.rs` | done |
| Pure helpers `move_control`, `set_stop`, `stop_value`, `ramp_insert`, `ramp_move`, `rebuild_ramp`, `fill_arm`, `arm_position`, `fill_in_force`, `set_own_attr` | `fill_edit.rs` | done |
| `MutateFill`, `mutate_fill` (the W8.2 mapping table), `FillShape`, `MutationLoss` | `xarast-doc/src/fill_mutate.rs` | done (XARA-US-0039, for the type menu) |
| Group transparency (T8.2.7), "no colour" value (T8.1.5), `.xarast` palette round trip (T8.1.7) | — | **not done**, filed |
| App `EditCommand::Fill` for the fill commands | `xarast-app/src/ops.rs`, `fill_tool.rs` | done (XARA-US-0039) |
| Walker using `PaletteResolver` + epoch in cache keys | `xarast-app` | **not done**, filed (XARA-T-0203) |
| `EditCommand::Palette(PaletteCommand)` — `Create`, `Redefine`, `Rename`, `Derive` (reparent + link mask in one step), `Delete` | `xarast-app/src/colour_editor.rs`, `ops.rs` | done (XARA-US-0041) |
| Colour editor model: `ColourEditorModel`, `ColourEditorView`, `ColourEditorOp`, `Intent::ColourEditor` | `xarast-app/src/colour_editor.rs` | done (XARA-US-0041) |
| Colour editor UI: 2D field + slider, numeric entry, derivation editor, Redefine / Apply | `xarast-ui/src/colour_field.rs`, `panels/colour.rs` | done (XARA-US-0041) |
| `History::discard_redo` | `xarast-doc/src/history.rs` | done (for the editor's Esc) |
| `History::{hold_redo, restore_held_redo, release_held_redo}` | `xarast-doc/src/history.rs` | done (Esc brings back the redo branch) |
| Colour-line order: `ColourTable::{listed, next_entry_index, move_listed}`, `MoveColour`, `PaletteCommand::Move` | `xarast-color/src/table/edit.rs`, `xarast-doc/src/palette.rs`, `colour_editor.rs` | done (XARA-US-0042) |
| Colour bar / gallery model: `ColourBarView`, `Swatch`, `ColourSource`, `ColourBarOp`, `DragPoint`, `DropKind`, `Intent::ColourBar`, `selected_stop` | `xarast-app/src/colour_bar.rs` | done (XARA-US-0042) |
| Drop picking `Picker::pick_drop`; `Tool::fill_selection` / `FillSelection` | `xarast-app/src/picking.rs`, `tool.rs`, `fill_tool.rs` | done (XARA-US-0042) |
| Colour editor edits the fill tool's selected stop (T-0247) | `colour_editor.rs` via `colour_bar::selected_stop` | done |
| Colour bar widget, colour gallery panel | `xarast-ui/src/colour_bar.rs`, `panels/gallery.rs` | done (XARA-US-0042) |
| Eyedropper (T8.7.6) | — | **not done**, XARA-T-0253 |

Tests (XARA-US-0042): `xarast-app/tests/colour_bar.rs` (13: the bar's
order; click = fill / right = line, one step each, undo by digest; a
click flattens a gradient except onto the fill tool's selected stop;
nothing selected = current attribute; a named swatch is a live
reference; **criterion 10** — a colour dropped on an intermediate stop
changes that stop only, whole `FillGeometry` compared; a drop on the arm
adds a stop; an empty interior takes a drop, Shift over the outline sets
the line; a gradient says "flat colour" and flattens; nothing / cancel
leave digest and history; reorder by drop and by menu, a move to where it
is records no step, a direct colour redefines a plain named colour but
not a tint; gallery rename and delete (objects keep their look, the
editor leaves the deleted entry); the editor edits the fill tool's end
blob only, one step), `xarast-doc/tests/fill_palette.rs`
(`colours_join_the_end_of_the_line_and_move_along_it`), `xarast-ui`
`colour_bar` (5) and `panels::gallery` (1) unit tests, the
accessibility test `the_colour_bar_and_the_gallery_name_every_swatch_with_its_value`,
the shell's `colour_drags_resolve_against_the_canvas_and_speak_in_the_status_line`.
Bench: `cargo bench -p xarast-app --bench pick -- "colour drag"` — two
moves over the 100k-object synthetic document **12.5 µs** (≈ 6 µs a
move; budget 1 ms).

Tests: `xarast-app/tests/colour_editor.rs` (14: a 60-event drag is one
step and undoes by digest; Esc mid-drag leaves digest, labels, serial and
redo list untouched; Esc after an undo brings the redo branch back (redo
re-applies it by digest), a committed drag drops it, a drag that changed
nothing keeps it; hue kept through S = 0; editing a linked object
unlinks only it and Redefine repaints every user; current attribute with
nothing selected; line target; tint drag = one "Link Colour" step, cycle
refused by digest; HSV link inheriting the hue follows the parent; new
named colour + rename; model change of an entry; undo mid-drag; locked
layer refused), `xarast-ui` `colour_field` (7) and `panels::colour` (8)
unit tests, `xarast-ui/tests/accessibility.rs` (colour editor named
controls, derivation editor, keyboard-only field operation),
`xarast-color/tests/palette.rs` (24, incl. `table::cycles` over 1 000
random graphs and the round-trip error table), `xarast-xar/tests/palette_corpus.rs`
(corpus oracle), `xarast-doc/tests/fill_palette.rs` (14, every command with
digest-exact undo/redo, a 60-event drag = one step, a 5 000-object
`ColourUses` dirty set, detach sweep, ramp-edit property test).

### Measured

**Conversion round trip** (`round_trip_error_table`, max |error| over a dense
grid, units of 1/255; the bound is 1 for RGB/HSV/grey, 2 through CMYK):

| from → via → from | error |
|---|---|
| RGB → HSV | 0.000091 |
| RGB → CMYK | 0 |
| HSV → RGB | 0 |
| HSV → CMYK | 0 |
| CMYK → RGB | 0 |
| CMYK → HSV | 0.000091 |
| grey → RGB / HSV / CMYK | 0.000030 |

RGB → grey → RGB is lossy by definition and not in the table. Through
CMYK only the colour channels are compared: CMYK has no transparency.

**Corpus oracle**: all **5 760** palette entries of the 59 files resolve to
their record's cached RGB **exactly** — normal CMYK 867, HSV 3 762, RGB 410,
grey 81; tints CMYK 231, HSV 300, grey 10; shades HSV 77, RGB 4; links 18.
Plain round-to-nearest would get **545** of them wrong.

**Benchmarks** (`cargo bench -p xarast-color --bench colour -- --quick`):
`redefine` on 256 entries with a 4-deep chain **0.94 µs** (budget 20 µs);
`ColourContext::convert` RGB→CMYK 6.5 ns, RGB→HSV 9.0 ns (budget 20 ns).

### W8.8 — fill import, round trip, corpus (XARA-US-0043)

| Piece | Where | State |
|---|---|---|
| T8.8.1 `.xar` fill records → model, one to one (corpus census, 355 831 fills) | `xarast-cli/tests/fills.rs`, `xar-import.md` finding 15 | done; fixed definitions lost inside dropped atomic subtrees |
| T8.8.2 `.xarast` fills: every shape × 7-stop ramp × profiles × sin × effects × mappings reload **equal** and the second save is byte-identical (criterion 9) | `xarast-format/tests/fill_round_trip.rs` | done; profiles are now written exactly and SVG decimals read correctly rounded |
| T8.8.3 baked ramp stops within 2/255 (measured 1.9) and the reader discards them | `svg/paint.rs` `bake`, same test | done |
| T8.8.4 `xarast-cli inspect --fills` | `xarast-cli/src/inspect.rs` | done |
| Conical/diamond/3-4-colour fills baked into SVG geometry (part of XARA-T-0102) | `xarast-format/src/svg/bake.rs` | done |

### Randomise (XARA-T-0336)

`RandomiseFill { node, slot, channel, seed }` (`fill_edit.rs`) sets the
seed of a fractal or noise fill and nothing else; any other fill refuses
it (`EditError::FillEdit`). The caller draws the seed, so the command
replays exactly. No UI yet (H7).

## Decisions taken (and why)

1. **Conversions follow the shipped (non-CMS) branch of the original.**
   LX defines `NO_XARACMS`. RGB → CMYK generates black past 50 %; the
   phase-1 note that "any forward transform other than `K = 0` loses
   colour" was wrong — this one is exactly invertible under the naive
   reverse (`(C − K) + K = C ≤ 1`).
2. **Grey is 0.305 / 0.586 / 0.109** (`GREY_MODEL_WEIGHTS`), the kernel's.
   Not the renderer's blend luminance, which is CDraw's and lives in
   `xarast-render` (`research/03 §2.10`). Two constants on purpose.
3. **Two quantisers.** `to_rgba8` rounds; `to_rgba8_packed` is the
   original's `+0x8000 … ×255 >> 24` and is what `cached_rgb`,
   `ColourContext::srgb_of` and `PaletteResolver` produce. `to_rgba8` was
   not changed, so no render golden moved; switching the walker is part of
   the filed wiring task.
4. **Tints apply in the child's model, shades in HSV, both after bringing
   the parent into the child's model.** The original skips the conversion
   for tints because a tint takes its parent's model when linked; converting
   is identical then and sensible if they drift. `reparent` to a tint or
   shade sets the model to the parent's, as the original does.
5. **A normal or spot colour ignores its `parent` field** in resolution, as
   the original's `GetSourceColour` does. `live_parent` (derived kind + the
   parent exists) is the one definition of "edge" everywhere: order,
   descendants, cycle checks.
6. **Shade coordinates are signed** and read with `ColourKind::from_raw`;
   `Fixed24::to_f32` clamps to `0..1` and would have turned every darkening
   shade into "no change".
7. **Palette edits swap the whole table** (`Action::SetPalette`). Undo of a
   delete brings the entry back under its **old `ColourId`** — impossible
   with per-entry re-insertion into a generational slot map. The clone is
   microseconds for a real palette.
8. **The epoch only moves forward**, undo included (`advance_epoch_past`):
   after undo + a different edit, a restored epoch number would otherwise
   name two different palettes.
9. **`reparent` is the only editing path to `kind`/`parent`.** It refuses
   `Cycle` (self or a descendant), `TooDeep` (any chain would reach
   `MAX_PARENT_DEPTH` ancestors), `MissingParent`, `UnexpectedParent`.
   Unlinking bakes the resolved value into the components (original
   behaviour); linking from another kind starts with all four components
   overriding. `set_parent` stays for loaders, followed by `repair_cycles`.
10. **`repair_cycles` demotes the youngest (highest slot) entry on a loop**
    to a normal colour holding what it resolves to now (its cached RGB), or
    the offending entry itself for a chain that is merely too deep. Runs in
    `DocumentBuilder::finish` with a `Repaired` warning. A file is never
    rejected for this.
11. **`refresh_from` reports `id` plus descendants whose packed value
    moved** — the repaint set — and rewrites their `cached_rgb`. Loaders do
    not refresh: the file's cached value is the fallback for unresolvable
    entries (and equals our resolution anyway, per the oracle).
12. **Delete policy**: `Reject` fails on any use (attribute, layer guide
    colour, guideline, derived colour); `Detach` (the original's forced
    delete) turns attribute uses into `Colour::Direct` of what they
    resolved to, local tint included, clears guide colours to the default,
    and makes derived entries normal. The sweep covers **every node in the
    arena**, reachable or retained by the history, so undoing an old
    deletion can never resurrect a dangling reference.
13. **Fill commands address `(node, PaintSlot, FillChannel)`** and edit the
    fill **in force**, writing it back as the node's own attribute (replace
    the attribute child of that slot, else add one first). Editing an
    inherited fill localises it; undo removes the added attribute.
    Effect and tiling are separate attributes, exactly as in the format.
14. **Coalescing is keyed by the drag**: `MoveFillControl`/`MoveStop` take
    `drag: Option<u64>` (the tool passes its bus gesture) and hash
    `(drag, node, slot, channel, handle)` into the `CoalesceKey`. Same
    handle, same drag → one step; the history only merges into its newest
    step, so anything in between splits it.
15. **Handle names per shape** are in `FillHandle`'s docs. A linear fill's
    third (skew) point is not in `FillGeometry::Linear`, so `End2` is
    refused there. Moving a centre moves the whole fill; an aspect-locked
    radial keeps its minor axis at 90° and equal length.
16. **`Ramp`'s stop list is private**, so the edit module rebuilds ramps
    through `Ramp::insert` (`rebuild_ramp`) instead of widening `fill.rs`.
    A moved stop re-sorts, after any stop at the same position;
    `ramp_move` returns the new index so a tool keeps hold of the stop.
17. **A transparency stop value is a level**; the stop keeps its mode.
    `SetTranspMode` rewrites every stop's mode.
17a. **`TranspMode` has ten modes, Hue = 31 included** (XARA-US-0018,
    2026-09-24). The original's transparency tool offers Mix … Luminosity
    and then Hue, and writes Hue as type byte 31 (`TT_HUE`,
    `Kernel/fillval.h:171`); without the variant `from_byte(31)` fell
    through to Mix and a Hue transparency rendered as a plain mix.
    `TRANSP_MODES` (the infobar menu) lists all ten, Hue last; the SVG
    profile writes `mix-blend-mode:hue` / `xarast:blend="hue"`. Bevel
    (34) stays renderer-internal: the tool never offers it. A mesh
    transparency's mode is `c0.mode`, and it now renders in that family
    (it was always Mix; see `render.md`, "Meshes tile mirrored").
18. **`mutate_fill` follows the W8.2 table**: flat → gradient takes the
    bounding box (diagonal for linear, inscribed circle otherwise) and a
    caller-given far value (`desaturated` for colour, `clear_end` = 255 for
    transparency); gradient ↔ gradient maps `start`/`centre` and
    `end`/`major`/`corner1`, the third point `start + perp(end − start)`
    unless the source had one; to three/four colour drops the ramp and
    seeds the extra colours from its middle stop or `to`. Bitmap, fractal
    and noise sources are refused. The fill commands derive `PartialEq`.

19. **The colour editor (W8.6) edits live and commits once.** A preview
    applies a real command inside a bus gesture opened by the first
    preview; they coalesce (`EditCommand::coalesces_with`: `Fill` with the
    same label and nodes, `Palette` of the same entry) into one step.
    Rejected: the fill tool's `Preview::attrs` route (decision 45 of
    `tools.md`) — a palette redefinition has no preview override in the
    walker, and the phase asks for the coalescing mechanism. **Esc** undoes
    the step (walks back to the serial recorded at the first preview) and
    `History::discard_redo` drops it: digest, labels and state serial
    return. The redo branch that existed before the drag comes back too
    (decision 24).
20. **Editor targets.** `Selection(PaintSlot)` edits each selected
    object's `StopTarget::From` via `SetStopValue` ("Set Fill Colour"):
    the colour of a flat fill, the start colour of a gradient — never
    flattening a gradient. With nothing selected it sets the current
    attribute (session state, not undoable; Esc restores it).
    `Entry(id)` redefines the palette entry in **its own model**: for an
    entry the display model *is* the definition's model, and switching
    the model tab converts the entry ("Edit Colour"); a tint's or shade's
    model is its parent's, so the tab is locked and the components are
    read-only (edit the derivation). A link's inherited components are
    read-only and stay `None` through a redefinition.
21. **Editing a linked object never redefines the named colour.** It
    writes a direct colour (the link breaks for those objects only).
    Redefining is the explicit switch of target to the entry
    (`ColourEditorView::linked_entry` names it); putting a named colour on
    the selection as a live reference is `ColourEditorOp::ApplyEntry`.
    These are T8.6.5's "two explicit actions".
22. **"HSV delta" is a link in HSV.** `Derivation::Linked { model,
    inherit }` = reparent to `Linked` then redefine with `None` in the
    inherited slots, in one step ("Link Colour"). An HSV link inheriting
    the hue follows the parent's hue with its own S/V; inheriting S/V
    gives a hue that is its own. No separate delta representation exists
    in the format, so none is invented.
23. **The editor remembers the components it showed** (`Shown`) while the
    document still resolves to exactly the value it wrote, so a hue
    survives a saturation dragged to zero; any other change (undo, another
    edit) and the view re-derives from the document.
24. **A drag sets the redo branch aside; it does not drop it.**
    `start_drag` calls `History::hold_redo`, which moves the redo steps
    and their serials out of `future` *without reaping* their retained
    nodes, so the drag's commits find nothing to drop. When the drag ends
    (`end_hold`): if the history is back at the serial the drag started
    from (Esc undid it all, or it never touched the document) the branch
    is restored (`restore_held_redo`); otherwise it is released
    (`release_held_redo` reaps it — exactly what the first commit would
    have done). Chosen over applying previews outside the history until
    release (the fill tool's route, `tools.md` decision 45) because a
    palette redefinition has no preview override in the walker; it is
    three small `History` methods and one call at each end of the drag.
    `settle` (an Undo/Redo mid-drag) goes through the same end, so redo
    after a drag that changed nothing still works. `History::clear` reaps
    a held branch too.

25. **The colour-line order is `entry_index`.** The file formats already
    carry it (`research/01 §9.2`: the entry's position in the document's
    colour list), so ordering needs no new table structure:
    `ColourTable::listed` sorts the **named** entries by `(entry_index,
    slot)`; `move_listed` renumbers them `1..=n`; `CreateColour` gives a
    named colour `next_entry_index` (the end of the line). Unnamed
    entries are local colours and never listed. A move to where the entry
    already is changes nothing — the app checks first, so no empty step is
    recorded (a doc command with no action would still commit one).
26. **What a swatch is.** "No colour", the named colours in line order,
    then 14 standard colours chosen for Xarast (`colour_bar::STANDARD`;
    not the original's template palette). A named swatch applies a live
    `Colour::Indexed` reference; a standard one a direct colour. "No
    colour" is `rgbt(0, 0, 0, 1)` — the importer's representation, which
    the renderer and the picker already treat as "paint nothing"
    (`colour_bar::no_colour`); XARA-T-0204 will replace it.
27. **A click follows the original's colour-change mutation**
    (`Kernel/fillattr.cpp`, `AttrColourChange::MutateFill`): with the fill
    tool's handle selected only that stop changes (`selected_stop`: tool in
    force = Fill, colour channel, an **interior** handle — an outline
    handle, XARA-T-0220, gives `None` — the set's nodes still selected, the
    stop exists); otherwise each selected object's fill (or line) is
    **replaced** by a flat colour — a gradient is flattened
    (`SetFillGeometry`, "Set Fill"). The colour *editor* never flattens
    (decision 20): it edits the start colour, or now the selected stop.
28. **A colour drag never touches the document before the drop.** The
    session holds `ColourDrag { source, point, target }`; `DragTo`
    re-resolves only when the point moved and reports `Changed::UI` only
    when the target changed; `DragDrop` resolves again at the last point
    (the document may have changed under a drag that sat still) and
    applies one command; `DragCancel` just forgets. No history hold is
    needed, unlike the editor (decision 24).
29. **Drop resolution order** (`phase-08 §W8.7`): fill-tool stop/end blob
    (`hit_sets` over `fill_sets::<ColourFill>`, 5 px) → arm (insert a stop
    at the arm parameter; graduated shapes only) → with Shift, the outline
    (`pick_drop`: the stroke's own half-width, or 3 px for thinner lines,
    via `HitTolerance::min_stroke_width = 6 px`) → the interior (fill;
    `FlattenFill` when the fill in force is not flat) → a named colour of
    the bar or gallery (a named source reorders to that slot; a direct one
    redefines a Normal/Spot entry in its own model; tints, shades, links
    and "no colour" are refused) → nothing. Without Shift an outline hit
    means the object's fill. Drops go to the **leaf** hit (the object
    under the pointer, inside its groups), and never change the selection.
30. **An interior painted with "no colour" still takes a drop.** The pick
    index's leaves keep `interior: Option<FillRule>` for every filled
    path, painted or not; `pick_drop` tests it, `pick` still does not
    (a transparent interior never selects, `tools.md` decision 25). A
    leaf with neither painted fill nor stroke is still not indexed, so a
    wholly invisible object inside a group cannot take a drop (a lone one
    is indexed by its box and can).
31. **Deleting from the gallery is `OnDelete::Detach`**: objects keep
    their look as direct colours, derived colours become normal. The
    editor moves off a deleted entry (`colour_editor::forget_entry`)
    first. No confirmation is asked (XARA-T-0255).
32. **The editor follows the fill tool's selected stop** (XARA-T-0247):
    `selection_colour` / `set_selection_colour` use `selected_stop` for
    the fill slot, and the title says which stop ("Fill end colour of 1
    object", "Fill stop 2 …", "Fill corner 3 …"). Edits coalesce as before
    (same label, same nodes).

## Invariants that must not be broken

1. Every `ColourValue` component is in `0..=1` and finite (phase 1); shade
   coordinates in `[-1, 1]`.
2. After any `ColourTable` edit, the derived graph is acyclic and no entry
   has `MAX_PARENT_DEPTH` or more ancestors (`table::cycles`).
3. After any edit, `cached_rgb == resolve(id).to_rgba8_packed()` for every
   entry the edit touched (`cached_values_stay_current`).
4. No table edit leaves a `parent` pointing at a removed slot.
5. The epoch is strictly increasing across every mutation, undo included.
6. `canonical_digest` covers the palette (slot order, ids, all fields but
   the epoch and the order cache): palette undo is proved by digest.
7. A refused command (palette or fill) leaves the digest and history
   unchanged.
8. A cancelled colour-editor drag leaves the document, the undo list, the
   redo list and the state serial exactly as they were before it.
   The panel must *send* that `Cancel`: egui 0.33 ends a drag by itself
   on `Esc` and reports it as a release in the same frame, so every value
   widget of the editor (numbers, tint/shade sliders, 2D field, strip)
   checks `Esc` on its release frame (XARA-T-0305; before the fix they
   all sent `Commit`, and decision 24's held redo branch was released).
   Pinned by `crates/xarast-ui/tests/colour_editor.rs`.
9. A colour drag (bar or gallery) changes nothing before its drop; a drop
   is at most one undo step; a cancelled drag or a drop on nothing leaves
   digest and history untouched.
   Its frames therefore repaint nothing, and a drop repaints only what
   the scene diff of XARA-T-0221 finds changed: the target object, or
   every user of a redefined named colour (`xarast-shell/tests/
   edit_damage.rs`, `a_colour_drop_repaints_its_target_and_matches_a_full_render`).
10. Every listed (named) entry's `entry_index` is unique after any
    `move_listed`; the order of `listed` is total (ties broken by slot).

## Dead ends (do not retry)

- **`RGB → CMYK` as `C = 1 − R, K = 0`** "because anything else breaks the
  round trip": false; the original's black generation is invertible.
- **Rec. 601 for the grey model**: the kernel uses 0.305/0.586/0.109.
- **`round(v × 255)` for cached values**: 545 corpus entries off by one.
- **Shade as `s × x, v × y`** (phase 1's guess): the original's shade is a
  signed move towards 0 or 1; the guess made `(0, 0)` black.
- **Dropping the drag's step with `discard_redo` alone on Esc**: the
  redo branch from before the drag is already gone by then (the first
  preview's commit dropped it). Hold it for the drag's lifetime instead.
- **Re-inserting a deleted palette entry on undo**: a slot map cannot put
  it back under the same key.

## Open TODOs

- [ ] Walker: resolve through `PaletteResolver`, fold `palette_epoch` into
      render cache keys, repaint `ColourUses::users_of(changed)`
      (XARA-T-0203).
- [x] App: `EditCommand::Fill` and the fill/transparency tools
      (XARA-US-0039); `EditCommand::Palette` (XARA-US-0041) closes the
      palette half of XARA-T-0212. The repaint set
      (`ColourUses::users_of`) is still the walker task XARA-T-0203; a
      palette edit today repaints through the whole-resources bump.
- [x] Colour editor on a gradient: follows the fill tool's selected stop
      (XARA-T-0247, decision 32); the start colour otherwise.
- [ ] Colour editor: "no colour" cannot be set from it (waits for
      XARA-T-0204); spot colours are editable as normal ones (no ink
      name / separation UI).
- [ ] `ColourUses` incremental maintenance in the attribute-set paths
      (today: rebuild on load and after a batch).
- [ ] T8.1.5 "no colour" as a first-class `Colour` value (XARA-T-0204;
      today it is `Option<Colour>` in the importer); T8.1.7 palette
      `.xarast` round trip with signed shades (XARA-T-0205); T8.2.5
      `MutateFill` (XARA-T-0210, done); T8.2.7 group transparency (XARA-T-0211).
- [ ] `ColourModel::Ciet` has no converter in the original at all; ours is
      a real XYZ transform. No corpus file uses it.
- [ ] The original drops transparency in every model conversion; we carry
      it. Revisit only if a file shows the difference.
- [ ] Eyedropper (T8.7.6, `Ctrl+E`): not done (XARA-T-0253).
- [ ] Colour drops on the **transparency** tool's handles, and on a
      stroke's own gradient handles: not resolved (only the fill tool's
      colour handles are targets). XARA-T-0255.
- [ ] "Set Fill" is also the label of a line-colour click/drop
      (`SetFillGeometry` names itself by payload, not slot). XARA-T-0255.
