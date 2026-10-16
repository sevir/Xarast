# tools

Memory note for **editing and tools** (phase 7): the command bus seen from
the application, the tool machine, the tools, the palette and the infobar.
Phase spec: [`../phases/phase-07-tools-and-editing.md`](../phases/phase-07-tools-and-editing.md)
(§W1, §W2). The document-side substrate (actions, `Tx`, `History`,
revisions) is in [`document-model.md`](document-model.md) decisions 7, 19,
34, 35; the shell/UI contract in [`app-core.md`](app-core.md).

## Current state (2026-09-23, round 3: XARA-US-0034)

| Piece | Where | State |
|---|---|---|
| `EditCommand` (`TransformNodes`, `DeleteNodes`, `CreateShape`, `SetShapeParams`), `CommandSink` | `xarast-app/src/ops.rs` | done; labels Move/Scale/Rotate/Skew/Transform, Create Rectangle/Ellipse, Edit Shape |
| Layer-lock refusal in every command | `ops.rs` `on_locked_layer` | done |
| Line-width scaling (`scale_line_widths`) | `ops.rs` | done: ×√|det| on every `LineWidth` in the subtree, inherited width pinned as a first child |
| `Session::apply_edit`, gestures, labels; selects a created shape; `ToolRequests::tool` | `session.rs` | done |
| `ToolMachine`, `ToolCtx` (+ `picker`, `grabs`, `enclosed`), `Preview`, `Infobar` (`Measure`/`Angle`/`Toggle`/`Anchor`), `InfobarValue`, `Anchor` | `tool.rs` | done |
| Precise picking: `Picker` over `xarast_geom::HitIndex` + `HitShape` | `picking.rs` | done (lazy rebuild; T-0152) |
| Selector: click, Adjust-toggle, dual state, scale/rotate/skew/centre drags, move, marquee, infobar X/Y/W/H/anchor/lock/angle/scale-lines, double click → creating tool | `selector.rs` | done |
| Rectangle, Ellipse (draw, Ctrl square/circle, Shift from centre, radius handle, W/H/Radius infobar) | `shapes.rs` | done |
| Push, Zoom | `tools.rs` | done |
| Shape editor (F4): node/handle select, marquee, move, reshape, add/delete point, line/curve, smooth/cusp, close, break, join, X/Y, even-odd | `node_edit.rs` | done (US-0034, T6.1–T6.7) |
| Pen (Shift+F5): click corner, drag smooth, continue an end, close, Enter/Esc | `pen.rs` | done (T6.8) |
| Freehand (F3): every sample, chunked incremental fit, smoothing slider, Shift rub-out, closed = filled | `freehand.rs` | done (T6.9, T6.10) |
| `SetPath`/`CreatePath`/`ConvertToPaths`/`SetWindingRule`, `PathEdit` labels | `ops.rs` | done |
| `Tool::action` + `ToolAction`; `AppCommand::Action` (L C S Z B J Enter), `ConvertToShapes` (Ctrl+Shift+C, see decision 30), Backspace = Delete | `tool.rs`, `command.rs` | done |
| Current attributes | `edit.rs` `CurrentAttributes`, `Intent::SetCurrentAttribute` | done (no UI to set them yet) |
| Keys | `AppCommand` + shell | as before, plus `3` = Zoom to selection; momentary Space / Alt+S (selector), Alt+Z (zoom), Alt+X (push) |
| `--probe drag|scale|rotate|rect|ellipse|nodes|pen|freehand` | `xarast-shell` | done |
| Incremental pick index (journal → re-walk the touched top-level objects) | `picking.rs`, `xarast-doc` `Tree::drain_changes` | done (XARA-T-0168) |
| Group/ungroup (localise + factor-out), z-order ×6, align/distribute, duplicate, cut | `structure.rs` `StructureCommand` | done (XARA-US-0035) |
| Clipboard: fragment document + SVG flavour, internal/system paste, paste in place | `structure.rs`, `app.rs` `InternalClipboard`, shell | done |
| Alignment panel, Arrange menu, Edit/View items | `xarast-ui/src/menus.rs` | done |
| Snapping: `SnapSource`, resolver, grid, guides, objects (corners, then outlines), NumPad toggles, marker | `snap.rs`, `ToolCtx::snap_point`/`snap_move` | done (XARA-US-0036, T-0153) |
| Guides/grid as undoable document edits; ruler guides and grid settings wired | `snap.rs` `GuideCommand`, shell `ui_intent` | done |
| `--probe snap|arrange|paste` | `xarast-shell` | done |
| Text tool (F8): caret, selection, visual/logical bidi navigation, pending point/column story; `Intent::TextNav`; text stories pickable by their line box; selector double click on text → text tool | `text_edit.rs`, `text_tool.rs`, `picking.rs` | done (XARA-US-0047, T9.4.1–T9.4.4 + the T9.4.5 gestures); typing T9.4.6 done; IME composition and the text clipboard (T9.4.7–T9.4.8) done (XARA-T-0224, decision 63) |
| Text infobar (font, size, B/I/U, align, spacing, tracking), OpenType panel, text ruler; `EditCommand::SetTextAttr`; `ToolRequests::current` | `text_infobar.rs`, `text_tool.rs`, `ops.rs`, `tool.rs` | done (XARA-T-0225, T9.4.9–T9.4.10; decision 59) |

Tests: `crates/xarast-app/tests/transforms.rs` (18: dual state, scale
corner/aspect/centre, live Ctrl mid-scale, line widths, rotate with
constrain + angle field, centre drag + snap, skew, the 72-case anchor
table, rectangle/ellipse creation, square/circle, from-centre, current
attributes, Esc mid-draw, radius handle + W/H/Radius typing, parametric
survives transforms, double click, locked layer), `tests/picking.rs` (4),
unit tests in `selector.rs`, `shapes.rs`, `ops.rs`; shell:
`input/momentary.rs` (3), viewer `held_switch_keys_change_the_tool_until_released`.

Live window (GardenPlan.xar, 2056×1286, GPU tiles, 100 frames each,
input → presented p50/p99): drag 2.5/4.9 ms, scale 2.5/6.7 ms, rotate
1.8/4.4 ms, rect 0.8/3.2 ms, ellipse 1.0/1.5 ms. `cargo bench -p
xarast-app --bench pick` at 100 000 objects: warm precise pick 2.3 µs;
first pick after a change (index rebuild) 62 ms.

Measured: `cargo bench -p xarast-app --bench undo` — undo+redo of a move of
one object at 250 000 nodes **339 µs the pair** through `Session` (≈0.17 ms
each; budget 1 ms), 314 µs through the bare bus. Live window
(`--probe drag`, GardenPlan.xar, 2056×1286, GPU tiles): input → presented
p50 2.1 ms, p99 7.8 ms over 100 drag frames, each a preview scene rebuild.

Round 3 (XARA-US-0034): `tests/node_edit.rs` (21: node selection incl.
Adjust/Adjust+Constrain/marquee/select all/Esc, `Arc::ptr_eq` on point
selection, node drag one step + undo exact, 45° constrain, Esc at 10 cut
points, smooth-handle sync + cusp independence, segment reshape, add/delete
exact on a line, continuity on curves, delete-all deletes the object, every
path operation and full undo, double click, typed X, winding rule,
rectangle never converted silently + Convert, pen lines/drag/close/Esc/
continue, selector double click on a path), `tests/freehand.rs` (4: the
4000-sample 200 Hz stroke, no sample dropped, rub-out, Esc + closed
stroke); geometry proptests in `xarast-geom/tests/path_edit.rs`.
Live window (GardenPlan.xar, 2056×1286, GPU tiles, 100 frames, input →
presented p50/p99): nodes 2.9/14.7 ms, pen 3.4/8.9 ms, freehand
3.2/9.8 ms.

Round 3 (2026-09-23): `bench pick` "undo + first pick after it" 2.26 ms
per undo+pick+redo+pick (≈1.1 ms per edit and pick; was 62 ms);
`bench structure`: group 5 000 + undo 10 ms, align+distribute 5 000 +
undo 1.7 ms (budgets 50 ms). Live, GardenPlan, 2056×1286, GPU tiles:
`--probe snap` (grid + guide + object snapping) p50 1.99 / p99 4.22 ms;
`--probe arrange` p50 2.32 / p99 4.32 ms; `--probe paste` (isolated GNOME
46 session, 939×708) p50 2.45 / p99 3.25 ms. Tests: `tests/structure.rs`
(10), `tests/structure_corpus.rs` (41 files), `tests/snapping.rs` (6),
`tests/picking.rs` (+1), `snap.rs` unit (4), `xarast-geom/tests/nearest.rs`
(3), `xarast-doc` journal and move-cost tests.

## Decisions taken (and why)

1. **No second transaction builder.** `xarast_doc::Tx` already is T1.2
   (applies as it goes, records inverses first, drops without commit =
   rollback) and `CommandBus` already had gestures; the app adds the typed
   command enum on top, not a parallel bus.
2. **The enum is `EditCommand`, not `Command`**: `xarast_doc::Command` is
   the trait every command implements. Same for the machine's state enum:
   `InteractionState` (the spec's `ToolState` is taken by
   `edit::ToolState`, which tool is chosen).
3. **Coalescing = gesture ∧ `coalesces_with`.** Inside an open gesture a
   command merges with the previous step only if it is the same kind of
   transform on the same nodes; otherwise `apply_edit` opens a new gesture,
   so the step splits. A drag needs none of this: it previews and commits
   exactly one command at `DragEnd`.
4. **The infobar is data.** `xarast-app` has no toolkit, so a tool returns
   an `Infobar` (fields + notes) and gets `infobar_edit(field, Mp)`;
   `xarast-ui` formats/parses units (`units.rs`) and raises
   `UiCommand::InfobarEdit` → `Intent::InfobarEdit`. The spec's
   `fn infobar(&mut self, ui: &mut egui::Ui)` would have broken app-core
   invariant 1.
5. **Overlay is data too** (`OverlayShape`), converted by the viewer to
   `xarast_ui::OverlayItem` and painted by egui over the canvas: moving a
   handle or a rubber band dirties no document tile.
6. **Preview = a scene group with the preview matrix** around each
   previewed node (id = tag | 1<<63). The display list already composes
   group transforms, so no path is cloned per frame. A non-empty preview
   ignores the dirty rect, and `rebuild_scene` widens `scene_ink` by the
   moved bounds (the render thread skips strips outside the ink).
7. **Preview changes report `Changed::DOCUMENT`** (the scene must be
   rebuilt); overlay-only changes report `SELECTION`.
8. **Session applies tool requests after the tool returns** (`run_tool`):
   selection changes, view changes, then commands through the bus. The tool
   holds only `&Document`; a compile-fail doctest in `tool.rs` pins that.
9. **Esc**: consumed by the machine when a gesture is in flight (cancel,
   preview dropped, nothing emitted); otherwise it is "select none". It is
   the only `works_in_drag` command. Undo, redo and every tool switch cancel
   a gesture in flight first.
10. **Scroll bounds are refreshed lazily** at the next `rebuild_scene`
    (`scroll_bounds_stale`). Recomputing them in `after_mutation` walked the
    whole cold-bounds document: 34 ms per undo at 250 000 nodes.
11. **Ctrl+Shift+Z is Redo**, as the maintainer asked and as every Linux
    program does. The original uses it for *zoom to selection*
    (`research/04 §4.4`); that command will need another key.
12. **Shifted letters are bound in both cases** in the shell: the layout
    reports Ctrl+Shift+Z as `"Z"`.
