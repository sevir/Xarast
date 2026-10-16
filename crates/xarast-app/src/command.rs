//! The named operations a menu item or a shortcut runs, and their keys.
//!
//! This is the application's command table (`phase-05 §W4.7`): one list
//! that the menu bar draws, the shell binds keys from and the tests walk.
//! A command is not an [`Intent`] — it is the *name* of an operation, with
//! a label and its shortcuts — and [`AppCommand::intent`] is the one place
//! that turns it into what the application core consumes. That keeps the
//! rule "the UI and the shell raise the same intents" true by construction:
//! both go through this table.
//!
//! Keys are described here in a toolkit-neutral way ([`KeyChord`]); the
//! shell turns them into its own `Shortcut`s and owns the matching rule,
//! including "never while a text field has the keyboard".

use std::fmt;

use crate::edit::ToolId;
use crate::geometry::DevicePoint;
use crate::intent::{Dialog, DockPane, Intent};
use crate::snap::SnapKind;
use crate::structure::ZOrder;
use crate::tool::ToolAction;
use crate::viewport::ZoomTarget;

/// The key of a [`KeyChord`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChordKey {
    /// A key that types a character, as the layout produces it.
    Char(char),
    /// The Home key.
    Home,
    /// The Delete key.
    Delete,
    /// The Backspace key.
    Backspace,
    /// The Enter (Return) key.
    Enter,
    /// The Escape key.
    Escape,
    /// A function key, `F1` to `F24`.
    Function(u8),
    /// A key of the numeric keypad, by the character it types (`.`, `2`,
    /// `*`): only the keypad's key matches, not the main block's.
    NumPad(char),
}

/// A key and the modifiers held with it.
///
/// `ctrl` is the platform's command modifier: Control on Linux and
/// Windows, and the one the shell maps to Command on macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    /// The key.
    pub key: ChordKey,
    /// The command modifier.
    pub ctrl: bool,
    /// Shift. For punctuation (`+`), the layout decides whether Shift is
    /// needed to type it, so the shell accepts it either way; see
    /// [`KeyChord::shift_is_layout_dependent`].
    pub shift: bool,
}

impl KeyChord {
    /// A key on its own.
    #[must_use]
    pub const fn plain(key: ChordKey) -> KeyChord {
        KeyChord {
            key,
            ctrl: false,
            shift: false,
        }
    }

    /// The key with the command modifier.
    #[must_use]
    pub const fn ctrl(c: char) -> KeyChord {
        KeyChord {
            key: ChordKey::Char(c),
            ctrl: true,
            shift: false,
        }
    }

    /// A character key on its own.
    #[must_use]
    pub const fn char(c: char) -> KeyChord {
        KeyChord::plain(ChordKey::Char(c))
    }

    /// The key with the command modifier and Shift.
    #[must_use]
    pub const fn ctrl_shift(c: char) -> KeyChord {
        KeyChord {
            key: ChordKey::Char(c),
            ctrl: true,
            shift: true,
        }
    }

    /// A function key on its own.
    #[must_use]
    pub const fn f(n: u8) -> KeyChord {
        KeyChord::plain(ChordKey::Function(n))
    }

    /// A function key with the command modifier.
    #[must_use]
    pub const fn ctrl_f(n: u8) -> KeyChord {
        KeyChord {
            key: ChordKey::Function(n),
            ctrl: true,
            shift: false,
        }
    }

    /// A key of the numeric keypad.
    #[must_use]
    pub const fn numpad(c: char) -> KeyChord {
        KeyChord::plain(ChordKey::NumPad(c))
    }

    /// A function key with Shift.
    #[must_use]
    pub const fn shift_f(n: u8) -> KeyChord {
        KeyChord {
            key: ChordKey::Function(n),
            ctrl: false,
            shift: true,
        }
    }

