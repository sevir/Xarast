//! The application core: selection, viewport, the scene walker, the
//! session and the command bridge.
//!
//! UI-toolkit agnostic on purpose. This crate has no windowing
//! dependency and no widget dependency, it builds and tests with no GPU
//! and no compositor present, and it is the layer both consumers sit on:
//! `xarast-ui` draws it and `xarast-shell` feeds it, but neither depends
//! on the other (`10-architecture.md` §2).
//!
//! # The four things this crate is
//!
//! | Module | What it owns |
//! |---|---|
//! | [`edit`] | [`EditState`]: selection, control points, active layer, tool state — session state, never serialised, never undone (architecture §3.5b) |
//! | [`viewport`] | [`Viewport`]: the document↔screen transform, pan, zoom, fits, scroll bounds |
//! | [`walker`] | [`SceneWalker`]: the arena→[`Scene`](xarast_render::Scene) walk, which is the seam the architecture puts here so that the renderer never sees a node |
//! | [`session`] | [`Session`]: a document, its edit state, its view, its history and its dirty tracking |
//! | [`command`] | [`AppCommand`]: the command table the menus draw and the shell binds keys from |
//! | [`recent`] | [`RecentFiles`]: the recently opened files and their store |
//! | [`render_thread`] | [`RenderThread`]: the one channel to the render thread — [`RenderRequest`], generations, supersession and cancellation |
//!
//! # The contract the shell and the UI build against
//!
//! ```no_run
//! use xarast_app::{AppState, Intent, Changed, ZoomTarget};
//!
//! let mut app = AppState::new();
//! app.new_document();
//!
//! // The shell turns a scroll wheel into this; the UI turns a menu
//! // item into the same thing.
//! let changed: Changed = app.apply(Intent::ZoomTo(ZoomTarget::Page))?;
//! if changed.needs_scene() {
//!     if let Some(s) = app.active_mut() {
//!         s.rebuild_scene(None)?;
//!     }
//! }
//! # Ok::<(), xarast_app::SessionError>(())
//! ```
//!
//! Three rules hold that contract together.
//!
//! 1. **[`Intent`] is the only input vocabulary.** The shell raises them
//!    from platform events, the UI raises the same ones from menus and
//!    panels, and neither has to know the other exists.
//! 2. **[`Changed`] is the only answer.** It says whether the caller
//!    owes a present, a scene rebuild or a panel rebuild, which is what
//!    keeps an idle frame idle.
//! 3. **A [`Scene`](xarast_render::Scene) is not self-contained.** Its
//!    paints hold ids into a [`Resolver`](xarast_render::Resolver);
//!    [`Session::resolver`] is where that lives. Hand the two to the
//!    backend together or every gradient renders transparent.
//!
//! # Units
//!
//! Millipoints in, device pixels out. The document model is integer
//! millipoints with `y` pointing **up**; a surface is `f64` pixels with
//! `y` pointing **down**. The flip belongs to [`Viewport`] and to
//! nothing else — see [`geometry`].

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![doc(html_no_source)]

pub mod app;
pub mod autosave;
pub mod bevel_tool;
pub mod bitmap_gallery;
pub mod blend_tool;
pub mod colour_bar;
pub mod colour_editor;
pub mod command;
pub mod commands;
pub mod contour_tool;
pub mod convert;
pub mod crash;
pub mod decoded;
pub mod edit;
pub mod export;
pub mod export_dialog;
pub mod fill_handles;
pub mod fill_tool;
pub mod fonts;
pub mod freehand;
pub mod geometry;
pub mod headless;
pub mod import;
pub mod intent;
pub mod live_edit;
pub mod locks;
pub mod mould_tool;
pub mod node_edit;
pub mod ops;
mod paint;
pub mod pen;
pub mod photo_panel;
pub mod picking;
pub mod place;
pub mod prefs;
mod procedural;
pub mod prompt;
pub mod recent;
pub mod render_thread;
mod reuse;
pub mod save;
pub mod schedule;
pub mod selector;
pub mod session;
pub mod shadow_tool;
pub mod shapes;
pub mod snap;
pub mod structure;
pub mod svg_text;
mod text;
pub mod text_clip;
pub mod text_edit;
pub mod text_infobar;
#[cfg(test)]
mod text_path_fidelity;
pub mod text_tool;
pub mod thumbnail;
pub mod tool;
pub mod tools;
pub mod viewport;
pub mod walker;

pub use app::{
    AppState, DiagnosticEntry, DiagnosticLog, DocumentSessions, InternalClipboard, PendingAction,
    Severity, TextClipboard, xarast_path,
};
pub use command::{AppCommand, ChordKey, KeyChord};
pub use decoded::{DecodedImages, DecodedImagesStats};
pub use edit::{
    ControlPoints, CurrentAttributes, EditState, Modifiers, SelectMode, ToolId, ToolState,
};
pub use geometry::{DevicePoint, DeviceSize, DocPoint, DocPointF, DocRect};
pub use headless::{HeadlessError, HeadlessFrame, HeadlessOptions, HeadlessResult, render_to_png};
pub use intent::{
    Changed, Dialog, DockPane, Intent, PlatformRequest, PointerButton, PointerSample,
};
pub use ops::{CommandSink, EditCommand};
pub use prefs::{Preferences, RendererPref, ThemePref, Unit};
pub use prompt::{ChoiceRole, Prompt, PromptAnswer, PromptChoice};
pub use recent::RecentFiles;
pub use render_thread::{
    FINAL_COLUMNS, FrameJob, FrameRenderer, FrameReuse, MIN_COLUMN_WIDTH, RenderRequest,
    RenderStats, RenderThread, RenderedFrame,
};
pub use session::{BuiltScene, Dirty, DocumentId, FileKind, Session, SessionError, build_scene};
pub use tool::{
    Anchor, CursorKind, FeatureOption, GestureEvent, HandleShape, HitResult, Infobar, InfobarDrag,
    InfobarField, InfobarItem, InfobarValue, InteractionState, Nudge, NudgeDir, NudgeStep,
    OverlayShape, Preview, TextInput, TextInputKind, TextKey, TextNav, TextRuler, Tool, ToolAction,
    ToolCtx, ToolMachine, ToolView,
};
pub use viewport::{MAX_ZOOM, MIN_ZOOM, Viewport, ZoomTarget};
pub use walker::{SceneWalker, WalkStats};

/// The three public types the other Phase 5 crates hold across threads
/// must stay `Send`. A change that breaks this fails to compile here
/// rather than in the shell.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<EditState>();
    assert_send::<Viewport>();
    assert_send::<Session>();
    assert_send::<AppState>();
    assert_send::<Intent>();
};
