# User interface

Durable notes for `xarast-ui` and `xarast-shell`. Two workstreams write
here; each keeps to its own headings.

## UI toolkit

> Owner: the Phase 5 `xarast-ui` workstream (W1, W5 panels). The shell's
> own findings — winit, portals, clipboard, the `egui-winit` shim — live
> under their own headings in this file; nothing here rewrites them.

### Architecture §7 question 2 — **closed: egui is confirmed**

`egui 0.33`, used as a library and never through `eframe`. Every axis that
can be measured without a display passes, three with room to spare; the
four axes that need a GPU or a compositor are **unmeasured**, not assumed,
and are listed below as open TODOs.

Reproduce with `cargo run -p xarast-ui --release --example density`. The
probe is `xarast_ui::density`, so the same code backs the example, the
`criterion` bench (`cargo bench -p xarast-ui`) and the unit tests.

**The nine numbers.** Measured on the Phase 5 container: no GPU, no
compositor, window 2560×1440, 200 frames per measurement, probe
instantiating **1,789 controls per frame** over six panels, a 5,000-row
virtualised `TableBuilder` tree, a 512-swatch palette strip, a
2,000-thumbnail gallery and 48 drag-adjust unit fields.

| Axis | Threshold | Measured | Verdict |
|---|---|---|---|
| P1 `build_ui_frame()` CPU | p50 ≤ 3 ms, p99 ≤ 8 ms | **p50 1.63–1.65 ms, p99 2.19–3.40 ms** over repeated runs (this container has four slow cores and three build jobs on it) | **PASS** |
| P2 main-thread total | ≤ 8 ms every frame | **4.71–6.68 ms** = build p99 + tessellate p99; event handling and submit unmeasured | **PARTIAL PASS** |
| P3 egui GPU pass | ≤ 1.5 ms | **unmeasured** — no adapter in this environment | **UNMEASURED** |
| P4 5,000-row tree scrolled 10 s | no frame over 16 ms | **worst CPU frame 2.84–2.99 ms** over 600 scrolled frames | **PARTIAL PASS** (CPU side only) |
| P5 partial update (one slider) | within 1 ms of idle | **+0.00 to +0.05 ms** against idle | **PASS** |
| P6 resident growth, all panels | ≤ 50 MB | **9.3–9.4 MB** | **PASS** |
| P7 20 pt rows at 1×/1.25/1.5 | legible, screenshots | rows measure **20 / 25 / 30 device px**; build 1.77–1.79 ms at all three. Legibility itself **unmeasured** (no display) | **PARTIAL** |
| P8 AT-SPI tree | tree, fields and toggles present and labelled | **2,415 nodes, 1,752 labelled**; list ✓, numeric field ✓, toggle ✓ — *after* the fix below | **PASS** |
| P9 pointer → handle update | ≤ 2 frames at 60 Hz | **1 frame** of interface latency by construction (a pointer event and the overlay it moves are the same frame); presentation **unmeasured** | **PARTIAL** |

**Verdict: egui is confirmed and no fallback is taken.** Immediate mode is
not the cost: a frame that instantiates 1,789 controls costs 1.65 ms at
the median, which is a fifth of the 8 ms main-thread rule of
`research/05 §10.2`, and the partial-update measurement (P5) shows that
dragging a slider costs the same as an idle frame — the immediate-mode
objection in the abstract does not survive contact with the measurement,
because virtualisation, not retention, is what decides the cost. Fallback
1 (mitigate inside egui) was not needed, fallback 2 (custom-painted
panels) was not needed, and fallback 3 (`iced 0.14`) is **not taken**. The
`Panel` trait that would make fallback 2 possible exists anyway, because
it costs nothing and it is also how a panel is registered.

**Two findings that came out of the spike and changed the code.**

1. **AccessKit exposes no list, tree, table or row structure.** The first
   P8 census found the 5,000-row tree published as a flat run of
   `Button` nodes inside anonymous `GenericContainer`s: labelled, but
   structureless, so a screen reader reads a toolbar where a user expects
   a layer stack. egui derives a role from its own widget type and has no
   widget type for a row. The fix is `Context::accesskit_node_builder`,
   wrapped in `xarast_ui::a11y::{set_role, set_list_item, set_label}`:
   the node stays egui's, the role and the position-in-set are ours. Any
   panel with a list must do this — it is not automatic.
2. **An icon-sized control is anonymous.** `Checkbox::new(&mut v, "")` has
   no accessible name at all. Every such control in this crate now sets
   one explicitly (`Hide layer Sky`, not a drawn eye), and the
   `accessibility` integration test fails the build if any interactive
   node in the layer or colour panel has an empty name.

**Version note.** `research/05 §2.2` names egui 0.36.2; this workspace
pins stable **1.94** and `rust-version = 1.90`, while egui 0.36 declares
`rust-version = 1.95` and 0.34/0.35 declare 1.92. 0.33.3 is therefore the
newest version that builds here. The upgrade is two lines — the workspace
`rust-version` and the `egui`/`egui_tiles` versions — and nothing in the
spike's conclusion depends on the difference.

**Measured with, and to re-measure on hardware:** the four unmeasured or
partial axes (P3, P7, P9 and the presented halves of P2 and P4) need a
display. Until then, no claim is made about them in either direction.

## Editing and tools (phase 7)

Full note: [`tools.md`](tools.md). What the interface side owns:

- **Layout**: menu bar (File, **Edit**, Arrange, View, **Window**, Help) → **infobar row**
  (`toolbar::InfobarRow`, 28 pt, tool name + the tool's described fields,
  lengths in the document unit, typed units parsed, commit on focus loss /
  Enter, Esc abandons) → **tool palette** docked left
  (`toolbar::ToolPalette`, 40 pt strip, one 30 pt button per `ToolId::ALL`)
  → canvas → dock on the right → status bar.
- **Palette**: glyphs are painted strokes (original, no image assets);
  selected = accent fill; later-phase tools (Text) disabled; implemented-later phase-7 tools enabled with a corner dot and
  "coming soon (phase 7)" in the tooltip. Each button is published as an
  AccessKit `Button` named after the tool with `toggled`, `disabled`,
  `keyboard_shortcut` and a description (the tooltip). Clicking raises
  `UiCommand::App(AppCommand::Tool(id))` — the same command as its key.
- **Edit menu**: "Undo Move" / "Redo …" from `EditingView.undo/redo`
  (greyed and plain "Undo" when empty), Delete and Select none greyed with
  no selection. Items close the menu on click.
- **Model**: `UiModel.editing: Option<EditingView>` (tool, undo, redo,
  selected count, infobar); `UiCommand::InfobarEdit { field, value:
  InfobarValue }` (lengths in mp, angles in degrees, toggles, anchors).
- **Infobar items**: `Measure` (unit text field), `Angle` (degrees,
  `format_angle`/`parse_angle` accept `45`, `45°`, `45deg`), `Toggle`
  (check box: Lock aspect, Scale lines), `Anchor` (the 9-anchor grid: one
  22 pt allocation, `ui.interact` per cell, each cell an AccessKit node
  "Anchor: Top left (chosen)"), `Note`.
- **Overlay**: `HandleKind::Skew` (hollow square) and `HandleKind::Radius`
  (filled dot) added; the viewer turns `OverlayShape::Polyline` into
  `OverlayItem::Line` segments (shape outlines while drawing, the
  transformed box while scaling/rotating/skewing).
- **Keys** (shell): momentary Space/Alt+S/Alt+Z/Alt+X
  (`input/momentary.rs`); View › Zoom to selection = `3`.
- **Tests**: `crates/xarast-ui/tests/toolbar.rs` (palette order/placement/
  state/keys, click → command, Edit labels, infobar typing `1in` → 72 pt);
  viewer end-to-end tests in `xarast-shell/src/viewer.rs` (keys and palette
  switch tools; select, drag, Edit › Undo Move, Ctrl+Shift+Z, Ctrl+Z,
  Ctrl+Y; Esc mid-drag; Delete + undo).
- **Real window**: `xarast --probe drag --screenshot out.png file.xar`
  presses on the object nearest the canvas centre, drags it 100 frames and
  releases (one Move), then captures. Needs no input on the desktop.
  `--probe scale|rotate` click that object (twice for rotate) and drag its
  top-right blob; `--probe rect|ellipse` choose the tool and drag out a
  shape.
- The colour panel has a "Fill" button of its own: query palette buttons
  by role *and* position in tests.

### Shadow and bevel tools (phase 13, XARA-T-0338)

- **Palette**: Shadow (Ctrl+F2) and Bevel (Ctrl+F3), after a separator
  below Text; glyphs are ours (a square over a soft stepped square; a
  square with a lit and a shaded rim). Keys come from the command table
  (`KeyChord::ctrl_f`), so the shell binds them with no change; both tools
  are in `AppCommand::ALL`. (F5/F6 are not in `AppCommand::ALL`, so the
  shell does not bind them — a gap older than this task.)
- **Infobars** use the existing items only, so `toolbar.rs` needed no new
  widget: shadow = type `Choice` (None/Wall/Floor/Glow), `Measure` X/Y
  (wall), `Angle` + `Scalar` % (floor), `Measure` width (glow), `Measure`
  blur, `Scalar` % transparency, `Real` bias/gain sliders (previewed,
  one step on release); bevel = type `Choice` (None + 15 styles), side
  `Choice`, `Measure` size, `Scalar` % contrast, `Angle` light, `Scalar` °
  elevation, join `Choice`. Seventeen new `InfobarField`s with labels and
  screen-reader descriptions. Rules: `tools.md` decisions 82–90.
- **Overlay**: the bevel tool reuses `HandleShape::Bounds` (size handles),
  `OverlayShape::Arrow` and `HandleShape::FillCentreSelected` (the light);
  the shadow tool draws only dashed boxes while dragging out a new shadow.

### Fill and transparency tools (phase 8, XARA-US-0039)

- **Palette**: Fill (F5) and Transparency (F6) are now available and
  implemented; only Text stays disabled.
- **State machine** (`xarast-app/src/fill_tool.rs`, full rules in
  `tools.md` decisions 45–52): hover a handle → Move cursor; press+drag a
  handle → live preview through `Preview::attrs`, one step on release; press
  elsewhere → drag out a new fill (a double click held and dragged: a
  conical one); double click on an arm → insert a stop;
  click a handle → it becomes the selected handle and the infobar rebinds;
  Esc mid-drag → nothing happened (no command was emitted, so nothing to
  undo); Esc idle → deselect the handle, then the objects. An outline
  painted with a gradient shows its own handles (`tools.md` decision 78).
- **Arrow keys** (XARA-T-0220, `tools.md` decision 79): with a fill handle
  selected and the canvas (or nothing) holding the keyboard, the viewer's
  `arrow_nudge` sends `Intent::Nudge` — Ctrl ×5, Shift ×10, Ctrl+Shift ⅕,
  Alt one pixel, Alt+Shift ten — and does **not** pan; with no handle
  selected the arrows pan as before. A run of nudges is one undo step.
- **Status line and cursors** (T8.4.6): the status bar shows
  `Session::tool_status()` after a colour/bitmap drag's text and before the
  viewer's own notices; over a handle the cursor is the move cross, over an
  arm the pointing hand (`CursorKind::Pointer` → `CursorShape::Hand`), over
  an object the crosshair, over nothing the arrow.
- **Pick radius**: 5 device px (`FILL_PICK_RADIUS_PX`, the phase's
  proposal), measured in device space at every zoom; stops beat end
  handles, which beat the arm line. The 8 px pen/touch radius is not wired:
  no input-device reporting reaches the tools yet.
- **Infobar items added**: `Choice` (an `egui::ComboBox`, AccessKit label
  "description: option") and `Real` (an `egui::Slider` over `min..=max`,
  disabled without a value), raising `InfobarValue::Choice(i)` /
  `InfobarValue::Real(v)`. **A `Real` slider drag is one gesture**
  (XARA-T-0220, `tools.md` decision 81): `InfobarRow::real_slider` sends
  `UiCommand::InfobarDrag(Preview { field, value })` each frame the
  dragged value moves, `Commit` on `drag_stopped` (after a last `Preview`
  when the release frame moved it), `Cancel` on `Esc` — also when egui
  reports the stop in the `Esc` frame — and ignores the rest of a
  cancelled drag (`cancelled_drag`); a keyboard step is still one
  `InfobarEdit`. The app previews and commits one undo step; the slider
  shows the previewed value because the tool's infobar reads the preview.
  Tests: `tests/toolbar.rs` (kittest drag: previews then exactly one
  `Commit`, no `InfobarEdit`; `Esc` mid-drag: a `Cancel`, nothing after).
  The freehand smoothing `Number` slider is a tool setting, not a
  document edit, and still applies as it goes.
- **Overlay items added**: `OverlayItem::Arrow` (accent line over a darker
  3-hairline halo, open 9 × 8 px head, no head for a zero-length arm),
  `HandleKind::FillBlob` (square) and `HandleKind::FillCentre` (round);
  stops reuse the `Fill` diamond; a selected handle is drawn `active`.