    /// Whether the character is one some layouts type with Shift and
    /// others without (`+` is Shift+`=` on a US keyboard and a key of its
    /// own on the numeric keypad). Letters and digits are not: Shift
    /// changes what they are.
    #[must_use]
    pub const fn shift_is_layout_dependent(&self) -> bool {
        matches!(self.key, ChordKey::Char(c) if !c.is_ascii_alphanumeric())
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        match self.key {
            ChordKey::Char(c) => write!(f, "{}", c.to_ascii_uppercase()),
            ChordKey::Home => f.write_str("Home"),
            ChordKey::Delete => f.write_str("Del"),
            ChordKey::Backspace => f.write_str("Backspace"),
            ChordKey::Enter => f.write_str("Enter"),
            ChordKey::Escape => f.write_str("Esc"),
            ChordKey::Function(n) => write!(f, "F{n}"),
            ChordKey::NumPad(c) => write!(f, "NumPad {c}"),
        }
    }
}

/// A named operation of the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppCommand {
    /// File › Open…: ask the platform for a file to open.
    Open,
    /// File › Import…: ask the platform for image files to place in the
    /// document (phase 10, T10.7.4).
    Import,
    /// File › Save: write the active document to its `.xarast`, asking
    /// for a name the first time.
    Save,
    /// File › Save As…: write it under a new name.
    SaveAs,
    /// File › Close: close the active document.
    Close,
    /// File › Quit.
    Quit,
    /// View › Zoom in.
    ZoomIn,
    /// View › Zoom out.
    ZoomOut,
    /// View › Fit page.
    FitPage,
    /// View › Fit drawing.
    FitDrawing,
    /// View › 100 %.
    Zoom100,
    /// View › Zoom to selection. The original's key for it is Redo here
    /// (`tools.md`), so it takes `3`, which is where other editors keep
    /// it.
    ZoomSelection,
    /// Edit › Undo.
    Undo,
    /// Edit › Redo.
    Redo,
    /// Edit › Delete: delete the selected objects.
    Delete,
    /// Edit › Select all.
    SelectAll,
    /// `Esc`: cancel the gesture in flight, or select nothing.
    Cancel,
    /// Choose a tool from the palette or by its key.
    Tool(ToolId),
    /// A command for the tool in force: the shape editor's path
    /// operations (`research/04 §4.11`), Enter.
    Action(ToolAction),
    /// Convert to editable shapes (`Ctrl+Shift+C`): rectangles, ellipses
    /// and quick shapes become paths.
    ConvertToShapes,
    /// Edit › Cut.
    Cut,
    /// Edit › Copy.
    Copy,
    /// Edit › Paste: into the middle of the view.
    Paste,
    /// Edit › Paste in place: at the copied coordinates.
    PasteInPlace,
    /// Edit › Duplicate.
    Duplicate,
    /// Arrange › Group.
    Group,
    /// Arrange › Ungroup.
    Ungroup,
    /// Arrange › a z-order operation.
    Arrange(ZOrder),
    /// Arrange › Alignment…: open the align panel.
    AlignDialog,
    /// View › Snap to grid (works mid-drag).
    SnapToGrid,
    /// View › Snap to guides (works mid-drag).
    SnapToGuides,
    /// View › Snap to objects (works mid-drag).
    SnapToObjects,
    /// View › Show grid.
    ShowGrid,
    /// View › Show guides.
    ShowGuides,
    /// Window › a pane: show it, re-docking it when the layout lacks it,
    /// and bring it to the front (`F9` colours, `F10` layers, `F11`
    /// bitmaps, as in the original's galleries, `research/04 §4.7`).
    ShowPane(DockPane),
}

/// How much one zoom-in or zoom-out step multiplies the zoom.
pub const ZOOM_STEP: f64 = std::f64::consts::SQRT_2;