13. **Constrained move snaps to multiples of 45°**, projecting the drag on
    that direction (the original's default constrain angle).
14. **Momentary switch cancels the gesture** (rule 5, explicit variant);
    suspension/resume is not implemented. Keys: decision 26.

15. **Ctrl+Shift+Z stays Redo** (open decision, not ours to change);
    **Zoom to selection is `3`** (beside `1` = 100 %, where Inkscape keeps
    it). The original's chord is recorded in `research/04 §4.4`.
16. **The dual state lives in the selector, keyed by the selection.** The
    tool remembers the selection its mode, rotation centre and angle belong
    to; any other selection reads as scale mode, centre in the middle,
    angle 0. No session state, nothing undone. A click on the rotation
    centre is not a click on the object under it.
17. **Handle grabbing is in device space**, `HANDLE_TOLERANCE_PX` = 6, and
    handles beat objects: a press on a blob starts a transform even over
    another object.
18. **Transform facts** are `research/04 §4.10`: scale/skew fix the
    opposite blob, Adjust fixes the box centre, rotation turns about the
    centre, Constrain = aspect for scale and 45° steps for rotate and
    skew (the default constrain angle). The rotation centre snaps to the
    box's nine anchor points within the grab distance, and is carried by a
    move or a scale.
19. **Labels come from the matrix** (`ops.rs transform_label`): translation
    → Move, orthonormal (a half turn included) → Rotate, diagonal →
    Scale, unit diagonal with one shear → Skew, else Transform. No new
    command variants for each gesture; coalescing still keys on the label.
20. **Line widths scale by √|det|** (the geometric mean of the axis
    scales), only for scale gestures and W/H typing with "Scale lines" on
    (the default). An inherited width is pinned as a new first attribute
    child so the object keeps it after scaling. Rotate and skew never
    scale lines.
21. **The infobar is typed**: `InfobarValue::{Length, Angle, Toggle,
    Anchor}` travels in `Intent::InfobarEdit`. X/Y are the *anchor point's*
    coordinates (default anchor SW, so X/Y = left/bottom as before); W/H
    scale about the anchor, the padlock makes the other axis follow. The
    Angle field is "rotation applied through the selector since this
    selection was made"; typing sets it, rotating about the centre by the
    difference (`research/04` has no fact about what the original shows).
22. **Rectangles and ellipses are `QuickShape`s**, as the importer makes
    them (`shapes.rs` module docs; `research/04 §4.10`). The corner radius
    is `primary_curvature × |major|`, so a *scale* keeps the ratio (along
    the major axis the radius scales, across it not); typing W/H keeps the
    absolute radius. Answer to K8 by representation, pending VM
    observation.
23. **Creation preview is an overlay outline**, not a phantom node: the
    walker (owned elsewhere) has no phantom support. Radius drags hide the
    node (`Preview::hidden`) and draw the new outline.
24. **Current attributes** are session state (`EditState::current`),
    applied as the new object's own attribute children; empty = document
    defaults (black 0.25 pt outline, no fill).
25. **Picking is precise and lazy** (`picking.rs`, contract in
    `geometry.md`): leaves of visible unlocked layers, z = render rank <<
    16, fill tested only if painted, stroke only if the line colour is not
    none. `Session::after_mutation` invalidates; the next pick rebuilds
    (62 ms at 100k, off the undo path). Constrain = leaf, Alternative =
    under the selected object. Top objects with no testable leaf (text,
    unrendered live effects) fall back to their box. **A transparent
    interior no longer selects** — fixtures that click inside shapes must
    fill them.
26. **Momentary switching** (`xarast-shell/src/input/momentary.rs`): press
    raises `MomentaryTool(Some)`, release of the *same key* (whatever the
    modifiers by then) or focus loss raises `MomentaryTool(None)`; repeats
    are swallowed; Space needs no text field focused.
27. **Double click** on a rectangle or ellipse (quick shape or
    `ShapeNode`) with the selector selects it and chooses its tool
    (`ToolRequests::tool`); on a path it opens the shape editor.
28. **Locked layers** refuse `TransformNodes`, `DeleteNodes`,
    `SetShapeParams` (any node on one) and `CreateShape` (the target
    layer) with `EditError::NotPermitted`, document untouched.

29. **Behaviour source for the path tools is `research/04 §4.11`.** The
    shape editor does not draw (the original's Bézier tool does; here the
    pen draws). Handles show for a path's one selected node only, as in
    the original.
30. **Parametric shapes are never converted silently.** The shape editor
    edits paths only (the original's Bézier tool ignores rectangles,
    ellipses and quick shapes too); with one selected it draws its bounds
    dashed and its infobar offers *Convert to editable shapes*
    (`Ctrl+Shift+C`, `EditCommand::ConvertToPaths`: same `NodeId`, same
    attribute children, `NodeKind::Path` of `picking::geometry_of`).
    **Key (XARA-US-0084):** the original binds it to Ctrl+Shift+S
    (`research/04 §4.11`, line 599), but Ctrl+Shift+S is Save As in every
    other desktop program and Xarast keeps that convention; Convert takes
    Ctrl+Shift+C, which is Inkscape's "Object to Path" and was free.
    `command.rs::no_two_commands_share_a_key` and the shell's
    `every_chord_of_the_command_table_reaches_its_command` guard clashes.
    **Text joins it (XARA-US-0049):** the same command converts a text
    story into a group of glyph outlines (`crate::convert`, `text.md`
    "Convert to shapes"), and a selected group converts every shape and
    story inside it, as the original's command does. The session runs
    `convert::ConvertCommand` (like `StructureCommand`, it reports
    `(before, after)` pairs through a `RefCell`) and reselects: a story's
    group replaces it in the selection, shapes keep their id. Nothing
    convertible → `NOTHING_TO_DO`, no undo step. `EditCommand::
    ConvertToPaths` shares `convert_nodes` for tools that emit it.
31. **Point selection** stays `EditState` point indices; tools ask for a
    new one through `ToolRequests::points` (applied after the commands,
    so indices refer to the new geometry) and `created_points` (the pen's
    new end on a created path). `node_edit::Nodes` maps indices ⇄ nodes.
32. **Keys go to the tool first**: Delete/Backspace, Esc (when no gesture
    is in flight) and Ctrl+A become `ToolAction`s the tool may take
    (delete points, deselect points, select all points); otherwise the
    object-level command runs. L/C/S/Z/B/J/Enter are `AppCommand::Action`
    and do nothing in tools that ignore them.
33. **A node edit previews by hiding the path** and drawing its new
    outline and nodes over it (decision 23's pattern); one `SetPath` step
    at `DragEnd`, labelled by `PathEdit` (Move Points, Reshape Curve, Add
    Point, Delete Points, Make Line/Curve, Smooth/Cusp Points, Close Path,
    Break Path, Join Ends, Add Segment). Deleting every node deletes the
    object (label "Delete").
34. **Segment reshape** is ours: both handles move along the remaining
    offset, weighted `(1−t)` and `t`, scaled so the grabbed point lands
    exactly under the pointer (`node_edit::reshape`). The original uses a
    constant 0.656875 factor over `t`; not copied.
35. **Pen**: one undo step per segment (Create Path, Add Segment, Close
    Path), as the original. Its state is the selection (the one selected
    open end) plus a start point held by the tool; Enter/Esc finish by
    deselecting the end; Esc drops an unused start. Clicking an open end
    of a selected path picks it up (ours; the original needs the shape
    editor for that). A closed pen path is filled.
36. **Freehand tolerance** = `(64 + 160 × smoothing) / zoom` mp, the
    original's (`research/04 §4.11`); default 50. The machine replays the
    pointer positions seen below the drag threshold as `DragUpdate`s once
    a drag starts, so the fitter sees every sample (other tools ignore
    the extra updates). The preview fits in chunks of 96 samples (frozen
    prefix + bounded tail); the release refits the whole stroke.
37. **Incremental pick index** (XARA-T-0168). `Action::apply` journals
    `(node, parent-at-the-time)` into the tree; the session drains it into
    the picker after every mutation (undo/redo included) and the next pick
    removes and re-walks only the touched *top-level objects* (layer
    children). Z is per top: `key + leaf ordinal`, tops `2^24` apart, new
    keys halve the gap to the nearest indexed neighbour. Rebuilds instead
    on: journal overflow (2^16 entries, a fresh tree, a resource change),
    any layer/spread change (visibility and lock included), a loose
    layer-level attribute change, > ¼ of the tops touched, no key left, or
    a layer with no indexed object yet. Undo + first pick at 100k objects
    1.1 ms the pair (was a 62 ms rebuild).
38. **Structure operations move through one primitive** (`structure.rs`):
    inherited-before → move → inherited-after → an attribute child per
    differing slot (`localise`); grouping then `factor_out`s leading
    attributes every member sets identically. No normalisation after other
    edits (`research/02 §10.6`). Multi attributes are not materialised.
    Group goes right above the topmost member; ungroup puts members where
    the group was, so group∘ungroup of a contiguous run restores z-order
    (41 corpus files pixel-identical, `tests/structure_corpus.rs`).
39. **Send to back goes after the parent's leading attributes**; forward/
    backward skip attribute siblings; layer up/down go to the top of the
    next *editable* layer of the same spread. An operation with nothing to
    do returns `structure::NOTHING_TO_DO`, rolled back: no empty undo step.
40. **A move is not charged as a deletion** (`Tx::move_node`): the detach
    used to charge the whole subtree to the 128 MiB budget, so grouping a
    large drawing evicted its own undo step (dead end found on
    `ProbeX16.xar`).
41. **Clipboard.** Copy = `copy_fragment`: a new `Document` (source
    defaults and resources cloned) holding self-contained copies, kept in
    `AppState` as `InternalClipboard { fragment, svg }`; the shell puts the
    `.xarast` SVG profile on the system clipboard **as text** — `arboard`
    has no custom MIME types, so `image/svg+xml` is not offered (task
    filed). Paste asks the shell (`PlatformRequest::ReadClipboard`); the
    answer `Intent::PasteText` pastes our fragment when the text is our SVG
    or there is no clipboard (`None`), else reads the text as SVG; other
    text → a diagnostic. Plain paste centres in the view, paste in place
    keeps coordinates; pasted objects go on the active layer, selected.
    Across documents palette references are resolved to direct colours
    unless the target palette has the identical entry; bitmaps are
    imported (dedup by content, outside undo — unreferenced ones are swept).
    Measured in an isolated GNOME session: `read=Ok(12046) ours=true`; the
    text does not outlive the process there (no clipboard manager).
42. **Duplicate** copies each object right above itself (so it inherits
    what the original does) and offsets by `DUPLICATE_OFFSET` (10 pt
    right, 10 pt down, provisional).
43. **Snapping belongs to the gesture**: tools call
    `ToolCtx::snap_point(p)` (creation corners, scale blob, rotation
    centre) or `snap_move(nodes, bounds, delta)` (a move: nine anchors,
    smallest correction per axis; not while Constrain holds). Per axis
    nearest wins, ties by priority guide > object > grid; object
    candidates are points, a point candidate wins over an axis pair at
    least as far. Radius 8 device px (provisional). Grid lines every
    `spacing / subdivisions`, integer-exact. Object snap = the picker's
    `object_snap`: corners/edge middles/centres of leaves near the pointer,
    failing those the nearest outline point (XARA-T-0153); it goes through
    the index (a first version walked every object per frame).
44. **Switches are session state** (`EditState::snap`, default: guides on,
    grid and objects off). `Intent::ToggleSnap` re-delivers the last
    pointer as a modifiers change, so a mid-drag toggle re-evaluates at
    once. Guides and the grid are **document** state: `GuideCommand`
    (Add/Move/Delete/DeleteAll/SetGrid) is undoable and survives `.xarast`
    save/reopen; Show guides = the guide layer's visibility (hidden guides
    do not snap). The snap marker is `HandleShape::Snap` while dragging.

## Phase 8: the fill and transparency tools (XARA-US-0039)

| Piece | Where | State |
|---|---|---|
| `FillHandles` derivation (`fill_handles`), `HandleKind`, `Guide`, `stop_target` | `xarast-app/src/fill_handles.rs` | done (T8.3.1) |
| Device-space `hit_handle`, `FILL_PICK_RADIUS_PX` = 5, z-order stop > end > arm | `fill_handles.rs` | done (T8.3.2) |
| Overlay: `OverlayShape::Arrow`, `HandleShape::Fill{Blob,Centre,Stop}[Selected]`; UI `OverlayItem::Arrow`, `HandleKind::{FillBlob, FillCentre}` (stops reuse the `Fill` diamond) | `tool.rs`, `xarast-ui/src/overlay.rs`, shell `viewer.rs` | done (T8.3.3) |
| Shared handle sets for selected objects with equal fills (`fill_sets`) | `fill_tool.rs` | done (T8.3.4) |
| `FillLikeTool<K: FillKind>`, `GradFillTool` (F5), `TransparencyTool` (F6) | `fill_tool.rs`, `tools.rs`, `command.rs` | done (T8.4.1, T8.4.2) |
| Infobars: type (mutate), effect / blend mode, tiling, ramp mapping, bias, gain, stop position, stop transparency | `fill_tool.rs`; `InfobarItem::{Choice, Real}`, `InfobarValue::{Choice, Real}`; UI combo box and slider in `toolbar.rs` | done (T8.4.3, T8.4.4) |
| `EditCommand::Fill { edits: Vec<FillCommand> }` wrapping the `xarast-doc` fill commands | `ops.rs`, `fill_tool.rs` | done (fill half of XARA-T-0212) |
| `Preview::attrs` — attribute overrides the walker applies | `tool.rs`, `walker.rs` | done |
| Axis lock (Constrain on a centre/origin) and aspect lock (Adjust on an elliptical radial or diamond axis) during a handle drag | `fill_tool.rs` (`handle_anchor`, `aspect_partner`, `moved_fill`) | done (rest of T8.3.6, XARA-T-0220) |
| Conical fill from a double click held and dragged; `GestureEvent::DragStart::count` | `fill_tool.rs`, `tool.rs` | done (XARA-T-0220) |
| Outline (stroke) handle sets (`paint_sets`, `FillSet::slot`, `FillSelection::slot`) | `fill_tool.rs`, `colour_bar.rs` | done (XARA-T-0220) |
| Keyboard nudge of the selected handle or stop: `Intent::Nudge(Nudge)`, `NudgeStep`, `Tool::{takes_nudge, nudge}`, one step per run | `tool.rs`, `session.rs`, shell `viewer.rs` | done (T8.3.5, XARA-T-0220) |
| Status line and cursor per hover target: `Tool::status`, `Session::tool_status`, `CursorKind::Pointer` | `fill_tool.rs`, `tool.rs`, shell `viewer.rs` | done (T8.4.6, XARA-T-0220) |
| Infobar slider drags as one gesture: `Intent::InfobarDrag(InfobarDrag)`, `Tool::infobar_preview` | `tool.rs`, `session.rs`, `fill_tool.rs`, UI `toolbar.rs` | done (XARA-T-0220) |

Tests: `tests/fill_tool.rs` (10: drag-out makes one "Set Fill" step and
undoes exactly; a handle drag leaves the digest untouched until release,
previews in pixels and commits pixel-identical to the preview; Esc on a
handle drag and on a drag-out leaves digest and history unchanged; double
click on the arm inserts a stop, drag moves it, Delete removes it; two
objects sharing a fill show one set and move together, and split into two
sets once they differ; type menu mutates; Constrain snaps to 15°; profile,
effect, tiling and mapping fields; the transparency tool's 0→255 drag-out,
blend mode and per-cent level; Shift drags a circular fill; a leaf object
previews as it commits); unit tests in `fill_handles.rs` (3: handle
counts per shape, hits at 5 %, 100 % and 3200 % zoom, z-order).

XARA-T-0220 added to `tests/fill_tool.rs` (7): Constrain keeps a centre on
its row and Adjust turns and stretches the other axis of an ellipse, each
one step that previews pixel-identically and undoes exactly; a double click
held and dragged makes a conical fill (a slow one a linear, Adjust a
circle); an outline gradient shows its handles, a drag edits only the
outline (one step, pixel-identical, undo exact, `Esc` restores digest and
pixels) and the colour bar ignores an outline handle; five arrow nudges and
a Ctrl nudge are one "Move Fill Handle" step, another intent ends the run,
a stop nudged across its arm writes nothing; the cursor and the status
line for a handle, the arm, the object, a drag and nothing; a 30-frame
profile slider drag leaves digest and history untouched per frame,
damages only its object, commits one "Fill Profile" step equal to the
preview, and `Esc`/undo drop it; a transparency level and a stop position
slider drag commit what they preview. `tool.rs` unit test: the
second press of a double click reports `DragStart { count: 2 }`. UI:
`xarast-ui/tests/toolbar.rs` (2, kittest: a slider drag is previews then
one commit, no `InfobarEdit`; `Esc` mid-drag is a cancel and nothing
after). Shell: `arrows_nudge_a_selected_fill_handle_and_pan_otherwise`.

45. **A fill drag previews, it does not emit.** `phase-08 §W8.4` sketches
    live `MoveFillControl`s per mouse move plus a restore on Esc; that would
    break invariants 2 and 7 (nothing before `DragEnd`, the pick index never
    sees a drag frame). Instead the tool snapshots the fill at the press,
    recomputes it each frame with `xarast_doc::fill_edit::move_control`, and
    puts it in `Preview::attrs`; the walker draws each node with that value
    standing in for its own attribute of the slot. Release emits one
    `EditCommand::Fill` (`MoveFillControl`/`MoveStop` per node with the final
    point, so the step is labelled "Move Fill Handle"/"Move Fill Stop"); Esc
    has nothing to undo. The command's `drag` coalescing key stays unused by
    the tool.
46. **Walker override rule**: a node in `Preview::attrs` gets the value
    pushed right after its `EnterScope` (where the command would add the
    attribute), its own attribute child of that slot is replaced at its
    visit, and a leaf is painted inside a pushed scope holding the value.
    The scope fingerprint mixes a hash of the value so the node's content
    hash changes every frame. `headless::render` renders the preview too.
47. **Handle sets**: the selected objects are grouped by equal fill in force
    (`fill_in_force`, interior slot); a set's drag edits every node in it.
    Outline fills have handle sets too since XARA-T-0220 (decision 78).
48. **Drag-out** (`research/04`, `tools/filltool.cpp` OnClick): a drag not on
    a handle makes a new fill of the infobar's type over the pressed object
    (selecting it) or over the selection; flat → linear; Adjust (Shift) →
    circular. Colours: a flat colour runs to itself at zero saturation (the
    W8.2 mutation rule; black/white when already grey), a gradient keeps its
    ends and ramp; transparency 0 → 255 (`Kernel/opgrad.cpp:2796-2802`),
    mix mode unless the object already has one. The end handle is selected
    afterwards, as the original does. A double click held and dragged makes
    a conical fill (decision 77).
49. **Stops**: a double click on an arm inserts a stop with the ramp sampled
    there (Fade), selected; Delete removes the selected stop
    (`ToolAction::Delete`, before object deletion); Esc with a handle
    selected deselects it. The selected stop keeps its identity across a
    re-sort (`ramp_move` returns the new index).
50. **Constrain during a handle drag** turns the handle about the arm's other
    end (or the centre, or the three/four-colour origin) in 15° steps
    (T8.3.6's snap-to-15°). A handle with no such anchor is kept on an axis
    instead, and Adjust locks the aspect (decision 76). Snapping goes
    through `ToolCtx::snap_point`.
51. **Tiling in the infobar is "Simple"/"Repeating"**: Repeating writes
    `Tiling::RepeatExtra` for a graduated fill (the only mapping that tiles
    one, `research/01 §8.3`) and `Tiling::Repeat` for three/four-colour
    fills. Repeat-inverted only renders for bitmaps and is not offered.
52. **Blend modes offered**: the ten `TranspMode`s other than `None`
    (`TRANSP_MODES`), Hue last since XARA-US-0018/T-0256 added it.

### The fill tools' follow-ups (XARA-T-0220)

76. **Axis lock and aspect lock** (rest of T8.3.6). Constrain on a handle
    with an anchor (`handle_anchor`: an arm end, an axis end, a corner, a
    three/four-colour colour point, a bitmap edge) still turns it about the
    anchor in 15° steps; on a handle without one (a centre, a
    three/four-colour origin, a perspective corner) it keeps the handle on
    the nearest 45° axis through its press point — the selector's
    constrained move (`tools::constrain_45`). Adjust on an axis of an
    **elliptical** radial or a diamond fill turns the other axis with it,
    at a right angle, scaled by the same ratio (facts:
    `Kernel/fillattr.cpp:7249-7310` radial, `:10146-10200` diamond). Ours:
    the other axis stays on the side it was on (the original always turns
    +90°/−90°), as decision 65 does for bitmaps. A circular radial fill is
    locked already. The commit is **two `MoveFillControl`s per object**
    (the handle, then the other axis) inside one `EditCommand::Fill`, so it
    is still one "Move Fill Handle" step and needs no new command; the
    preview runs the same two moves (`moved_fill`), which keeps invariant
    10. Without Adjust the other axis stays put (the original turns an
    elliptical radial's minor axis to stay perpendicular even then; not
    copied, open in the TODOs).
77. **Conical from a double click held and dragged** (facts:
    `tools/filltool.cpp:921-955`: a double click sets a flag that the drag
    reads; Adjust's circle wins). `GestureEvent::DragStart` now carries
    `count`: 1 for a plain press, 2 when the press is the second of a
    double click, counted by the same rule as `Click::count`
    (`ToolMachine::follows_click`, 500 ms / 6 px from the last click). The
    fill-like tools drag out a conical fill from `count >= 2` without
    Adjust. Other tools ignore the field.
78. **Outline handle sets.** `paint_sets` = the interior sets
    (`fill_sets`, unchanged: the colour bar still resolves drops against
    those only) followed by one set per distinct **outline** fill that has
    control points (a flat outline shows nothing). Outline sets come last
    so their handles win where they overlap. `FillSet::slot`,
    `FillSelection::slot` and the tool's selection carry the slot; every
    command the tool emits for a set uses the set's slot, and the preview
    writes `StrokeColour`/`StrokeTransp` (`FillKind::stroke_attr`). The
    walker's override rule is per slot already, so nothing changed there.
    **The infobar edits the outline set when the selected handle is an
    outline's, else every interior set** (`edited_sets`); the outline bar
    has a "Outline" note and no tiling choice (the mapping attribute is the
    interior's). Drag-out still only makes interior fills. The colour bar
    and the colour editor ignore an outline handle (`selected_stop` returns
    `None`) — dropping a colour on an outline stop is open.
79. **Keyboard nudges** (T8.3.5, `research/04 §4.5`). The shell sends
    `Intent::Nudge(Nudge { dir, step })` for an arrow key when
    `Session::takes_nudge()` (the tool in force has a shown handle
    selected) and pans otherwise; `NudgeStep::from_keys`: none 1 unit, Ctrl
    ×5, Shift ×10, Ctrl+Shift ⅕, Alt 1 device pixel, Alt+Shift 10 pixels.
    The unit is `NUDGE_UNIT_MP` = 1 mm (provisional: the `.xar` document
    nudge, tag 4114, is imported and dropped). The fill tool moves the
    selected handle by the vector exactly as a drag would commit it
    (`handle_edits`: `MoveFillControl`, `MoveStop` projected on the arm,
    `MoveBitmapControl`); a nudge that changes nothing (a stop nudged across
    its arm) is taken and writes nothing. **A run is one undo step**: the
    session opens a coalescing gesture at the first nudge
    (`Session::nudge_run`) and ends it at the next intent that is not a
    nudge or pointer/modifier/resize traffic; the commands coalesce under
    the existing `Fill`/`Fill` rule (same label, same objects). When
    `apply_edit` splits a gesture it hands the run the new gesture id, so
    ending the run always closes what is open.
80. **Status line and cursors** (T8.4.6). `Tool::status(state)` →
    `ToolMachine::status` → `Session::tool_status`; the shell shows it
    after a colour or bitmap drag's text and before its own notices, so a
    notice shows again as soon as the tool has nothing to say (`None` over
    empty canvas with nothing selected). The fill tool keeps a hover
    target (`Hover`: a handle — with whether Constrain turns it, whether
    Adjust locks its aspect, whether it moves the whole fill — the arm of a
    graduated fill, an object or selection a drag would fill, or nothing),
    recomputed on every hover and click. Cursors: handle → `Move`, arm →
    `Pointer` (new `CursorKind`, the hand), object → `Crosshair`, nothing →
    `Default`; a handle drag `Move`, a drag-out `Crosshair`. The texts are
    ours (the original's resource strings are not copied); an outline
    handle's text starts "Outline:". A status change reports
    `Changed::UI` from `run_tool`.
81. **An infobar slider drag previews, it does not emit** — decisions 45
    and 74 again. The UI sends `InfobarDrag::Preview { field, value }`
    every frame the slider moves, `Commit` on release, `Cancel` on `Esc`
    (egui's `drag_stopped` in the `Esc` frame counts as a cancel, `ui.md`);
    a keyboard step stays one `InfobarEdit`. The session asks
    `Tool::infobar_preview`, which puts the would-be fills in
    `Preview::attrs` (`slider_fills`: profile bias/gain on every edited
    graduated set, a stop's position through `ramp_move`, a transparency
    level through `set_stop` keeping the mode — the same pure functions
    the commands run, so the commit renders what was previewed); on
    `Commit` it clears the preview and applies the last value through
    `infobar_edit`: **one step**. `Esc` (before select-none), undo, redo,
    an `InfobarEdit` and a tool switch drop the drag with nothing to undo
    (`settle_infobar_drag`). A field a tool does not preview is applied as
    it goes (the old behaviour; none today). The overlay and the infobar
    read the previewed fill (`live_sets`), so handles and the slider follow
    the drag.
    On a feathered object the preview goes through the walker inside the
    feather's `push_effect`, so `scene_damage` pads it like any edit under
    an effect; `a_slider_preview_on_a_feathered_object_repaints_all_it_changes`
    (`tests/fill_tool.rs`) checks the repainted damage equals a full render,
    byte for byte, and stays within the object plus the feather's reach. A
    feather fades inward and a fill preview moves no geometry, so that test
    passes even with `effect_pad` zeroed: padding matters for geometry
    edits, which `xarast-render`'s damage property covers.

## Phase 9: the text tool (XARA-US-0047)

53. **The text tool (XARA-US-0047)** is a state machine over two states
    (`text_tool.rs` module docs): **Story** (a `TextSelection { story,
    anchor, head }` of byte offsets with affinity, tool state only; the
    model's `TextCursor` is derived on demand with `cursor(doc)`) and
    **Pending** (`{ at, column }`: where typing *will* create a point
    story or a column). No story is created by a click or a column drag:
    the first typed character will (T9.4.6), so a stray click leaves no
    empty story and no undo step (the original creates the story at once
    and deletes it if left empty). Gestures: click text → caret at the
    nearest stop (Shift+click extends in the same story); drag on text →
    select; double click → word (UAX #29 segment, spaces included);
    triple click → the laid-out line; click empty canvas → pending point
    story at the click (the baseline origin); drag on empty canvas →
    pending column of the drag's width, first line hanging from the drag's
    top (narrower than 8 px → point story); Esc mid-drag restores the
    caret as it was. Esc with no drag leaves the text, story still
    selected. Ctrl+A selects the story's text. Delete, Backspace and
    Enter belong to the text while a caret is up (decision 57), so they
    never delete the story object. Choosing the tool
    with one story selected enters it with the caret at the end; the
    selector's double click on a story does that (decision 27).
54. **The tool lays stories out itself**, with the walker's own function
    (`text::lay_story`: bridge, `layout_text`, and on a path the path's
    column plus the `PathFit`) with the attributes resolved at the story
    (`resolve_inherited`), cached per story and dropped wholesale when
    `Document::epoch` moves. Hit testing walks every story on a visible,
    unlocked, non-guide layer (topmost wins), inverse-maps the point with
    the story matrix and asks `CaretMap::hit_point` for the caret and the
    distance to the text (line boxes, or fitted cluster boxes on a path),
    with a 4 px tolerance. Fonts are the process's shared service
    (`TextTool::with_fonts` for pinned fonts).
55. **Text is picked by its line box.** The pick index adds each
    `TextStory` as one `Geometry::Bounds` leaf (the `story_rect` of its
    laid-out lines, with the attribute stack in force) and skips its
    subtree: before this, the selector could not click text at all (text
    items have no geometry and no cached bounds).
56. **Navigation keys are an intent, not `ToolAction`s**: `Intent::TextNav(
    TextNav { key: TextKey, word, extend })` → `Tool::text_nav`, offered
    only while `Tool::text_editing()` is `Some`. Ctrl = by word
    (arrows), to the story's ends (Home/End); Shift = extend. Mapping and
    shell routing: `ui.md` decision 40.
57. **Typing (T9.4.6, XARA-T-0223)** is `Intent::TextInput(TextInput {
    kind: Insert(text) | Backspace { word } | Delete { word }, time_ms })`
    → `Tool::text_input`; the tool emits `EditCommand::TypeText { story,
    replace, text, burst }`, `DeleteText { story, range, burst }` or, at a
    pending caret, `CreateText { layer, story, attrs, text, burst }`
    (active layer, current attributes). **One undo step per burst**: the
    burst number is the command's `CoalesceKey` (`gesture: burst`, kind
    `"text-typing"` or `"text-delete"`), so the history's existing
    merge-on-equal-key does it — no gesture is opened and nothing is
    time-based inside the history (the phase doc's "one implementation, in
    the history"). The tool decides the burst: the next key joins when it
    is the same kind, in the same story, at the caret the last one left,
    **less than 500 ms** (`TYPING_BURST_MS`) after it, with a collapsed
    caret, and the document's epoch is still the one the tool saw right
    after its own edit. The last check is what makes "any other command
    ends the burst" (undo included) hold without the session telling
    tools: **`Tool::after_commands(doc, created)`** is called by
    `run_tool` after the tool's commands applied, with the object a
    creation made. The text tool uses it to note the epoch and to adopt the
    story `CreateText` made (the caret moves into it). Burst numbers come
    from a process-wide counter, so two documents never share one. Setting
    the caret any other way (`TextTool::set`) ends the burst. Labels:
    "Typing", "Delete Text", "New Text" (a merged step keeps its first
    label, so undoing a new story's first burst says "Undo New Text" and
    removes the story too). `ToolAction::Delete` (Edit › Delete) deletes
    forwards and `ToolAction::Finish` breaks the paragraph, never joining a
    burst.
58. **Text on a path in the tool (XARA-T-0250).** The overlay is built
    from story-space geometry the `CaretMap` computes
    (`caret_segments`, `selection_quads`), mapped by the story matrix
    only; the tool no longer builds carets or highlights from
    `caret_geometry`/`selection_rects` itself. So on a path the caret is
    the fitted cluster's edge, turned (and sheared) with the glyph, the
    highlight is one `OverlayShape::Highlight` quad per selected cluster
    (plus one for a selected paragraph break), and no shell or `xarast-ui`
    change was needed (`Caret` was already any segment, `Highlight` any
    quad). Caret motion (arrows, Home/End, Up/Down with a goal x) stays in
    the straight layout: bending a line changes neither logical nor
    visual order.

59. **Text attributes from the infobar (XARA-T-0225, T9.4.9–T9.4.10).**
    The text tool describes font family (`InfobarItem::FontFamily`,
    `InfobarValue::Choice` = index into the list it offered), size
    (`InfobarItem::Scalar`, points), bold/italic/underline (`Toggle`),
    alignment (`Choice`), line spacing and tracking (`Scalar`), the
    OpenType panel (`InfobarItem::Features`, `InfobarField::TextFeature(tag)`
    + `Toggle`) and, for a straight story with a caret, the tab kind choice
    and `InfobarItem::TextRuler` (drawn by the UI on the horizontal ruler,
    raising `TextLeftMargin`/`TextRightMargin`/`TextFirstIndent`/
    `TextTabAdd`/`TextTabMove(i)`/`TextTabRemove(i)`). All through the
    existing `Intent::InfobarEdit`, so no shell change. **Where an edit
    goes**, in order: a selection in a story → one `EditCommand::SetTextAttr`
    step (label from the slot: "Bold", "Font Size", "Tab Stops"…); a caret →
    a paragraph attribute at once on its paragraph, a character attribute
    into the tool's **pending style** (document untouched, bar shows it),
    which the next `TypeText` gets through a `SetTextAttr` with the same
    burst key (**one undo step "Typing"**); any caret change drops it; a
    pending caret keeps its style (any attribute) and `CreateText` gets it
    over the current attributes; no caret but stories selected → each whole
    story, all in one step (a shared fresh burst key); nothing selected →
    `ToolRequests::current` (new, additive) and the session sets the current
    attributes. An attribute edit ends a typing burst. An edit whose value
    is already what the bar shows emits nothing (no empty undo steps). The
    ruler commits **on release**, one step per drag.
60. **Invariant 11 widened**: the text tool mutates the document only by
    typing and by attribute edits the user asked for; choosing an attribute
    at a caret changes nothing until text is typed.
63. **IME and the text clipboard (XARA-T-0224, T9.4.7–T9.4.8).** New tool
    hooks, all defaulting to "not taken": `Tool::text_preedit(Option<
    Preedit>)` (from `Intent::TextPreedit`), `Tool::text_clipboard(
    TextClipOp::{Cut, Paste(Arc<StyledText>)})`, `Tool::text_copy(doc)`
    and `Tool::text_caret(view)` (the caret's segment even while it is
    hidden, for `Session::ime_cursor_area`). `Preview` gained `text:
    Option<TextPreview>`: a composition is drawn through the preview, so it
    never touches the document or the undo history (`text.md`, "IME
    composition and the text clipboard"). **While a caret is up, Copy,
    Cut and Paste are the text's** (`AppState::copy`, `Intent::Cut`,
    `paste_text` check `Session::text_editing()` first): Copy/Cut with a
    collapsed caret do nothing — never the story object under it; the
    copy is kept as `AppState::text_clipboard` (at most one of it and the
    object clipboard is set) and its plain text goes out through
    `SetClipboardText`; Paste still asks the shell (`ReadClipboard`) and
    the answer pastes our styled copy when the text matches it (or there
    is no clipboard), the text as plain text otherwise, and refuses our
    own object SVG with a status line ("leave the text (Esc) to paste
    them"). Edits: `EditCommand::PasteText { target: PasteTarget::{Story,
    New}, text }` ("Paste", no coalescing) and `CutText` ("Cut"); the
    session treats `PasteTarget::New` as a creation (selected, reported to
    `after_commands`).
70. **A story emptied by deletion is removed (XARA-T-0237, maintainer's
    decision 2026-09-24; widened by XARA-T-0295).** `DeleteText`,
    `CutText`, and `TypeText`/`PasteText` replacing a range with nothing,
    remove the story *inside their own transaction* when it is left
    holding no characters, only paragraph breaks and tabs
    (`xarast_doc::remove_empty_story`), so the removal is part of the step
    that emptied it and merges with the rest of a Backspace burst: one
    `Ctrl+Z` restores story and text (the original gets the same effect by
    merging its story deletion into the previous operation). The tool
    predicts the emptying when it emits (the range covers the whole laid-out
    text and nothing is inserted: `Emptying::of`) and, in
    `after_commands`, finding the story gone: the caret becomes **pending
    at the story's origin** (its matrix translation; a column keeps its
    width, rotation is not kept) and gets the removed text's attributes that
    differ from the current ones as its **pending style**, so typing on
    makes a story that looks like the old one; a story **on a path** leaves
    its path as an ordinary shape (as the original keeps it,
    `tools/textops.cpp:4281-4293`) and editing ends — there is no straight place for a caret. Selection:
    the story is pruned from it (the session prunes after every command),
    the freed path is not selected (the original deselects it). **Leaving
    the text removes a story of breaks (XARA-T-0295, revising T-0237's
    "leaving removes nothing").** Esc, a tool switch or a click elsewhere
    (all through `TextTool::set` → `leave`) emits
    `EditCommand::RemoveEmptyStory` ("Delete Text") when the story left
    holds no characters *and the tool edited it since the caret went in*
    (`edited`), as the original does when editing ends
    (`tools/texttool.cpp:1570`, `:2403`). It carries the key of the tool's
    last burst on that story (`joins`) and merges into it when that is
    still the last undo step (the original merges into the previous
    operation unconditionally); otherwise it is a step of its own. Enter
    typed over the whole text is typing, not deletion: the story stays
    until editing ends. An imported empty story only entered and left is
    never touched.

### Shortcuts added (`research/04 §4.2–4.4`)

| Keys | Command | Note |
|---|---|---|
| Ctrl+X / Ctrl+C / Ctrl+V | Cut / Copy / Paste | Backspace, Shift+Del, Ctrl+Ins, Ins, Shift+Ins not bound |
| Ctrl+Shift+V | Paste in place | |
| Ctrl+D | Duplicate | Ctrl+K (clone) not implemented |
| Ctrl+G / Ctrl+U | Group / Ungroup | |
| Ctrl+F / Ctrl+B | Bring to front / Send to back | |
| Ctrl+Shift+F / Ctrl+Shift+B | Forward / Backward one | |
| Ctrl+Shift+U / Ctrl+Shift+D | Layer up / down | |
| Ctrl+Shift+L | Alignment panel | |
| NumPad . / NumPad 2 / NumPad * | Snap to grid / guides / objects | work mid-drag (`works_in_drag`) |
| `#` | Show grid | |
| NumPad 1 | Show guides | keypad only (`ChordKey::NumPad`), so `1` stays 100 % |
| F5 / F6 | Fill tool / Transparency tool | `research/04 §4.4` line 640 |
| F8 | Text tool | `research/04` (`TOOL21`, F8); arrows/Home/End/PgUp/PgDn (+Ctrl, +Shift) move the caret while one is up |

Clashes: none with the path tools' plain L/C/S/Z/B/J/Enter, Backspace and
Ctrl+Shift+S (XARA-US-0034), checked by `no_two_commands_share_a_key`.
Known ones stay: Ctrl+Shift+Z is Redo (original: zoom
to selection, here `3`); `<`/`>` for undo/redo not bound. Ctrl+D is
duplicate while plain `D` is fit drawing — different chords.

## Phase 8: colour drops (XARA-US-0042)

Numbered after phase 9's decisions because the colour bar landed later.

61. **The fill tools report their selected handle** (XARA-US-0042):
    `Tool::fill_selection` (default `None`) → `FillSelection { channel,
    nodes, handle }`, read through `ToolMachine::fill_selection`. The
    colour bar's click and the colour editor use it to target that stop
    (`colour.md` decisions 27, 32); the tool itself is unchanged.
62. **`Picker::pick_drop`** is the colour-drop variant of `pick`: leaf
    mode, an unpainted closed interior counts as the fill (leaves keep
    `interior: Option<FillRule>`), an outline is hit within its half-width
    or 3 px. `pick`, `enclosed` and object snapping are unchanged.

## Phase 10: bitmap fills and placing bitmaps (XARA-T-0271, XARA-T-0272)

| Piece | Where | State |
|---|---|---|
| Bitmap fill virtual points, `move_bitmap_control`, `set_bitmap_dpi`, `bitmap_fill_dpi`, `natural_length`; commands `MoveBitmapControl`, `SetBitmapTiling`, `SetBitmapDpi` | `xarast-doc/src/bitmap_fill.rs` | done (T10.3.1) |
| Bitmap fill handles (centre + edge middles; four corners in perspective), `stop_target` for a contone pair | `fill_handles.rs` | done (T10.3.2) |
| Fill tool on bitmap fills: drag, Constrain about the centre, Adjust aspect lock, tiling with Repeat inverted, `BitmapDpi` field, Natural size button (`ToolAction::NaturalSize`) | `fill_tool.rs`, `tool.rs` | done |
| `default_bitmap_attrs`, `bitmap_node_centred`, `PlaceBitmap` | `xarast-doc/src/bitmap_place.rs` | done (T10.3.6) |
| `Intent::ImportImage` (drop), `Intent::PasteImage` (clipboard picture), `Session::place_image`, `place` module (decode once, store, natural size, `bitmap_pixels`) | `place.rs`, `app.rs`, `session.rs`, `intent.rs` | done (T10.3.8) |
| Shell: image files in a drop are placed at the drop point; Paste falls back to the clipboard's picture | `xarast-shell/src/viewer.rs` | done |
| Bitmap gallery model: entries (uses, memory, colour space), thumbnails off-thread, Place, Delete unused, drag → bitmap fill or placement (`Intent::BitmapGallery`) | `bitmap_gallery.rs`; `xarast-doc` `bitmap_usage`, `remove_unused_bitmap`, `bitmap_key` | done (XARA-US-0055, T10.7.1–T10.7.3) |
| File › Import… (`AppCommand::Import`, `Intent::ShowImportDialog`), background imports with progress and cancel (`import.rs`, `Intent::CancelImports`), file-list paste (`place::image_paths_in_text`) | `import.rs`, `place.rs`, `app.rs`, `command.rs` | done (T10.7.4, T10.7.5) |

Tests: `tests/bitmap_fill.rs` (6: handle positions and arms; an edge drag
previews without writing, commits one "Move Fill Handle" step
pixel-identical to its preview, undo and redo exact; the centre moves the
whole fill and Esc leaves nothing; Adjust locks the aspect; tiling offers
Repeat inverted and writes both places; natural size and a typed
resolution resize about the centre, undo exact); `tests/place_bitmap.rs`
(4: paste at natural size in the view with the default attributes, one
exact step, decoded by the walker; a drop centred on its point at the
file's own 300 dpi; a non-image and a malformed picture change nothing;
the same picture twice shares one resource); unit tests in
`xarast-doc/src/bitmap_fill.rs` (6), `bitmap_place.rs` (1),
`xarast-app/src/place.rs` (4); shell: viewer tests (4: a synthetic drop
places at the drop point and still opens a `.xar` beside it, off-canvas
goes to the view centre; a non-image drop; Ctrl+V with a fake clipboard
picture is one undo step; our own copy wins over a picture) and
`tests/bitmap_damage.rs` (2: every preview frame and the commit of a
bitmap fill drag, and a placement, repaint only their object and match a
full render byte for byte).

XARA-US-0055 added `tests/bitmap_gallery.rs` (9: uses through place,
group, ungroup, delete and undo, Delete refused while used or held by the
history and outside the history when it works; a gallery drop on an
object is one exact "Set Fill" step at natural size centred on the
bounds; on empty canvas or on a bitmap object it places, one exact
"Place Bitmap" step each; a cancelled or off-canvas drag changes nothing;
thumbnails arrive; a background import lands on its drop point in
document space after a pan, one exact step; a cancelled import places
nothing; a file manager's copy imports each file; File › Import… asks
only with a document), unit tests in `import.rs` (3), `bitmap_gallery.rs`
(3), `place.rs` (+1) and `xarast-doc` (1: usage split, guarded removal);
shell viewer tests (3: the Import chooser and its answer, Ctrl+V of a
file list beats a picture, a gallery drag mapped to the canvas point)
and `tests/bitmap_damage.rs` (+2: a gallery drop as a fill repaints only
its object, as a placement only where it lands; a drag in flight
repaints nothing).

64. **Bitmap fill handles are virtual points** (facts:
    `Kernel/opgrad.cpp:3701-3747`, `Kernel/fillattr.cpp:14041-14215`).
    The fill stores three corners (`origin` = bottom-left, `axis_x` =
    bottom-right, `axis_y` = top-left); the canvas shows the centre (moves
    the whole fill, `FillHandle::Centre`), the middle of the x-axis edge
    (`End`) and of the y-axis edge (`End2`). A drag moves one virtual point
    and the corners are rebuilt from the three (`bitmap_real_points`,
    exact in integers), so the centre stays while an edge handle turns and
    stretches its axis. `move_control` on a bitmap fill delegates here, so
    `MoveFillControl` means the same thing. A fill **in perspective** shows
    its four corners (`Start`, `End`, `End2` = the perspective's top-left,
    which `axis_y` follows, `End3` = top-right) and the centre of the
    quadrilateral — ours; the original's perspective bitmap handles were
    not researched.
65. **Constrain and Adjust on a bitmap edge handle**, as the original:
    Constrain turns it about the centre (in our 15° steps, decision 50);
    **Adjust locks the aspect** — the other axis turns with it, stays
    perpendicular and scales by the same ratio. Ours: the lock keeps the
    side the other axis was on, so a mirrored fill stays mirrored (the
    original always turns +90°). Because the lock changes the result, the
    commit is `MoveBitmapControl { lock_aspect }` (label "Move Fill
    Handle"), not `MoveFillControl`: the command must reproduce the preview
    exactly (invariant 10).
66. **Bitmap tiling is written in both places.** The renderer lets a
    fill's own `tiling` win over the mapping attribute (`image.md`), so
    `SetBitmapTiling` sets both and neither can contradict the other. The
    menu is Simple / Repeating / Repeat inverted for bitmap fills only
    (decision 51 still holds for the others); unset shows as Repeating, the
    original's default.
67. **Resolution and natural size.** `SetBitmapDpi` keeps the centre and
    the axis directions and gives each axis `pixels × 72 000 / dpi` mp
    (`natural_length`, 96 dpi when unknown); labelled "Natural Size" when
    the dpi is the image's own, "Bitmap Resolution" when typed. The field
    shows the horizontal resolution the fill renders at, rounded. The
    image's size comes from the resource's layout, or from a header probe
    of its bytes when the `.xar` importer left the layout empty
    (`place::bitmap_pixels`). Perspective fills are left alone.
68. **Placing an image** (T10.3.8) is `Intent::ImportImage { path, at }`
    (a drop) or `Intent::PasteImage { width, height, rgba }` (a clipboard
    picture), both handled by `AppState` so a refusal becomes a notice
    ("Could not place the image: …") that changes nothing. The image is
    decoded once under `DecodeLimits::default()`; PNG, JPEG and GIF keep
    their bytes, anything else is stored as a lossless PNG with its
    resolution in `pHYs`; a clipboard picture has none and is 96 dpi. The
    resource goes in **outside the history**, deduplicated by content, as a
    pasted fragment's bitmaps do (decision 41); then `PlaceBitmap` (one
    step, "Import Bitmap" or "Paste") puts a `BitmapNode` of the natural
    size on the active layer, centred on the drop point or in the view, and
    the session selects it. The object carries the original's default
    bitmap attributes as its own children: **no line colour, no fill
    colour, zero line width** (`Kernel/nodebmp.cpp:997-1055`, facts) — not
    a bitmap fill as `research/02 §8.6` said; the object draws its image
    itself.
69. **Which drop is a placement, and which paste is a picture**
    (`xarast-shell/src/viewer.rs`). A drop with a document open places
    every file with an image extension (`place::is_image_path`) and opens
    the rest as before; the drop point is mapped through the canvas region,
    and a point off the canvas means "the view's centre". Paste asks the
    clipboard for a picture only when its text is neither our own copy nor
    SVG and no text caret is up; our copy therefore always wins.
70. **A gallery bitmap dropped on the canvas** (XARA-US-0055; facts
    `Kernel/sgbitmap.cpp:428-583`): on an object that is **not** a bitmap
    object (the drop pick, `Picker::pick_drop`, leaf mode) it becomes
    that object's own **bitmap fill** — `SetFillGeometry` on the fill
    slot, one "Set Fill" step — at the bitmap's natural size centred on
    the object's bounds, upright: origin = centre − half the size,
    `axis_x` along +x, `axis_y` along +y (`Kernel/fillattr.cpp:14259-14330`,
    the default for a fill with no points), tiling unset (= repeat), the
    image's own horizontal dpi. On a bitmap object or on empty canvas it
    is **placed** as a new bitmap object centred on the drop point
    (`Session::place_resource`, "Place Bitmap"). Off the canvas nothing
    happens. The drag lives in `Session::bitmap_drag`, changes nothing
    before the drop and is resolved again at the drop, like a colour drag.
    Not done: the original's Ctrl-drop on empty canvas, which sets the
    page background; and Ctrl ("inside") versus a group (XARA-T-0292).
71. **The gallery's uses** are `xarast_doc::bitmap_usage`: references
    (bitmap objects, fill and transparency attributes) from the live tree
    are `live`, from nodes only the history retains `retained`. Grouping
    and ungrouping keep the count (only attributes and bitmap objects
    count, not groups). **Delete is for a bitmap nothing refers to, the
    history included** (`remove_unused_bitmap`); like insertion it is
    outside the history (decision 68; XARA-T-0283), because an
    unreferenced resource draws nothing and the save sweep drops it
    anyway. The view is cached per document on (history serial,
    `resources_rev`, the bitmap ids), so a frame does not walk the arena.
72. **Background imports** (T10.7.5): a file over
    `import::INLINE_IMPORT_BYTES` (1 MiB) is read in 256 KiB chunks and
    decoded on its own thread; progress is bytes read (the first half of
    the bar) then "decoding" (a decode reports nothing). The drop point
    is converted to **document space when the import starts**, so a pan
    before it lands does not move it; it lands in the document it was
    started in, one "Import Bitmap" step. Cancel drops the job at once;
    a read stops at the next chunk, a decode under way runs out under its
    own deadline and is discarded. `AppState::with_inline_import_limit(0)`
    sends everything to the background (tests).
73. **Pasting a file manager's copy imports the files**
    (`place::image_paths_in_text`): the text must be a file list — every
    line a `file://` URI (empty or `localhost` host, percent-decoded) or
    an absolute path, after an optional `copy`/`cut` line and `#`
    comments — and only its image files are imported, each one step, in
    the view's centre. Ordinary text never reads as a list. Our own copy
    still wins. File › Import… (Ctrl+Shift+I; Ctrl+I is the original's
    image slicer) raises the same `ImportImage` per chosen file.

## Phase 10: photo adjustments (XARA-US-0054)

The photo panel (XARA-T-0301, `xarast_app::photo_panel`; the widgets in
`ui.md`, "Photo panel") edits the chain of the one selected bitmap
object. Its entry point is `Session::set_photo_ops(node, &ops)`: it reads the master's size
(`place::bitmap_pixels`), dispatches `xarast_doc::SetPhotoOps` as **one
undo step labelled "Adjust Photo"**, and records nothing when the chain
normalises to the one the object has (so re-applying a chain in another
order is not a step). It refuses a non-bitmap node (`WrongKind`) and a
chain holding an unknown operation (`NotPermitted`). A crop moves the
object so the kept pixels stay put; a turn swaps its width and height
about its centre (`image.md`, "Photo adjustments").

74. **A photo slider drag previews, it does not emit** — decision 45's
    pattern, not the colour editor's live gesture (`colour.md` 19).
    Applying `SetPhotoOps` each frame would put a full-resolution
    evaluation (≈ 60 ms plus a 200 ms pyramid on 24 Mpx) on the walk
    thread every frame and prune a derived image per frame.
    `PhotoPanelOp::Preview(chain)` stores `(node, chain)` in
    `Preview::photo`; the walker draws that object from a **proxy**
    (`image.md`); `Commit` takes it and calls `set_photo_ops` once —
    one "Adjust Photo" step; `Cancel`, `Intent::Cancel` (`Esc`, consumed
    by the drag so the selection stays), undo and redo drop it with
    nothing to undo (`photo_panel::settle`). The preview lives in the
    session's `Preview` because only a canvas gesture's commit/cancel
    clears that, and one pointer cannot drive both at once.
75. **Only tone previews; geometry is set.** A `Preview` whose crop or
    orientation differs from the object's would be drawn into the old
    placement (`SetPhotoOps` moves the object), so it is applied at once
    as `Set`. The panel edits crop and orientation with typed fields and
    buttons only. `Set`, `Reset` and keyboard steps are one step each; a
    chain with an unknown op is never previewed or set (`NotPermitted`).

Tests: `crates/xarast-app/tests/photo_panel.rs` (5): a 60-event drag
leaves digest and history untouched frame by frame, damages only its
object, evaluates nothing full-size, commits one step and undoes
exactly; `Esc` mid-drag restores digest, history, label, chain and
pixels; turns are set at once, levels preview on top, reset is one
step, an unknown chain is read-only; the committed render equals a
fresh walker's and a hand-baked full-resolution master's; a 24 Mpx drag
draws proxies only (its timing is the perf gate `photo-slider-24mpx`).

## Phase 13: the shadow and bevel tools (XARA-T-0338, C7, D6)

| Piece | Where | State |
|---|---|---|
| `LiveEdit` (`Apply`, `Remove`, `SetParams`, `SetJoin`), `EditCommand::Live { edits }` (one step), `effect_of`, `reselect`/`removed_members` | `xarast-app/src/live_edit.rs`, `ops.rs`, `session.rs` | done |
| `Preview::live` — controllers drawn with previewed parameters (`shadow_of`/`bevel_of` take the override) | `tool.rs`, `walker.rs` | done |
| `ShadowTool` (Ctrl+F2): drag-out, wall/floor/glow drags, infobar | `shadow_tool.rs` | done |
| `BevelTool` (Ctrl+F3): eight size handles, light blob + arrow, infobar | `bevel_tool.rs` | done |
| `ToolId::{Shadow, Bevel}`, `KeyChord::ctrl_f`, palette glyphs (own drawings) | `edit.rs`, `command.rs`, `xarast-ui/src/toolbar.rs` | done |

Tests: `tests/live_tools.rs` (14: registration, keys, cursor/status;
drag-out wall shadow = one exact "Apply Shadow" step, selection follows
into the controller; a wall drag leaves digest and history untouched
every frame, commits one step pixel-identical to its preview, `Esc`
leaves nothing; floor lean/stretch with the ±69°/1–150 % clamps and its
typed fields; glow widening and its clamp at 0; every shadow field typed,
one exact step each, the same value again no step, a gain slider drag
previews then commits once; apply + remove is the identity by digest
and pixels; clicks on the object or on the shadow alone select the
controller (A7), the selector too; bevel size handles in → inner, out →
outer, preview = commit; light blob position, drag, Ctrl 45° snap; every
bevel field one exact step; a bevel nests inside a shadow and comes out
again; a locked layer refuses; the golden `tests/golden/live_tools.png`
of a shadow and a bevel made by scripted gestures). Unit tests in
`bevel_tool.rs` (4).

### Facts from the original (paraphrased; `tools/shadtool.cpp`, `shadinfo.cpp`, `opshadow.cpp`, `bevtool.cpp`, `bevinfo.cpp`, `opbevel.cpp`, `Kernel/nodecont.{h,cpp}`, `attrbev.cpp`)

- **Shadow, drag** (`shadtool.cpp:1167-1314`, `1459-1566`): a drag on empty
  space deselects; on an unselected object (or its shadow) selects it and
  acts on it alone; on a selected one acts on the selection. A target
  without a shadow (or Shift held) gets a new **wall** shadow in the same
  op, then the drag edits it. Wall: offset = start offset + pointer delta,
  no constraint. Floor: the ratio of shadow height to pointer height and
  the sideways offset are fixed at the press; height = vector.y / object
  height in 0.01..1.5, angle from the vertical within ±69°
  (`nodecont.h:188-189`); a press at or below the base does nothing.
  Glow: width = start + dx + dy, each sign-flipped by the side of the
  centre the press was on, ≥ 0. Every same-type shadow in the selection
  takes the master's value. **No on-canvas handles** (tiny blobs only);
  the live shadow is the feedback. One undo step per drag.
- **Shadow keys**: plain 1–4 = none/wall/floor/glow, A = select the
  shadows (`shadtool.cpp:1041-1098`). Ctrl and Alt unused.
- **Shadow infobar** (`shadinfo.cpp`): type buttons None/Wall/Floor/Glow
  (apply when the selection has no consistent shadow, change type
  otherwise, None removes); blur slider 0–40 px (0..30000 mp, typed values
  outside refused); transparency 0–100 % (= 100 − darkness); bias/gain
  profile; X/Y in pixels for wall shadows only (±999 px) with nudge
  buttons. **No angle, height or glow-width control in the LX bar** (the
  constants exist, no gadget). Slider drags preview live and are one op.
- **Shadow defaults** (`nodecont.cpp:183-192`): wall, offset (+3750,
  −3750), blur 4500, darkness 0.25, floor 45° at 0.5 height, glow width
  3000 — `ShadowParams::default()` already matched. A new **glow is red**,
  the rest black (`opshadow.cpp:468-474`). One controller per selected
  object.
- **Bevel, drag** (`bevtool.cpp:3373-3650`): only from one of **eight size
  handles** round the selection's bounds; elsewhere a drag does nothing.
  Width = (signed width now − at the press) + the original signed indent;
  signed width = distance to the nearest edge inside the box (inner),
  negative outside (outer). Finish applies a bevel of that width, Flat,
  contrast 50, light angle 300, tilt the default 32. Feedback: a red XOR
  outline of the contoured path, no live bevel. Snaps.
- **Bevel light** (`bevtool.cpp:2284-2575`): one arrow per group of
  bevels sharing a light angle, from the group's centre, 40 px, head at
  the centre; a red blob 55 px out on the side opposite the angle's
  direction; only the blob is hit; dragging it points the light at the
  pointer; Constrain snaps down to the constrain angle; live re-render;
  one op.
- **Bevel infobar** (`bevinfo.cpp`): type combo "None" + 15 styles in the
  order Flat, Rounded (SMOOTH_1), Rounded 2, Chiselled (MESA_1),
  Chiselled 2, Round Frame (ROUND), Round Frame 2 (HALFROUND), Flat Top
  Frame (FRAME), Point Frame 1–3, Ruffle Frame 1–4 — **not** the record
  order. A type chosen with no depth applies 3000 mp, outer. One shared
  slider switched between Size (0..250000 mp, non-linear), Contrast
  (0–100), Light angle (0–360, shown as 360 − value), Elevation (0–90).
  Inner/Outer buttons, join buttons Mitre/Round/Bevel. Sliders apply on
  release (one op).
- **J3**: during shadow drags the original sets a quick-render flag that
  only stops new caching; shadows render at full quality while dragging
  (the controller's check is commented out, `nodecont.cpp:3897-3901`).
  Bevels have no draft mode either.

### Decisions (ours)

82. **One command, one step.** `EditCommand::Live { edits }` runs a list
    of `LiveEdit`s as one undo step labelled by the first: "Apply Shadow",
    "Remove Shadow", "Shadow Type", "Shadow Position", "Floor Shadow",
    "Glow Width", "Shadow Blur", "Shadow Transparency", "Shadow Profile";
    "Apply Bevel", "Remove Bevel", "Bevel Type", "Bevel Side", "Bevel
    Size", "Bevel Contrast", "Bevel Light Angle", "Bevel Light Elevation",
    "Bevel Join" (ours, not the original's strings). A list that changes
    nothing returns `NOTHING_TO_DO`, which `run_tool` turns into no step;
    the tools also skip equal values.
83. **Regeneration is not a step, by construction**: shadows and bevels are
    pixel effects (`document-model.md` 48, 49), so `SetParams` is one
    `set_kind` on the controller *and* its generated and source nodes
    (kept equal, as the readers build them). Undo is the inverse
    `set_kind`; the canonical digest proves it.
84. **Apply wraps in place; remove is ungroup.** The controller takes the
    object's place with no attributes, so inheritance is unchanged; remove
    moves the source's objects out before the controller with
    `structure::materialise` (localise) and deletes it. Apply then remove
    is the identity by digest. Wrapping twice is skipped (`effect_of`).
85. **A bevel lives inside a shadow.** Applying a bevel to a shadow
    controller wraps the shadow's source contents; applying a shadow to a
    bevel wraps the bevel. `effect_of(doc, n, Bevel)` finds a bevel that
    is a shadow's only object, so the bevel tool edits it through the
    selected shadow.
86. **The selection follows** (`live_edit::reselect`): after a `Live`
    command `run_tool` replaces each wrapped object by its controller and
    each removed controller by its objects. A7 needed nothing: the pick
    index already reports the layer child (the controller) as
    `top_group`. The shadow tool adds a hit on the **shadow's own area**
    (`shadow_under`: the source box through `silhouette_map`, a glow
    grown by its width).
87. **Previews.** A drag on an existing effect writes `Preview::live`; the
    walker reads the override in `shadow_of`/`bevel_of`, and
    `scene_damage` sees the changed `PushEffect` op by equality, so only
    the effect's area repaints. A **new** shadow or bevel is not in the
    document during its drag: its preview is an overlay (the objects'
    boxes moved by the offset; the box inset or outset by the width,
    dashed — the original's XOR outline). The shadow bias/gain sliders
    preview the same way and commit once (decision 81;
    `settle_infobar_drag` clears `live` too).
88. **Shadow tool, ours**: Shift does not stack a second shadow; keys
    1–4/A are not bound (1 and 3 are zoom keys here); the infobar adds
    angle and height (floor) and width (glow), so every parameter is
    typed (J2); X/Y and blur are lengths in the user's unit, not pixels;
    out-of-range values are clamped to the original's limits rather than
    refused. The colour is set from the colour bar (decision 91).
89. **Bevel tool, ours**: the size box is the union of the selected
    objects' *source* boxes (a bevel's own growth excluded); an existing
    bevel previews live plus the dashed box, a new one the dashed box only;
    Ctrl rounds the light to 45° (the original floors to its constrain
    angle preference); the light angle field shows the stored value
    (direction of travel, anticlockwise), not 360 − value; elevation is a
    typed 0–90° field; the size field is linear. Tool-made bevels take the
    original's drag defaults (`bevel_tool::new_bevel`: Flat, 50 %, 300°,
    32°); a type chosen for a bare object 3000 mp outer
    (`chosen_bevel`). `BevelParams::default()` (inner, 0.75 pt, 45°/45°)
    is left alone. Join edits the controller's own `JoinType` attribute
    (`LiveEdit::SetJoin`), which strokes its objects too.
90. **J3**: drags already run at Draft through the scheduler (Final 120 ms
    after input stops), but the effect pass ignores the quality: a shadow
    or bevel preview is computed at full resolution every frame, as in the
    original. A reduced-resolution Draft effect is a render-crate change
    (follow-up, with XARA-T-0315).

91. **Shadow follow-ups (XARA-T-0339).**
    - *F5/F6 bug*: `ToolId::Fill` and `ToolId::Transparency` were missing
      from `AppCommand::ALL`, so the shell (which binds keys by walking
      `ALL`) never bound them. Added; the test
      `every_tool_with_a_key_is_in_the_command_table` walks `ToolId::ALL`
      so a new tool without a table entry fails. **A new tool must add its
      `AppCommand::Tool` to `ALL` (and bump its length).**
    - *Colour*: the infobar shows a read-only `InfobarItem::Swatch`
      (`InfobarField::ShadowColour`, mixed selection = empty). A **left
      click** in the colour bar while the Shadow tool is in force and the
      selection has a shadow sets the shadows' colour (one "Shadow Colour"
      step via `shadow_tool::colour_command`) and does not touch the
      fill; right/Shift-click (line) and a selection without a shadow
      behave as ever. Dropping a dragged colour on a shadow is not done
      (drops still target the object's fill), and the colour editor does
      not target shadows.
    - *Nudges*: arrows move the selected **wall** shadows' offset by the
      standard `Nudge` (same unit/modifiers as everywhere), clamped to
      ±999 px; a run is one "Shadow Position" step (the `Live`
      coalescing); floor/glow shadows ignore the keys (the arrows pan).
    - *Bevel size drag outline*: `bevel_tool::size_regions` collects the
      clean silhouettes of the ink nodes under each selected object (the
      bevel source for a bevelled one) once at the press;
      `xarast_geom::contour::offset_region` (round joins) offsets them by
      the signed width every frame and they are drawn as dashed polylines.
      More than 24 objects, or nothing contourable (text), falls back to
      the dashed box. No snapping.
    - *Not done, by decision*: the original's Shift-drag second shadow and
      plain keys 1–4/A stay unbound (1 and 3 are zoom keys here; the
      infobar's type choice covers 1–4 and Shift-click selection covers
      A); revisit only if users ask. Draft-quality effects stay with
      XARA-T-0315.

## Phase 13: the contour and blend tools (XARA-T-0340, E6, F8)

| Piece | Where | State |
|---|---|---|
| `LiveEffect::{Contour, Blend}`, `LiveEdit::Blend { start, end, kind }`, contour apply/remove/side, blend new/add/merge/remove, `contour_start`, `blend_members`, `blend_holding` | `xarast-app/src/live_edit.rs` | done |
| `ContourTool` (Ctrl+F7): eight size handles, live resize, dashed outline of a new contour, infobar | `contour_tool.rs` | done |
| `BlendTool` (F7): object-to-object drag with a line, add to a blend at the touched end, merge, Ctrl-drag an end (live), infobar, Remove | `blend_tool.rs` | done |
| `ToolAction::RemoveEffect` (infobar "Remove" button) | `tool.rs`, `command.rs` | done |
| Previewed regeneration: `RegenOverride` (`kind`, `moved`), `regen_key_with`, `regenerate_with`, `LiveCache::current_or_regenerate_with`/`regenerated`, `contour::generate_with`, `blend::generate_with` | `xarast-doc/src/regen.rs`, `contour.rs`, `blend.rs` | done |
| Walker draws `Preview::live` and `Preview::transform` into contours and blends (`live_output`); `WalkStats::live_regenerated` | `walker.rs` | done |

Tests: `tests/contour_blend_tools.rs` (17: registration, keys,
cursors, status; handle out = outer contour, one exact "Apply Contour"
step, dashed outline while dragging, lightened start colour; handle in =
inner contour after its source; a resize regenerates **exactly once per
drag frame** and none for a repeated frame, digest/history untouched,
commit = preview, `Esc` leaves nothing; the render thread's repaint of a
shrinking and of a growing contour equals a fresh full frame; every
contour field typed, one exact step each, same value no step, clamps; a
colour-profile slider previews and commits once; apply + remove =
identity, one contour per object; a contour inside a shadow, none on a
bevel; object → object blend, one exact "Blend" step, members in order
with one generated node between; drags to nothing/itself/`Esc` do
nothing; an object joins a blend at the touched end (both ends); a
Ctrl-dragged end regenerates once per frame and commits one "Move" equal
to its preview; every blend field one exact step, one regeneration, the
generated nodes carry the parameters; blend + remove = identity; two
blends merge; the golden `tests/golden/contour_blend_tools.png`). Unit
tests in `contour_tool.rs` (2) and `xarast-doc` `tests/blend.rs`
(the override keys and outputs, the cache counter).

### Facts from the original (paraphrased; `tools/cntrtool.cpp`, `tools/opcntr.cpp`, `Kernel/ncntrcnt.cpp`, `wxOil/xrc/barsource/cntrbar.ini`, `tools/blndtool.cpp`, `Kernel/nodeblnd.cpp`)

- **Contour drag** (`cntrtool.cpp:463-638`, `1180-1233`, `1289-1558`,
  `3836-4109`): only from one of eight handles round the selection's
  bounds (corners and edge middles; hit area about twice a blob); a drag
  elsewhere does nothing. Signed width against the selection's box:
  outside = negative (outer), the distance to the nearest edge (the
  larger of the x and y gaps diagonally); inside = positive (inner), the
  distance to the nearest edge; |width| clamped to 10..250000 mp. Applied
  width = signed(pointer) − signed(handle) + the first selected contour's
  signed width. No modifiers; the pointer snaps. Feedback: a red XOR
  hairline of the offset paths (existing contour's join, mitre by
  default); the infobar's width and side update live. Release: with no
  contour selected one create op (3 steps, the dragged width, the inset
  flag), else one change-width op; Esc or no movement = no op. Inner
  widths are clamped to the largest possible (`opcntr.cpp:1580-1600`). A
  click goes to the selector; a bevel in the selection refuses
  ("Can't contour bevelled objects", `cntrtool.cpp:779-838`).
- **New contour** (`ncntrcnt.cpp:187-192`, `2689-2693`, `2833-2969`):
  model defaults 5 steps, 5000 mp, fade, no inset; the tool uses 3 steps
  (`cntrtool.cpp:3243`, `4089`) and the slider/edit creation is always
  outer. The contour node copies the object's colour fill, transparency
  fill, stroke colour, stroke transparency, line width, join and the two
  mappings; a visible flat fill is lightened in HSV (V four fifths of the
  way to full, S divided by 5) unless inset; with no fill the first
  colour fill under the controller is cloned and lightened; the winding
  rule is forced negative. Outer = node before the first ink child,
  inner = last child (`2904-2916`). An open path first gets a flat fill
  in its stroke colour (`opcntr.cpp:328-389`).
- **Contour infobar** (`cntrbar.ini:20-46`, `cntrtool.cpp:1682-3256`):
  steps/distance toggle and one edit (steps 0..999, entry n stored n − 1,
  shown steps + 1; distance = |width| / (steps + 1), > 0); Remove
  button; effect combo fade/rainbow/alt-rainbow; width slider 10..250000
  (logarithmic, bias 0.7), XOR preview while sliding, op on release,
  keeps each contour's side, with none makes an outer one; width edit
  (≤ 0 warns); Outer/Inner buttons (mixed = both off, Outer lit by
  default); object and attribute profiles (only when everything selected
  is a contour); inset-path toggle (disabled inside a blend); join
  buttons apply a join attribute through the attribute manager. No tool
  keys; no hotkey found in the original's tables (`research/04` gives
  Ctrl+F7). Cursors: pointer, a select cursor over a handle. Status:
  "click select; drag handles to create a contour or change its width".
- **Contour remove** (`opcntr.cpp:745-1063`): greyed without a contour;
  localise, move the objects out before the controller, select them,
  hide the controller; the lightened copy goes. One contour per item of
  the selection (a group is one); existing contours dissolve first;
  a shadowed object's shadow source is contoured (`opcntr.cpp:257-479`).
- **Blend drag** (`blndtool.cpp:584-1143`, `3138-3328`, `4230-4630`,
  `4783-4841`): starts on any object that can become a path; a straight
  XOR line (about 2 px, black; red and thicker for point mapping/remap)
  from the press to the pointer; the end is hit-tested only on release;
  a missing end or the same object does nothing. Dispatch: blend + blend
  merge (the end blend's children move into the start blend, which stays
  selected); blend + object (either order) adds a stage at the end whose
  member's box centre is nearest the drag point; else a new blend. No
  modifiers; a click goes to the base tool. One op, "Blend". Point
  mapping (drag between end-point blobs of selected paths) and remap
  (between corresponding blobs of a selected blend) exist.
- **New blend** (`nodeblnd.cpp:143-258`, `blndtool.cpp:4293-4310`):
  5 steps, one-to-one off, fade, tangential off, antialias on, steps
  mode. All deselected; the blend is inserted right after (above) the
  end object; the start then the end move in (start lower) with the
  blender after the start; common attributes are factored onto the
  blend; the blend is selected. Ends are promoted to enclosing
  shadow/bevel/contour controllers (`3241-3259`).
- **Blend infobar** (`blndtool.cpp:1412-2431`): one steps/distance edit
  applied on commit (1..999; distance only for blends on a curve with a
  linear profile); effect combo fade/rainbow/alt-rainbow, applied at
  once; one-to-one and antialias toggles; tangential only on a curve;
  position and attribute profiles (a run of drags is one undo step, only
  when the selection is all blends); Remove; blend-along-path toggle.
- **Blend ends** (`nodeblnd.cpp:3010-3045`, `blndtool.cpp:8943-9296`):
  only blends on a curve show end blobs; dragging one shows XOR
  outlines and recalculates the blend **on drop**, not live. Off-curve
  ends move through the selector (select inside).
- **Remove blend** (`blndtool.cpp:5437-5706`): localise; every object
  (not the blenders) moves before the blend's place in its order and is
  selected; a blend path is detached; refused when the blend is
  bevelled or contoured. "Remove blend". F7 = tool 16
  (`Kernel/tool.h:145`, `STANDARD_HOTKEYS.res:90`); no tool keys.

### Decisions (ours)

92. **Contours and blends regenerate; the edit is the command.** A
    parameter change is one `set_live_kind` on the controller and its
    generated/source nodes — for a blend also the generated nodes
    *under the source* (`Tx::set_live_kind` now covers them, so a
    tool-edited blend digests as a reloaded one). Rings and steps are
    never written into the tree; the walker recomputes them keyed on the
    document's state. Undo is the inverse `set_kind`.
93. **Previews regenerate under an override, not in the document.**
    `Preview::live` (other parameters) and `Preview::transform` (an end
    moved) reach `regenerate_with` through `RegenOverride`; the key
    (`regen_key_with`) hashes the override's parameters in place of the
    controller's and the moved nodes' tags plus the matrix, so a drag
    frame regenerates **once**, a repeated frame never, and the commit
    regenerates once more (the content revisions moved). A move of the
    controller itself (or anything above it) is inert: the walker's
    group transform moves the output whole. Moved ends transform their
    outline and every geometry-linked attribute (fills), as the walker's
    group transform draws the moved object itself. A controller with a
    previewed kind is never culled, and `Session::rebuild_scene` widens
    the scene ink by a previewed contour's growth.
94. **A generated node may be furnished in the transaction that made
    it.** `Tx::attach` lets a node created in this transaction become
    the `LastChild` of another node created in it, even a generated one
    (not a locked one): a new contour's start attributes. Every other
    edit of generated data is still refused. A contour that changes side
    moves its **source** (after the controller's attributes for inner,
    last for outer); the generated node never moves.
95. **Contour tool, ours**: the box is the union of the selected
    objects' own boxes (a contour's source, rings excluded); four rings
    (the original's 3 steps); the steps field shows rings (1–999) and
    stores rings − 1, as the original's box; the width is a typed length
    clamped to 10..250000 mp (not refused); a width or a side chosen for
    bare objects gives them a contour (the side: 5 pt; the width:
    outer), where the original's buttons do nothing; join is the
    parameter (`ContourParams::join = Some`), not a join attribute; the
    inner-width clamp to the largest possible inset is not done (the
    generator lets inner rings collapse); no snapping; an open path is
    not given a fill first. New contours copy the eight slots and lighten
    a flat fill (`live_edit::contour_start`); the "no fill" clone is not
    done. Feedback for an existing contour is the live contour; for a new
    one the dashed offset outline (mitre), or the dashed box past 24
    objects.
96. **Blend tool, ours**: the drag starts on any object the picker hits
    (not only path-convertible ones); the line is a solid overlay
    polyline; the cursor is a crosshair over a target. A new blend stands
    right after the end object with the start beneath (as the original)
    but common attributes are **not** factored onto the controller (each
    object keeps or gets — `materialise` — what it inherited); an object
    joining a blend is localised the same way; merging takes the end
    blend's objects (its pair parameters are dropped: the start blend's
    apply). Undo labels "Blend", "Remove Blend", "Blend Steps", "Blend
    Colours", "Blend One To One", "Blend Antialias", "Blend Profile",
    "Blend Attribute Profile"; contour: "Apply Contour", "Remove
    Contour", "Contour Steps", "Contour Width", "Contour Side", "Contour
    Join", "Contour Colours", "Contour Profile", "Contour Colour
    Profile", "Inset Path". **Ends move live**: Ctrl-drag an end in the
    blend tool (or move one selected inside with the selector) and the
    steps follow every frame (the original recomputes on drop only). The
    effect list adds "None" (no colour blend, the model's fourth type).
    Not done: point mapping and remap, blend along a path (F6), the
    distance mode, tangential, the "long job" question, refusing to
    remove a bevelled/contoured blend.

## Phase 13: the mould tool (XARA-T-0342, G9)

| Piece | Where | State |
|---|---|---|
| `LiveEffect::Mould`, `LiveEdit::Mould { targets, kind, label }` (the whole selection in **one** controller), `mould_params` | `xarast-app/src/live_edit.rs` | done |
| `MouldTool` (Shift+F6): presets, cage drags, vanishing points, numbers, copy/paste, rotate, detach, mesh, remove | `mould_tool.rs` | done |
| `ToolAction::{CopyMould, PasteEnvelope, PastePerspective, RotateMould}`; six `InfobarField::Mould*` | `tool.rs` | done |
| Previewed cage: `mould::generate_preview(doc, c, params, text)`, `regenerate_with` honours a `Mould` override, `paint_mould` keys by `regen_key_with` | `xarast-doc/src/mould.rs`, `regen.rs`, `walker.rs` | done |
| Scene ink widened by a previewed cage's extent | `session.rs` | done |
| Picking a moulded object by its **moulded** outline (A7, part of XARA-T-0337) | `picking.rs` (`moulded_leaf`) | done |
| `vanishing_points`, `drag_vanishing_point`, `Vanishing` | `xarast-geom/src/mould.rs` | done |

Tests: `tests/mould_tool.rs` (11: registration, key, cursors, status,
12 cage handles drawn; a preset moulds a two-object selection into one
controller, objects in render order, one exact "Apply Envelope" step,
the rectangular preset is the identity cage; apply + remove = identity
by digest and pixels for an envelope and a perspective, selection back
to the objects; a corner drag regenerates **once per frame** and none
for a repeated frame, digest/history untouched, the corner carries its
two handles, commit = preview pixels, one exact "Edit Mould", a
Constrain-dragged handle moves alone on 45°, `Esc` leaves nothing; a
folded perspective commits nothing (the status says so) and a floor's
vanishing point drags in one exact step with the base fixed; every
number (point list, X, Y, accuracy clamp, same value no step, a folding
value refused) and a preset replacing a selected mould's shape over its
own source ("Mould Shape"); copy, paste perspective in place, paste
envelope centred with Constrain, rotate (four turns = identity);
detach: drags change nothing until attach, then one "Edit Mould"; the
mesh adds 14 lines and no step; a click on the undeformed source
outside the moulded copy picks nothing, inside it the controller; the
golden `tests/golden/mould_tool.png`). Unit tests in `mould_tool.rs`
(4: presets fit and are usable, corner/handle moves, rotation, kind
conversion) and `xarast-geom` `mould.rs` (2: vanishing points, their
drag).

### Facts from the original (paraphrased; `tools/moldtool.cpp`, `Kernel/moldedit.cpp`, `Kernel/moldenv.cpp`, `Kernel/moldpers.cpp`, `Kernel/nodemold.cpp`, `wxOil/xrc/EN/mouldbar.xrc`, `mike-strings.xrc`)

- **Tool**: `TOOL19`, "Mould Tool Shift+F6"; no tool keys. The infobar
  holds: default envelope, default perspective, copy mould, paste
  envelope, mesh toggle, circular / elliptical / concave / banner
  envelopes, left / right / floor / ceiling perspectives, paste
  perspective, Remove, Detach (a toggle button), Rotate contents.
  Status: a default message, "drag vanishing point" over one, "drag
  coordinate" over a cage point (`moldtool.cpp:678-748`). The tool draws
  no blobs of its own: the selected mould's cage path shows its path
  blobs and a perspective its vanishing point blobs.
- **Create** (`moldedit.cpp:188-456`): one mould round **the whole
  selection**, inserted after its last (topmost) node; the objects move
  into the mould group. When every selected object is a mould, each
  mould's shape is replaced instead, fitted to that mould's own source
  rectangle (`moldedit.cpp:825-940`). Presets are drawn on a 72 000 mp
  square and scaled to the selection's bounds; the circular envelope
  locks its aspect: a square on the larger side about the centre
  (`FindScaleRect`, `moldedit.cpp:500-527`). Greyed while anything
  selected sits in a "needs parent" node (`moldtool.cpp:1197-1245`).
- **Presets** (unit square, cage from the bottom-left clockwise):
  rectangular = the box; circular/elliptical = a circle with its four
  corners on the diagonals (an ellipse of quarter arcs, handle inset
  1/4.4672 of the box, i.e. the usual circle constant, turned 45°);
  concave = straight top and bottom, the left side bowing in with
  handles at x = 3/8 (y = 2/8, 6/8), the right at x = 5/8; banner =
  straight sides, the top bowing down with handles at y = 5/8
  (x = 2/8, 6/8), the bottom up to y = 3/8. Perspectives (BL, TL, TR,
  BR): left wall (0,0)(0,1)(1,5/8)(1,3/8); right wall
  (0,3/8)(0,5/8)(1,1)(1,0); floor (0,0)(3/8,1)(5/8,1)(1,0); ceiling
  (3/8,0)(0,1)(1,1)(5/8,0). A 2×2 envelope preset exists but its button
  is commented out.
- **Editing the cage**: the mould's path node is edited by the generic
  path drag (an end point carries its adjacent control points; a control
  point moves alone). After the edit the mould validates the new shape
  and the op **fails** when it is not valid (`nodemold.cpp:2046-2090`):
  an envelope needs width and height, a perspective no coincident
  corners and convexity. The contents re-mould on the drop, not live (an
  XOR outline while dragging).
- **Vanishing points** (`moldpers.cpp:1236-1400`, `1840-1980`,
  `moldedit.cpp:1649-1815`): where the left and right sides meet and
  where the bottom and top meet, when not parallel; a click on one
  starts its drag. The corners on the side the point lies beyond slide
  along their own edge's line, on the lines from the two far corners
  through the pointer; the far edge stays. The contents re-mould on the
  drop; one op, "drag vanishing point".
- **Copy** puts the mould's path on the clipboard as a plain path
  object; **paste envelope/perspective** takes the first clipboard
  object that can become a path and uses it as the shape **where it
  is**; with Constrain (or when it would land off the pasteboard) it is
  centred on the selection first (`moldtool.cpp:2053-2160`). It creates
  or replaces moulds as Create does. Remove with Constrain also puts the
  first mould's path on the clipboard.
- **Remove** (`moldedit.cpp:545-690`): per selected mould, localise the
  attributes, move the objects out after the mould (order kept), select
  them, hide the mould.
- **Rotate contents** (`moldedit.cpp:1074-1110`): the path's start moves
  to its next end point, so the contents turn a side round the cage.
- **Detach** (`moldtool.cpp:2724-2900`, `nodemold.cpp:1987-2000`,
  `2370-2445`, `2530-2546`): a per-mould flag; while set, editing the
  cage does not re-mould; clearing it re-moulds by the cage as edited.
  It is **not saved** (saving a detached mould warns).
- **Mesh** (`moldenv.cpp:376-520`): a per-envelope toggle (not saved):
  the seven inner lines of an 8 × 8 grid over the source, plotted as
  dots through the envelope; perspectives draw none.

### Decisions (ours)

97. **One mould for the selection, one step per gesture.** Presets and
    paste wrap the selection in one controller (`LiveEdit::Mould`, which
    `relocate`s each object so it looks as it did) or, when the
    selection is all moulds, `SetParams` on each over its own source.
    Labels: "Apply Envelope", "Apply Perspective", "Mould Shape",
    "Edit Mould" (drags, X/Y, attach), "Mould Accuracy", "Paste
    Envelope", "Paste Perspective", "Rotate Mould", "Remove Mould".
    Regeneration is never a step: the cage is a parameter
    (`document-model.md` 52) and `SetParams` is one `set_live_kind`.
98. **Live cage drags.** Unlike the original (re-mould on the drop), the
    contents follow every frame: `Preview::live` carries the dragged
    `MouldParams` and `paint_mould` regenerates under `RegenOverride`
    (once per changed frame). A cage that is not usable (a folded or
    collapsed perspective, a flat envelope) is not previewed: the
    overlay shows the cage being dragged, the status says it would fold,
    and release commits nothing, as the original's validation refuses
    it. The rectangular presets use `identity_cage` exactly; the others
    go through `cage_from_path` (the original's straight-side thirds).
99. **Handles**: end points (perspective corners, envelope corners, a
    2×2's side middles) are hit before control points; Constrain keeps
    the move to 45° steps (and does not snap), otherwise the dragged
    point snaps. Vanishing points are `HandleShape::Centre` with two
    dashed construction lines to the nearest corners; their drag is
    computed from the press's corners every frame
    (`drag_vanishing_point`) and refused when the result is not a valid
    perspective.
100. **J2 numbers**: a point list (`MouldPoint`, names per kind) and the
    shared `X`/`Y` fields place that point (an end point carries its
    handles, as a drag does); `MouldThreshold` ("Accuracy") is the
    refitting threshold, clamped to 0..18 000 mp (ours; the original has
    no field). A click on a cage point picks it for the fields.
101. **Copy/paste is the tool's own**: copy keeps `(kind, cage)` in the
    tool (not the system clipboard; no other path can be pasted as a
    mould); paste converts between kinds (a perspective from an
    envelope's corners, an envelope from straight sides) and places the
    cage where it was kept, centred on the selection with Constrain.
    Remove with Constrain does not copy.
102. **Detach is tool state**, as the original's flag is unsaved: a
    detached mould's cage edits (drags, numbers, presets, rotate) go to
    a cage held by the tool and drawn in the overlay; the document is
    untouched until Detach is cleared, which commits one "Edit Mould".
    Held cages of moulds that are gone are dropped. Saving while
    detached saves the attached cage (no warning).
103. **Mesh for both kinds**: the 7 + 7 inner lines of the 8 × 8 grid,
    sampled 32 times each, drawn as dashed polylines through the mould
    (perspectives too; the original draws envelopes only). One toggle
    for the tool, not per mould; never a step.
104. **Picking through the mould** (part of XARA-T-0337): the picker
    skips a mould's source and indexes its generated node as **one leaf
    keyed by the controller**: every moulded shape's outline, filled
    non-zero when any shape paints a fill, stroked as the first shape
    that paints a stroke. A constrained (leaf) click therefore selects
    the mould, never something inside it. Text inside a mould is not
    outlined by the picker (it has no fonts): a mould of text alone
    falls back to its box. Object snapping sees a mould only through
    this leaf.

## Phase 13: the transparency type selector carries the family (XARA-T-0347, I5)

`fill_tool::transp_mode_choice(mode)` builds the infobar choice: the ten
offered modes in the original's menu order, plus, when the family in
force is not one of them (Bevel), that family appended and selected, so
all twelve are shown as they are. Choosing the appended entry is a no-op
(the setter only maps indices into `TRANSP_MODES`). `None` shows as Mix.
`transp_mode_label` is exhaustive over the twelve. Test:
`tests/fill_tool.rs::the_transparency_selector_carries_all_twelve_families`.

## Provisional values (observe in the VM before trusting)

- Segment grab = 4 + 3 device px; freehand chunk 96 samples; freehand
  closes when its ends are within the 6 px grab distance.
- Drag threshold 4 device px; double click 500 ms / 6 px; pick tolerance
  3 device px; handle grab 6 device px; auto-scroll band 16 px, step ≤ 24
  px/frame; constrain angle 45°.
- Nudge unit 1 mm (`NUDGE_UNIT_MP`) until the document's own nudge size
  (tag 4114) is kept.

## Invariants that must not be broken

1. `ToolCtx` has no `&mut Document` (compile-fail doctest).
2. Nothing is emitted before `DragEnd`; `Cancel` leaves the canonical
   digest and the history length unchanged (`tests/tools.rs`, 10 cut
   points).
3. A drag is one undo step, labelled ("Move", "Scale", "Rotate", "Skew",
   "Create Rectangle", "Create Ellipse", "Edit Shape").
4. The machine, not the tool, clears the preview on commit and cancel.
5. `edit.tool.drag_from` mirrors `ToolMachine::is_pressed`; the shell's
   shortcut gate reads it.
6. A rectangle or ellipse stays a `QuickShape` through every transform and
   edit (`parametric_shapes_stay_parametric_through_transforms`).
7. The pick index never sees a drag frame: it is invalidated only by
   committed mutations.
8. Selecting path points never changes the path (`Arc::ptr_eq`,
   `tests/node_edit.rs`); a node drag, a pen segment and a freehand
   stroke are each one undo step.
9. The shape editor never changes a quick shape's kind except through
   `ConvertToPaths`.
10. A fill or transparency drag emits nothing before release; the release
    is one `EditCommand::Fill` step, and what the preview drew is what the
    commit renders (pixel-identical, `tests/fill_tool.rs`).
11. The text tool never mutates the document except by typing and by
    infobar/ruler attribute edits: entering, selecting, navigating,
    preparing a new story and choosing a character attribute at a caret
    leave the canonical digest and the undo history as they were
    (`tests/text_tool.rs`, `tests/text_infobar.rs`); a click on empty
    canvas never creates an empty story. A deletion that leaves a story
    without characters removes it in that same undo step (decision 70).
    **The one exception to "leaving is no edit" (XARA-T-0295):** leaving
    a story the tool edited that now holds only paragraph breaks (and
    tabs) removes it, merged into the burst that left it so when that is
    still the last step, else as its own "Delete Text" step; a story only
    entered is never touched.
12. While a text caret is up, Delete/Backspace/Enter never reach the
    object-level commands.
13. A typing burst is one undo step and one `Ctrl+Z` restores the digest
    from before it, story creation included
    (`typing_200_characters_is_one_undo_step`,
    `the_first_character_at_a_pending_caret_creates_the_story`).
14. An infobar attribute edit is at most one undo step, and none when the
    value is already in force; at a caret it merges into the typing step
    it styles.
15. A bitmap fill handle drag emits nothing before release and commits
    one step whose render equals the preview (`tests/bitmap_fill.rs`); a
    placed bitmap is one step and adds at most one resource
    (`tests/place_bitmap.rs`). Undo of a placement restores everything but
    the resource table, which is outside the history (decision 68).
16. A photo slider drag emits nothing before release: the document
    digest and the history are unchanged on every frame, the release is
    one "Adjust Photo" step, and `Esc` leaves nothing
    (`tests/photo_panel.rs`).
17. An infobar slider drag emits nothing before release (digest and
    history unchanged every frame), the release is one step whose render
    equals the preview, and `Esc`, undo, redo or a tool switch leave
    nothing; a run of arrow nudges is one undo step; an outline handle's
    edits never touch the interior fill (`tests/fill_tool.rs`).
18. A shadow or bevel drag emits nothing before release (digest and
    history unchanged every frame), commits one `EditCommand::Live` step
    whose render equals the preview, and `Esc` leaves nothing; applying
    then removing an effect is the identity by digest
    (`tests/live_tools.rs`).
19. A contour or blend drag emits nothing before release, regenerates
    its output **exactly once per frame that changes it** (none for a
    repeated frame: `WalkStats::live_regenerated`) and never records a
    regeneration; the release is one step whose render equals the
    preview; applying then removing a contour or a blend is the identity
    by digest; a contour change repaints exactly what a full frame shows
    (`tests/contour_blend_tools.rs`).

## Dead ends (do not retry)

- **Building a contour's generated node and then attaching its
  attributes to it**: `Tx::check_permitted` refuses every anchor inside
  generated data. Furnish it while it is new (decision 94).
- **Moving a contour's generated node to change sides**: refused for the
  same reason; move the source instead.
- **Contour test drags of 2000 mp per frame** at page fit stay under the
  4 px drag threshold: the first frames are not a drag yet.

- **An anchor grid built from nested egui `horizontal` rows**: each row
  takes a full interaction height and pushed the whole infobar out of its
  28 pt row. Allocate the grid once and `interact` per cell.
- **Computing a probe's handle from the object nearest the centre**: the
  click selects whatever is on top at that point, so the blob belonged to
  another object and the "scale" probe moved instead. Pick first.

- **Fitting scroll bounds after every mutation** — see decision 10.
- **Benchmarking "one edit" on the first selectable object of the synthetic
  document**: it is a layer-sized group of thousands of nodes (1.5 ms per
  undo pair through the bus). The bench now moves the smallest object.

## Open TODOs

- [ ] Fill tools (phase 8): linear `end2` (skew) handle; a colour dropped
      on an **outline** stop or arm (the colour bar resolves interior sets
      only, decision 78); dragging out a new outline fill; the original's
      perpendicular follow of an elliptical radial's minor axis without
      Adjust (decision 76); the document's own nudge size (tag 4114) —
      all in XARA-T-0308.
      Done in XARA-T-0220: outline handles, nudges, axis/aspect locks,
      status line and cursors, the double-click conical, one-step infobar
      slider drags (decisions 76–81); Hue came with T-0256. Bitmap-fill
      handles are done (decision 64); the original's perspective bitmap
      handles are not.

- [x] **Incremental pick index** (decision 37). Image alpha picking
      is still open.
- [ ] `image/svg+xml` and PNG flavours on the clipboard (needs a
      data-control / X11 selection writer beside `arboard`); Inkscape
      paste checked manually per release. Pasting a *picture* is done
      (decision 68), and so is a copied image *file* when the clipboard's
      text carries the list (decision 73).
- [x] Dropping a gallery bitmap on an object (bitmap fill) or on the
      canvas (placement) — decision 70. Open: Ctrl-drop as the page
      background, and a dropped *file* on an object (the file drop still
      always places).
- [ ] Guide properties dialog; Delete all guides has no menu item yet;
      snapping of the shape editor's nodes (W6) and of guide drags.
- [ ] Clone (Ctrl+K), duplicate-offset preference, Paste attributes.
- [ ] Phantom-node preview (a filled shape while drawing) needs walker
      support (`Preview::phantom`).
- [ ] Constrain+Adjust while drawing is "square from the centre" here; the
      original's radius mode (rotates with the pointer) is not done.
- [ ] Observe in the VM: the radius under a non-uniform scale (K8), what
      the original's Angle field shows, the default of "scale lines".
- [ ] Undo of a large group move (thousands of nodes) costs ~1.5 ms: the
      per-node `Transform` inverse; a subtree-level action would fix it.
- [ ] Nudges, snapping (W4 T4.10, W8), flip and copy-and-transform (T4.6,
      T4.7), bump buttons, UI to set the current attributes (phase 8).
- [x] Double click on a path → shape editor.
- [ ] Path-point nudges (T6.11, needs the W4 nudge family), Tab/Home/End
      point cycling, mid-drag snapping of nodes (W8).
- [ ] Freehand: Alt straight segments, joining to a selected path's end,
      refitting the last stroke when the smoothing changes (retro fit),
      pressure. Pen: re-editing the start point's handle.
- [ ] Shape editor: editable paths inside blends/moulds, the neighbouring
      handles' X/Y in the infobar, reverse path, arrowheads; join across
      two path objects.
- [ ] The menu bar has no entries for the path operations yet (keys and
      infobar buttons only).
- [ ] Shadow and bevel tools (XARA-T-0338 follow-ups): the shadow colour
      from the infobar or the colour bar; the original's Shift-drag second
      shadow and keys 1–4/A; the bevel size drag's contoured-path outline
      (we draw the box) and snapping; Draft-quality effects during drags
      (J3, render crate); arrow-key nudges of a wall shadow's offset.
- [ ] Mould tool (XARA-T-0342 follow-ups): pasting any clipboard path
      as a mould (ours keeps the copied cage in the tool); Remove with
      Constrain copying the cage; editing a moulded object's source
      inside the mould (the shape editor through
      `MouldTransform::source_point`); the source rectangle does not
      follow edits made inside the mould (the original re-reads its
      bounds); snapping to moulded outlines; a 2×2 envelope preset;
      Draft-quality regeneration while dragging (J3).
- [ ] Contour and blend tools (XARA-T-0340 follow-ups): blend along a
      path (F6) with attach/detach, tangential and the distance mode;
      point mapping and remap drags; the contour steps/distance toggle;
      the inner-width clamp to the deepest possible inset; the open-path
      fill and the "no fill" clone on a new contour; snapping of the size
      handles; refusing to remove a bevelled or contoured blend; the
      "long job" question for heavy blends.

### Keyboard-only access to the effect tools (XARA-T-0348, 2026-10-16)

`crates/xarast-app/tests/keyboard_only_effects.rs` sets every parameter
of every infobar of the shadow, bevel, contour, blend and mould tools with
`Intent::InfobarEdit` alone (the intent the shell's widgets raise), after
choosing the tool with an effect selected; no pointer event is sent. The
parameter set is pinned by field name per tool, so a new field fails the
test until it is listed. `MouldPoint`, `MouldGrid` and `MouldDetach` are
tool state (no document change, no undo step) and are asserted as such.
The status of all 17 phase 13 acceptance criteria is in `render.md`,
"Phase 13 conformance audit".