- **Bitmap fills (phase 10, XARA-T-0271)** reuse the same overlay: a round
  centre blob, two arrow heads at the middles of the axis edges, arms from
  the centre and the placement dashed (`tools.md` decision 64). The infobar
  adds a "Resolution" `Scalar` field (suffix `dpi`,
  `InfobarField::BitmapDpi`) and a "Natural size" `Command` button
  (`AppCommand::Action(ToolAction::NaturalSize)`, no key); the tiling combo
  gains "Repeat inverted" for bitmap fills. No new UI widget was needed.

### Text infobar and text ruler (phase 9, XARA-T-0225)

- **Infobar items added** (`toolbar.rs`): `Scalar` (a text field with a
  unit suffix — `format_scalar`/`parse_scalar`, the suffix optional when
  typing, clamped to `min..=max`, committed on Enter/focus loss, raising
  `InfobarValue::Real`); `FontFamily` (a combo box 160 pt wide, 320 pt
  tall, with a filter field at the top of its popup and at most
  `MAX_FONTS_LISTED` = 400 matches listed; the family in force shows even
  when it is not in the list; AccessKit "Font family of the text: <name>");
  `Features` (a menu button "OpenType" with one check box per feature,
  indeterminate for a mixed selection, each named "Small capitals (smcp):
  off"); `TextRuler` is skipped by the row.
- **The text ruler** (`text_ruler.rs`, `CanvasWidget::set_text_ruler`, fed
  by the workspace from the infobar each frame): drawn over the horizontal
  ruler strip after its ticks — the column's band tinted with the accent,
  the first-line indent a triangle hanging from the top, the left and right
  margins triangles standing on the bottom, tab stops └ ┘ ┴ (decimal ┴ with
  a dot) on the bottom edge. **Interaction model**: a drag that *starts* on
  a marker (grab 5 pt; `press_origin`, not where egui recognises the drag,
  or the neighbouring marker is taken) moves it, drawn at the pointer, and
  raises one `InfobarEdit` on release; the strip's upper half picks the
  indent, the lower half the margin, where they coincide; a tab dropped
  more than a strip's depth below the ruler is removed; a click on no
  marker inside the column adds a stop of the kind the infobar's "Tab"
  choice says; a drag that starts on no marker is still a guide pulled out
  of the ruler (`handle_guides` runs after and skips while a marker is
  held). **`Esc`** mid-drag (egui reports the stop in the `Esc` frame,
  `FrameCtx::escape_pressed`): a marker makes no edit, a guide pulled
  out of a ruler is not added, a guide dragged along the canvas goes back
  in one `MoveGuide` to where it started (XARA-T-0305). A guide is
  picked at `press_origin`, not where egui recognises the drag — its
  6 pt threshold is outside the 4 pt grab, so a quick sideways drag
  used to miss the guide. The text infobar's own fields are text and
  combos: nothing to drag. Shown only for a straight story (no turn, shear or mirror) with a
  caret; a point story's band runs from its anchor rightwards.

### Colour editor (phase 8, XARA-US-0041)

- **Seam**: `UiModel::colour_editor: Option<ColourEditorView>` in,
  `UiCommand::ColourEditor(ColourEditorOp)` out; the shell maps it 1:1 to
  `Intent::ColourEditor`. The model, the targets and the live/committed
  split are `xarast-app`'s (`colour.md` decisions 19–23). The panel keeps
  only interaction state: the slider component, the press in flight, a
  name being typed. With no document the panel says so and still draws
  the colour line.
- **Layout** (`panels::colour`): target chooser ("Selection fill",
  "Selection line", each named colour) + "New colour" → title → model
  tabs (RGB/HSV/Grey/CMYK, locked for a tint or shade) + "Slider:"
  chooser for RGB/CMYK → 128 pt 2D field + 18 pt strip → numeric grid →
  before/after preview + hex → "Redefine ‘name’" (selection using a named
  colour) or "Apply to fill"/"Apply to line" (entry) → derivation editor
  (name, "Colour type", "Parent colour", tint %, shade shifts, link
  "Follow the parent's:" checkboxes).
- **Field** (`colour_field`): HSV = S across × V up, hue on the strip
  (drawn at full S and V); RGB/CIE = the two channels not on the strip;
  CMYK = two of C/M/Y, key numeric only; grey = a horizontal strip only.
  **Meshes, not textures**: vertex colours interpolate exactly over RGB
  planes and HSV S×V squares (bilinear; asserted by
  `bilinear_vertex_colours_are_exact_over_rgb_and_hsv`), CMYK's clip
  bends within a pixel at 16×16. 289 + 74 vertices a frame, nothing to
  upload or invalidate — the phase's "texture per fixed axis" mitigation
  is not needed.
- **Interaction**: `PressState` turns a press into `Preview` every frame
  it is held, `Commit` on release, `Cancel` on `Esc` (the rest of that
  press is ignored). Numbers and derivation sliders: dragged →
  `Preview`, `drag_stopped` → `Commit` (preceded by a `Preview` when the
  release frame moved the value), typed or stepped → `Set`. **`Esc`
  arrives as the end of the drag** (egui 0.33 drops the drag and the
  press itself, in the `Esc` frame): both `classify` and
  `PressState::track` turn an end in a frame with `Esc` pressed into
  `Cancel` (XARA-T-0305, measured: before the fix the number fields, the
  tint/shade sliders, the field and the strip all committed). Tests:
  `tests/colour_editor.rs` (5, kittest pointer drags with and without
  `Esc`, each drag widget kind).
- **Sliders are handed the value they show** (`panels::slider_value`):
  an `egui::Slider` with `fixed_decimals` rounds its value every frame
  and reports it as changed, so a stored 0.4 tint (40.000000596 %) sent a
  `Set` each frame. The tint and shade sliders (and the photo panel's)
  round first.
  A focused field or strip takes the arrow keys (focus-lock filter) at
  1 % a press, 10 % with Shift, each press its own `Set`.
- **Units** (T8.6.3, `component_specs`): RGB 0–255, hue in degrees,
  everything else per cent; typed values clamp to the range.
- **Accessibility**: the field is a `Slider` named "Green (across) and
  Blue (up)", the strip "Red slider", each number a `SpinButton` named
  after its component, combos "Edit: …", "Slider: …", "Colour type: …",
  "Parent colour: …", the preview "Colour preview #RRGGBB". The
  accessibility tests assert names on every interactive node of both the
  selection and the entry views, and drive the field by keyboard alone.
- **egui density (architecture question 2)**: no frame-time number was
  taken for the editor; it adds ~40 widgets and two meshes to a panel,
  far below the spike's 1,789-control probe. Unmeasured, not assumed.

### Colour bar, gallery and colour drag (phase 8, XARA-US-0042)

- **Seam**: `UiModel::colour_bar: Option<ColourBarView>` in (swatches +
  the drag in flight), `UiCommand::ColourBar(ColourBarOp)` and
  `UiCommand::ColourDragAt { x, y, shift }` out. The shell maps the first
  1:1 onto `Intent::ColourBar`, and turns the second (window points) into
  `DragPoint::Canvas` (canvas device pixels, via the adapter's
  `CanvasRegion`) or `DragPoint::Elsewhere`. What a click or drop *does*
  is `xarast-app`'s (`colour.md` decisions 25–32). `SetFill`/`SetLine`
  survive and become colour-bar clicks.
- **Layout**: the bar is a `TopBottomPanel::bottom` declared after the
  status bar, so it sits between the canvas and the status bar
  (`COLOUR_BAR_HEIGHT` = swatch + 10 pt), only with a document open. The
  colour panel lost its colour line (the editor keeps the panel); the
  gallery (`panels::gallery`, "Colour gallery") is a third dock pane
  under the colour panel.
- **One painted strip** (the phase's mitigation for a 256-swatch bar):
  one allocation, then `ui.interact` per **visible** swatch only, each a
  focusable AccessKit `Button` named `swatch_label` ("Brand red
  (document colour)") with `current_text_value` = `#RRGGBB` ("none" for no
  colour). Paging: ◀/▶ buttons ("Previous colours"/"More colours") a
  page at a time; the wheel over the strip 3 swatches a notch.
- **Gestures**: left click = fill, right click or Shift+click = line
  (`click_slot`); a drag picks the colour up (`drive_drag`). The menu
  (right click on the strip's empty part, or the "Colour menu" ☰ button)
  acts on the named colour last clicked: New colour, Edit, Move left/right,
  Delete. Right click on a swatch is the line colour, as in the original,
  so the menu cannot live there.
- **Drag plumbing** (`colour_bar::drive_drag`, shared by bar and gallery):
  the owner widget and a "cancelled" flag live in egui temp memory under
  one id — **not** egui's drag state, which egui itself drops on `Esc`
  (the release then still reported `drag_stopped`, dropping a cancelled
  colour; measured). Each frame the primary button is down: `Esc` →
  `DragCancel` (the rest of the press is ignored); pointer over a
  registered named slot → `DragTo(Entry(id))`; else `ColourDragAt`. The
  button up → `DragDrop` unless cancelled. Named slots register their
  rectangles per pass (`register_slot`); the owner reads this pass's and
  the previous pass's, so it works whichever widget is drawn first. The
  pointer shape is `Copy` / `NoDrop` from `ColourDragView::allowed`
  (`Grabbing` before the first answer); the status line shows
  `ColourDragView::status` (the viewer puts it in `StatusInfo::message`).
- **Intra-window only**: no `wl_data_device` or OS drag is involved; the
  shell already routes motion and releases to its adapter, and a release
  without a canvas press is ignored there, so a drop never reaches a tool.
- **Spatial index (phase-08 §W8.7 asks to record it)**: no new index —
  drop hit-testing goes through phase 7's pick index (`Picker::pick_drop`,
  `HitIndex` + exact `HitShape`), ≈ 6 µs a move at 100k objects.
- **Gallery**: named colours as a derivation tree (`gallery::tree`:
  children under their parent, a colour whose parent is not named is a
  root), each row a draggable swatch (also a drop slot) and a selectable
  name ("Paper (tint)"); New / Edit / Rename / Delete act on the chosen
  row (accessible names "New named colour", "Edit the chosen colour", …,
  so none clashes with the Edit menu's "Edit"); double click edits it in
  the colour editor. `F9` (or Window › Colour gallery) shows it and
  brings it to the front (XARA-T-0254, "Showing a pane" below).

### Bitmap gallery, File › Import… and import progress (phase 10, XARA-US-0055)

- **Seam**: `UiModel::bitmap_gallery: Option<BitmapGalleryView>` (entries,
  total, drag in flight) and `UiModel::imports: Vec<ImportProgress>` in;
  `UiCommand::BitmapGallery(BitmapGalleryOp)`, `UiCommand::BitmapDragAt
  { x, y }` (window points) and `UiCommand::CancelImports` out. The shell
  maps them onto `Intent::BitmapGallery` / `BitmapDragPoint::Canvas` (via
  `CanvasRegion`, `Elsewhere` off the canvas) / `Intent::CancelImports`.
  What an operation does is `xarast-app`'s (`tools.md` decisions 70–73).
- **Panel** (`panels::bitmaps`, "Bitmap gallery", the fourth dock pane
  under the colour gallery): a header ("2 bitmaps, 2.3 MB in memory"), a
  filter field ("Filter bitmaps by name"), a sort combo (document order,
  name, size, memory, uses), Place and Delete ("Place the chosen
  bitmap", "Delete the chosen bitmap"; Delete is disabled with a reason
  in its tooltip unless `GalleryEntry::deletable`). One painted row per
  bitmap: the thumbnail centred in a 64 pt box (uploaded once per content
  hash as an egui texture, dropped when the hash leaves the view), the
  name, and `details()` — size, depth, dpi, format, colour space, stored
  and decoded bytes, uses. Each row is one `click_and_drag` response named
  "Bitmap NAME: details" with its list position; double click = Place.
- **Drag** (`bitmaps::drive_drag`): the colour drag's plumbing again —
  the owner and a cancelled flag in egui temp memory under their own id,
  `Esc` → `DragCancel`, release → `DragDrop` unless cancelled, the
  pointer reported every frame, `Copy`/`NoDrop` from
  `BitmapDragView::allowed`. The status line shows the drag's status (the
  viewer puts it in `StatusInfo::message` after a colour drag's).
- **Import progress**: `StatusBar::ui` (which has the sink) draws, right
  to left, "Cancel" ("Cancel the import"), a 120 pt animated
  `ProgressBar` and `import_text` (the oldest job's words, "+N more");
  `status_bar_ui` (no sink) draws none of it. While shown it asks egui for
  a repaint every 100 ms, which reaches the shell as a `RedrawAfter`.
- Tests: `tests/bitmap_gallery.rs` (6, `egui_kittest` over the AccessKit
  tree and synthetic pointer events: named rows, Place/Delete, filter,
  drag to canvas, `Esc`, File › Import…, Cancel) and unit tests in
  `panels::bitmaps` (2) and `panels::status` (1). An animated progress
  bar never settles, so its test uses `run_steps`, not `run`.
- **F11** (or Window › Bitmap gallery) shows the gallery and brings its
  tab in front of the photo panel's (XARA-T-0254, which closes that item
  of XARA-T-0292).

### Photo panel (phase 10, T10.6.8, XARA-T-0301)

- **Seam**: `UiModel::photo_panel: Option<PhotoPanelView>` in (`None`
  with no document; `view.node == None` when the selection is not
  exactly one bitmap object), `UiCommand::PhotoPanel(PhotoPanelOp)` out;
  the shell maps it 1:1 to `Intent::PhotoPanel`. The model, the proxy
  preview and the undo steps are `xarast-app`'s (`tools.md` decisions
  74–75, `image.md` "Photo adjustments"). The chain's types reach this
  crate as re-exports of `xarast_app::photo_panel` (`PhotoOp`,
  `PhotoOps`, `Levels`, …): `xarast-ui` still does not depend on
  `xarast-doc` outside its tests.
- **Dock**: "Photo" is a **second tab in the bitmap gallery's cell**
  (`UiHost::set_default_layout_grouped`, new): a fifth pane in the
  column squeezed the gallery's rows out of a 1000 pt window (its
  kittest tests failed). Tabs used to be anonymous to AccessKit —
  egui_tiles paints the title — so `HostBehavior::on_tab_button` now
  publishes each tab as `Role::Tab` named after its panel; a test finds
  and clicks "Photo" in the whole workspace.
- **Panel** (`panels::photo`, "Photo"): name and master size; the chain as a list (one
  `ListItem` per op, named by `op_label`: "Brightness +20 %", "Levels
  (red) 10–240 → 0–255"); sliders Brightness, Contrast, Saturation
  (−100…100 %), Gamma (0.05…8, logarithmic); a Greyscale checkbox; a
  "Levels channel" combo and four 0–255 sliders named "Input black
  (red)" etc.; Rotate left/right, Flip horizontal/vertical; four crop
  `DragValue`s ("Crop left", "Crop top", "Crop width", "Crop height") in
  master pixels and "Remove crop"; "Reset all". A chain with an unknown
  operation shows the list and "Non-editable: unknown operation
  ‘curves’…" and no control at all. With nothing (or several objects)
  selected it says what to select.
- **Interaction**: a slider **dragged** → `Preview(chain)` each frame,
  release → `Commit` (the release frame's own value is previewed first:
  `LiveCommit`); `Esc` while held → `Cancel` and the rest of that drag is
  ignored. **egui ends the drag itself on `Esc` and reports
  `drag_stopped` in that same frame** (measured in kittest): the
  classifier therefore turns a `drag_stopped` frame with `Esc` pressed
  into `Cancel`, not `Commit`. Keyboard steps, typed values, buttons and
  the checkbox → `Set` (one step each). The crop fields never preview
  (a crop moves the object): a dragged crop field is kept in the panel
  and `Set` on release.
- **Accessibility**: every slider, spin field and button carries a name
  (`a11y::set_label`), the chain is a list; `tests/photo_panel.rs` (7,
  kittest) asserts the names, reaches Brightness and Rotate right with
  Tab alone and operates them with arrows and Enter, drives a pointer
  drag (previews then exactly one `Commit`, no `Set`), `Esc` mid-drag
  (`Cancel`, nothing after it), the read-only unknown chain, the
  empty states, and the tab in the workspace.
- **Shown** by Window › Photo (no key: the original has no photo
  panel); a saved layout from before this panel gets it back as a tab
  beside the bitmap gallery (XARA-T-0254).
- The sliders are handed their rounded value (`panels::slider_value`):
  a Brightness of 0.2 or a Gamma of 1.3 used to send a `Set` every
  frame. A crop field whose drag `Esc` ends applies nothing.

### Showing a pane: F9, F10, F11 and the Window menu (XARA-T-0254)

- **Command**: `AppCommand::ShowPane(DockPane)` (`xarast-app`;
  `DockPane::{Layers, ColourEditor, ColourGallery, BitmapGallery,
  Photo}`) → `Intent::ShowDialog(Dialog::Pane(p))` →
  `PlatformRequest::ShowDialog(Dialog::Pane(p))`, the alignment panel's
  route. The shell calls `Workspace::show_pane(p)` and redraws. Keys are
  the original's gallery keys (`research/04 §4.7`): **F9** colour gallery,
  **F10** layers, **F11** bitmap gallery; the colour editor and the
  photo panel have none (the command table test allows exactly those
  two to be keyless). No pane needs a document.
- **Window menu** (between View and Help): one item per `DockPane::ALL`,
  label + key, enabled with or without a document.
- **`UiHost::show(id)`**: docks the panel again if the tree lacks it,
  makes it and every ancestor visible, makes it the active tab of every
  tab group above it, and asks for its **tab** to take the keyboard on
  the next frame (`HostBehavior::on_tab_button` → `request_focus`), so a
  Tab key walks into the pane. `is_showing` is that state (docked,
  visible, active tab all the way up); `is_docked` only "in the tree".
- **Re-docking** (`UiHost::dock`): the host remembers the cells of the
  last default layout (`homes`). A missing panel goes in as a tab beside
  a docked panel of its home cell when that one sits in a tab group;
  otherwise it becomes a new cell of the root column (a root that is a
  tab group or a lone pane is first put in a column, so the panel does
  not hide behind a tab). `load_layout` re-docks every registered panel
  the saved tree lacks: **a layout saved before a panel existed still
  shows it**. The shell does not persist the layout yet; the rule lives
  in the host so it holds when it does.
- Tests: `panel::tests` (4: tab activation, hidden → visible, a layout
  from before the photo panel, a panel with no docked neighbour),
  `tests/menus.rs` (Window lists every pane with its key and raises
  `ShowPane`; `show_pane` brings the tab to the front and focuses it),
  `viewer::tests::the_gallery_keys_and_the_window_menu_show_their_panes`
  (Window › Photo, F11, F9, F10 through the shell; F9 with no document).

## Panels and canvas

### Current state

`xarast-ui` builds, tests and benchmarks with no window, no GPU and no
compositor. `cargo test -p xarast-ui` runs 121 tests (114 unit, 7
integration) in about a tenth of a second.

| Module | State |
|---|---|
| `workspace` | `Workspace::ui()` — the whole interface in one call: menu bar, dock, canvas (or the empty state), status bar, theming. This is the shell's entry point |
| `canvas` | Region reservation and reporting in whole device pixels, transparent so the shell's canvas pass shows through, wheel/Ctrl-wheel/pinch/middle-drag/keyboard navigation, guide creation and dragging, page edge, grid and guides; `CanvasNavigation::External` hands navigation to the host |
| `overlay` | Handles (bounds, rotate, node, fill, centre), lines, rectangles, dashes, hit-testing; device-pixel snapped |
| `rulers` | `Ruler::draw` plus the pure `ticks()`/`major_step()` used by the tests; 1-2-5 steps, imperial and pica subdivisions |
| `grid`, `guides` | Rectangular grid with subdivision dropout and snapping arithmetic; guides created by dragging from a ruler, moved, deleted by dropping back |
| `panels::layers` | Virtualised list, visibility, lock, active layer, reorder, rename, full keyboard operation |
| `panels::colour` | The phase-8 colour editor over `xarast_app::colour_editor` (target, 2D field, numbers, derivation, Redefine/Apply) |
| `colour_bar` | The colour bar under the canvas (paging, menu, click = fill / right = line, colour drag shared with the gallery) |
| `panels::gallery` | The colour gallery: named colours as a derivation tree, New/Edit/Rename/Delete, draggable swatches |
| `panels::bitmaps` | The bitmap gallery: thumbnails and details, filter, sort, Place, Delete (unused only), rows dragged onto the canvas (XARA-US-0055) |
| `panels::photo` | The photo panel: the selected bitmap's chain, tone and levels sliders (previewed live, one step per drag), orientation, crop, reset (XARA-T-0301) |
| `colour_field` | The editor's field/strip meshes, axes per model, component ranges and units, press tracking |
| `panels::status` | Coordinates in the document's unit, zoom, quality, cache pressure, renderer tier |
| `panel` | `Panel` trait, `PanelId`, `UiHost` over `egui_tiles`, versioned `LayoutState`; `show`/`is_showing` and re-docking of panels a layout lacks (XARA-T-0254) |
| `theme` | Dark and light token sets, density, WCAG-asserted contrast, live scheme change |
| `scale` | The one `Scale` of a frame, hairline and edge snapping, device rectangles |
| `units` | `10mm`, `1in`, `3p6`, `12mm + 3pt`, bumps, formatting that round-trips |
| `a11y` | Names and roles for what egui does not name itself |
| `density` | The spike probe, shared by the example, the bench and the tests |
| `menus` | `AppMenu`: the in-window File/Edit/Arrange/View/Window/Help menu bar, the About box, and `empty_state` (Open… + recent files) (XARA-US-0082) |

**Stubbed or absent on purpose:** the command palette, the problem list
(needs the diagnostics feed), the galleries other than colour and bitmap, and
anything Phase 7 and later own. No tool handles are produced — the overlay takes them, it does not
invent them.

### Decisions taken (and why)

- **The interface is a projection, not a model.** Each frame it reads a
  `UiModel` the application fills and writes `UiCommand`s into a
  `CommandSink`. Nothing in this crate mutates a document, a viewport or a
  selection. That is what makes every panel testable headlessly, and it
  is the seam that let this crate be written while `xarast-app` was being
  written next to it.
- **The canvas is not a dockable pane.** It shares the surface with the
  shell's passes; a pane can be dragged into a tab group, which the
  canvas cannot survive. The dock sits beside it.
- **The canvas region is transparent and reported in whole device
  pixels**, rounded *outwards*, so the document pass covers every pixel
  egui left clear. Rounding inwards leaves a seam. The `CentralPanel`
  that hosts it is transparent too whenever a document is open; the
  `canvas_backdrop` fill is only for the empty state
  (`nothing_opaque_is_painted_over_the_canvas_region` enforces it).
- **The view transform is read-only here.** Pan and zoom are commands.
  There is exactly one owner of the transform and it is not the
  interface.
- **The host chooses who navigates the canvas** (`CanvasNavigation`).
  `Internal` (the default, used by this crate's tests and the density
  probe) lets the widget turn wheel, pinch, middle/space drag and view
  keys into `Pan`/`ZoomAbout`. `External` (what the shell uses) emits
  none of that: the widget still hosts guides, reports the pointer and
  its focus, and hands tool input on, but a middle or space drag is
  neither a command nor tool input. Two navigators reading one wheel is
  how a notch zooms twice (XARA-US-0002).
- **`ViewTransform` carries the viewport's orientation (`y_up`)**, it
  does not decide it. `doc_to_view_y`/`view_to_doc_y` honour it, the
  vertical ruler negates its tick values under it, and "top" in
  `DocumentView::page` and `visible_doc_rect` means the edge at the top of
  the screen — the larger `y` when `y_up`. Grid lines need no change: a
  lattice through the origin is orientation-free (XARA-T-0026).
- **Handles are drawn with a light-on-dark outline pair, not XOR.** XOR is
  unavailable on a composited surface and looks wrong over antialiased
  content; the outline pair buys the same "visible on any background"
  property.
- **Hairlines snap to the centre of a device pixel, filled edges to the
  boundary**, after the `f64` transform, at every scale factor.
- **`LayoutState` is versioned and refuses an incompatible or unknown
  tree** rather than opening an empty window; a layout naming one missing
  panel still loads and says which panel went.
- **Focus and selection look different.** egui 0.33 paints a focused
  widget with `widgets.active`, so the focus ring is a two-point stroke in
  a `focus` token that is deliberately not the accent.
- **The menu bar is drawn in the window by egui**, as the workspace's
  first `TopBottomPanel::top`, never as a native or global menu: GNOME
  and COSMIC on Wayland have none (the maintainer saw "no menu anywhere").
  Items come from `xarast_app::AppCommand` (label + primary shortcut) and
  raise `UiCommand::App(cmd)`; Open Recent raises `OpenRecent(path)`.
  Items needing a document are disabled without one. Each item is
  re-published to AccessKit as `Role::MenuItem` named by its label alone,
  with the shortcut in `keyboard_shortcut` — egui otherwise names a
  button after all its text ("Open… Ctrl+O", "Open Recent ⏵").
- **With no document the canvas area is the empty state**: "No document
  open", an Open… button, a Ctrl+O/drop hint and the recent files.
- **Labels, not icons, for state.** Partly accessibility, partly the
  clean-room rule: no icon of another program is copied, and any icon
  this crate ever needs is drawn with the painter.

27. **Wayland drops through our own `wl_data_device`** (XARA-T-0042).
    `winit` 0.30 has no Wayland drag-and-drop. `wayland_dnd` wraps
    `winit`'s `wl_display` as a foreign display (sctk 0.19 and
    wayland-client 0.31, the versions `winit` links, `client_system`, so
    libwayland stays dlopen'ed), binds one data device per seat and runs
    its queue on a thread of its own blocked in libwayland's read. Only
    `text/uri-list` is accepted, with action `copy`; the payload is read on
    a throw-away thread (the source writes when it likes); the main thread
    turns protocol messages into `DragEvent`s with the scale in force. X11
    drops still come from `winit`.
28. **A dropped image is placed, not opened** (XARA-T-0272, T10.3.8).
    `Viewer::handle` splits a `DragEvent::Dropped`: with a document open,
    paths with an image extension (`xarast_app::place::is_image_path`)
    become `Intent::ImportImage { path, at }`, the drop point mapped from
    window to canvas pixels through `CanvasRegion` (`None` when it lands
    off the canvas: the view's centre); every other path still goes to
    `to_open`. On X11 `winit` gives no drop point, so an X11 drop is
    centred in the view. **Paste reads a picture** in `perform_requests`
    (`ReadClipboard`) only when the text is neither our own copy nor SVG
    and no text caret is up, then answers `Intent::PasteImage`; tests use
    a synthetic `Dropped` event and the viewer's `FakeClipboard` (now with
    an `image`), never a real window or clipboard.
29. **A file manager's copy is files, not a picture** (XARA-US-0055).
    When the clipboard text names image files
    (`xarast_app::place::image_paths_in_text`: `text/uri-list`, GNOME's
    `copy`/`cut` + URIs, or absolute paths, every line a local file), the
    shell does not ask for a picture even if one is offered beside it; it
    answers `PasteText` and the core imports the files, one step each.
    `arboard`'s text getter is the route. **Not observed on a desktop**:
    which flavour Nautilus/Dolphin put under `text/plain` for a file
    copy (paths or URIs — both parse); if one offers only `text/uri-list`
    or `x-special/gnome-copied-files`, the shell needs a data-control
    reader for them (XARA-T-0047's territory).

### Invariants that must not be broken

- `egui` appears at `xarast-ui` and above, never below. This crate does
  not name `winit` or `wgpu`.
- No `unsafe`: the crate is `#![forbid(unsafe_code)]`.
- Every public item is documented (`#![deny(missing_docs)]`).
- A panel never keeps document state between frames. The only state a
  panel may keep is interaction state (which row is being renamed, which
  guide is being dragged).
- Every interactive widget has a non-empty accessible name; the
  `accessibility` test enforces it for the shipped panels.
- Ruler, grid and guide geometry is derived in document units and snapped
  to device pixels **after** the transform, and never cached across a
  scale change.
- Tick and grid-line generation is bounded (2,048 and 4,096 per axis) so
  that an absurd zoom cannot become an unbounded allocation.
- Every ruler tick sits where the view puts its value:
  `view.doc_to_view_*(tick.value) == tick.position`, on both sides of the
  origin and under both orientations (pinned by two tests in `rulers`).
- A text field that appears in answer to a gesture (the layer rename)
  takes the keyboard in the same frame, or there is nowhere to type.

### Dead ends (do not retry)

- **`rfd`'s portal backend on Linux.** Every portal failure becomes
  "cancelled", then it spawns `zenity`; `Failed` was unreachable. Use
  `ashpd`'s FileChooser (decision 9).
- **Dispatching the Wayland drag-and-drop queue from `about_to_wait`.**
  During a drag the compositor holds the pointer, `winit` gets nothing,
  the loop stays parked, the offer is accepted too late and every drop
  arrives as a cancelled drag. The queue needs its own thread.
- **`gsettings` inside `dbus-run-session` without a private
  `DCONF_PROFILE` and `XDG_CONFIG_HOME` exported *before* the bus
  starts.** The bus activates `dconf-service` with the caller's
  environment, and the writes land in the maintainer's real dconf database
  (it happened once this round; see the XARA-US-0012 comment).
- **Synthetic input with one mutter RemoteDesktop session per step.** Each
  session is a new virtual device: the pointer vanishes between steps
  (cursor changes never show), the first key of each session is swallowed,
  and keyboard focus follows the device. Keep one session alive.

- **`egui::Checkbox` with an empty label as a compact toggle.** It is
  invisible to AT-SPI. Set the name explicitly.
- **Expecting `egui_extras::TableBuilder` to publish a table to
  AccessKit.** It does not; see the P8 finding.
- **Deriving `serde` on anything holding `xarast_geom::Mp`.** `Mp` has no
  serde support; `crate::serde_mp` adapts it as a raw count.
- **Asserting interaction in a single headless egui frame.** egui
  resolves hovering and dragging from the *previous* frame's widget
  rectangles, so a one-frame test asserts that the widget ignored an
  event it had not been told about. The canvas tests run a warm-up frame.
- **egui 0.34–0.36 on this toolchain.** They need rustc ≥ 1.92/1.95.
- **An opaque `CentralPanel` frame under the canvas.** The shell draws
  the document *beneath* the interface; an opaque panel fill hid it and
  the first composed window came up blank.
- **The canvas widget as the owner of wheel navigation in the shell.**
  egui smooths a wheel: one `Line` notch reaches `smooth_scroll_delta`
  spread over about eleven frames (12.7, 8.7, 5.9, … points), so a zoom
  or pan driven from it lands late and in pieces. The shell's adapter
  applies a notch whole, at once.
- **`RichText::strong()` for headings.** The theme maps egui's strong
  text to `widgets.active.fg_stroke` = the on-accent colour, almost
  invisible on the dark backdrop. Colour headings with `tokens.text`.
- **egui's own drag state for a drag `Esc` can cancel.** egui ends a
  drag on `Esc` by itself, yet still reports `drag_stopped` at the
  release; keep the owner and the cancel flag yourself
  (`colour_bar::drive_drag`).
  For a slider the `drag_stopped` comes in the **`Esc` frame itself**
  (egui 0.33, measured in the photo panel's kittest): a classifier that
  maps `drag_stopped` to "commit" commits a cancelled drag. Check
  `key_pressed(Escape)` on that frame (`panels::photo`,
  `panels::colour`, the canvas's ruler and guide drags). The same holds
  for `is_pointer_button_down_on`: it goes false in the `Esc` frame, so
  a press tracker that commits "on release" must check `Esc` there too
  (`colour_field::PressState`). Every drag widget has a kittest or
  frame-level `Esc` test now (XARA-T-0305).
- **Handing an `egui::Slider` with `fixed_decimals` an unrounded
  value.** With the default clamping it rounds the value every frame and
  reports `changed()`: a `Set` per frame. Round first
  (`panels::slider_value`).
- **Picking a guide at `pointer` on `drag_started`.** The drag is
  recognised past egui's 6 pt threshold, already outside the 4 pt grab.
  Use `press_origin` (as the text ruler does).
- **Querying a menu item by its visible text in kittest.** The status bar
  also says "100 %"; use `get_by_role_and_label(Role::MenuItem, …)`.
- **Minor ruler tick values from `index / subdivisions` plus
  `index.rem_euclid(subdivisions)`.** Truncating division and a Euclidean
  remainder disagree for negative indices: the minor tick one step left of
  zero read four steps right. Use `div_euclid` with `rem_euclid`.

### Open TODOs

- [x] **Photo panel** (phase 10 T10.6.8, XARA-T-0301): done, see
  "Photo panel" above; Window › Photo shows it (XARA-T-0254).
- [ ] The other galleries' keys (`Shift+F9` fonts, `F12` lines, …) wait
  for their panels. Persisting the dock layout in preferences is not
  wired in the shell; `load_layout` already re-docks new panels.
- Re-measure P3, P7, P9 and the presented halves of P2 and P4 on hardware
  with a display; until then they stay "unmeasured" in this note.
- `egui_kittest` **image** snapshots (phase criterion 17) need the `wgpu`
  feature and an adapter. The AccessKit-tree tests stand in for them
  here; add the image baselines when CI has `lavapipe`.
- [x] **For the shell:** `accesskit_winit` must be the release built
  against the same AccessKit major as egui 0.33 — done: `0.29.2` on
  `accesskit 0.21.1` (XARA-US-0003). Upgrading egui means upgrading the
  adapter in the same change.
- Bump to egui 0.36 when the workspace moves to rustc 1.95, and drop
  `egui_tiles` 0.14 for 0.17 at the same time.
- [x] Menus wired to `xarast-app`'s command table (XARA-US-0082).
  Still open: the command palette, an Edit menu (undo/redo exist as
  intents), and the problem list when the diagnostics feed does.
- The About box's third-party licence list is a placeholder; generate
  it at packaging time (`cargo about` or similar) and show it there.
- Replace the crate-local `UiModel`/`UiCommand` adapter with a direct
  mapping onto `xarast_app::{AppState, Intent}` now that that crate has
  landed its API: `UiCommand` → `Intent` is a one-function translation,
  and `Unit`, `ZoomTarget` and the theme preference exist on both sides
  and should converge on the `xarast-app` spelling. The translation now
  exists, in the composition root (`xarast_shell::viewer::Viewer::ui_intent`
  and `document_view`); converging the types would delete most of it.
- [x] **`ViewTransform` has no Y flip** — fixed by `y_up`, copied from
  the viewport by the composition root, which no longer negates `y`
  (XARA-T-0026).
- [x] **Vertical ruler labels were clipped by the 18 pt strip** (XARA-T-0032).
  They are now turned a quarter turn anticlockwise and read up the strip
  from just above their tick, at the horizontal ruler's 9 pt size
  (`TextShape::with_angle`: the same glyph atlas, only the quads turn).
  `rulers::vertical_label_rect` is the placement, and
  `vertical_labels_fit_the_strip_at_every_scale` lays out every label the
  ruler produces, at scales 1, 1.25 and 2. Dead end: stacking the digits.
  `-10000mm` is 8 glyphs, taller than the 48 pt label separation.

---

## Shell and platform integration

> Owner: the Phase 5 `xarast-shell` workstream (W2 shell foundations, W3
> Wayland/portals/clipboard/DnD, W4 input and tablet). The toolkit and
> panel findings above are the `xarast-ui` workstream's; nothing here
> rewrites them.

### Architecture §7 question 3 — **closed: do not pin the `winit` 0.31 beta yet**

**Verdict: stay on `winit 0.30.13`.** Deferred, not rejected — and it must
be re-opened before v0.1, because a drawing application without stylus
pressure is not finishable.

The phase document's decision rule is "pass E1–E7 → pin". Phase 5 could
not run any of them (no compositor, no bus, no GPU). **XARA-US-0012 ran
them** on the pinned `winit 0.30.13`, on a machine with a GPU, a live COSMIC
desktop and a tablet, inside an **isolated GNOME 46 session** (recipe
below) so that nothing touched the maintainer's desktop. They measure what
0.30 gives us and what the shell had to add; the 0.31 beta itself was still
not run, so the verdict stands: nothing measured argues for pinning a beta,
and the one thing that would (stylus axes, E2) belongs to XARA-US-0013.

**E1–E7, measured (XARA-US-0012, 2026-09-23).**

| ID | Criterion | Result on `winit` 0.30.13 |
|---|---|---|
| E1 | Build and run on GNOME/mutter, KDE/kwin, sway | **GNOME 46 / mutter 46.2: pass** (Vulkan on the RTX 4000; app, probe, dialogs, input, resize, minimise). **COSMIC: pass** (XARA-US-0001, 59/59 corpus; not re-run, it is the live desktop). **KDE and sway: not installed here — unmeasured.** |
| E2 | Pressure, tilt, twist | **Not available on 0.30 by construction** (no tablet API). Hardware exists: `Wacom USB Bamboo PAD Pen` (`/dev/input/event21`, ABS X/Y/PRESSURE — pressure, no tilt). No `uinput` tablet was created: libinput on the live session would pick it up. Owned by XARA-US-0013. |
| E3 | Fractional scaling 1.25 / 1.5 / 2.0 | **Pass.** `wp_fractional_scale_v1` `preferred_scale` 150/180/240 → `ScaleChanged` then `Resized` with physical = round(logical × scale); screenshots crisp. GNOME's "1.5" is 1.5038 (so logical sizes stay integral) while the protocol carries 1.5: mutter resamples by 0.25 %, inherent to the protocol. IME candidate window lands under the caret at 1× and 1.25×. |
| E4 | CSD on GNOME via sctk-adwaita | **Pass.** Title bar and close button drawn; the button layout follows the settings portal (close only under GNOME defaults; minimise/maximise/close when no portal answers). Live border-drag resize: 60 resizes, no validation error. |
| E5 | Drag and drop with file URIs | **0.30 fails on Wayland: no drag-and-drop code at all** (X11 only). A GTK drag of two files produced no event. **Fixed in the shell** (`wayland_dnd`, decision 27): Entered/Moved/Dropped with device-pixel positions and all files in one event; the app opens both dropped files. Drag *out* of Xarast: impossible on 0.30, not implemented. |
| E6 | Pinch/pan/rotate reach the viewport | **Not on 0.30 on Linux**: mutter advertises `zwp_pointer_gestures_v1` v3, but `winit` 0.30's Linux backends contain no gesture code (grep: nothing). No way to inject touchpad gestures into the nested session either. Pinch-zoom stays Ctrl+wheel. |
| E7 | Eight-hour soak | **Shortened, not eight hours:** 30 minutes on GNOME 46 with BLUECAR.xar, 1,663 rounds of synthetic input (wheel pan, middle-drag pan, Ctrl+wheel zoom, `+`/`-`/`0` keys, pointer sweeps). RSS 199–247 MB over 60 samples, peaking in the first minute and flat at 202.6 MB for the last ten; 40 threads and 77 fds constant throughout; no error, no panic, no protocol error in the log. No leak visible at this length; the eight-hour run is still owed. |

**What the source says, which is the part that could be checked.**

1. `winit-core-0.31.0-beta.3/src/event.rs` really does carry
   `PointerSource::TabletTool { kind: TabletToolKind, data: TabletToolData }`,
   and `TabletToolData` really does carry `force: Option<Force>`,
   `tangential_force: Option<f32>`, `twist: Option<u16>`,
   `tilt: Option<TabletToolTilt>` and `angle: Option<TabletToolAngle>`.
2. The Wayland backend implements it for real, not as a stub:
   `winit-wayland-0.31.0-beta.3/src/types/wp_tablet_input_v2.rs` dispatches
   `zwp_tablet_tool_v2`, maps every `ToolType` onto a `TabletToolKind`, and
   fills tilt and the tool buttons.
3. **X11 gains nothing.** `winit-x11-0.31.0-beta.3/src` contains no tablet
   code at all — a grep for `TabletTool` finds one unrelated comment. X11
   pressure needs `octotablet` in 0.31 exactly as much as in 0.30, so the
   beta is not the answer to the X11 half of the problem.
4. **`winit 0.30` has no tablet API whatsoever.** Its richest pressure
   channel is `Touch { force }`, documented as always `None` on Wayland
   and X11. So the fallback's cost is not "worse pressure", it is *no*
   pressure.
5. **No published `accesskit_winit` supports `winit` 0.31.** Every
   release from 0.29.1 to 0.34.0 requires `winit ^0.30.5` — read straight
   off the crates.io index, and confirmed by adding `accesskit_winit 0.34`
   to the 0.31 probe, which resolved a *second* `winit` into the graph
   (`cargo tree -i winit@0.30.13` → `accesskit_winit v0.34.0`). Adopting
   0.31 today therefore means either two `winit` versions linked into one
   binary or a hand-written AT-SPI adapter over `accesskit_unix`. The
   changelog does not mention this, and it is the single biggest reason
   the beta is not worth taking now: the accessibility transport (S10 /
   U4.2) is in scope for this phase and 0.31 removes the only way to
   build it.

   The version pairing is tight in both directions, so record it here.
   `xarast-ui` pins `egui 0.33`, which resolves `accesskit 0.21.1`; the
   `accesskit_winit` that pairs with `accesskit 0.21.1` is **0.29.2**, and
   it wants `winit ^0.30.5`. That is the combination to use when the
   transport is wired — *not* `accesskit_winit 0.34`, which pairs with
   `accesskit 0.25` and would put two incompatible `TreeUpdate` types at
   the shell↔UI boundary. `xarast-shell` deliberately has **no** AccessKit
   dependency today, so nothing in this crate constrains that choice yet.

   One loose end, not this crate's to fix: `[workspace.dependencies]`
   declares `accesskit = "0.25"`, which no crate references, so it does
   not appear in `Cargo.lock`. The first crate to write
   `accesskit = { workspace = true }` will pull 0.25 against egui's
   0.21.1 and fail. The line should become `0.21` when the transport
   lands, or be removed.
6. 0.31 is a structural break, not a version bump: the crate is split into
   `winit-core`/`-common`/`-wayland`/`-x11`/…, `ActiveEventLoop` and
   `Window` become `dyn` traits, `create_window` returns
   `Box<dyn Window>`, `inner_size` becomes `surface_size`, window creation
   moves from `resumed` to a new `can_create_surfaces` callback, and the
   `rwh_06` Cargo feature is gone (raw-window-handle 0.6 is now
   unconditional). Migrating blind — with no way to run the result — while
   two sibling crates are being written against the 0.30 loop is the
   highest-risk change available in this phase.

**What the fallback costs us, precisely.**

| Loss | Consequence today | When it is paid back |
|---|---|---|
| No tablet axes at all | `StrokeSample::pressure` is always `None`; tools draw at full width through `pressure_or_full()` | A backend swap. The whole pipeline — `ToolAxes` → `normalise` → `SampleQueue` — is written and unit-tested against axes that no backend yet supplies |
| No trackpad gestures on Linux | `winit 0.30`'s `PinchGesture`/`PanGesture`/`RotationGesture` are documented **macOS and iOS only**; on Wayland they never fire. Pinch-zoom degrades to the scroll wheel | Same swap; `GestureEvent` already exists and the translator already routes them |
| No drag and drop at all on Wayland; on X11 no drop position and no multi-file grouping | `winit 0.30` emits `HoveredFile`/`DroppedFile` on X11 only, **per file, with no coordinates**; on Wayland nothing (measured) | **Paid on Wayland** by `wayland_dnd` (decision 27): position and all files. X11 keeps `winit`'s per-file, position-less events |
| No `file:` URI payloads | Nothing today; the decoder exists and is tested | `input::translate::parse_uri_list` is already written and covered, because X11 XDND and `winit` 0.31 both hand over `text/uri-list` |

**The fallback that was *not* taken, and why.** `winit 0.30 + vendored
octotablet` is the phase document's pre-planned answer to a failed E2. It
is not adopted now, for a measured reason on top of the known one
(crates.io frozen at 0.1.0 since 2024, bus factor 1): **`octotablet 0.1.0`
has no `dlopen` path for `wayland-client`.** Building it here fails in
`wayland-sys`'s build script demanding `wayland-client.pc`, because
`octotablet` exposes no feature that turns `wayland-sys/dlopen` on. That
hard-links `libwayland-client.so` into the binary, which collides with
`packaging.md` decision 2 and invariant 3 — the AppImage must start on a
host that has no Wayland client library. Vendoring it therefore means
vendoring *and patching* it, not just copying it. That is a real cost and
it should be paid deliberately, when someone can also run E1–E7.

**Re-open this when** any of the following happens, and before v0.1 in any
case: a machine with a compositor and a tablet is available to run E1–E7;
`winit` 0.31 reaches stable; or `accesskit_winit` and `egui` adopt 0.31.
All of `winit` is behind `xarast-shell` and, within it, behind
`input::translate` — criterion 16 holds and the switch is one module plus
a version line.

### Current state

`crates/xarast-shell`, building on the phase 0 walking skeleton (window,
surface, resize, scale change, `--selftest-window`, cold-start
instrumentation), which was extended rather than replaced.

| Module | What it owns | State |
|---|---|---|
| `scale` | `ScaleFactor`, `PhysicalSize`, `LogicalSize`, `PhysicalPos` | Done; the single owner of the fractional scale |
| `display` | `DisplayServer`, `DisplayEnvironment`, `PlatformCapabilities`, `headless_skip_reason` | Done; the X11-versus-Wayland difference table in executable form |
| `decorations` | `DecorationPlan`, `DecorationMode` | Done; GNOME ⇒ CSD mandatory, KDE/wlroots ⇒ SSD expected, XWayland ⇒ SSD |
| `input::event` | `ShellEvent` and everything under it | Done; the platform-neutral contract |
| `input::keyboard` | `Modifiers` with the Xara roles, `Key`, `KeyEvent`, `ModifierTracker`, `Shortcut`, `ShortcutMap<C>` | Done |
| `ime` | `ImeEvent`, `ImeState`, `ImeCursorArea` | Done: the viewer drives the text tool's composition through it (XARA-T-0224, decision 41); compositor behaviour not yet observed |
| `input::tablet` | `ToolAxes`, `StrokeSample`, `normalise`, `TabletSource`, `MouseOnlySource`, `ScriptedSource` | Done; no backend supplies real axes yet (see the verdict above) |
| `input::coalesce` | `SampleQueue` | Done; never drops a sample |
| `input::translate` | `winit` 0.30 → `ShellEvent`, `parse_uri_list` | Done; **the only module phase 14 rewrites** |
| `portal` | `PortalService` on a services thread, `PortalHandle`, `ashpd` FileChooser + Settings | Done; **measured on GNOME 46** (open, save, cancel, missing bus, quit with a dialog open) |
| `clipboard` | `Clipboard` trait, `SystemClipboard` (`arboard`), `NullClipboard` | Done; **measured on GNOME 46**: text and RGBA image both ways with Wayland and X11 peers (through XWayland); **no clipboard on GNOME without XWayland** (XARA-T-0047) |
| `wayland_dnd` (private) | `wl_data_device` drop target on `winit`'s `wl_display`, own thread; `DropTracker` | Done (XARA-T-0042); measured on GNOME 46 |
| `window` | Event loop, `Gpu`, `ShellCtx`, `ShellApp`, `FrameRequest`, `ShellWaker`, `--screenshot` read-back, the adapter ladder | Done; **runs on real hardware** (Intel Arrow Lake iGPU and NVIDIA RTX 4000 SFF Ada, Vulkan; Mesa GL; lavapipe; COSMIC/Wayland) |
| `tiles` | `TilePlanner`, `CpuTileStore`/`GpuTileStore`, `Compositor`: the canvas retained as tiles, GPU and CPU tiers | Done (XARA-T-0050) |
| `probe` | Scripted pan/zoom latency probe (`--probe`) | Done (XARA-T-0050) |
| `intents` | `IntentAdapter`, `semantic_modifiers`, `semantic_button`, `CanvasRegion` — physical `ShellEvent` → semantic `xarast_app::Intent` | Done (XARA-T-0001) |
| `paint` (private) | `Painter`: canvas pass + egui pass in one render pass; `CanvasFrame`, `UiFrame`; `TextureLedger` (CPU copy of the egui textures, for a device rebuild) | Done (XARA-T-0003; ledger XARA-US-0064) |
| `viewer` | `Viewer`, the composition root: `ShellApp` over `AppState` + `Workspace` + `RenderThread` | Done (XARA-T-0003); panels respond (XARA-US-0002) |
| `egui_input` | `EguiInput`: `ShellEvent` → `egui::RawInput`; `cursor_shape` | Done (XARA-US-0002) |
| `gpu_errors` | `GpuErrorSink` (the uncaptured-error and device-lost handler, with a lost flag), `GpuRecovery` (the escalation policy), `DeviceLoss` (the rebuild ladder), `DeviceRebuilt` | Done (XARA-T-0027; loss XARA-US-0064) |
| `window` (AccessKit) | `A11y`: `accesskit_winit` adapter, handlers → `ShellEvent::Accessibility*` | Done (XARA-US-0003), behind the default `accessibility` feature |

161 unit tests (shell), all passing with no compositor; the one that needs
a GPU (`a_validation_error_on_a_real_device_is_counted_not_fatal`) skips
without an adapter.

### The running application (XARA-US-0001)

`xarast FILE.xar …` opens the files and shows the last one;
`xarast --screenshot out.png FILE.xar` renders, reads the composed
swapchain back, writes it and exits. Over the whole 59-file corpus in a
real Wayland window on the NVIDIA machine: **59/59 open, render and
capture**, the three `*GradFilledShapes*` files at 4–5 s each (XARA-T-0014)
and everything else under 2.2 s from launch to capture; cold start to
first frame ~440 ms (budget 400 ms, XARA-T-0010).

```text
 ShellEvent ─► intents::IntentAdapter ─► Intent ─► AppState::apply ─► Changed
 egui frame ─► Workspace::ui ─► UiCommand ─► Viewer::ui_intent ─┘   │
   │                                           needs_scene: rebuild_scene
   ▼                                           needs_redraw: Session::frame_job
 UiFrame ─► ShellCtx::show_ui            RenderThread::submit (CPU backend)
 CanvasFrame ─► ShellCtx::show_canvas ◄─ take_latest ◄─ ShellWaker (proxy)
```

### Decisions taken (and why)

1. **`winit 0.30.13`, not the 0.31 beta.** Above.
2. **One `ScaleFactor` owner.** `scale::ScaleFactor` is constructed in the
   translator from the window and handed downwards; the canvas never reads
   the window and the UI never computes its own `pixels_per_point`. Its
   constructor is validating — a non-finite or non-positive factor
   collapses to 1.0 and anything outside 0.25–8.0 is clamped, because a
   poisoned scale divides into every later coordinate. Sizes **round**,
   they do not truncate: at 1.5× a 801-unit window truncates to 1201 px
   and leaves an unpainted column.
3. **`ShellEvent` is the contract, `input::translate` is the only thing
   phase 14 replaces.** No type above the shell mentions `winit`. The
   model is shaped to the *richer* platform — `DragEvent` carries a path
   list and an optional position even though 0.30 supplies neither — so
   the newer backends fill fields rather than change signatures.
4. **`ToolAxes` is the tablet seam, not a `winit` type.** A backend
   reports raw axes; `tablet::normalise` is the one place that decides
   what they mean. That is what makes pressure testable with no device:
   `ScriptedSource` replays a stroke through the real pipeline.
5. **The sample queue grows; it is not a ring.** Every pointer sample of a
   frame must reach the application (a Wacom samples at ~200 Hz and the
   compositor coalesces to frame rate). A fixed ring would make sample
   loss silent and load-dependent, which is the worst failure mode
   available to a drawing tool. `SampleQueue` records a high-water mark
   instead, so an unreasonable burst shows up in diagnostics rather than
   in the geometry.
6. **Modifiers are sampled, never latched.** `ModifierTracker` publishes a
   change even when no pointer event accompanies it, because `Ctrl`,
   `Shift` and `Alt` change what a drag is doing *while* it happens
   (`research/04 §4.9` item 4). Losing focus clears them: a stale `Ctrl`
   silently constrains the next drag. Xara's names are used for the roles
   — `constrain()`, `adjust()`, `alternative()` — so the code reads as the
   feature inventory does and macOS can remap one function in phase 14.
7. **`Shortcut::works_in_drag` is opt-in.** Everything else is suppressed
   mid-drag, so a stray keystroke cannot run a command in the middle of
   one; the numeric-keypad snapping toggles are the reason the exception
   exists.
8. **The shortcut *table* is not the shell's.** `ShortcutMap<C>` is
   generic over the command type. The command vocabulary belongs to
   `xarast-app`; the shell owns only the matching rule.
9. **Portals on a services thread, and no `async` above it.** File
   dialogs call `org.freedesktop.portal.FileChooser` through `ashpd`
   directly (XARA-T-0041) — the portal path is the one taken inside an
   AppImage or a Flatpak, and no toolkit reaches the image. A dismissal
   is `Cancelled` (response 1, **and 2**: that is what
   xdg-desktop-portal-gtk answers for Escape); anything else is `Failed`
   with a reason. Dropping the service discards queued requests and does
   not wait for a thread still inside a dialog. `ashpd` with `async-io`,
   not `tokio`:
   nothing needs a full runtime and `research/05 §10.1` says no async in
   the core. The main thread posts a `PortalRequestId` and later receives
   a `PortalEvent`; a dialog can never wedge the frame loop.
10. **Portal availability is decided before the first request.** If
    `DBUS_SESSION_BUS_ADDRESS` is unset and `$XDG_RUNTIME_DIR/bus` is
    absent, every request answers `Failed` with that reason immediately,
    instead of a forty-second D-Bus timeout.
11. **The clipboard's caveat is survival after exit, not focus loss.**
    Measured on GNOME 46: a copy survives the window losing focus, and
    even the process exiting, because mutter adopts the selection.
    `Clipboard::persists_after_exit()` is `true` on X11 and GNOME and
    `false` on other Wayland compositors until measured, and the status
    bar is meant to say so (XARA-T-0043). `arboard` uses
    `ext`/`wlr-data-control` where offered (COSMIC, KDE, wlroots) and X11
    otherwise, which on GNOME means XWayland.
12. **Nothing fails for want of a display.** `run` returns
    `ShellError::NoDisplay` with a reason and `is_missing_display()` says
    so; `--selftest-window` prints "skipped" and exits **0**;
    `system_clipboard()` returns a `NullClipboard` that explains itself.
    A red CI result on a machine that simply has no compositor teaches
    everyone to ignore the check.
13. **`ShellApp` has defaults on every method.** The self-tests and the
    cold-start measurement implement nothing, and the default
    `FrameRequest` is `Idle`, so an application with no opinion parks the
    loop rather than spinning it.
14. **One `unsafe` call in the crate, at a documented FFI boundary:**
    `wayland_backend::Backend::from_foreign_display` on `winit`'s
    `wl_display` (decision 27). Everything else — `winit`, `wgpu`, the
    platform setters — is safe interfaces behind `cfg`.
15. **The composition root lives in `xarast-shell` (`viewer.rs`), and the
    shell depends on `xarast-ui`.** Architecture §1 draws the shell under
    the UI and phase-05 criterion 16 allows `egui` in exactly those two
    crates; U5.1 (frame composition) and U4.1 (the egui shim) are both
    the shell's. A separate binary crate would have moved the AppImage's
    `-p xarast-shell --bin xarast` for no gain. `xarast-ui` still does not
    name `winit` or `wgpu`, and `xarast-app` still names neither.
16. **The shell paints `egui` itself** (`paint.rs`, ~400 lines): one
    pipeline, one texture table, one uniform that switches between
    points (interface) and device pixels (canvas). `egui-wgpu` 0.33 is
    built against `wgpu` 27 and the workspace is on 30; two `wgpu`s cannot
    share a device.
17. **The swapchain is 8-bit non-sRGB (`Bgra8Unorm`/`Rgba8Unorm`), chosen
    by exact match.** The CPU canvas and egui both hand over *encoded*
    sRGB; the textures are `Rgba8Unorm` and no stage converts, blending
    premultiplied in encoded space as egui expects. This reverses the
    walking skeleton's "prefer sRGB", which only ever drew a clear colour.
18. **The ShellEvent → Intent table is `intents.rs`, outside
    `input::translate`.** It names no `winit` type, so phase 14 inherits
    it. `xarast-app` cannot host it: it must not see `ShellEvent`.
    Pointer motion comes from the ordered `ShellEvent::Pointer` stream,
    not the per-frame `Stroke` drain, which arrives after that frame's
    presses and releases; the stroke path carries pressure only, which
    `winit` 0.30 never supplies. Wheel: pan 50 px a notch; constrain +
    wheel zooms √2 a notch about the pointer; adjust + wheel pans
    sideways. DPI is `96 × scale`, sent as `Intent::SetDpi`.
19. **The render thread wakes the loop through an `EventLoopProxy`**
    wrapped as `ShellWaker` (`user_event` → `request_redraw`), so the loop
    parks at 0 % between frames and still presents a finished render at
    once. `ShellCtx::waker()` is how anything off the main thread gets it.
20. **`--screenshot` reads back the swapchain**, not the canvas surface:
    it proves the composition (canvas + interface + format), and it works
    on compositors without `wlr-screencopy` (COSMIC has none; `grim`
    fails there). It captures once the rendered frame matches the canvas
    size and nothing is pending.
21. **A `wgpu` error is a diagnosis, never a panic** (XARA-T-0027).
    `GpuErrorSink` is installed with `Device::on_uncaptured_error` (and
    the device-lost callback, ignoring `Destroyed` at exit) before the
    device is used. It logs, counts and keeps the innermost cause line.
    After each frame the loop asks `GpuRecovery`: the first three failing
    frames reconfigure the surface, then presentation backs off
    exponentially from 16 ms to a 2 s cap, while events and application
    state keep flowing. One clean frame resets it. It never exits. The
    application hears `ShellEvent::GpuError` and the viewer shows it in the
    status bar. `XARAST_INJECT_GPU_ERRORS=N` raises a real validation error
    in each of the first N frames, to watch the path in a real window.
22. **Every `ShellEvent` goes to egui; canvas navigation has one owner,
    the `IntentAdapter`** (XARA-US-0002). The canvas widget runs with
    `CanvasNavigation::External`. Two gates keep the adapter out of
    egui's way. First, a press or a wheel at a point where
    `egui::Context::layer_id_at` finds a layer above `Order::Background`
    (a popup or window) is not the canvas's; motion and releases always
    pass, so a drag in progress is never cut. Second, view keys are
    ignored while a text field has the keyboard. Arrows pan only when the
    canvas or nothing has egui focus. The shim is built on `ShellEvent`,
    not `winit`, so phase 14 inherits it.
23. **"A text field has the keyboard" is `platform_output.ime.is_some()`**,
    not `Context::wants_keyboard_input()`, which in egui 0.33 is true for
    *any* focused widget (a Tab-focused button included).
24. **egui's platform output is the shell's to carry**: `CopyText` to the
    clipboard, IME allowed exactly while a text field is focused (with the
    caret area in device pixels, sent only when it moves), and the pointer
    shape through `ShellCtx::set_cursor(CursorShape)`, a toolkit-neutral
    enum mapped to `winit` in `window.rs` and from egui in `egui_input`.
    Ctrl+C/X/V become egui `Copy`/`Cut`/`Paste(text)`.
25. **AccessKit through `accesskit_winit 0.29.2` with direct handlers**
    (XARA-US-0003). The window is created invisible, the adapter is
    attached, then the window is shown (the adapter panics on a visible
    window). The handlers run on AccessKit's thread: they only record
    activation, deactivation and actions in a mutex-guarded inbox and
    wake the loop. The loop drains the inbox into `ShellEvent`s. The
    viewer calls `egui::Context::enable_accesskit` on activation and
    publishes each frame's tree through `ShellCtx::update_accessibility`,
    after naming the root after the window title and setting the toolkit
    to egui. Actions reach egui as `Event::AccessKitActionRequest`.
26. **The colour-scheme watcher is a thread of its own**
    (`xarast-settings`, XARA-US-0004), because the services thread blocks
    inside an `rfd` dialog. It awaits `receive_color_scheme_changed()`
    under `pollster` and is detached: it ends at the first change after
    the service is dropped. Both service threads call a waker after each
    answer, because a loop parked in `Wait` otherwise sees a portal answer
    only at the next input event.
27. **The canvas is retained as tiles and composited at every present**
    (XARA-T-0050, `tiles.rs`). The viewer hands each rendered frame over
    as a `TiledFrame` (`ShellCtx::show_tiled_frame`) and, every frame, the
    session's current view as a `CanvasView` (`set_canvas_view`). The
    shell's `TilePlanner` cuts frames into 256² tiles of a level (one zoom,
    one pixel grid) and uploads only what the store lacks; the composite of
    the current view goes into the painter's canvas texture before the
    canvas + interface pass, in the same encoder. A pan or a Draft zoom is
    therefore on screen at input time; the render thread's strips fill the
    exposed areas when they arrive. `show_canvas` (a whole image) still
    works and deactivates the compositor.
28. **The capability ladder, as built.** Adapters: `WGPU_ADAPTER_NAME`
    (substring, case-insensitive; no match is a warning, not `wgpu`'s
    panic), then high-performance, then `force_fallback_adapter`
    (lavapipe/llvmpipe); the first that yields a device wins, and only
    none is `ShellError::NoAdapter`. `WGPU_BACKEND` is honoured
    (`Backends::with_env`); it was documented but ignored before. Canvas
    tier: **GPU tiles** by default; **CPU** (`compose_cpu` over the same
    tiles in memory, uploaded whole) when `XARAST_RENDERER=cpu`, when the
    tile cache cannot be created, or when GPU errors persist into
    `Recovery::Backoff` (the demotion refills from the last frame and is
    permanent for the session). `gpu` and `hybrid` both mean GPU tiles:
    there is no GPU rasteriser to tell them apart. The status bar reads
    "<tier> · <adapter> (<backend>)" from `ShellCtx::renderer()`.
    Verified live: lavapipe (GPU tiles on a software adapter),
    `XARAST_INJECT_GPU_ERRORS=6` (demotes to CPU at the fourth failing
    frame and keeps presenting), a `WGPU_ADAPTER_NAME` that matches
    nothing, and `WGPU_BACKEND=gl` (Mesa Intel GL, GPU tiles).
29. **The instance gets the window's display handle**
    (`InstanceDescriptor::new_with_display_handle`). The GL backend needs
    it for EGL on Wayland: without it `WGPU_BACKEND=gl` found no adapter.
30. **`FrameRequest::RedrawAfter` really redraws.** It used to set a
    `WaitUntil` and nothing else, so the owed Final appeared only if some
    other event asked for a frame; once Draft zooms stopped publishing,
    `--screenshot` hung about one run in three. `ShellLoop::redraw_at`
    is checked in `about_to_wait`.
31. **Latency probes drive the viewer, never the desktop.**
    `xarast --probe pan|zoom [--probe-samples N] [--synthetic N]` applies
    one scripted intent per frame once the document has settled,
    presents without vsync, waits for the GPU after each present
    (`ShellConfig::probe`, `PresentTiming`) and prints input → presented
    and input → GPU idle percentiles, then exits (or, with
    `--screenshot`, captures the view it left). `examples/canvas_probe`
    runs the same path offscreen at any size (COSMIC tiles the window,
    so the live canvas is 1796 × 1338 whatever `--size` asks).
32. **Commands, shortcuts and platform requests** (XARA-US-0082). The
    viewer maps `UiCommand::App` through `AppCommand::intent(canvas
    centre)` and binds keys with a `ShortcutMap<AppCommand>` built from
    the same table (`command_shortcuts`): Ctrl+O/W/Q, `+`/`=`/Ctrl+`=`,
    `-`/Ctrl+`-`, `0`/Home, `d`, `1`. Punctuation is bound with and
    without Shift (`+` is Shift+`=` on US layouts); letters and digits
    exactly. The old hard-coded `key_intent` is gone. No shortcut fires
    while a text field has the keyboard (`text_input`, decision 23) or,
    unless `works_in_drag`, during a drag. `AppState::take_requests()` is
    drained after every apply and carried out in `perform_requests`
    (from `on_event` and `on_frame`): `ShowOpenDialog` → one
    `PortalHandle::open_files` at a time (filters "Xara documents
    (*.xar)" and "All files"), `Quit` → `ctx.exit()`. The answer is a
    `ShellEvent::Portal` matched on the outstanding request id; a chosen
    path goes through the same `to_open` queue as the command line and
    drops (`Intent::OpenFile`, replace). A portal failure goes to the
    status bar and the problem list. The binary passes
    `default_store_path()` to `Viewer::with_recent_store`; tests don't.
33. **Portal dialogs are parented** (XARA-T-0048). On Wayland,
    `wayland_export` exports the toplevel once at window creation with
    `xdg-foreign-unstable-v2` on its own connection over `winit`'s
    `wl_display` and keeps the export alive with the window (dropped in
    `exiting`); the portal gets `wayland:<handle>`. X11 gets `x11:<xid>`.
    `ShellCtx::parent_window()`; `OpenFileRequest`/`SaveFileRequest`
    carry `parent`, parsed with `ashpd::WindowIdentifierType` (an
    unparsable one opens unparented, never fails). Measured: COSMIC and
    GNOME 46 both hand out a handle. This adds two `unsafe` calls at the
    same FFI boundary as decision 27 (`from_foreign_display`, and
    `ObjectId::from_ptr` for `winit`'s `wl_surface`).
34. **`--screenshot` with no document waits six frames** (50 ms apart)
    before capturing. On Wayland the fractional scale arrives after the
    first frame: at 1.25 the first frame was laid out at 1.0 in a 1.25
    surface.
35. **Saving in the shell** (XARA-US-0084; the flow is `app-core.md`
    "Saving"). File › Save (Ctrl+S) and Save As… (Ctrl+Shift+S) are
    `AppCommand`s; `PlatformRequest::ShowSaveDialog` becomes one parented
    `PortalHandle::save_file` at a time, filtered to "Xarast documents
    (*.xarast)" only and pre-filled with `name.xarast` in the document's
    directory. `SaveChosen` → `Intent::SaveTo`; Cancelled/Failed →
    `SaveDialogClosed` (a failure also goes to the status bar and the
    problem list). `Viewer::housekeeping` runs at every event and frame:
    `poll_saves` (the save thread wakes the loop through `ShellWaker`),
    the core's notice into the status bar, `tick` for autosave (its due
    time joins the Final's in the `RedrawAfter`), the recovery question
    once (not with `--screenshot`/`--probe`), and the signal flag.
36. **The title shows unsaved work**: `• name — Xarast`, and
    `name (read-only) — Xarast` for a document another session holds.
    It is compared every frame rather than tracked, because any edit,
    undo, redo or save can change it.
37. **The window's close button is Quit.** `ShellApp::handles_close()`
    (default false) keeps `window.rs` from exiting on `CloseRequested`;
    the viewer applies `Intent::Quit`, which asks about unsaved changes
    and queues `PlatformRequest::Quit` → `ctx.exit()` only when done.
38. **Questions are egui modals** (`xarast-ui::dialogs`, from
    `UiModel::prompt`): the core's `Prompt` choices as buttons, the
    default focused, Escape/click outside = the cancel answer, Enter = the
    default. There is no portal for a question dialog.
39. **Signals** (`signals.rs`, `signal-hook`'s safe iterator on its own
    thread): SIGINT/SIGTERM/SIGHUP set a flag and wake the loop; the
    viewer calls `AppState::emergency_shutdown` (autosave modified
    documents, drop every session → locks released) and exits; `main`
    returns `128 + signal`. If that has not happened 5 s later, or a
    second signal arrives, the thread calls `locks::release_all()` and
    `process::exit`. Measured on COSMIC with a real window holding a
    `.xarast` lock: SIGTERM → orderly shutdown in 214 ms, lock file gone,
    exit status 143.
40. **The text tool on screen** (XARA-US-0047, phase 9 W9.4). The state
    machine is the core's (`text_tool.rs`, `tools.md` decision 53); the
    shell and the interface only route keys and draw.
    - **Keys.** While `Session::text_editing()` (a caret is up, in a
      story or pending), a pressed key goes first to `Viewer::text_nav`:
      arrows, Home, End, Page Up/Down become `Intent::TextNav { key,
      word: Ctrl, extend: Shift }` (canvas focused or nothing focused,
      the same gate as arrow panning), so they neither pan nor fit the
      page. Then `Viewer::text_typing`: Backspace and Delete (Ctrl: by
      word), Enter (no Ctrl/Alt), and any key whose `KeyEvent::text` holds
      a printable character or a tab with neither Ctrl nor Alt held become
      `Intent::TextInput { kind, time_ms }`, stamped with
      `IntentAdapter::now_ms` (the pointer samples' clock) for the 500 ms
      typing-burst window (`tools.md` decision 57). So `l c s z b j d 0 1
      3 # + -`, Space (momentary selector) and Tab type rather than act
      while a caret is up; plain character keys without text are still
      swallowed. Esc, F-keys and Ctrl chords still run their commands; Esc
      leaves the text. IME composition: decision 41.
    - **Caret.** `OverlayShape::Caret { from, to, primary, moved }` →
      `OverlayItem::Caret`: a black line with a white halo (1.5 hairline
      for the primary caret, 1 for the secondary half of a split caret)
      between the line's descent and ascent, turned with the story.
      Blink: 0.53 s on / 0.53 s off from the moment `moved` changed
      (tracked in egui temp memory), stopping — caret left on — after
      10 s idle, so an idle window stops repainting; the painter asks
      egui for the repaint at the next toggle (`caret_blink`).
    - **Selection.** `OverlayShape::Highlight { corners }` →
      `OverlayItem::Highlight`: accent at 35 % over the text, one
      quadrilateral per visual span of a line (a rotated story's spans
      are turned too).
    - **Pointer.** `CursorKind::Text` → `CursorShape::Text` (I-beam) over
      the whole canvas with the text tool.
41. **IME and clipboard at the text caret** (XARA-T-0224, T9.4.7–T9.4.8).
    - **The IME is on exactly while a caret wants it**:
      `Viewer::ime_request(field)` is egui's focused text field (its
      cursor rect in points × scale) or, when the canvas has the keyboard
      (`canvas_has_keyboard`: no egui text input, canvas focused or
      nothing focused), `Session::ime_cursor_area()` — the text caret's
      box in canvas pixels, offset by the canvas origin; else `None`.
      `platform_output` turns `set_ime_allowed` on/off on change only and
      sends `set_ime_cursor_area` when the box moves (physical pixels,
      the candidate window opens under the caret's bottom). Turning it off
      drops a composition still shown (`ime_ended` → `TextPreedit(None)`).
    - **Routing.** `ShellEvent::Ime` with a caret up and the canvas
      holding the keyboard goes to `Viewer::ime_event`: the viewer's own
      `ImeState` (egui keeps its own for its fields) turns
      `PreeditChanged` into `Intent::TextPreedit(Some(Preedit { text,
      cursor }))` (empty text → `None`), `Cancelled` into
      `TextPreedit(None)`, and `Committed` into `Intent::TextInput(
      Insert)` stamped with `IntentAdapter::now_ms`. Enabled alone does
      nothing.
    - **Keys while composing are the IME's**: with `ImeState::is_composing`
      and a caret up, a pressed key is dropped before the text-caret
      branch (no navigation, typing, shortcut or momentary switch). Most
      platforms send none then; X11/XIM stacks may.
    - **Clipboard**: nothing new in the shell. Ctrl+C/X/V reach the same
      `AppCommand`s; the core decides text versus objects (`tools.md`
      decision 63) and the existing `SetClipboardText`/`ReadClipboard`
      requests carry plain text through `arboard` — no new dependency, so
      the release binary's links are unchanged. Tests use a
      `FakeClipboard` behind the `Clipboard` trait (`with_clipboard`) and
      synthetic `winit::event::Ime` values through
      `input::translate::translate_ime` (now `pub(crate)`).
    - **Not yet observed on a compositor**: whether the candidate window
      lands under the caret on GNOME (IBus via text-input-v3) and on a
      wlroots compositor; whether any stack sends a key event *and* a
      commit for one keystroke (would double the character). XARA-T-0267.
42. **File › Import… and background imports** (XARA-US-0055, T10.7.4,
    T10.7.5). `AppCommand::Import` (Ctrl+Shift+I; the original's Ctrl+I
    is its image slicer) → `PlatformRequest::ShowImportDialog` → one
    parented `PortalHandle::open_files` at a time (`import_dialog`, apart
    from the Open chooser), title "Import", `multiple: true`, filters
    "Images (PNG, JPEG, GIF, WebP, TIFF, BMP, PNM)" and "All files". Each
    chosen path becomes `Intent::ImportImage { at: None }` (the view's
    centre); a portal failure goes to the status bar and the problem list
    as Open's does. `housekeeping` calls `AppState::poll_imports` beside
    `poll_saves` (the same waker wakes the loop for both, and for
    thumbnails) and redraws when `poll_thumbnails` says one arrived.
    Tests: `PortalService::offline` and synthetic `FilesChosen` answers.
43. **Crashes, device loss and safe mode** (XARA-US-0064; the core half
    is `app-core.md` "Crashes").
    - **Panic hooks.** `install_panic_hook` chains three: the crash
      report (`xarast_app::crash`, installed *last* so it runs *first*),
      a log line naming thread and location only (a run-time message may
      carry document text, and the log feeds later reports), then Rust's
      own message. `init_tracing` adds `CrashLogLayer`, which copies every
      event that passes the filter into the report's 200-line ring.
    - **Panic on the interface thread**: it unwinds through `winit` and
      drops the `Viewer`, whose `Drop` runs `emergency_shutdown` when
      `std::thread::panicking()`. **Panic on the render thread**:
      `crash::run_guarded` raises `FatalWatch::global()`, which wakes the
      loop; `housekeeping` answers exactly as for a signal and `main`
      returns 101. Tests: `a_fatal_worker_panic_autosaves_the_work_and_exits`,
      `a_panic_on_the_interface_thread_autosaves_while_unwinding`.
    - **The sentinel.** `main` calls `crash::begin_session` after the
      arguments (not for `--screenshot`/`--probe`), and `Sentinel::finish`
      only on a clean return; a fatal panic returns 101 before it, a
      main-thread panic unwinds past it.
    - **Device loss (F2).** The device-lost callback calls
      `GpuErrorSink::record_lost` (a flag besides the error count). After a
      frame, a set flag makes the loop drop the `Gpu` into a `GpuCarry`
      (compositor's last `TiledFrame` and `CanvasView`, the
      `TextureLedger`, the last egui meshes and scale — all CPU memory,
      nothing read back) and call `Gpu::new` again on the same window;
      `restore` refills the compositor and replays every texture whole.
      `DeviceLoss`: the first loss rebuilds as configured, from the second
      on with `RendererPreference::Cpu`; a failed rebuild waits 250 ms
      doubling to 2 s and retries at a redraw, absorbing whatever the
      application hands in meanwhile into the carry (only the last frame
      and view are kept). Never exits. The application hears
      `ShellEvent::GpuRebuilt` (status line "…restarted (CPU · …); nothing
      was lost"). The document is never involved: it lives in `AppState`.
    - **Testing it without a GPU.** `wgpu`'s `noop` backend, as a
      dev-dependency feature only (`Cargo.lock` unchanged; the binary
      never has it): `a_lost_device_hands_its_picture_to_a_cpu_compositor_on_a_new_device`
      moves a GPU-tier compositor's picture to a CPU-tier one on a second
      no-op device and compares pixels. In a window,
      `XARAST_INJECT_DEVICE_LOSS=N` raises the lost flag after N frames —
      a simulation: no device is destroyed. Not run on the maintainer's
      desktop (no windows from agents).
    - **Safe mode (F5).** `ShellConfig::safe_mode` (from `--safe-mode` or
      a forced crash loop): `RendererPreference::Cpu` and
      `candidate_adapters(.., prefer_software)`, which tries the fallback
      (lavapipe/llvmpipe) adapter first. Chosen from the crash prompt,
      `PlatformRequest::EnterSafeMode` → `ShellCtx::request_safe_mode` →
      the loop rebuilds the device through the same carry path
      (`RebuildCause::SafeMode`). Preferences reset in memory; there is
      no persisted layout, theme, plugin or gallery scan to skip yet.

### Invariants that must not be broken

1. **`winit` and `wgpu` appear only in `xarast-shell`** (architecture §2,
   phase criterion 16). Nothing else may name them, and within the crate
   `winit` types appear only in `input::translate` and `window`.
2. **The app id, the desktop entry basename and `StartupWMClass` are all
   `xarast`** (`packaging.md` invariant 1). `lib.rs`'s
   `app_id_matches_the_desktop_entry` test enforces it; it reads the real
   `packaging/linux/xarast.desktop`.
3. **One `ScaleFactor` per frame, computed by the shell.** A surface size
   and the scale it was computed under always travel together —
   `ShellEvent::Resized` carries both — so no consumer can pair a new size
   with a stale factor.
4. **Every pointer sample of a frame reaches the application.**
   `SampleQueue::received() == delivered()` after a drain, asserted.
5. **No `async` outside the services thread.**
6. **A missing display, a missing session bus or a missing adapter is a
   diagnosis, never a panic.** Every public entry point either returns an
   error naming the cause or returns a working object that explains
   itself.
7. **A surface is never configured at zero size.** `PhysicalSize::new`
   clamps to 1×1 and `PhysicalSize::is_degenerate` is what the resize path
   checks. (Measured on GNOME 46: minimising does *not* produce 0×0 on
   Wayland — xdg-shell has no minimised state — the size stays, frame
   callbacks stop, the loop keeps answering at ~0 % CPU and restores
   cleanly. The 0×0 case is Windows' and X11's.)
8. **A surface is never reconfigured while a `SurfaceTexture` is held.**
   `Suboptimal` presents first, then reconfigures. `wgpu` treats the
   violation as a fatal validation error (it killed the window on
   `Designs/Groucho2.xar`).
9. **Texture deltas from egui reach the GPU exactly once, in order**,
   applied right after `on_frame` whether or not the present succeeds;
   two interface frames between presents merge their deltas.
10. **Pass order is fixed: canvas, then interface**, in one render pass
    with one encoder. The interface is premultiplied over the canvas.
11. **No GPU error reaches `wgpu`'s default panic handler.** The sink is
    installed right after `request_device`, before any resource exists.
12. **Exactly one party navigates the canvas.** The shell's canvas widget
    is `External`. `one_ctrl_wheel_notch_over_the_canvas_zooms_exactly_one_step`
    and `a_plain_wheel_notch_pans_exactly_once` pin it. With the widget
    back on `Internal`, one notch zoomed 1.466× instead of √2.
13. **The AccessKit adapter exists before the window is first shown**,
    and its handlers never touch interface state: they only queue and
    wake.
14. **`WaylandDrops` and `ExportedWindow` are dropped in `exiting`,
    before the event loop.** It
    borrows `winit`'s `wl_display`; its `Drop` wakes its thread with a
    `wl_display.sync` on its own queue and joins it (measured: clean quit
    in 146 ms).
15. **No test posts a request to a live portal.** Tests use
    `PortalService::offline`; a real service on a developer's desktop
    would open dialogs on their screen and wait for them.
16. **The GPU and CPU canvas tiers are byte-identical.** Both run the
    same `TilePlanner` placements; the GPU through `GpuTileCache`, the CPU
    through `compose_cpu`. `the_gpu_tier_composites_byte_for_byte_as_the_cpu_tier`
    pins it on whatever adapter the test finds.
17. **A tile's valid area is always a rectangle of pixels it holds.** A
    piece that would not join it into one restarts the tile.
18. **A device rebuild needs nothing from the application.** Everything
    a new `Gpu` shows comes from the `GpuCarry` (`TextureLedger`,
    compositor carry, last meshes). Every egui texture delta applied to
    the painter is applied to the ledger too, in the same order.
19. **No test loses, destroys or resets a real device.** Device loss is
    simulated (`record_lost`, the `noop` backend); a real-GPU test still
    takes `gpu_test_lock::acquire`.
20. **The `Viewer` is never destructured or moved out of by field**: its
    `Drop` is the interface thread's emergency save.

### Dead ends (do not retry)

- **Adding `accesskit_winit 0.34` alongside `winit 0.31`.** It resolves a
  second `winit 0.30.13` into the graph. Measured with `cargo tree -i`.
- **`winit`'s `rwh_06` feature on 0.31.** It no longer exists; passing it
  fails resolution outright. raw-window-handle 0.6 is unconditional there.
- **`octotablet 0.1.0` as a drop-in.** Its `wayland-client` has no
  `dlopen` feature, so it needs `wayland-client.pc` to build and links
  `libwayland-client.so` hard. Vendor *and patch*, or not at all.
- **`rfd` with default features.** They include the `wayland` backend,
  which drags in the same non-`dlopen` `wayland-client`; `xdg-portal`
  alone is what builds and what belongs in the AppImage.
- **`arboard` without `image-data`.** `get_image`/`set_image` simply do
  not exist; the feature is what compiles them in.
- **A fixed-capacity ring buffer for stroke samples.** See decision 5.
- **An sRGB swapchain.** Double-encodes both passes; everything comes out
  washed out.
- **"The first non-sRGB format the surface offers".** On NVIDIA/Wayland
  that can be a 10-bit or half-float format: wrong colours and not four
  bytes a pixel (the read-back failed validation). Match
  `Bgra8Unorm | Rgba8Unorm` exactly.
- **`egui-wgpu` 0.33.** Pairs with `wgpu` 27; see decision 16.
- **`grim` for screenshots on COSMIC.** No `wlr-screencopy`; the desktop
  portal screenshot captures the whole desktop, other windows included.
  Use `xarast --screenshot`.
- **Finding the window on AT-SPI by its title.** AccessKit does not know
  the `winit` title; before the root was named, the frame was anonymous.
  Match the application by process id (`Atspi.Accessible.get_process_id`).
- **Expecting AccessKit to appear with AT-SPI off.** `accesskit_unix` only
  registers when `org.a11y.Status.IsEnabled` (or `ScreenReaderEnabled`)
  is true. On this COSMIC session both are false unless a screen reader
  runs. The check script sets `IsEnabled` for the run and restores it.
- **Flipping the live desktop theme to test the colour-scheme signal.**
  Unnecessary and intrusive. Run the app under `dbus-run-session` with a
  small fake `org.freedesktop.portal.Settings` (python `Gio`: `ReadOne`,
  `Read`, `ReadAll`, `version = 2`, `SettingChanged`).

### How the compositor experiments are run (no human, no live desktop)

The maintainer's COSMIC session is never touched. Everything runs in an
isolated GNOME session on a private bus:

1. Export *first* a private `XDG_RUNTIME_DIR` (0700), `XDG_CONFIG_HOME`,
   `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME` and a private
   `DCONF_PROFILE`, then `dbus-run-session`. **`DCONF_PROFILE` must be
   the path of a profile *file* containing `user-db:xarasttest`**, not the
   string itself: given the string, dconf warns "unable to open named
   profile" and uses the null configuration, so every `gsettings set`
   fails (harmless, but mutter then offers no fractional scales). With
   the file, the db lands in `$XDG_CONFIG_HOME/dconf/xarasttest`.
2. Inside: `gsettings set org.gnome.mutter experimental-features
   "['scale-monitor-framebuffer']"`, hot corners off, then
   `gnome-shell --headless --wayland [--no-x11] --wayland-display
   wayland-xa --virtual-monitor 1600x1000`, then
   `/usr/libexec/xdg-desktop-portal-gtk -r` and `xdg-desktop-portal -r`.
   With XWayland, clients need `DISPLAY` (from "Using public X11 display"
   in the log) and `XAUTHORITY=$XDG_RUNTIME_DIR/.mutter-Xwaylandauth.*`.
3. Input: one long-lived `org.gnome.Mutter.RemoteDesktop` session
   (`NotifyPointerMotionRelative` from a clamp at (0,0) is exact at 1×,
   `NotifyPointerButton` with evdev codes, `NotifyKeyboardKeysym`).
4. Screenshots of the nested desktop: own the bus name
   `org.gnome.Screenshot` (gnome-shell's allow-list), then call
   `org.gnome.Shell.Screenshot.Screenshot`. Our own window:
   `xarast --screenshot`.
5. Scale: `org.gnome.Mutter.DisplayConfig.ApplyMonitorsConfig` (method 1,
   temporary) with a scale from the mode's supported list.
6. Harness: `cargo run -p xarast-shell --example platform_probe`
   (stdin commands, every `ShellEvent` on stdout); a GTK3 python window as
   drag source and clipboard peer. Unmount `$XDG_RUNTIME_DIR/{doc,gvfs}`
   (FUSE) when done.

### Open TODOs

- [x] **Run E1–E7 for real** — done on GNOME 46 (table above,
  XARA-US-0012). Still open: KDE/kwin and sway (not installed here), an
  eight-hour soak, and the 0.31 beta itself.
- **Hardware tablet validation is outstanding** (phase risk K6), and so is
  the `uinput` virtual tablet: neither exists in this environment.
  `ScriptedSource` covers the pipeline; it does not cover the driver.
- [x] Portal dialogs, clipboard round-trips and drag-and-drop — measured
  and fixed (XARA-T-0041/42/43); dialogs are parented (decision 33,
  XARA-T-0048). Open: clipboard on GNOME without
  XWayland (XARA-T-0047), drag *out* of Xarast (no API in `winit` 0.30).
- [x] `wgpu` adapter selection and the capability ladder — decision 28
  (XARA-T-0050): adapter ladder, GPU tiles / CPU tier, runtime demotion.
  Still open: `--version --verbose` does not print the tier (it needs a
  device), there is no GPU rasteriser tier (by the GPU decision), a
  lost device is rebuilt since XARA-US-0064, and the GL swapchain cannot be read back,
  so `--screenshot` under `WGPU_BACKEND=gl` logs a failed capture.
  Frame pacing beyond `Wait`/`WaitUntil` is still the skeleton's.
- [x] **`wgpu` validation errors are fatal** — no longer: decision 21
  (XARA-T-0027). A *lost device* is rebuilt since decision 43
  (XARA-US-0064). Still open: the rebuild has never met a real driver
  reset (soak under a driver-reset script on the reference machine, phase
  12 risk table), and whether `wgpu` 30 reports a suspend-cycle loss
  through the device-lost callback or only as surface errors is unmeasured.
- [x] **The panels are drawn but inert** — they respond; decision 22
  (XARA-US-0002). Real mouse and keyboard were not injected on the
  maintainer's desktop (`ydotool` exists but would type into whatever
  has focus). The end-to-end tests drive real egui frames headlessly.
- [x] **Live** colour-scheme change — decision 26 (XARA-US-0004),
  verified against a fake portal on a private bus. The real COSMIC portal
  was not flipped.
- [x] AccessKit transport (S10/U4.2) — decision 25 (XARA-US-0003),
  verified with a real AT-SPI client (`gi.Atspi`): 39 nodes, 29 labelled,
  and an AT-SPI `click` on "Hide layer Main layer" came back as "Show
  layer Main layer". **The audible Orca pass is still to do by hand**
  (Orca is installed, but running it speaks on the desktop and writes
  its settings).
- [x] The `egui` shim (S9/U4.1) — `egui_input`, on `ShellEvent`.
- [x] Cursor shapes and the IME caret area — measured on GNOME 46 (12
  shapes; ibus candidate window under the caret at 1× and 1.25×). Not
  measured on COSMIC.
- [ ] **The live Save As dialog has not been driven end to end**
  (XARA-US-0084): tests use `PortalService::offline` and synthetic
  `SaveChosen`. Manual check: open a `.xar`, draw a rectangle → title
  `• name.xar — Xarast`; Ctrl+S → a chooser titled "Save As" attached to
  the window, filter "Xarast documents (*.xarast)", name `name.xarast`
  in the file's folder; Save → status "Saved name.xarast (…)", the
  marker goes, the `.xar` is untouched; Ctrl+Z → marker back, Ctrl+Shift+Z
  → gone; Ctrl+W after an edit → "Unsaved changes" dialog, Cancel keeps
  it, Discard closes; the window's close button asks the same; open the
  same `.xarast` in a second `xarast` → "Document in use" with read-only /
  copy / force; `kill -TERM` a session with unsaved work → next start
  offers "Recover unsaved work".
- [ ] **The live File › Import… chooser, a file-manager copy and the
  bitmap gallery have not been seen in a window** (XARA-US-0055): tests
  use `PortalService::offline`, `FakeClipboard` and synthetic drags.
  Manual check: open a `.xar`, Ctrl+Shift+I → a chooser titled "Import"
  attached to the window, "Images (…)" selected, several files
  selectable; pick two PNGs → both land in the middle of the view, one
  undo step each; pick a > 1 MB photograph → the status bar shows
  "Importing …" with a bar and Cancel, then it lands (Cancel → nothing);
  copy two images in Nautilus, Ctrl+V → both placed; drag a row of the
  bitmap gallery onto a rectangle → it takes a bitmap fill, onto empty
  canvas → a new bitmap there; the gallery's thumbnails appear.
- [ ] **The live File › Open dialog has not been driven end to end**
  (XARA-US-0082): tests use `PortalService::offline` and synthetic
  `PortalEvent`s; opening a real dialog needs a click or Ctrl+O on a
  desktop. Manual check: launch `xarast`, Ctrl+O → a chooser titled
  "Open" appears *attached to the Xarast window*, filter "Xara documents
  (*.xar)" selected, "All files" offered; pick a `.xar` → it replaces the
  document, the title changes, it heads File › Open Recent; Escape →
  nothing changes; a non-`.xar` via "All files" → status bar says "Could
  not open …" and the old document stays.
- [x] **Bridge `ShellEvent` to `xarast_app::Intent`** — `intents.rs`,
  decision 18 (XARA-T-0001).
- X11 pressure via `octotablet` remains deferred; X11 is a documented
  degradation (`PlatformCapabilities::X11`), not a target.
- [ ] **The text caret has not been seen in a live window** (XARA-US-0047):
  the key routing and the overlay are covered headlessly
  (`a_text_caret_takes_the_navigation_and_character_keys`, the overlay
  painting test). Manual check: F8, click a story of
  `TextDesigns/hebrew.xar` → a blinking caret; Right walks the Hebrew
  leftwards on screen through the bytes backwards; Shift+arrows
  highlight; the caret stops blinking after 10 s; the IME caret area is
  not yet fed from the text caret (T9.4.7).