impl AppCommand {
    /// Every command, in menu order, then the tools in palette order.
    /// Tools reserved for later phases are not here: they have no key yet.
    pub const ALL: [AppCommand; 65] = [
        AppCommand::Open,
        AppCommand::Import,
        AppCommand::Save,
        AppCommand::SaveAs,
        AppCommand::Close,
        AppCommand::Quit,
        AppCommand::Undo,
        AppCommand::Redo,
        AppCommand::Cut,
        AppCommand::Copy,
        AppCommand::Paste,
        AppCommand::PasteInPlace,
        AppCommand::Duplicate,
        AppCommand::Delete,
        AppCommand::SelectAll,
        AppCommand::Group,
        AppCommand::Ungroup,
        AppCommand::Arrange(ZOrder::BringToFront),
        AppCommand::Arrange(ZOrder::BringForward),
        AppCommand::Arrange(ZOrder::SendBackward),
        AppCommand::Arrange(ZOrder::SendToBack),
        AppCommand::Arrange(ZOrder::LayerUp),
        AppCommand::Arrange(ZOrder::LayerDown),
        AppCommand::AlignDialog,
        AppCommand::SnapToGrid,
        AppCommand::SnapToGuides,
        AppCommand::SnapToObjects,
        AppCommand::ShowGrid,
        AppCommand::ShowGuides,
        AppCommand::Cancel,
        AppCommand::ZoomIn,
        AppCommand::ZoomOut,
        AppCommand::FitPage,
        AppCommand::FitDrawing,
        AppCommand::Zoom100,
        AppCommand::ZoomSelection,
        AppCommand::Tool(ToolId::Selector),
        AppCommand::Tool(ToolId::ShapeEditor),
        AppCommand::Tool(ToolId::Rectangle),
        AppCommand::Tool(ToolId::Ellipse),
        AppCommand::Tool(ToolId::Pen),
        AppCommand::Tool(ToolId::Freehand),
        AppCommand::Tool(ToolId::Zoom),
        AppCommand::Tool(ToolId::Pan),
        AppCommand::Tool(ToolId::Fill),
        AppCommand::Tool(ToolId::Transparency),
        AppCommand::Tool(ToolId::Text),
        AppCommand::Tool(ToolId::Blend),
        AppCommand::Tool(ToolId::Contour),
        AppCommand::Tool(ToolId::Shadow),
        AppCommand::Tool(ToolId::Bevel),
        AppCommand::Tool(ToolId::Mould),
        AppCommand::Action(ToolAction::Finish),
        AppCommand::Action(ToolAction::MakeLine),
        AppCommand::Action(ToolAction::MakeCurve),
        AppCommand::Action(ToolAction::Smooth),
        AppCommand::Action(ToolAction::Cusp),
        AppCommand::Action(ToolAction::Break),
        AppCommand::Action(ToolAction::Join),
        AppCommand::ConvertToShapes,
        AppCommand::ShowPane(DockPane::Layers),
        AppCommand::ShowPane(DockPane::ColourEditor),
        AppCommand::ShowPane(DockPane::ColourGallery),
        AppCommand::ShowPane(DockPane::BitmapGallery),
        AppCommand::ShowPane(DockPane::Photo),
    ];

    /// The menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            AppCommand::Open => "Open…",
            AppCommand::Import => "Import…",
            AppCommand::Save => "Save",
            AppCommand::SaveAs => "Save As…",
            AppCommand::Close => "Close",
            AppCommand::Quit => "Quit",
            AppCommand::ZoomIn => "Zoom in",
            AppCommand::ZoomOut => "Zoom out",
            AppCommand::FitPage => "Fit page",
            AppCommand::FitDrawing => "Fit drawing",
            AppCommand::Zoom100 => "100 %",
            AppCommand::ZoomSelection => "Zoom to selection",
            AppCommand::Undo => "Undo",
            AppCommand::Redo => "Redo",
            AppCommand::Delete => "Delete",
            AppCommand::SelectAll => "Select all",
            AppCommand::Cancel => "Select none",
            AppCommand::Tool(t) => t.label(),
            AppCommand::Action(a) => a.label(),
            AppCommand::ConvertToShapes => "Convert to editable shapes",
            AppCommand::Cut => "Cut",
            AppCommand::Copy => "Copy",
            AppCommand::Paste => "Paste",
            AppCommand::PasteInPlace => "Paste in place",
            AppCommand::Duplicate => "Duplicate",
            AppCommand::Group => "Group",
            AppCommand::Ungroup => "Ungroup",
            AppCommand::Arrange(z) => z.label(),
            AppCommand::AlignDialog => "Alignment…",
            AppCommand::SnapToGrid => "Snap to grid",
            AppCommand::SnapToGuides => "Snap to guides",
            AppCommand::SnapToObjects => "Snap to objects",
            AppCommand::ShowGrid => "Show grid",
            AppCommand::ShowGuides => "Show guides",
            AppCommand::ShowPane(p) => p.label(),
        }
    }

    /// Whether the key works in the middle of a drag (`WorksInDrag` in
    /// the original): `Esc`, which cancels it, and the snapping toggles
    /// (`research/04 §4.4`).
    #[must_use]
    pub const fn works_in_drag(self) -> bool {
        matches!(
            self,
            AppCommand::Cancel
                | AppCommand::SnapToGrid
                | AppCommand::SnapToGuides
                | AppCommand::SnapToObjects
        )
    }

    /// Every key that runs the command; the first is the one a menu shows.
    #[must_use]
    pub const fn shortcuts(self) -> &'static [KeyChord] {
        const OPEN: &[KeyChord] = &[KeyChord::ctrl('o')];
        // The original's Ctrl+I is its image slicer (`research/04 §4.8`),
        // which may come later; Ctrl+Shift+I leaves it free.
        const IMPORT: &[KeyChord] = &[KeyChord::ctrl_shift('i')];
        const SAVE: &[KeyChord] = &[KeyChord::ctrl('s')];
        const SAVE_AS: &[KeyChord] = &[KeyChord::ctrl_shift('s')];
        const CLOSE: &[KeyChord] = &[KeyChord::ctrl('w')];
        const QUIT: &[KeyChord] = &[KeyChord::ctrl('q')];
        const ZOOM_IN: &[KeyChord] = &[
            KeyChord::char('+'),
            KeyChord::char('='),
            KeyChord::ctrl('+'),
            KeyChord::ctrl('='),
        ];
        const ZOOM_OUT: &[KeyChord] = &[KeyChord::char('-'), KeyChord::ctrl('-')];
        const FIT_PAGE: &[KeyChord] = &[KeyChord::char('0'), KeyChord::plain(ChordKey::Home)];
        const FIT_DRAWING: &[KeyChord] = &[KeyChord::char('d')];
        const ZOOM_100: &[KeyChord] = &[KeyChord::char('1')];
        const ZOOM_SELECTION: &[KeyChord] = &[KeyChord::char('3')];
        const UNDO: &[KeyChord] = &[KeyChord::ctrl('z')];
        // Ctrl+Shift+Z first because every other Linux program shows it;
        // Ctrl+Y is the original's (`research/04 §4.1`).
        const REDO: &[KeyChord] = &[KeyChord::ctrl_shift('z'), KeyChord::ctrl('y')];
        const DELETE: &[KeyChord] = &[
            KeyChord::plain(ChordKey::Delete),
            KeyChord::plain(ChordKey::Backspace),
        ];
        const SELECT_ALL: &[KeyChord] = &[KeyChord::ctrl('a')];
        const CANCEL: &[KeyChord] = &[KeyChord::plain(ChordKey::Escape)];
        // The tool keys of `research/04 §4.6`.
        const SELECTOR: &[KeyChord] = &[KeyChord::f(2)];
        const FREEHAND: &[KeyChord] = &[KeyChord::f(3)];
        const SHAPE: &[KeyChord] = &[KeyChord::f(4)];
        const RECTANGLE: &[KeyChord] = &[KeyChord::shift_f(3)];
        const ELLIPSE: &[KeyChord] = &[KeyChord::shift_f(4)];
        const PEN: &[KeyChord] = &[KeyChord::shift_f(5)];
        const ZOOM_TOOL: &[KeyChord] = &[KeyChord::shift_f(7)];
        const PUSH: &[KeyChord] = &[KeyChord::shift_f(8)];
        // `research/04 §4.4`: F5 graduated fill, F6 transparency.
        const FILL_TOOL: &[KeyChord] = &[KeyChord::f(5)];
        const TRANSP_TOOL: &[KeyChord] = &[KeyChord::f(6)];
        const TEXT: &[KeyChord] = &[KeyChord::f(8)];
        // `research/04 §4.6`: Ctrl+F2 shadow (`TOOL22`), Ctrl+F3 bevel
        // (`TOOL23`).
        const SHADOW_TOOL: &[KeyChord] = &[KeyChord::ctrl_f(2)];
        // F7 blend (`TOOL16`), Ctrl+F7 contour (`TOOL24`).
        const BLEND_TOOL: &[KeyChord] = &[KeyChord::f(7)];
        const CONTOUR_TOOL: &[KeyChord] = &[KeyChord::ctrl_f(7)];
        const BEVEL_TOOL: &[KeyChord] = &[KeyChord::ctrl_f(3)];
        const MOULD_TOOL: &[KeyChord] = &[KeyChord::shift_f(6)];
        // The shape editor's keys (`research/04 §4.11`).
        const FINISH: &[KeyChord] = &[KeyChord::plain(ChordKey::Enter)];
        const MAKE_LINE: &[KeyChord] = &[KeyChord::char('l')];
        const MAKE_CURVE: &[KeyChord] = &[KeyChord::char('c')];
        const SMOOTH: &[KeyChord] = &[KeyChord::char('s')];
        const CUSP: &[KeyChord] = &[KeyChord::char('z')];
        const BREAK: &[KeyChord] = &[KeyChord::char('b')];
        const JOIN: &[KeyChord] = &[KeyChord::char('j')];
        // The original's Ctrl+Shift+S is Save As here, as on every other
        // desktop program; Ctrl+Shift+C is Inkscape's "Object to Path" (tools.md).
        const CONVERT: &[KeyChord] = &[KeyChord::ctrl_shift('c')];
        const NONE: &[KeyChord] = &[];
        // Edit and Arrange, `research/04 §4.2`, §4.3.
        const CUT: &[KeyChord] = &[KeyChord::ctrl('x')];
        const COPY: &[KeyChord] = &[KeyChord::ctrl('c')];
        const PASTE: &[KeyChord] = &[KeyChord::ctrl('v')];
        const PASTE_IN_PLACE: &[KeyChord] = &[KeyChord::ctrl_shift('v')];
        const DUPLICATE: &[KeyChord] = &[KeyChord::ctrl('d')];
        const GROUP: &[KeyChord] = &[KeyChord::ctrl('g')];
        const UNGROUP: &[KeyChord] = &[KeyChord::ctrl('u')];
        const FRONT: &[KeyChord] = &[KeyChord::ctrl('f')];
        const FORWARD: &[KeyChord] = &[KeyChord::ctrl_shift('f')];
        const BACKWARD: &[KeyChord] = &[KeyChord::ctrl_shift('b')];
        const BACK: &[KeyChord] = &[KeyChord::ctrl('b')];
        const LAYER_UP: &[KeyChord] = &[KeyChord::ctrl_shift('u')];
        const LAYER_DOWN: &[KeyChord] = &[KeyChord::ctrl_shift('d')];
        const ALIGN: &[KeyChord] = &[KeyChord::ctrl_shift('l')];
        // View, `research/04 §4.4`.
        const SNAP_GRID: &[KeyChord] = &[KeyChord::numpad('.')];
        const SNAP_GUIDES: &[KeyChord] = &[KeyChord::numpad('2')];
        const SNAP_OBJECTS: &[KeyChord] = &[KeyChord::numpad('*')];
        const SHOW_GRID: &[KeyChord] = &[KeyChord::char('#')];
        const SHOW_GUIDES: &[KeyChord] = &[KeyChord::numpad('1')];
        // The galleries, `research/04 §4.7`.
        const COLOURS: &[KeyChord] = &[KeyChord::f(9)];
        const LAYERS: &[KeyChord] = &[KeyChord::f(10)];
        const BITMAPS: &[KeyChord] = &[KeyChord::f(11)];
        match self {
            AppCommand::Open => OPEN,
            AppCommand::Import => IMPORT,
            AppCommand::Save => SAVE,
            AppCommand::SaveAs => SAVE_AS,
            AppCommand::Close => CLOSE,
            AppCommand::Quit => QUIT,
            AppCommand::ZoomIn => ZOOM_IN,
            AppCommand::ZoomOut => ZOOM_OUT,
            AppCommand::FitPage => FIT_PAGE,
            AppCommand::FitDrawing => FIT_DRAWING,
            AppCommand::Zoom100 => ZOOM_100,
            AppCommand::ZoomSelection => ZOOM_SELECTION,
            AppCommand::Undo => UNDO,
            AppCommand::Redo => REDO,
            AppCommand::Delete => DELETE,
            AppCommand::SelectAll => SELECT_ALL,
            AppCommand::Cancel => CANCEL,
            AppCommand::Tool(t) => match t {
                ToolId::Selector => SELECTOR,
                ToolId::Freehand => FREEHAND,
                ToolId::ShapeEditor => SHAPE,
                ToolId::Rectangle => RECTANGLE,
                ToolId::Ellipse => ELLIPSE,
                ToolId::Pen => PEN,
                ToolId::Zoom => ZOOM_TOOL,
                ToolId::Pan => PUSH,
                ToolId::Fill => FILL_TOOL,
                ToolId::Transparency => TRANSP_TOOL,
                ToolId::Text => TEXT,
                ToolId::Blend => BLEND_TOOL,
                ToolId::Contour => CONTOUR_TOOL,
                ToolId::Shadow => SHADOW_TOOL,
                ToolId::Bevel => BEVEL_TOOL,
                ToolId::Mould => MOULD_TOOL,
            },
            AppCommand::Action(a) => match a {
                ToolAction::Finish => FINISH,
                ToolAction::MakeLine => MAKE_LINE,
                ToolAction::MakeCurve => MAKE_CURVE,
                ToolAction::Smooth => SMOOTH,
                ToolAction::Cusp => CUSP,
                ToolAction::Break => BREAK,
                ToolAction::Join => JOIN,
                ToolAction::Delete
                | ToolAction::Cancel
                | ToolAction::SelectAll
                | ToolAction::ClosePath
                | ToolAction::NaturalSize
                | ToolAction::RemoveEffect
                | ToolAction::CopyMould
                | ToolAction::PasteEnvelope
                | ToolAction::PastePerspective
                | ToolAction::RotateMould => NONE,
            },
            AppCommand::ConvertToShapes => CONVERT,
            AppCommand::Cut => CUT,
            AppCommand::Copy => COPY,
            AppCommand::Paste => PASTE,
            AppCommand::PasteInPlace => PASTE_IN_PLACE,
            AppCommand::Duplicate => DUPLICATE,
            AppCommand::Group => GROUP,
            AppCommand::Ungroup => UNGROUP,
            AppCommand::Arrange(z) => match z {
                ZOrder::BringToFront => FRONT,
                ZOrder::BringForward => FORWARD,
                ZOrder::SendBackward => BACKWARD,
                ZOrder::SendToBack => BACK,
                ZOrder::LayerUp => LAYER_UP,
                ZOrder::LayerDown => LAYER_DOWN,
            },
            AppCommand::AlignDialog => ALIGN,
            AppCommand::SnapToGrid => SNAP_GRID,
            AppCommand::SnapToGuides => SNAP_GUIDES,
            AppCommand::SnapToObjects => SNAP_OBJECTS,
            AppCommand::ShowGrid => SHOW_GRID,
            AppCommand::ShowGuides => SHOW_GUIDES,
            AppCommand::ShowPane(p) => match p {
                DockPane::ColourGallery => COLOURS,
                DockPane::Layers => LAYERS,
                DockPane::BitmapGallery => BITMAPS,
                DockPane::ColourEditor | DockPane::Photo => NONE,
            },
        }
    }

    /// The shortcut a menu shows next to the label.
    #[must_use]
    pub fn primary_shortcut(self) -> Option<KeyChord> {
        self.shortcuts().first().copied()
    }

    /// Whether the command needs an open document to mean anything; a
    /// menu greys such an item out when nothing is open.
    #[must_use]
    pub const fn needs_document(self) -> bool {
        !matches!(
            self,
            AppCommand::Open | AppCommand::Quit | AppCommand::ShowPane(_)
        )
    }

    /// The intent the command raises. A keyboard or menu zoom has no
    /// pointer to zoom about, so it keeps `centre` (the canvas centre, in
    /// device pixels) fixed.
    #[must_use]
    pub fn intent(self, centre: DevicePoint) -> Intent {
        match self {
            AppCommand::Open => Intent::ShowOpenDialog,
            AppCommand::Import => Intent::ShowImportDialog,
            AppCommand::Save => Intent::Save,
            AppCommand::SaveAs => Intent::SaveAs,
            AppCommand::Close => Intent::CloseDocument,
            AppCommand::Quit => Intent::Quit,
            AppCommand::ZoomIn => Intent::Zoom {
                factor: ZOOM_STEP,
                anchor: centre,
            },
            AppCommand::ZoomOut => Intent::Zoom {
                factor: 1.0 / ZOOM_STEP,
                anchor: centre,
            },
            AppCommand::FitPage => Intent::ZoomTo(ZoomTarget::Page),
            AppCommand::FitDrawing => Intent::ZoomTo(ZoomTarget::Drawing),
            AppCommand::Zoom100 => Intent::ZoomTo(ZoomTarget::Percent100),
            AppCommand::ZoomSelection => Intent::ZoomTo(ZoomTarget::Selection),
            AppCommand::Undo => Intent::Undo,
            AppCommand::Redo => Intent::Redo,
            AppCommand::Delete => Intent::DeleteSelection,
            AppCommand::SelectAll => Intent::SelectAll,
            AppCommand::Cancel => Intent::Cancel,
            AppCommand::Tool(t) => Intent::ChooseTool(t),
            AppCommand::Action(ToolAction::Delete) => Intent::DeleteSelection,
            AppCommand::Action(ToolAction::Cancel) => Intent::Cancel,
            AppCommand::Action(ToolAction::SelectAll) => Intent::SelectAll,
            AppCommand::Action(a) => Intent::ToolAction(a),
            AppCommand::ConvertToShapes => Intent::ConvertToShapes,
            AppCommand::Cut => Intent::Cut,
            AppCommand::Copy => Intent::Copy,
            AppCommand::Paste => Intent::Paste { in_place: false },
            AppCommand::PasteInPlace => Intent::Paste { in_place: true },
            AppCommand::Duplicate => Intent::Duplicate,
            AppCommand::Group => Intent::Group,
            AppCommand::Ungroup => Intent::Ungroup,
            AppCommand::Arrange(z) => Intent::Arrange(z),
            AppCommand::AlignDialog => Intent::ShowDialog(Dialog::Align),
            AppCommand::SnapToGrid => Intent::ToggleSnap(SnapKind::Grid),
            AppCommand::SnapToGuides => Intent::ToggleSnap(SnapKind::Guide),
            AppCommand::SnapToObjects => Intent::ToggleSnap(SnapKind::Object),
            AppCommand::ShowGrid => Intent::ToggleGrid,
            AppCommand::ShowGuides => Intent::ToggleGuides,
            AppCommand::ShowPane(p) => Intent::ShowDialog(Dialog::Pane(p)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_two_commands_share_a_key() {
        let mut seen = std::collections::HashMap::new();
        for c in AppCommand::ALL {
            for k in c.shortcuts() {
                if let Some(other) = seen.insert(*k, c) {
                    panic!("{k} is bound to both {other:?} and {c:?}");
                }
            }
        }
    }

    #[test]
    fn the_edit_and_tool_keys_are_the_documented_ones() {
        let key = |c: AppCommand| c.primary_shortcut().unwrap().to_string();
        assert_eq!(key(AppCommand::Undo), "Ctrl+Z");
        assert_eq!(key(AppCommand::Save), "Ctrl+S");
        assert_eq!(key(AppCommand::SaveAs), "Ctrl+Shift+S");
        assert_eq!(key(AppCommand::Import), "Ctrl+Shift+I");
        assert!(AppCommand::Import.needs_document());
        assert_eq!(
            AppCommand::Import.intent(DevicePoint::new(0.0, 0.0)),
            Intent::ShowImportDialog
        );
        assert_eq!(key(AppCommand::ConvertToShapes), "Ctrl+Shift+C");
        assert_eq!(
            AppCommand::Save.intent(DevicePoint::new(0.0, 0.0)),
            Intent::Save
        );
        assert!(AppCommand::SaveAs.needs_document());
        assert_eq!(key(AppCommand::Redo), "Ctrl+Shift+Z");
        assert_eq!(AppCommand::Redo.shortcuts()[1].to_string(), "Ctrl+Y");
        assert_eq!(key(AppCommand::Delete), "Del");
        assert_eq!(key(AppCommand::Cancel), "Esc");
        assert!(AppCommand::Cancel.works_in_drag());
        assert!(!AppCommand::Undo.works_in_drag());
        for (tool, k) in [
            (ToolId::Selector, "F2"),
            (ToolId::Freehand, "F3"),
            (ToolId::ShapeEditor, "F4"),
            (ToolId::Rectangle, "Shift+F3"),
            (ToolId::Ellipse, "Shift+F4"),
            (ToolId::Pen, "Shift+F5"),
            (ToolId::Zoom, "Shift+F7"),
            (ToolId::Pan, "Shift+F8"),
            (ToolId::Fill, "F5"),
            (ToolId::Transparency, "F6"),
            (ToolId::Text, "F8"),
            (ToolId::Blend, "F7"),
            (ToolId::Contour, "Ctrl+F7"),
            (ToolId::Shadow, "Ctrl+F2"),
            (ToolId::Bevel, "Ctrl+F3"),
            (ToolId::Mould, "Shift+F6"),
        ] {
            assert_eq!(key(AppCommand::Tool(tool)), k, "{tool:?}");
            assert_eq!(
                AppCommand::Tool(tool).intent(DevicePoint::new(0.0, 0.0)),
                Intent::ChooseTool(tool)
            );
        }
    }

    #[test]
    fn every_tool_with_a_key_is_in_the_command_table() {
        for tool in ToolId::ALL {
            let command = AppCommand::Tool(tool);
            if !command.shortcuts().is_empty() {
                assert!(AppCommand::ALL.contains(&command), "{tool:?}");
            }
        }
    }

    #[test]
    fn every_command_has_a_label_and_a_menu_shortcut() {
        for c in AppCommand::ALL {
            assert!(!c.label().is_empty());
            // The panes the original has no gallery key for are reached
            // from the Window menu alone.
            let keyless = matches!(
                c,
                AppCommand::ShowPane(DockPane::ColourEditor | DockPane::Photo)
            );
            assert_eq!(c.primary_shortcut().is_none(), keyless, "{c:?}");
        }
        assert_eq!(
            AppCommand::Open.primary_shortcut().unwrap().to_string(),
            "Ctrl+O"
        );
        assert_eq!(
            AppCommand::FitDrawing
                .primary_shortcut()
                .unwrap()
                .to_string(),
            "D"
        );
        assert_eq!(KeyChord::plain(ChordKey::Home).to_string(), "Home");
    }

    #[test]
    fn the_gallery_keys_show_their_panes_with_or_without_a_document() {
        let key = |c: AppCommand| c.primary_shortcut().unwrap().to_string();
        let c = DevicePoint::new(0.0, 0.0);
        for (pane, k) in [
            (DockPane::ColourGallery, "F9"),
            (DockPane::Layers, "F10"),
            (DockPane::BitmapGallery, "F11"),
        ] {
            assert_eq!(key(AppCommand::ShowPane(pane)), k);
        }
        for pane in DockPane::ALL {
            let cmd = AppCommand::ShowPane(pane);
            assert!(AppCommand::ALL.contains(&cmd));
            assert!(!cmd.needs_document());
            assert!(!cmd.works_in_drag());
            assert_eq!(cmd.intent(c), Intent::ShowDialog(Dialog::Pane(pane)));
            assert_eq!(cmd.label(), pane.label());
        }
    }

    #[test]
    fn view_commands_raise_view_intents_about_the_centre() {
        let c = DevicePoint::new(400.0, 300.0);
        assert_eq!(
            AppCommand::ZoomIn.intent(c),
            Intent::Zoom {
                factor: ZOOM_STEP,
                anchor: c
            }
        );
        assert_eq!(AppCommand::Open.intent(c), Intent::ShowOpenDialog);
        assert_eq!(
            AppCommand::FitPage.intent(c),
            Intent::ZoomTo(ZoomTarget::Page)
        );
        assert!(!AppCommand::Open.needs_document());
        assert!(AppCommand::ZoomIn.needs_document());
    }

    #[test]
    fn only_punctuation_is_shift_tolerant() {
        assert!(KeyChord::char('+').shift_is_layout_dependent());
        assert!(!KeyChord::char('d').shift_is_layout_dependent());
        assert!(!KeyChord::char('1').shift_is_layout_dependent());
        assert!(!KeyChord::plain(ChordKey::Home).shift_is_layout_dependent());
    }
}
