//! The composition root: shell, interface and application core as one
//! running program.
//!
//! Each of the three crates is correct on its own and none of them knows
//! the others' types beyond the contract below it. This module is the only
//! place all three meet, and it is deliberately small:
//!
//! ```text
//!  ShellEvent ──► intents::IntentAdapter ──► Intent ──► AppState::apply ──► Changed
//!                                              ▲                              │
//!  egui frame ──► Workspace::ui ──► UiCommand ─┘            needs_scene: rebuild_scene
//!        │                                                   needs_redraw: frame_job
//!        ▼                                                                    │
//!  UiFrame ──► ShellCtx::show_ui     Canvas::pump (Draft/Final) ◄─────────────┘
//!                                                  │ (render thread, CPU backend)
//!  CanvasFrame ──► ShellCtx::show_canvas ◄─ take_latest ◄─ waker ◄─┘
//! ```
//!
//! # Input ownership
//!
//! Every [`ShellEvent`] goes to `egui` through [`crate::egui_input`], so
//! the panels respond to the pointer, the keyboard, the wheel and the input
//! method. Canvas **navigation** has exactly one owner, the
//! [`IntentAdapter`]: the canvas widget runs with
//! [`xarast_ui::CanvasNavigation::External`] and emits no pan or zoom of its
//! own, so one wheel notch is one zoom step. Two gates keep the adapter out
//! of `egui`'s way:
//!
//! * a press or a wheel at a point where `egui` shows something above the
//!   canvas (a popup, a menu, a floating window) is not the canvas's;
//! * shortcuts do nothing while a text field has the keyboard.
//!
//! # Commands and the platform
//!
//! Menu items and shortcuts are the same [`AppCommand`]s from
//! `xarast-app`'s command table: the menu raises them as
//! [`UiCommand::App`], the keyboard through a [`ShortcutMap`] built from the
//! same table. Either way they become [`Intent`]s applied to [`AppState`].
//! What only the platform can do — show a file chooser, quit — comes back
//! from the core as a [`PlatformRequest`], and this module carries it out
//! with the shell: the dialog is a [`PortalHandle`](crate::portal::PortalHandle)
//! request whose answer arrives later as a [`ShellEvent::Portal`]. The
//! interface never calls the portal itself.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use xarast_app::schedule::{Backdrop, Canvas};
use xarast_app::snap::GuideOp;
use xarast_app::{AppCommand, AppState, Changed, ChordKey, Intent, PlatformRequest, Session};
use xarast_ui::model::{
    DocumentView, EditingView, LayerInfo, LayerKey, StatusInfo, UiCommand, UiModel, ViewTransform,
};
use xarast_ui::{Scale, Workspace};

use crate::egui_input::{EguiInput, cursor_shape};
use crate::input::event::{ColorScheme, DragEvent, PointerPhase, ShellEvent};
use crate::input::keyboard::{Key, KeyEvent, KeyState, Modifiers, NamedKey, Shortcut, ShortcutMap};
use crate::intents::{CanvasRegion, IntentAdapter};
use crate::paint::UiFrame;
use crate::portal::{FileFilter, OpenFileRequest, PortalEvent, PortalRequestId, SaveFileRequest};
use crate::probe::Probe;
use crate::scale::{PhysicalPos, PhysicalSize, ScaleFactor};
use crate::tiles::{CanvasView, TiledFrame};
use crate::{CursorShape, FrameRequest, ShellApp, ShellCtx};

/// The pasteboard, premultiplied sRGB: a neutral mid grey that reads as
/// "not the page" under both themes.
pub const PASTEBOARD: [u8; 4] = [0x80, 0x80, 0x84, 0xff];

/// Frames to draw before capturing a window with no document in it
/// (`--screenshot` with no file); see `on_frame`.
const EMPTY_CAPTURE_FRAMES: u32 = 6;

/// The page underneath the drawing.
pub const PAGE: [u8; 4] = [0xff, 0xff, 0xff, 0xff];

/// The running application.
pub struct Viewer {
    app: AppState,
    adapter: IntentAdapter,
    egui: egui::Context,
    workspace: Workspace,
    /// The render thread behind the Draft → Final scheduler.
    render: Option<Canvas>,
    /// When the owed Final frame is due, if one is.
    final_due: Option<Instant>,
    /// Files to open at the next frame, from the command line or a drop.
    to_open: Vec<PathBuf>,
    /// The canvas has not been primed with a size and scale yet.
    primed: bool,
    /// A document was just opened and should be framed once the canvas
    /// has its real size.
    fit_pending: bool,
    scene_stale: bool,
    render_stale: bool,
    /// The generation of the frame on screen, so that a stale frame from
    /// a closed document is never shown.
    shown: u64,
    /// Row index → layer node, rebuilt with the model every frame.
    layer_keys: Vec<xarast_doc::NodeId>,
    /// The guideline behind each index of `DocumentView::guides`.
    guide_keys: Vec<xarast_doc::NodeId>,
    scheme: ColorScheme,
    message: Option<String>,
    renderer_label: String,
    /// Write the first settled frame here and quit.
    screenshot: Option<PathBuf>,
    /// `egui`'s input for the next interface frame.
    input: EguiInput,
    /// The scale of the last event or frame, in pixels per point.
    ppp: f32,
    /// A text field had the keyboard in the last interface frame.
    text_input: bool,
    /// The canvas had keyboard focus in the last interface frame.
    canvas_focused: bool,
    /// The pointer was over the canvas in the last interface frame, so the
    /// tool's cursor applies.
    canvas_hovered: bool,
    ime_allowed: bool,
    ime_area: Option<[i32; 4]>,
    /// The input method's composition for the canvas's text caret (egui
    /// keeps its own for its text fields).
    ime: crate::ime::ImeState,
    cursor: CursorShape,
    /// The window title, which also names the accessibility tree's root.
    title: String,
    /// A scripted pan or zoom measuring input-to-present latency.
    probe: Option<Probe>,
    /// An exact frame of the document has been shown: the probe may start.
    settled_once: bool,
    /// A canvas view has been handed to the shell and not cleared.
    view_shown: bool,
    /// Frames drawn with nothing open while a screenshot waits.
    empty_frames: u32,
    /// The keys of the command table.
    shortcuts: ShortcutMap<AppCommand>,
    /// The momentary tool switch held down, if any (Space, Alt+S/Z/X).
    momentary: crate::input::momentary::MomentarySwitch,
    /// What the core asked the platform to do, not yet done.
    requests: Vec<PlatformRequest>,
    /// The file chooser on screen, if one is.
    open_dialog: Option<PortalRequestId>,
    /// The File › Import… chooser on screen, if one is (T10.7.4).
    import_dialog: Option<PortalRequestId>,
    /// The bitmap gallery's Replace chooser on screen, if one is
    /// (XARA-T-0290).
    replace_dialog: Option<PortalRequestId>,
    /// The bitmap gallery's Save a copy chooser on screen, if one is.
    copy_dialog: Option<PortalRequestId>,
    /// The active document changed: the window title is owed.
    title_stale: bool,
    /// The save chooser on screen, if one is.
    save_dialog: Option<PortalRequestId>,
    /// The File › Export… destination chooser on screen, if one is.
    export_dialog: Option<PortalRequestId>,
    /// SIGINT/SIGTERM/SIGHUP, from the handler thread.
    signals: crate::signals::SignalWatch,
    /// A thread the process cannot run without (the render thread) died
    /// of a panic: shut down as for a signal (XARA-US-0064 F3).
    fatal: xarast_app::crash::FatalWatch,
    /// The save thread and the signal handler can wake the loop.
    wakers_set: bool,
    /// The recovery question has been asked (or is not to be).
    recovery_offered: bool,
    /// When the next autosave is due.
    autosave_due: Option<Instant>,
}

/// One interface frame, before it is handed to the shell.
struct UiStep {
    frame: UiFrame,
    region: Option<CanvasRegion>,
    repaint: Option<Duration>,
    output: egui::PlatformOutput,
}

impl std::fmt::Debug for Viewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewer")
            .field("documents", &self.app.docs.len())
            .field("to_open", &self.to_open)
            .field("render", &self.render)
            .finish_non_exhaustive()
    }
}

impl Viewer {
    /// A viewer that will open `files` once its window exists. The last
    /// one opened is the active document.
    #[must_use]
    pub fn new(files: Vec<PathBuf>) -> Viewer {
        Viewer {
            app: AppState::new(),
            adapter: IntentAdapter::new(),
            egui: egui::Context::default(),
            workspace: Workspace::new(),
            render: None,
            final_due: None,
            to_open: files,
            primed: false,
            fit_pending: false,
            scene_stale: false,
            render_stale: false,
            shown: 0,
            layer_keys: Vec::new(),
            guide_keys: Vec::new(),
            scheme: ColorScheme::NoPreference,
            message: None,
            renderer_label: "CPU".to_owned(),
            screenshot: None,
            input: EguiInput::new(),
            ppp: 1.0,
            text_input: false,
            canvas_focused: false,
            canvas_hovered: false,
            ime_allowed: false,
            ime_area: None,
            ime: crate::ime::ImeState::new(),
            cursor: CursorShape::Default,
            title: "Xarast".to_owned(),
            probe: None,
            settled_once: false,
            view_shown: false,
            empty_frames: 0,
            shortcuts: command_shortcuts(),
            momentary: crate::input::momentary::MomentarySwitch::new(),
            requests: Vec::new(),
            open_dialog: None,
            import_dialog: None,
            replace_dialog: None,
            copy_dialog: None,
            title_stale: false,
            save_dialog: None,
            export_dialog: None,
            signals: crate::signals::SignalWatch::inert(),
            fatal: xarast_app::crash::FatalWatch::new(),
            wakers_set: false,
            recovery_offered: false,
            autosave_due: None,
        }
        .with_external_navigation()
    }

    /// The adapter owns canvas navigation; see the module documentation.
    fn with_external_navigation(mut self) -> Viewer {
        self.workspace
            .set_canvas_navigation(xarast_ui::CanvasNavigation::External);
        self
    }

    /// Keeps the recent files in `file` between runs (the binary passes
    /// `$XDG_STATE_HOME/xarast/recent`). Without it the list lives only as
    /// long as the process, which is what the tests want.
    #[must_use]
    pub fn with_recent_store(mut self, file: PathBuf) -> Viewer {
        self.app = std::mem::take(&mut self.app).with_recent_store(file);
        self
    }

    /// Autosaves modified documents into `dir` and offers, once the window
    /// is up, to recover what an earlier session left there (the binary
    /// passes `$XDG_STATE_HOME/xarast/autosave`). Tests and probes leave it
    /// off.
    #[must_use]
    pub fn with_autosave(mut self, dir: PathBuf) -> Viewer {
        self.app = std::mem::take(&mut self.app)
            .with_autosave(dir, xarast_app::autosave::AutosavePolicy::default());
        self
    }

    /// Shuts down in order (autosave, release locks) when `signals` says a
    /// signal arrived.
    #[must_use]
    pub fn with_signals(mut self, signals: crate::signals::SignalWatch) -> Viewer {
        self.signals = signals;
        self
    }

    /// Autosaves and exits when `fatal` is raised: the binary passes
    /// `FatalWatch::global()`, which the render thread raises if a panic
    /// escapes it. Tests pass their own.
    #[must_use]
    pub fn with_fatal_watch(mut self, fatal: xarast_app::crash::FatalWatch) -> Viewer {
        self.fatal = fatal;
        self
    }

    /// What the start of the session found: a crash to report before the
    /// recovery question, and whether safe mode is on
    /// (`xarast_app::crash::begin_session`).
    #[must_use]
    pub fn with_startup_check(mut self, check: &xarast_app::crash::StartupCheck) -> Viewer {
        self.app = std::mem::take(&mut self.app).with_startup_check(check);
        self
    }

    /// Writes the composed window to `path` as a PNG as soon as the
    /// document has been rendered at the canvas's size, then quits. This
    /// is how a real window is checked without a screenshot tool, and why
    /// the capture reads back the swapchain rather than the canvas alone:
    /// it proves the composition, not just the rasteriser.
    #[must_use]
    pub fn with_screenshot(mut self, path: PathBuf) -> Viewer {
        self.screenshot = Some(path);
        self
    }

    /// Runs a scripted latency probe once the document has settled, prints
    /// its report and quits (`xarast --probe pan|zoom`).
    #[must_use]
    pub fn with_probe(mut self, probe: Probe) -> Viewer {
        self.probe = Some(probe);
        self
    }

    /// Opens a synthetic document of about `nodes` nodes (250 000 give
    /// about 100 000 filled and stroked paths), for the probes.
    #[must_use]
    pub fn with_synthetic(mut self, nodes: usize) -> Viewer {
        let doc = xarast_doc::synthetic_document(xarast_doc::SynthSpec {
            nodes,
            ..xarast_doc::SynthSpec::default()
        });
        self.app.adopt(doc);
        self.fit_pending = true;
        self.scene_stale = true;
        self.render_stale = true;
        self
    }

    /// The application state, for tests and for the caller after the loop.
    #[must_use]
    pub const fn app(&self) -> &AppState {
        &self.app
    }

    /// Opens what the command line or a drop queued. Each one replaces the
    /// document before it (the single-document model), so of several files
    /// the last that opens is the one shown.
    fn open_queued(&mut self) {
        let paths = std::mem::take(&mut self.to_open);
        let intents = paths.into_iter().map(Intent::OpenFile).collect();
        self.apply(intents);
    }

    /// Applies intents and records what is owed: a scene, a frame, a new
    /// title, a platform request. A failure is shown, never fatal.
    fn apply(&mut self, intents: Vec<Intent>) -> Changed {
        let mut changed = Changed::empty();
        for intent in intents {
            let opening = match &intent {
                Intent::OpenFile(p) => Some(p.clone()),
                _ => None,
            };
            match self.app.apply(intent) {
                Ok(c) => {
                    if let Some(path) = opening {
                        tracing::info!(path = %path.display(), "opened");
                        self.message = None;
                    }
                    changed |= c;
                }
                Err(e) => {
                    self.message = Some(match opening {
                        Some(path) => {
                            tracing::error!(path = %path.display(), error = %e, "could not open");
                            format!("Could not open {e}")
                        }
                        None => e.to_string(),
                    });
                }
            }
        }
        if changed.contains(Changed::ACTIVE) {
            // A different document, or none: frame it once the canvas has
            // its size, re-title the window, forget the old frame.
            self.primed = false;
            self.fit_pending = self.app.active().is_some();
            self.title_stale = true;
            self.shown = 0;
            self.settled_once = false;
        }
        self.requests.extend(self.app.take_requests());
        if let Some(n) = self.app.take_notice() {
            self.message = Some(n);
        }
        self.scene_stale |= changed.needs_scene();
        self.render_stale |= changed.needs_redraw();
        if let Some(canvas) = self.render.as_mut() {
            canvas.note(Instant::now(), changed);
        }
        changed
    }

    /// Runs a command of the table, from a menu or a key. A command that
    /// needs a document does nothing when none is open.
    fn run_command(&mut self, command: AppCommand) -> Changed {
        if command.needs_document() && self.app.active().is_none() {
            return Changed::empty();
        }
        let intent = command.intent(self.canvas_centre());
        self.apply(vec![intent])
    }

    /// The centre of the canvas, in canvas device pixels: where a keyboard
    /// or menu zoom is anchored.
    fn canvas_centre(&self) -> xarast_app::DevicePoint {
        let c = self.adapter.canvas();
        xarast_app::DevicePoint::new(f64::from(c.width) / 2.0, f64::from(c.height) / 2.0)
    }

    /// The command a key press runs, if any. Nothing fires while a text
    /// field has the keyboard (typing "1" into a layer name must not zoom),
    /// nor, unless marked otherwise, in the middle of a drag.
    fn shortcut(&self, key: &KeyEvent) -> Option<AppCommand> {
        if self.text_input {
            return None;
        }
        let in_drag = self
            .app
            .active()
            .is_some_and(|s| s.edit.tool.drag_from.is_some());
        self.shortcuts.resolve(key, in_drag)
    }

    /// Carries out what the core asked of the platform.
    fn perform_requests(&mut self, ctx: &mut ShellCtx<'_>) {
        for request in std::mem::take(&mut self.requests) {
            match request {
                PlatformRequest::ShowOpenDialog => {
                    // One chooser at a time: a second Ctrl+O while one is
                    // open would stack another dialog behind it.
                    if self.open_dialog.is_none() {
                        let id = ctx.portal().open_files(open_request(ctx.parent_window()));
                        self.open_dialog = Some(id);
                    }
                }
                PlatformRequest::ShowImportDialog => {
                    if self.import_dialog.is_none() {
                        let id = ctx.portal().open_files(import_request(ctx.parent_window()));
                        self.import_dialog = Some(id);
                    }
                }
                PlatformRequest::ShowReplaceBitmapDialog => {
                    if self.replace_dialog.is_none() {
                        let id = ctx
                            .portal()
                            .open_files(replace_request(ctx.parent_window()));
                        self.replace_dialog = Some(id);
                    }
                }
                PlatformRequest::ShowSaveCopyDialog {
                    title,
                    file_name,
                    directory,
                    filter,
                    extensions,
                } => {
                    if self.copy_dialog.is_none() {
                        let id = ctx.portal().save_file(SaveFileRequest {
                            title,
                            filters: vec![FileFilter::new(filter, &extensions)],
                            file_name: Some(file_name),
                            directory,
                            parent: ctx.parent_window(),
                        });
                        self.copy_dialog = Some(id);
                    }
                }
                PlatformRequest::ShowSaveDialog {
                    title,
                    file_name,
                    directory,
                } => {
                    if self.save_dialog.is_none() {
                        let id = ctx.portal().save_file(save_request(
                            title,
                            file_name,
                            directory,
                            ctx.parent_window(),
                        ));
                        self.save_dialog = Some(id);
                    }
                }
                PlatformRequest::ShowExportFileDialog {
                    title,
                    file_name,
                    directory,
                    filter,
                    extensions,
                } => {
                    if self.export_dialog.is_none() {
                        let id = ctx.portal().save_file(SaveFileRequest {
                            title,
                            filters: vec![FileFilter::new(filter, &extensions)],
                            file_name: Some(file_name),
                            directory,
                            parent: ctx.parent_window(),
                        });
                        self.export_dialog = Some(id);
                    }
                }
                PlatformRequest::Quit => ctx.exit(),
                PlatformRequest::SetClipboardText(text) => {
                    // The SVG flavour of a copy. `arboard` offers text,
                    // HTML and images only, so it goes out as text; the
                    // full-fidelity copy stays in the application.
                    if let Err(e) = ctx.clipboard().set_text(&text) {
                        self.message = Some(format!("Copied inside Xarast only: {e}"));
                    }
                }
                PlatformRequest::ReadClipboard { in_place } => {
                    let read = ctx.clipboard().text();
                    let ours = self.app.clipboard().map(|c| c.svg.as_str());
                    let is_ours = read.as_ref().ok().map(String::as_str) == ours;
                    tracing::info!(
                        read = ?read.as_ref().map(String::len),
                        ours = is_ours,
                        "clipboard read for a paste"
                    );
                    // A picture is pasted as a bitmap (phase 10, T10.3.8)
                    // when the text is not something Xarast pastes itself:
                    // not our copy, not SVG, and no text caret is up.
                    let text_editing = self.app.active().is_some_and(Session::text_editing);
                    // A file manager's copy names files: those are
                    // imported by the core, not looked for as a picture.
                    let wants_image = !text_editing
                        && match &read {
                            Ok(t) => {
                                !is_ours
                                    && !looks_like_svg(t)
                                    && xarast_app::place::image_paths_in_text(t).is_empty()
                            }
                            Err(crate::clipboard::ClipboardError::Unavailable(_)) => false,
                            Err(_) => true,
                        };
                    let image = if wants_image {
                        ctx.clipboard().image().ok()
                    } else {
                        None
                    };
                    let intent = match (image, read) {
                        (Some(img), _) => Intent::PasteImage {
                            width: u32::try_from(img.width).unwrap_or(0),
                            height: u32::try_from(img.height).unwrap_or(0),
                            rgba: std::sync::Arc::from(img.rgba),
                        },
                        (None, Ok(t)) => Intent::PasteText {
                            text: Some(t),
                            in_place,
                        },
                        // No clipboard at all: paste our own last copy.
                        (None, Err(crate::clipboard::ClipboardError::Unavailable(_))) => {
                            Intent::PasteText {
                                text: None,
                                in_place,
                            }
                        }
                        // Neither text nor a picture.
                        (None, Err(_)) => Intent::PasteText {
                            text: Some(String::new()),
                            in_place,
                        },
                    };
                    self.apply(vec![intent]);
                }
                PlatformRequest::ShowDialog(xarast_app::Dialog::Align) => {
                    self.workspace.menu().set_align_open(true);
                }
                PlatformRequest::ShowDialog(xarast_app::Dialog::Pane(pane)) => {
                    // F9/F10/F11 or the Window menu: the pane is docked
                    // again if needed, brought to the front and given the
                    // keyboard on the next frame.
                    self.workspace.show_pane(pane);
                    ctx.request_redraw();
                }
                PlatformRequest::EnterSafeMode => {
                    ctx.request_safe_mode();
                    ctx.request_redraw();
                }
                other => tracing::warn!(?other, "platform request not handled"),
            }
        }
    }

    /// The window title for the active document: `• name — Xarast` while
    /// it has unsaved changes, `(read-only)` when another session holds it.
    fn window_title(&self) -> String {
        match self.app.active() {
            Some(s) => {
                let marker = if s.is_modified() { "\u{2022} " } else { "" };
                let ro = if s.read_only { " (read-only)" } else { "" };
                format!("{marker}{}{ro} — Xarast", s.display_name())
            }
            None => "Xarast".to_owned(),
        }
    }

    /// Takes the answer to the save dialog. Returns false for an answer to
    /// something else.
    fn save_answer(&mut self, event: &PortalEvent) -> bool {
        let request = match event {
            PortalEvent::SaveChosen { request, .. }
            | PortalEvent::Cancelled { request }
            | PortalEvent::Failed { request, .. } => *request,
            _ => return false,
        };
        if self.save_dialog != Some(request) {
            return false;
        }
        self.save_dialog = None;
        let intent = match event {
            PortalEvent::SaveChosen { path, .. } => Intent::SaveTo(path.clone()),
            PortalEvent::Failed { reason, .. } => {
                let message = format!("Could not show the save dialog: {reason}");
                tracing::warn!("{message}");
                self.app.diagnostics.push(xarast_app::DiagnosticEntry {
                    severity: xarast_app::Severity::Error,
                    message: message.clone(),
                    document: None,
                });
                self.message = Some(message);
                Intent::SaveDialogClosed
            }
            _ => Intent::SaveDialogClosed,
        };
        self.apply(vec![intent]);
        true
    }

    /// Takes the answer to the export destination chooser. Returns false
    /// for an answer to something else.
    fn export_answer(&mut self, event: &PortalEvent) -> bool {
        let request = match event {
            PortalEvent::SaveChosen { request, .. }
            | PortalEvent::Cancelled { request }
            | PortalEvent::Failed { request, .. } => *request,
            _ => return false,
        };
        if self.export_dialog != Some(request) {
            return false;
        }
        self.export_dialog = None;
        let intent = match event {
            PortalEvent::SaveChosen { path, .. } => Intent::ExportTo(path.clone()),
            PortalEvent::Failed { reason, .. } => {
                let message = format!("Could not show the export file chooser: {reason}");
                tracing::warn!("{message}");
                self.app.diagnostics.push(xarast_app::DiagnosticEntry {
                    severity: xarast_app::Severity::Error,
                    message: message.clone(),
                    document: None,
                });
                self.message = Some(message);
                Intent::ExportFileDialogClosed
            }
            _ => Intent::ExportFileDialogClosed,
        };
        self.apply(vec![intent]);
        true
    }

    /// The core's own work between events: finished saves, due autosaves,
    /// the recovery question, a signal. Runs at every event and frame.
    fn housekeeping(&mut self, ctx: &mut ShellCtx<'_>) {
        if !self.wakers_set {
            self.wakers_set = true;
            let waker = ctx.waker();
            self.app.set_save_waker(Box::new(move || waker.wake()));
            self.signals.set_waker(ctx.waker());
            let waker = ctx.waker();
            self.fatal.set_waker(Box::new(move || waker.wake()));
        }
        if self.signals.requested() {
            let n = self.app.emergency_shutdown();
            tracing::info!(autosaved = n, "signal: autosaved and released every lock");
            ctx.exit();
            return;
        }
        if self.fatal.raised() {
            let n = self.app.emergency_shutdown();
            tracing::error!(
                thread = self.fatal.thread().as_deref().unwrap_or("?"),
                autosaved = n,
                "a worker thread panicked: autosaved and released every lock"
            );
            ctx.exit();
            return;
        }
        let mut changed = self.app.poll_saves();
        // Background imports that finished (T10.7.5) land now; thumbnails
        // that arrived redraw the gallery.
        changed |= self.app.poll_imports();
        // A finished export (T11.1.8) shows its report in the dialog.
        changed |= self.app.poll_exports();
        if self.app.poll_thumbnails() {
            ctx.request_redraw();
        }
        if !changed.is_empty() {
            self.requests.extend(self.app.take_requests());
            if let Some(n) = self.app.take_notice() {
                self.message = Some(n);
            }
            if changed.contains(Changed::ACTIVE) {
                self.primed = false;
                self.fit_pending = self.app.active().is_some();
                self.shown = 0;
                self.settled_once = false;
            }
            self.scene_stale |= changed.needs_scene();
            self.render_stale |= changed.needs_redraw();
            ctx.request_redraw();
        }
        self.autosave_due = self.app.tick(Instant::now());
        if !self.recovery_offered {
            self.recovery_offered = true;
            if self.screenshot.is_none() && self.probe.is_none() {
                let c = self.app.offer_recovery();
                if !c.is_empty() {
                    ctx.request_redraw();
                }
            }
        }
        self.perform_requests(ctx);
    }

    /// Takes the answer to the File › Import… chooser: every file chosen
    /// is placed in the middle of the view. Returns false for an answer to
    /// something else.
    fn import_answer(&mut self, event: &PortalEvent) -> bool {
        let request = match event {
            PortalEvent::FilesChosen { request, .. }
            | PortalEvent::Cancelled { request }
            | PortalEvent::Failed { request, .. } => *request,
            _ => return false,
        };
        if self.import_dialog != Some(request) {
            return false;
        }
        self.import_dialog = None;
        match event {
            PortalEvent::FilesChosen { paths, .. } => {
                let intents = paths
                    .iter()
                    .map(|path| Intent::ImportImage {
                        path: path.clone(),
                        at: None,
                    })
                    .collect();
                self.apply(intents);
            }
            PortalEvent::Failed { reason, .. } => {
                let message = format!("Could not show the file chooser: {reason}");
                tracing::warn!("{message}");
                self.app.diagnostics.push(xarast_app::DiagnosticEntry {
                    severity: xarast_app::Severity::Error,
                    message: message.clone(),
                    document: None,
                });
                self.message = Some(message);
            }
            _ => {}
        }
        true
    }

    /// Takes the answer to the bitmap gallery's Replace chooser: the one
    /// file chosen takes the bitmap's place. Returns false for an answer to
    /// something else.
    fn replace_answer(&mut self, event: &PortalEvent) -> bool {
        let request = match event {
            PortalEvent::FilesChosen { request, .. }
            | PortalEvent::Cancelled { request }
            | PortalEvent::Failed { request, .. } => *request,
            _ => return false,
        };
        if self.replace_dialog != Some(request) {
            return false;
        }
        self.replace_dialog = None;
        match event {
            PortalEvent::FilesChosen { paths, .. } => {
                if let Some(path) = paths.first() {
                    self.apply(vec![Intent::ReplaceBitmapWith(path.clone())]);
                }
            }
            PortalEvent::Failed { reason, .. } => self.chooser_failed(reason),
            _ => {}
        }
        true
    }

    /// Takes the answer to the bitmap gallery's Save a copy chooser.
    /// Returns false for an answer to something else.
    fn copy_answer(&mut self, event: &PortalEvent) -> bool {
        let request = match event {
            PortalEvent::SaveChosen { request, .. }
            | PortalEvent::Cancelled { request }
            | PortalEvent::Failed { request, .. } => *request,
            _ => return false,
        };
        if self.copy_dialog != Some(request) {
            return false;
        }
        self.copy_dialog = None;
        match event {
            PortalEvent::SaveChosen { path, .. } => {
                self.apply(vec![Intent::SaveBitmapCopyTo(path.clone())]);
            }
            PortalEvent::Failed { reason, .. } => self.chooser_failed(reason),
            _ => {}
        }
        true
    }

    /// Says that a file chooser could not be shown.
    fn chooser_failed(&mut self, reason: &str) {
        let message = format!("Could not show the file chooser: {reason}");
        tracing::warn!("{message}");
        self.app.diagnostics.push(xarast_app::DiagnosticEntry {
            severity: xarast_app::Severity::Error,
            message: message.clone(),
            document: None,
        });
        self.message = Some(message);
    }

    /// Takes the answer to the open dialog. Returns false for an answer to
    /// something else.
    fn portal_answer(&mut self, event: &PortalEvent) -> bool {
        let (request, outcome) = match event {
            PortalEvent::FilesChosen { request, paths } => (request, Ok(paths.first())),
            PortalEvent::Cancelled { request } => (request, Ok(None)),
            PortalEvent::Failed { request, reason } => (request, Err(reason)),
            _ => return false,
        };
        if self.open_dialog != Some(*request) {
            return false;
        }
        self.open_dialog = None;
        match outcome {
            Ok(Some(path)) => self.to_open.push(path.clone()),
            Ok(None) => {}
            Err(reason) => {
                let message = format!("Could not show the file chooser: {reason}");
                tracing::warn!("{message}");
                self.app.diagnostics.push(xarast_app::DiagnosticEntry {
                    severity: xarast_app::Severity::Error,
                    message: message.clone(),
                    document: None,
                });
                self.message = Some(message);
            }
        }
        true
    }

    fn ui_model(&mut self, scale: f64) -> UiModel {
        // A substituted font is reported, never silent (phase 9, W9.1).
        if let Some(last) = self.app.collect_font_substitutions().pop() {
            self.message = Some(last);
        }
        self.layer_keys.clear();
        self.guide_keys.clear();
        let document = self
            .app
            .active()
            .map(|s| document_view(s, scale, &mut self.layer_keys, &mut self.guide_keys));
        let colour_bar = self.app.active().map(Session::colour_bar_view);
        let bitmap_gallery = self.app.bitmap_gallery_view();
        let editing = self.app.active().map(|s| EditingView {
            tool: s.tools().current(),
            undo: s.undo_label().map(str::to_owned),
            redo: s.redo_label().map(str::to_owned),
            selected: s.edit.selection_len(),
            infobar: s.infobar(),
            snap: s.edit.snap,
        });
        UiModel {
            document,
            editing,
            status: StatusInfo {
                quality: xarast_ui::model::RenderQuality::Final,
                renderer: self.renderer_label.clone(),
                // While a colour is dragged the status line says what a
                // drop would do (`phase-08 §W8.7`).
                message: colour_bar
                    .as_ref()
                    .and_then(|v| v.drag.as_ref())
                    .map(|d| d.status.clone())
                    .or_else(|| {
                        bitmap_gallery
                            .as_ref()
                            .and_then(|v| v.drag.as_ref())
                            .map(|d| d.status.clone())
                    })
                    // Then what the tool says about the pointer or its
                    // gesture (T8.4.6); notices when it has nothing.
                    .or_else(|| self.app.active().and_then(Session::tool_status))
                    .or_else(|| self.message.clone()),
                problem_count: self.app.diagnostics.entries().len(),
                ..StatusInfo::default()
            },
            recent: self.app.recent.paths().to_vec(),
            prompt: self.app.prompt().cloned(),
            palette: vec![xarast_ui::model::PaletteEntry::none()],
            colour_editor: self.app.active().and_then(Session::colour_editor_view),
            colour_bar,
            bitmap_gallery,
            photo_panel: self.app.active().map(Session::photo_panel_view),
            imports: self.app.import_progress(),
            export_dialog: self.app.export_dialog_view(),
            system_scheme: match self.scheme {
                ColorScheme::NoPreference => xarast_ui::ColorScheme::NoPreference,
                ColorScheme::Dark => xarast_ui::ColorScheme::Dark,
                ColorScheme::Light => xarast_ui::ColorScheme::Light,
            },
            ..UiModel::default()
        }
    }

    /// Runs one interface frame: feeds `egui` the input gathered since the
    /// last one, lays out the workspace and applies what it asked for.
    /// Needs no window, which is what lets the input path be tested whole.
    fn ui_step(&mut self, scale: ScaleFactor, size: PhysicalSize) -> UiStep {
        let ppp = scale.pixels_per_point();
        self.ppp = ppp;
        let model = self.ui_model(f64::from(ppp));
        let raw = self.input.take(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size.width as f32 / ppp, size.height as f32 / ppp),
        ));
        let mut out = None;
        let overlay = self.app.active().map(overlay_items).unwrap_or_default();
        let workspace = &mut self.workspace;
        let full = self.egui.run(raw, |c| {
            out = Some(workspace.ui(c, &model, Scale::new(f64::from(ppp)), &overlay));
        });
        let primitives = self.egui.tessellate(full.shapes, full.pixels_per_point);
        let frame = UiFrame {
            primitives,
            textures: full.textures_delta,
            pixels_per_point: full.pixels_per_point,
        };
        let repaint = full
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.repaint_delay);
        // A focused text field is exactly when egui asks for an input
        // method, so that is the signal that the keyboard is egui's.
        self.text_input = full.platform_output.ime.is_some();

        let mut region = None;
        if let Some(out) = out {
            self.canvas_focused = out.canvas.as_ref().is_some_and(|c| c.focused);
            self.canvas_hovered = out.canvas.as_ref().is_some_and(|c| c.hovered);
            let intents: Vec<Intent> = out
                .commands
                .into_iter()
                .filter_map(|c| self.ui_intent(c, f64::from(ppp)))
                .collect();
            self.apply(intents);
            region = out.canvas.map(|c| {
                let r = c.rect_device;
                CanvasRegion::new(r.x, r.y, r.width, r.height)
            });
        }
        UiStep {
            frame,
            region,
            repaint,
            output: full.platform_output,
        }
    }

    /// Carries the interface's requests to the platform: the accessibility
    /// tree, the clipboard, the input method and the pointer shape.
    fn platform_output(&mut self, output: egui::PlatformOutput, ctx: &mut ShellCtx<'_>) {
        if let Some(mut update) = output.accesskit_update {
            name_the_tree(&mut update, &self.title);
            ctx.update_accessibility(update);
        }
        for command in output.commands {
            if let egui::OutputCommand::CopyText(text) = command
                && let Err(e) = ctx.clipboard().set_text(&text)
            {
                self.message = Some(format!("Could not copy: {e}"));
            }
        }
        let wanted = self.ime_request(output.ime.map(|i| i.cursor_rect));
        if wanted.is_some() != self.ime_allowed {
            ctx.set_ime_allowed(wanted.is_some());
            self.ime_allowed = wanted.is_some();
            self.ime_area = None;
            if wanted.is_none() {
                self.ime_ended();
            }
        }
        if let Some(area) = wanted
            && self.ime_area != Some(area)
        {
            self.ime_area = Some(area);
            let [x, y, w, h] = area.map(f64::from);
            ctx.set_ime_cursor_area(x, y, w, h);
        }
        // Over the canvas, where egui asks for nothing in particular, the
        // tool in force chooses the pointer.
        let shape = match self.app.active() {
            Some(s) if self.canvas_hovered && output.cursor_icon == egui::CursorIcon::Default => {
                tool_cursor(s.cursor())
            }
            _ => cursor_shape(output.cursor_icon),
        };
        if shape != self.cursor {
            self.cursor = shape;
            ctx.set_cursor(shape);
        }
    }

    /// Whether the input method should be on, and where its candidate
    /// window goes, in window device pixels: under egui's focused text
    /// field (`field`, in points) or, when the canvas has the keyboard,
    /// under the text tool's caret (T9.4.7). `None`: off. The IME is on
    /// exactly while one of those carets is up; with it on for no reason
    /// every key goes through the input method first.
    fn ime_request(&self, field: Option<egui::Rect>) -> Option<[i32; 4]> {
        if let Some(r) = field {
            let ppp = self.ppp;
            return Some(
                [r.min.x, r.min.y, r.width(), r.height()].map(|v| (v * ppp).round() as i32),
            );
        }
        if !self.canvas_has_keyboard() {
            return None;
        }
        let [x, y, w, h] = self.app.active()?.ime_cursor_area()?;
        let c = self.adapter.canvas();
        Some([
            (x + f64::from(c.x)).round() as i32,
            (y + f64::from(c.y)).round() as i32,
            w.round().max(1.0) as i32,
            h.round().max(1.0) as i32,
        ])
    }

    /// Whether key and input-method events are the canvas's: it has the
    /// keyboard, or nothing does.
    fn canvas_has_keyboard(&self) -> bool {
        !self.text_input && (self.canvas_focused || self.egui.memory(|m| m.focused().is_none()))
    }

    /// The input method went off (or away from the canvas): a composition
    /// still showing in the story ends without a commit.
    fn ime_ended(&mut self) {
        let was = self.ime.is_composing();
        self.ime = crate::ime::ImeState::new();
        if was && self.text_caret_up() {
            self.apply(vec![Intent::TextPreedit(None)]);
        }
    }

    /// An input-method event for the canvas's text caret (T9.4.7): the
    /// composition is shown in the story, a commit is typed. Returns
    /// whether a redraw is owed.
    fn ime_event(&mut self, event: &crate::ime::ImeEvent) -> bool {
        use crate::ime::ImeChange;
        let change = self.ime.apply(event.clone());
        let intent = match change {
            ImeChange::None | ImeChange::Started => return false,
            ImeChange::PreeditChanged => {
                let text = self.ime.preedit();
                Intent::TextPreedit((!text.is_empty()).then(|| xarast_app::tool::Preedit {
                    text: text.to_owned(),
                    cursor: self.ime.cursor(),
                }))
            }
            ImeChange::Cancelled => Intent::TextPreedit(None),
            ImeChange::Committed => match self.ime.take_commit() {
                Some(text) if !text.is_empty() => Intent::TextInput(xarast_app::TextInput {
                    kind: xarast_app::TextInputKind::Insert(text),
                    time_ms: self.adapter.now_ms(),
                }),
                _ => Intent::TextPreedit(None),
            },
        };
        self.apply(vec![intent]).needs_redraw()
    }

    /// Sizes and places the canvas from the interface's layout. Returns
    /// the new origin when the canvas moved without being resized.
    fn place_canvas(&mut self, region: CanvasRegion, scale: ScaleFactor) -> Option<(i32, i32)> {
        let mut intents = Vec::new();
        let mut moved = None;
        if self.primed {
            if region != self.adapter.canvas() {
                moved = Some((region.x, region.y));
            }
            self.adapter.set_canvas(region, &mut intents);
        } else {
            self.adapter.prime(region, scale, &mut intents);
            self.primed = true;
        }
        self.apply(intents);
        if self.fit_pending {
            self.fit_pending = false;
            self.apply(vec![Intent::ZoomTo(xarast_app::ZoomTarget::Page)]);
        }
        moved
    }

    /// Maps an interface command onto an intent. The interface speaks in
    /// logical points; intents are device pixels.
    fn ui_intent(&self, cmd: UiCommand, ppp: f64) -> Option<Intent> {
        let line = matches!(cmd, UiCommand::SetLine(_));
        let layer = |k: LayerKey| {
            usize::try_from(k.0)
                .ok()
                .and_then(|i| self.layer_keys.get(i).copied())
        };
        Some(match cmd {
            UiCommand::SetLayerVisible { layer: k, visible } => Intent::SetLayerVisible {
                layer: layer(k)?,
                visible,
            },
            UiCommand::SetLayerLocked { layer: k, locked } => Intent::SetLayerLocked {
                layer: layer(k)?,
                locked,
            },
            UiCommand::SetActiveLayer(k) => Intent::SetActiveLayer(layer(k)?),
            UiCommand::RenameLayer { layer: k, name } => Intent::RenameLayer {
                layer: layer(k)?,
                name,
            },
            UiCommand::Pan { dx, dy } => Intent::Pan {
                dx: dx * ppp,
                dy: dy * ppp,
            },
            UiCommand::ZoomAbout {
                factor,
                anchor_x,
                anchor_y,
            } => Intent::Zoom {
                factor,
                anchor: xarast_app::DevicePoint::new(anchor_x * ppp, anchor_y * ppp),
            },
            UiCommand::ZoomTo(t) => Intent::ZoomTo(zoom_target(t)),
            UiCommand::App(c) if c.needs_document() && self.app.active().is_none() => return None,
            UiCommand::App(c) => c.intent(self.canvas_centre()),
            UiCommand::OpenRecent(path) => Intent::OpenFile(path),
            UiCommand::ClearRecent => Intent::ClearRecent,
            UiCommand::AnswerPrompt(a) => Intent::AnswerPrompt(a),
            UiCommand::InfobarEdit { field, value } => Intent::InfobarEdit { field, value },
            UiCommand::InfobarBump { field, up } => Intent::InfobarBump { field, up },
            UiCommand::InfobarDrag(op) => Intent::InfobarDrag(op),
            UiCommand::ColourEditor(op) => Intent::ColourEditor(op),
            UiCommand::ColourBar(op) => Intent::ColourBar(op),
            UiCommand::ColourDragAt { x, y, shift } => {
                let at = PhysicalPos::new(f64::from(x) * ppp, f64::from(y) * ppp);
                let canvas = self.adapter.canvas();
                Intent::ColourBar(xarast_app::colour_bar::ColourBarOp::DragTo(
                    if canvas.contains(at) {
                        xarast_app::colour_bar::DragPoint::Canvas {
                            at: canvas.to_canvas(at),
                            shift,
                        }
                    } else {
                        xarast_app::colour_bar::DragPoint::Elsewhere
                    },
                ))
            }
            UiCommand::BitmapGallery(op) => Intent::BitmapGallery(op),
            UiCommand::PhotoPanel(op) => Intent::PhotoPanel(op),
            UiCommand::BitmapDragAt { x, y } => {
                use xarast_app::bitmap_gallery::{BitmapDragPoint, BitmapGalleryOp};
                let at = PhysicalPos::new(f64::from(x) * ppp, f64::from(y) * ppp);
                let canvas = self.adapter.canvas();
                Intent::BitmapGallery(BitmapGalleryOp::DragTo(if canvas.contains(at) {
                    BitmapDragPoint::Canvas(canvas.to_canvas(at))
                } else {
                    BitmapDragPoint::Elsewhere
                }))
            }
            UiCommand::CancelImports => Intent::CancelImports,
            UiCommand::ExportDialog(op) => Intent::ExportDialog(op),
            UiCommand::SetFill(v) | UiCommand::SetLine(v) => {
                use xarast_app::colour_bar::{ColourBarOp, ColourSource};
                use xarast_app::colour_editor::PaintSlot;
                Intent::ColourBar(ColourBarOp::Apply {
                    source: v.map_or(ColourSource::NoColour, ColourSource::Direct),
                    slot: if line {
                        PaintSlot::Stroke
                    } else {
                        PaintSlot::Fill
                    },
                })
            }
            UiCommand::Align(spec) => Intent::Align(spec),
            UiCommand::AddGuide(g) => Intent::Guides(GuideOp::Add {
                horizontal: g.axis == xarast_ui::guides::Axis::Horizontal,
                position: g.position,
            }),
            UiCommand::MoveGuide { index, position } => Intent::Guides(GuideOp::Move {
                guide: *self.guide_keys.get(index)?,
                position,
            }),
            UiCommand::RemoveGuide(index) => {
                Intent::Guides(GuideOp::Delete(*self.guide_keys.get(index)?))
            }
            UiCommand::SetGuidesVisible(on) => {
                let s = self.app.active()?;
                if xarast_app::snap::guides_visible(&s.doc) == on {
                    return None;
                }
                Intent::ToggleGuides
            }
            UiCommand::SetGrid(g) => {
                let s = self.app.active()?;
                let mut grid = xarast_app::snap::grid_of(&s.doc);
                grid.visible = g.visible;
                grid.spacing = g.spacing;
                grid.subdivisions = g.subdivisions.max(1);
                grid.origin = xarast_geom::Point::new(g.origin.0, g.origin.1);
                Intent::Guides(GuideOp::SetGrid(grid))
            }
            // Unit, colours, layer order and the theme have no intent
            // yet; they are phase 8 commands.
            _ => return None,
        })
    }

    fn submit_render(&mut self, ctx: &ShellCtx<'_>) {
        if self.render.is_none() {
            let waker = ctx.waker();
            let backdrop = Backdrop {
                pasteboard: PASTEBOARD,
                page: PAGE,
            };
            match Canvas::spawn(Box::new(move || waker.wake()), backdrop) {
                Ok(mut canvas) => {
                    // The shell composites retained tiles at input time
                    // (`tiles.rs`), so a Draft zoom is already on screen;
                    // resampling it again on the CPU would be thrown away.
                    canvas.set_cpu_rescale(false);
                    self.render = Some(canvas);
                }
                Err(e) => {
                    self.message = Some(format!("Could not start the render thread: {e}"));
                    return;
                }
            }
        }
        let Some(session) = self.app.active_mut() else {
            return;
        };
        let Some(canvas) = self.render.as_mut() else {
            return;
        };
        if self.render_stale || self.scene_stale {
            canvas.invalidate();
        }
        // The canvas rebuilds the scene when the session says it is stale,
        // and decides between a Draft now and a Final after 120 ms idle.
        match canvas.pump(Instant::now(), session) {
            Ok(due) => self.final_due = due,
            Err(e) => {
                self.message = Some(e.to_string());
                return;
            }
        }
        self.scene_stale = false;
        self.render_stale = false;
    }
}

impl Viewer {
    /// Tells the shell which view to composite this frame: the session's
    /// current one, which during a gesture is ahead of the last rendered
    /// frame. That is what puts a pan on screen at input time.
    fn show_view(&mut self, ctx: &mut ShellCtx<'_>) {
        let Some(session) = self.app.active() else {
            if std::mem::take(&mut self.view_shown) {
                ctx.clear_canvas();
            }
            return;
        };
        let c = self.adapter.canvas();
        let vp = session.viewport.device_rect();
        if !self.primed || c.width == 0 || c.height == 0 || vp.is_empty() {
            return;
        }
        ctx.set_canvas_view(CanvasView {
            origin: (c.x, c.y),
            width: vp.width(),
            height: vp.height(),
            transform: session.viewport.transform(),
            backdrop: PASTEBOARD,
        });
        self.view_shown = true;
    }

    /// One step of the latency probe: records the previous step's present
    /// and applies the next scripted intent, as input would.
    fn drive_probe(&mut self, ctx: &mut ShellCtx<'_>) {
        if !self.settled_once || self.app.active().is_none() {
            return;
        }
        let Some(probe) = self.probe.as_mut() else {
            return;
        };
        probe.record(ctx.last_present());
        if probe.done() {
            if let Some(release) = probe.finish() {
                self.apply(vec![release]);
            }
            let Some(probe) = self.probe.as_mut() else {
                return;
            };
            let vp = self
                .app
                .active()
                .map(|s| s.viewport.device_rect())
                .unwrap_or_default();
            println!(
                "{}",
                probe.report(&self.renderer_label, (vp.width(), vp.height()))
            );
            if let Some(canvas) = self.render.as_ref() {
                println!("  render thread: {:?}", canvas.render_thread().stats());
            }
            self.probe = None;
            if self.screenshot.is_none() {
                ctx.exit();
            }
            return;
        }
        let c = self.adapter.canvas();
        let mut anchor = (f64::from(c.width) / 2.0, f64::from(c.height) / 2.0);
        let setup = if probe.kind().is_gesture()
            && !probe.has_anchor()
            && let Some(s) = self.app.active()
        {
            Some(gesture_setup(s, probe.kind(), anchor))
        } else {
            None
        };
        let Some(probe) = self.probe.as_mut() else {
            return;
        };
        if let Some((prelude, press)) = setup {
            anchor = press;
            probe.set_gesture(prelude, press);
        }
        let intent = probe.next(anchor);
        self.apply(vec![intent]);
    }
}

/// The shell's key for a command table key.
fn chord_key(key: ChordKey) -> Key {
    match key {
        ChordKey::Char(c) | ChordKey::NumPad(c) => Key::char(c),
        ChordKey::Home => Key::Named(NamedKey::Home),
        ChordKey::End => Key::Named(NamedKey::End),
        ChordKey::Tab => Key::Named(NamedKey::Tab),
        ChordKey::Arrow(d) => Key::Named(match d {
            xarast_app::NudgeDir::Up => NamedKey::ArrowUp,
            xarast_app::NudgeDir::Down => NamedKey::ArrowDown,
            xarast_app::NudgeDir::Left => NamedKey::ArrowLeft,
            xarast_app::NudgeDir::Right => NamedKey::ArrowRight,
        }),
        ChordKey::Delete => Key::Named(NamedKey::Delete),
        ChordKey::Backspace => Key::Named(NamedKey::Backspace),
        ChordKey::Enter => Key::Named(NamedKey::Enter),
        ChordKey::Escape => Key::Named(NamedKey::Escape),
        ChordKey::Function(n) => Key::Named(NamedKey::Function(n)),
    }
}

/// The command table's keys as the shell's shortcuts.
///
/// Punctuation some layouts type with Shift (`+` is Shift+`=` on a US
/// keyboard, a key of its own on the keypad) is bound with and without it;
/// letters and digits are bound exactly, because Shift changes what they
/// are.
fn command_shortcuts() -> ShortcutMap<AppCommand> {
    let mut map = ShortcutMap::new();
    for command in AppCommand::ALL {
        for chord in command.shortcuts() {
            let key = chord_key(chord.key);
            let mut modifiers = Modifiers::NONE;
            if chord.ctrl {
                modifiers = modifiers.with_ctrl();
            }
            if chord.shift {
                modifiers = modifiers.with_shift();
            }
            let mut shortcut = Shortcut::new(key.clone(), modifiers);
            if matches!(chord.key, ChordKey::NumPad(_)) {
                shortcut = shortcut.at(crate::input::keyboard::KeyLocation::Numpad);
            }
            if command.works_in_drag() {
                shortcut = shortcut.works_in_drag();
            }
            if let Some(clash) = map.bind(shortcut, command) {
                tracing::warn!(%chord, ?clash, ?command, "shortcut bound twice");
            }
            if chord.shift_is_layout_dependent() && !chord.shift {
                map.bind(Shortcut::new(key, modifiers.with_shift()), command);
            }
            // Shift makes a letter a capital: the layout reports Ctrl+Shift+Z
            // as "Z", so a shifted letter is bound in both cases.
            if let ChordKey::Char(c) = chord.key
                && chord.shift
                && c.is_ascii_alphabetic()
            {
                map.bind(
                    Shortcut::new(Key::char(c.to_ascii_uppercase()), modifiers),
                    command,
                );
            }
        }
    }
    map
}

/// File › Open…: `.xar` documents first, then anything.
/// File › Save As…: `.xarast` only — nothing else is ever written.
fn save_request(
    title: String,
    file_name: String,
    directory: Option<PathBuf>,
    parent: Option<String>,
) -> SaveFileRequest {
    SaveFileRequest {
        title,
        filters: vec![FileFilter::new("Xarast documents (*.xarast)", &["xarast"])],
        file_name: Some(file_name),
        directory,
        parent,
    }
}

fn open_request(parent: Option<String>) -> OpenFileRequest {
    OpenFileRequest {
        title: "Open".to_owned(),
        filters: vec![
            FileFilter::new("Xarast documents (*.xarast)", &["xarast"]),
            FileFilter::new("Xara documents (*.xar)", &["xar"]),
            FileFilter::new("All files", &["*"]),
        ],
        multiple: false,
        directory: None,
        parent,
    }
}

/// File › Import…: the images Xarast places, then anything; several at
/// once.
fn import_request(parent: Option<String>) -> OpenFileRequest {
    OpenFileRequest {
        title: "Import".to_owned(),
        filters: vec![
            FileFilter::new(
                "Images (PNG, JPEG, GIF, WebP, TIFF, BMP, PNM)",
                &[
                    "png", "jpg", "jpeg", "jpe", "gif", "webp", "tif", "tiff", "bmp", "pnm", "pbm",
                    "pgm", "ppm", "pam",
                ],
            ),
            FileFilter::new("All files", &["*"]),
        ],
        multiple: true,
        directory: None,
        parent,
    }
}

/// The bitmap gallery's Replace: the Import chooser, for one file.
fn replace_request(parent: Option<String>) -> OpenFileRequest {
    OpenFileRequest {
        title: "Replace bitmap".to_owned(),
        multiple: false,
        ..import_request(parent)
    }
}

fn zoom_target(t: xarast_ui::model::ZoomTarget) -> xarast_app::ZoomTarget {
    use xarast_app::ZoomTarget as A;
    use xarast_ui::model::ZoomTarget as U;
    match t {
        U::Page => A::Page,
        U::Spread => A::Spread,
        U::Drawing => A::Drawing,
        U::Selection => A::Selection,
        U::Percent100 => A::Percent100,
        U::Previous => A::Previous,
    }
}

/// Whether clipboard text is an SVG document, which Xarast pastes as
/// objects rather than looking for a picture beside it.
fn looks_like_svg(t: &str) -> bool {
    let t = t.trim_start();
    t.starts_with("<svg") || (t.starts_with("<?xml") && t.contains("<svg"))
}

impl Viewer {
    /// Routes one platform event: files and the colour scheme to the
    /// viewer, view keys and everything the adapter understands to the
    /// active document. Returns whether a redraw is owed.
    ///
    /// Separate from [`ShellApp::on_event`] so that the whole input path
    /// — shell event, adapter, intent, session, render bookkeeping — runs
    /// in a test with no window.
    pub fn handle(&mut self, event: &ShellEvent) -> bool {
        let mut redraw = false;
        match event {
            ShellEvent::Drag(DragEvent::Dropped { paths, at }) => {
                // Images dropped on an open document are placed in it, at
                // the drop point (phase 10, T10.3.8); everything else is
                // opened as a document.
                let (images, docs): (Vec<PathBuf>, Vec<PathBuf>) =
                    paths.iter().cloned().partition(|p| {
                        self.app.active().is_some() && xarast_app::place::is_image_path(p)
                    });
                self.to_open.extend(docs);
                if !images.is_empty() {
                    let canvas = self.adapter.canvas();
                    let at = at
                        .map(|p| PhysicalPos::new(f64::from(p.x), f64::from(p.y)))
                        .filter(|p| canvas.contains(*p))
                        .map(|p| canvas.to_canvas(p));
                    let intents = images
                        .into_iter()
                        .map(|path| Intent::ImportImage { path, at })
                        .collect();
                    self.apply(intents);
                }
                redraw = true;
            }
            ShellEvent::AccessibilityActivated => {
                // egui builds the tree from the next frame on; the shell
                // publishes it (`platform_output`).
                self.egui.enable_accesskit();
                redraw = true;
            }
            ShellEvent::AccessibilityDeactivated => self.egui.disable_accesskit(),
            ShellEvent::GpuRebuilt(r) => {
                self.message = Some(match r.cause {
                    crate::gpu_errors::RebuildCause::Lost { .. } => format!(
                        "The graphics device was lost and has been restarted \
                         ({}); nothing was lost",
                        r.renderer
                    ),
                    crate::gpu_errors::RebuildCause::SafeMode => {
                        format!("Safe mode: {}", r.renderer)
                    }
                });
                redraw = true;
            }
            ShellEvent::GpuError(report) => {
                // Already logged and being recovered from by the shell;
                // the status bar says so rather than the window vanishing.
                self.message = Some(format!(
                    "Graphics error ({} so far), recovering: {}",
                    report.total, report.message
                ));
                redraw = true;
            }
            ShellEvent::Portal(answer) if self.portal_answer(answer) => redraw = true,
            ShellEvent::Portal(answer) if self.import_answer(answer) => redraw = true,
            ShellEvent::Portal(answer) if self.replace_answer(answer) => redraw = true,
            ShellEvent::Portal(answer) if self.copy_answer(answer) => redraw = true,
            ShellEvent::Portal(answer) if self.save_answer(answer) => redraw = true,
            ShellEvent::Portal(answer) if self.export_answer(answer) => redraw = true,
            ShellEvent::CloseRequested => {
                // The window's close button is File › Quit: it asks about
                // unsaved changes first.
                self.apply(vec![Intent::Quit]);
                redraw = true;
            }
            ShellEvent::ColorSchemeChanged(s)
            | ShellEvent::Portal(PortalEvent::ColorSchemeChanged(s)) => {
                if self.scheme != *s {
                    tracing::info!(scheme = ?s, "following the desktop colour scheme");
                }
                self.scheme = *s;
                redraw = true;
            }
            // A text field has the keyboard: typing "1" into a layer name
            // must not zoom to 100 %.
            // The release of a momentary switch's key restores the tool
            // even while a text field has the keyboard.
            ShellEvent::Key(k) if k.state == KeyState::Released => {
                if let (Some(intent), _) = self.momentary.key(k) {
                    redraw |= self.apply(vec![intent]).needs_redraw();
                }
            }
            ShellEvent::Focused(false) => {
                if let Some(intent) = self.momentary.release() {
                    redraw |= self.apply(vec![intent]).needs_redraw();
                }
            }
            ShellEvent::Ime(ime) if self.text_caret_up() && self.canvas_has_keyboard() => {
                redraw |= self.ime_event(ime);
            }
            // While the input method composes, keys are its own: a
            // composition keystroke must never run a shortcut or move the
            // caret (most platforms do not send them at all then).
            ShellEvent::Key(k)
                if k.state == KeyState::Pressed
                    && !self.text_input
                    && self.ime.is_composing()
                    && self.text_caret_up() => {}
            ShellEvent::Key(k)
                if k.state == KeyState::Pressed && !self.text_input && self.text_caret_up() =>
            {
                // A text caret is up on the canvas (phase 9, W9.4): the
                // navigation keys move it, and plain character keys are the
                // text's (typing, T9.4.6), never tool keys or momentary
                // switches. Named keys (Delete, Esc, Enter, F-keys) and
                // Ctrl chords still run their commands.
                if let Some(nav) = self.text_nav(k) {
                    redraw |= self.apply(vec![Intent::TextNav(nav)]).needs_redraw();
                } else if let Some(kind) = self.text_typing(k) {
                    let input = xarast_app::TextInput {
                        kind,
                        time_ms: self.adapter.now_ms(),
                    };
                    redraw |= self.apply(vec![Intent::TextInput(input)]).needs_redraw();
                } else if !(matches!(k.key, Key::Character(_))
                    && !k.modifiers.ctrl
                    && !k.modifiers.alt)
                    && let Some(command) = self.shortcut(k)
                {
                    let changed = self.run_command(command);
                    redraw |= changed.needs_redraw()
                        || changed.contains(Changed::ACTIVE)
                        || !self.requests.is_empty();
                }
            }
            ShellEvent::Key(k) if k.state == KeyState::Pressed && !self.text_input => {
                if let Some(nudge) = self.arrow_nudge(k) {
                    redraw |= self.apply(vec![nudge]).needs_redraw();
                } else if !k.modifiers.constrain()
                    && let Some(pan) = self.arrow_pan(&k.key)
                {
                    redraw |= self.apply(vec![pan]).needs_redraw();
                }
                let (momentary, consumed) = if self.app.active().is_some() {
                    self.momentary.key(k)
                } else {
                    (None, false)
                };
                if let Some(intent) = momentary {
                    redraw |= self.apply(vec![intent]).needs_redraw();
                }
                let tool_key = if consumed { None } else { self.tool_key(k) };
                if consumed {
                    // A momentary switch, not a shortcut.
                } else if let Some(intent) = tool_key {
                    // The tool claims the key before the command table.
                    redraw |= self.apply(vec![intent]).needs_redraw();
                } else if let Some(command) = self.shortcut(k) {
                    let changed = self.run_command(command);
                    redraw |= changed.needs_redraw()
                        || changed.contains(Changed::ACTIVE)
                        || !self.requests.is_empty();
                }
            }
            _ => {}
        }
        if self.egui_is_above_the_canvas(event) {
            return redraw;
        }
        let mut intents = Vec::new();
        self.adapter.translate(event, &mut intents);
        if !intents.is_empty() {
            redraw |= self.apply(intents).needs_redraw();
        }
        redraw
    }
}

impl Viewer {
    /// Whether the active document's tool has a text caret up.
    fn text_caret_up(&self) -> bool {
        self.app.active().is_some_and(Session::text_editing)
    }

    /// The caret movement a key asks for, when the canvas has the keyboard
    /// (or nothing does): arrows, Home, End, Page Up and Page Down; Ctrl
    /// moves by word (or to the story's ends), Shift extends.
    fn text_nav(&self, k: &KeyEvent) -> Option<xarast_app::TextNav> {
        use xarast_app::TextKey as T;
        let nothing_focused = self.egui.memory(|m| m.focused().is_none());
        if !(self.canvas_focused || nothing_focused) {
            return None;
        }
        let key = match k.key {
            Key::Named(NamedKey::ArrowLeft) => T::Left,
            Key::Named(NamedKey::ArrowRight) => T::Right,
            Key::Named(NamedKey::ArrowUp) => T::Up,
            Key::Named(NamedKey::ArrowDown) => T::Down,
            Key::Named(NamedKey::Home) => T::Home,
            Key::Named(NamedKey::End) => T::End,
            Key::Named(NamedKey::PageUp) => T::PageUp,
            Key::Named(NamedKey::PageDown) => T::PageDown,
            _ => return None,
        };
        Some(xarast_app::TextNav {
            key,
            word: k.modifiers.ctrl,
            extend: k.modifiers.shift,
        })
    }

    /// What a key types into the text being edited, when the canvas has the
    /// keyboard (or nothing does): Backspace and Delete (Ctrl: by word),
    /// Enter, and any key producing printable text or a tab with neither
    /// Ctrl nor Alt held (so Ctrl chords stay shortcuts).
    fn text_typing(&self, k: &KeyEvent) -> Option<xarast_app::TextInputKind> {
        use xarast_app::TextInputKind as T;
        let nothing_focused = self.egui.memory(|m| m.focused().is_none());
        if !(self.canvas_focused || nothing_focused) {
            return None;
        }
        let word = k.modifiers.ctrl;
        match k.key {
            Key::Named(NamedKey::Backspace) => return Some(T::Backspace { word }),
            Key::Named(NamedKey::Delete) => return Some(T::Delete { word }),
            Key::Named(NamedKey::Enter) if !k.modifiers.ctrl && !k.modifiers.alt => {
                return Some(T::Insert("\n".to_owned()));
            }
            _ => {}
        }
        if k.modifiers.ctrl || k.modifiers.alt {
            return None;
        }
        let text = k.text.as_deref()?;
        text.chars()
            .any(|c| c == '\t' || !c.is_control())
            .then(|| T::Insert(text.to_owned()))
    }

    /// Arrow keys nudge what the tool in force has selected (a fill
    /// handle, XARA-T-0220) when the canvas has the keyboard, or when
    /// nothing does; the step follows the modifiers (`research/04 §4.5`).
    fn arrow_nudge(&self, k: &KeyEvent) -> Option<Intent> {
        use xarast_app::{Nudge, NudgeDir, NudgeStep};
        let nothing_focused = self.egui.memory(|m| m.focused().is_none());
        if !(self.canvas_focused || nothing_focused) {
            return None;
        }
        let dir = match k.key {
            Key::Named(NamedKey::ArrowLeft) => NudgeDir::Left,
            Key::Named(NamedKey::ArrowRight) => NudgeDir::Right,
            Key::Named(NamedKey::ArrowUp) => NudgeDir::Up,
            Key::Named(NamedKey::ArrowDown) => NudgeDir::Down,
            _ => return None,
        };
        if !self.app.active().is_some_and(Session::takes_nudge) {
            return None;
        }
        let m = k.modifiers;
        Some(Intent::Nudge(Nudge {
            dir,
            step: NudgeStep::from_keys(m.constrain(), m.adjust(), m.alternative()),
        }))
    }

    /// A context key the tool in force claims before the command table
    /// (`xarast_app::ToolKey`), when the canvas has the keyboard or nothing
    /// does: Tab, Shift+Tab, Home and End move the shape editor's point
    /// selection while it has a path; `NumPad +` drops a copy during a
    /// transform drag. Otherwise the key runs its shortcut (Home fits the
    /// page, `NumPad +` zooms in).
    fn tool_key(&self, k: &KeyEvent) -> Option<Intent> {
        use crate::input::keyboard::KeyLocation;
        use xarast_app::ToolAction;
        let nothing_focused = self.egui.memory(|m| m.focused().is_none());
        if !(self.canvas_focused || nothing_focused) {
            return None;
        }
        let session = self.app.active()?;
        let m = k.modifiers;
        if m.ctrl || m.alt {
            return None;
        }
        if k.location == KeyLocation::Numpad
            && k.key == Key::char('+')
            && session.edit.tool.drag_from.is_some()
        {
            return Some(Intent::DropCopy);
        }
        let action = match (&k.key, m.shift) {
            (Key::Named(NamedKey::Tab), false) => ToolAction::NextPoint,
            (Key::Named(NamedKey::Tab), true) => ToolAction::PreviousPoint,
            (Key::Named(NamedKey::Home), false) => ToolAction::FirstPoint,
            (Key::Named(NamedKey::End), false) => ToolAction::LastPoint,
            _ => return None,
        };
        session
            .claims_action(action)
            .then_some(Intent::ToolAction(action))
    }

    /// Arrow keys pan the view when the canvas has the keyboard, or when
    /// nothing does; otherwise they belong to the focused widget.
    fn arrow_pan(&self, key: &Key) -> Option<Intent> {
        let nothing_focused = self.egui.memory(|m| m.focused().is_none());
        if !(self.canvas_focused || nothing_focused) {
            return None;
        }
        let step = xarast_ui::canvas::KEY_PAN_STEP * f64::from(self.ppp);
        let (dx, dy) = match key {
            Key::Named(NamedKey::ArrowLeft) => (step, 0.0),
            Key::Named(NamedKey::ArrowRight) => (-step, 0.0),
            Key::Named(NamedKey::ArrowUp) => (0.0, step),
            Key::Named(NamedKey::ArrowDown) => (0.0, -step),
            _ => return None,
        };
        Some(Intent::Pan { dx, dy })
    }

    /// Whether this press or wheel lands on something `egui` shows above
    /// the canvas — a popup, a menu, a floating window — and so is not the
    /// canvas's. Panels share the background layer with the canvas and are
    /// told apart by the adapter's own region test instead. Motion and
    /// releases always reach the adapter, so a drag it owns is never cut.
    fn egui_is_above_the_canvas(&self, event: &ShellEvent) -> bool {
        let ShellEvent::Pointer(p) = event else {
            return false;
        };
        if !matches!(
            p.phase,
            PointerPhase::Pressed(_) | PointerPhase::Scroll { .. }
        ) {
            return false;
        }
        let at = points(p.position, self.ppp);
        self.egui
            .layer_id_at(at)
            .is_some_and(|l| l.order != egui::Order::Background)
    }
}

/// Names what egui leaves anonymous: the root node is the window, and a
/// screen reader announces it by the window's title; the toolkit is
/// reported as egui's so an assistive technology can apply its quirks.
fn name_the_tree(update: &mut egui::accesskit::TreeUpdate, title: &str) {
    let Some(tree) = update.tree.as_mut() else {
        // An incremental update keeps the names already published.
        return;
    };
    tree.toolkit_name = Some("egui".to_owned());
    tree.toolkit_version = Some("0.33".to_owned());
    let root = tree.root;
    if let Some((_, node)) = update.nodes.iter_mut().find(|(id, _)| *id == root) {
        node.set_label(title.to_owned());
    }
}

/// What a gesture probe does before it presses, and where it presses.
///
/// The drag presses on the object nearest the canvas centre, so it moves
/// something rather than drawing a marquee; the scale clicks that object
/// first and presses on its top-right blob; the rotation clicks it twice
/// (a second apart, so not a double click) for the rotate handles; the
/// shape tools choose their tool and press beside the centre.
fn gesture_setup(
    s: &Session,
    kind: crate::probe::ProbeKind,
    centre: (f64, f64),
) -> (Vec<Intent>, (f64, f64)) {
    use crate::probe::ProbeKind as K;
    use xarast_app::{PointerButton, PointerSample};
    let target = drag_target(s);
    let click = |at: (f64, f64), time_ms: u64| {
        let sample = PointerSample {
            at: xarast_app::DevicePoint::new(at.0, at.1),
            pressure: None,
            time_ms,
        };
        [
            Intent::PointerMove(sample),
            Intent::PointerDown {
                button: PointerButton::Primary,
                sample,
            },
            Intent::PointerUp {
                button: PointerButton::Primary,
                sample,
            },
        ]
    };
    match (kind, target) {
        (K::Scale | K::Rotate, Some((at, node))) => {
            // The click selects whatever is on top at that point, which
            // need not be the object whose centre it is.
            use xarast_app::geometry::DocPointF64Ext;
            let p = s
                .viewport
                .device_to_doc_f64(xarast_app::DevicePoint::new(at.0, at.1))
                .to_doc_point();
            let node = xarast_app::tool::pick(&s.doc, p, xarast_geom::Mp::ZERO)
                .map_or(node, |h| h.top_group);
            let b = xarast_app::viewport::nodes_rect(&s.doc, [node]);
            let blob = xarast_app::selector::blob_points(b)[7];
            let d = s.viewport.doc_to_device(blob);
            let mut prelude: Vec<Intent> = click(at, 0).into();
            if kind == K::Rotate {
                prelude.extend(click(at, 1000));
            }
            (prelude, (d.x, d.y))
        }
        (K::Snap, Some((at, _))) => {
            use xarast_app::snap::{GuideOp, SnapKind};
            let mut grid = xarast_app::snap::grid_of(&s.doc);
            grid.visible = true;
            grid.spacing = xarast_geom::Mp::from_mm(10.0);
            grid.subdivisions = 2;
            let guide_x = s
                .viewport
                .device_to_doc(xarast_app::DevicePoint::new(at.0 + 90.0, at.1))
                .x;
            let mut prelude = vec![
                Intent::Guides(GuideOp::SetGrid(grid)),
                Intent::Guides(GuideOp::Add {
                    horizontal: false,
                    position: guide_x,
                }),
                Intent::ToggleSnap(SnapKind::Grid),
                Intent::ToggleSnap(SnapKind::Object),
            ];
            if s.edit.snap.guides {
                // Guides snap by default; keep them on.
            } else {
                prelude.push(Intent::ToggleSnap(SnapKind::Guide));
            }
            (prelude, at)
        }
        (K::Arrange, Some((at, node))) => {
            use xarast_app::structure::{AlignSpec, AlignTarget, AxisAlign, ZOrder};
            let select = |mode| Intent::Select {
                nodes: vec![node],
                mode,
            };
            // Each Duplicate copies the selection and selects the copy: a
            // stair of four. The original joins the last copy, the two are
            // aligned on their left edges and grouped, and the group goes to
            // the back. The Alignment panel opens for the screenshot.
            let prelude = vec![
                select(xarast_app::SelectMode::Replace),
                Intent::Duplicate,
                Intent::Duplicate,
                Intent::Duplicate,
                select(xarast_app::SelectMode::Add),
                Intent::Align(AlignSpec {
                    x: AxisAlign::Min,
                    y: AxisAlign::None,
                    to: AlignTarget::Selection,
                }),
                Intent::Group,
                Intent::Arrange(ZOrder::SendToBack),
                Intent::ShowDialog(xarast_app::Dialog::Align),
            ];
            (prelude, at)
        }
        (K::Paste, Some((_, node))) => (
            vec![
                Intent::Select {
                    nodes: vec![node],
                    mode: xarast_app::SelectMode::Replace,
                },
                Intent::Copy,
                Intent::Paste { in_place: false },
                // A frame for the shell to read the clipboard back.
                Intent::SelectNone,
            ],
            centre,
        ),
        (K::Rect, _) => (
            vec![Intent::ChooseTool(xarast_app::ToolId::Rectangle)],
            (centre.0 - 120.0, centre.1 - 80.0),
        ),
        (K::Ellipse, _) => (
            vec![Intent::ChooseTool(xarast_app::ToolId::Ellipse)],
            (centre.0 - 120.0, centre.1 - 80.0),
        ),
        (K::Nodes, _) => match path_node_target(s) {
            Some((at, node)) => (
                vec![
                    Intent::Select {
                        nodes: vec![node],
                        mode: xarast_app::SelectMode::Replace,
                    },
                    Intent::ChooseTool(xarast_app::ToolId::ShapeEditor),
                ],
                at,
            ),
            None => (
                vec![Intent::ChooseTool(xarast_app::ToolId::ShapeEditor)],
                centre,
            ),
        },
        (K::Pen, _) => {
            let mut prelude = vec![Intent::ChooseTool(xarast_app::ToolId::Pen)];
            prelude.extend(click((centre.0 - 160.0, centre.1 + 60.0), 0));
            (prelude, (centre.0 - 40.0, centre.1 - 40.0))
        }
        (K::Freehand, _) => (
            vec![Intent::ChooseTool(xarast_app::ToolId::Freehand)],
            (centre.0 - 150.0, centre.1),
        ),
        (_, Some((at, _))) => (Vec::new(), at),
        (_, None) => (Vec::new(), centre),
    }
}

/// Where the node probe presses: the path node nearest the canvas
/// centre, in canvas pixels, and its path.
fn path_node_target(s: &Session) -> Option<((f64, f64), xarast_doc::NodeId)> {
    let size = s.viewport.size();
    let (cx, cy) = (f64::from(size.width) / 2.0, f64::from(size.height) / 2.0);
    xarast_app::edit::selectable_objects(&s.doc)
        .filter_map(|n| Some((n, xarast_app::node_edit::path_of(&s.doc, n)?)))
        .flat_map(|(n, p)| {
            let e = xarast_geom::EditPath::from_path(p);
            e.node_refs()
                .filter_map(|r| e.node(r).map(|x| x.at))
                .map(|at| (s.viewport.doc_to_device(at), n))
                .collect::<Vec<_>>()
        })
        .filter(|(d, _)| {
            d.x > 40.0
                && d.y > 40.0
                && d.x < f64::from(size.width) - 40.0
                && d.y < f64::from(size.height) - 40.0
        })
        .min_by(|(a, _), (b, _)| {
            (a.x - cx)
                .hypot(a.y - cy)
                .total_cmp(&(b.x - cx).hypot(b.y - cy))
        })
        .map(|(d, n)| ((d.x, d.y), n))
}

/// Where the drag probe presses: the centre of the visible selectable
/// object nearest the canvas centre, in canvas pixels, and the object.
fn drag_target(s: &Session) -> Option<((f64, f64), xarast_doc::NodeId)> {
    let size = s.viewport.size();
    let (cx, cy) = (f64::from(size.width) / 2.0, f64::from(size.height) / 2.0);
    xarast_app::edit::selectable_objects(&s.doc)
        .filter_map(|n| {
            let b = xarast_app::viewport::nodes_rect(&s.doc, [n]);
            if b.is_empty() {
                return None;
            }
            let d = s.viewport.doc_to_device(b.centre());
            let inside = d.x > 0.0
                && d.y > 0.0
                && d.x < f64::from(size.width)
                && d.y < f64::from(size.height);
            inside.then_some(((d.x, d.y), n))
        })
        .min_by(|(a, _), (b, _)| {
            let da = (a.0 - cx).hypot(a.1 - cy);
            let db = (b.0 - cx).hypot(b.1 - cy);
            da.total_cmp(&db)
        })
}

/// The tool's overlay as the interface draws it.
fn overlay_items(s: &Session) -> Vec<xarast_ui::OverlayItem> {
    use xarast_app::{HandleShape, OverlayShape};
    use xarast_ui::{HandleKind, OverlayItem};
    let mut out = Vec::new();
    for o in s.overlay() {
        match o {
            OverlayShape::Handle { at, shape } => out.push(OverlayItem::Handle {
                x: at.x,
                y: at.y,
                kind: match shape {
                    HandleShape::Bounds => HandleKind::Bounds,
                    HandleShape::Rotate => HandleKind::Rotate,
                    HandleShape::Skew => HandleKind::Skew,
                    HandleShape::Centre => HandleKind::Centre,
                    HandleShape::Node | HandleShape::NodeSelected => HandleKind::Node,
                    HandleShape::Control => HandleKind::Control,
                    HandleShape::Radius => HandleKind::Radius,
                    HandleShape::Snap => HandleKind::Snap,
                    HandleShape::FillBlob | HandleShape::FillBlobSelected => HandleKind::FillBlob,
                    HandleShape::FillCentre | HandleShape::FillCentreSelected => {
                        HandleKind::FillCentre
                    }
                    HandleShape::FillStop | HandleShape::FillStopSelected => HandleKind::Fill,
                },
                active: matches!(
                    shape,
                    HandleShape::NodeSelected
                        | HandleShape::FillBlobSelected
                        | HandleShape::FillCentreSelected
                        | HandleShape::FillStopSelected
                ),
            }),
            OverlayShape::Arrow { from, to } => out.push(OverlayItem::Arrow {
                from: (from.x, from.y),
                to: (to.x, to.y),
            }),
            OverlayShape::Rect { rect, dashed } => out.push(OverlayItem::Rect {
                bounds: (rect.lo.x, rect.hi.y, rect.hi.x, rect.lo.y),
                dashed,
            }),
            OverlayShape::Polyline {
                points,
                closed,
                dashed,
            } => {
                let n = points.len();
                let segments = if closed { n } else { n.saturating_sub(1) };
                for i in 0..segments {
                    let (a, b) = (points[i], points[(i + 1) % n]);
                    out.push(OverlayItem::Line {
                        from: (a.x, a.y),
                        to: (b.x, b.y),
                        dashed,
                    });
                }
            }
            OverlayShape::Caret {
                from,
                to,
                primary,
                moved,
            } => out.push(OverlayItem::Caret {
                from: (from.x, from.y),
                to: (to.x, to.y),
                primary,
                moved,
            }),
            OverlayShape::Highlight { corners } => out.push(OverlayItem::Highlight {
                corners: corners.map(|c| (c.x, c.y)),
            }),
        }
    }
    out
}

/// The pointer shape for a tool's request.
const fn tool_cursor(c: xarast_app::CursorKind) -> CursorShape {
    use xarast_app::CursorKind as K;
    match c {
        K::Default => CursorShape::Default,
        K::Move => CursorShape::Move,
        K::Crosshair => CursorShape::Crosshair,
        K::Resize => CursorShape::ResizeNwSe,
        K::Rotate => CursorShape::Grabbing,
        K::Grab => CursorShape::Grab,
        K::Grabbing => CursorShape::Grabbing,
        K::ZoomIn => CursorShape::ZoomIn,
        K::NotAllowed => CursorShape::NotAllowed,
        K::Text => CursorShape::Text,
        K::Pointer => CursorShape::Hand,
    }
}

fn points(p: PhysicalPos, ppp: f32) -> egui::Pos2 {
    egui::pos2(p.x as f32 / ppp, p.y as f32 / ppp)
}

/// Projects a session for the interface.
///
/// The viewport stays the only thing that decides which way up a document
/// is (`app-core.md` §4); the interface is told, not asked. The projection
/// copies the viewport's scale, the device position of the document
/// origin and its `y`-up orientation into [`ViewTransform`], so the page
/// edge, the rulers, the grid and the pointer read-out land on exactly the
/// pixels the renderer used and read document `y` the right way up.
fn document_view(
    s: &Session,
    ppp: f64,
    keys: &mut Vec<xarast_doc::NodeId>,
    guide_keys: &mut Vec<xarast_doc::NodeId>,
) -> DocumentView {
    use xarast_geom::Mp;
    let vp = &s.viewport;
    let origin = vp.doc_to_device_f64(xarast_app::DocPointF::new(0.0, 0.0));
    let view = ViewTransform {
        // Logical points per document point.
        zoom: vp.scale() * f64::from(Mp::PER_PT) / ppp,
        origin_x: origin.x / ppp,
        origin_y: origin.y / ppp,
        y_up: true,
    };
    let page = xarast_app::viewport::page_rect(&s.doc);
    let doc = &s.doc;
    let spread = doc.active_spread();
    let mut layers = Vec::new();
    let mut active = None;
    for child in doc.tree.children(spread) {
        if let Some(xarast_doc::NodeKind::Layer(l)) = doc.tree.kind(child) {
            let key = LayerKey(keys.len() as u64);
            keys.push(child);
            if l.active {
                active = Some(key);
            }
            layers.push(LayerInfo {
                key,
                name: l.name.to_string(),
                visible: l.visible,
                locked: l.locked,
                printable: l.printable,
                object_count: doc.tree.children(child).count(),
            });
        }
    }
    let title = s.path.as_ref().and_then(|p| p.file_name()).map_or_else(
        || "Untitled".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let g = xarast_app::snap::grid_of(doc);
    let grid = xarast_ui::grid::GridSettings {
        visible: g.visible,
        spacing: g.spacing,
        subdivisions: g.subdivisions.max(1),
        origin: (g.origin.x, g.origin.y),
    };
    let mut guides = Vec::new();
    for gl in xarast_app::snap::guidelines(doc) {
        guide_keys.push(gl.node);
        guides.push(if gl.horizontal {
            xarast_ui::guides::Guide::horizontal(gl.position)
        } else {
            xarast_ui::guides::Guide::vertical(gl.position)
        });
    }
    DocumentView {
        title,
        // Left, top, right, bottom: under a y-up view the top is `hi.y`.
        page: (page.lo.x, page.hi.y, page.hi.x, page.lo.y),
        layers,
        active_layer: active,
        view,
        grid,
        guides,
        snap_to_grid: s.edit.snap.grid,
        show_guides: xarast_app::snap::guides_visible(doc),
        ..DocumentView::default()
    }
}

/// A panic on the interface thread unwinds through the event loop and
/// drops the viewer on its way out: save what can be saved, as a signal
/// would (XARA-US-0064 F3). The crash report was written by the panic hook
/// before the unwinding began.
impl Drop for Viewer {
    fn drop(&mut self) {
        if std::thread::panicking() && !self.app.docs.is_empty() {
            let n = self.app.emergency_shutdown();
            eprintln!("xarast: autosaved {n} document(s) after a panic");
        }
    }
}

impl ShellApp for Viewer {
    fn on_event(&mut self, event: ShellEvent, ctx: &mut ShellCtx<'_>) {
        self.ppp = ctx.scale().pixels_per_point();
        self.input.push(&event, self.ppp, Some(ctx.clipboard()));
        // Anything egui was told about needs an interface frame to act on.
        let for_egui = !self.input.is_empty();
        if self.handle(&event) || for_egui {
            ctx.request_redraw();
        }
        self.perform_requests(ctx);
        self.housekeeping(ctx);
    }

    fn handles_close(&self) -> bool {
        true
    }

    fn on_frame(&mut self, ctx: &mut ShellCtx<'_>) -> FrameRequest {
        xarast_app::crash::force_panic_point("ui");
        if !self.to_open.is_empty() {
            self.open_queued();
        }
        self.housekeeping(ctx);

        self.drive_probe(ctx);

        // A drag held at the canvas edge scrolls one step per frame.
        let autoscroll = self.app.active().is_some_and(Session::wants_autoscroll);
        if autoscroll {
            self.apply(vec![Intent::AutoScroll]);
        }

        let step = self.ui_step(ctx.scale(), ctx.surface_size());
        ctx.show_ui(step.frame);
        self.platform_output(step.output, ctx);
        let repaint = step.repaint;
        self.perform_requests(ctx);
        // The title follows the modified marker too, which any edit, undo
        // or save can change: compare rather than track.
        let title = self.window_title();
        if std::mem::take(&mut self.title_stale) || title != self.title {
            self.title = title;
            ctx.set_title(&self.title);
        }

        if let Some(region) = step.region.filter(|r| r.width > 0 && r.height > 0)
            && let Some(origin) = self.place_canvas(region, ctx.scale())
        {
            ctx.move_canvas(origin);
        }

        self.submit_render(ctx);

        if let Some(canvas) = self.render.as_mut()
            && let Some(frame) = canvas.take_latest()
        {
            let active = self.app.active().map(|s| s.id);
            if Some(frame.doc) == active && frame.generation > self.shown {
                self.shown = frame.generation;
                if let Some(e) = &frame.error {
                    self.message = Some(e.to_string());
                }
                tracing::debug!(
                    generation = frame.generation,
                    total_us = frame.timings.total_us(),
                    quality = ?frame.view.quality,
                    reuse = ?frame.reuse,
                    "canvas frame"
                );
                let c = self.adapter.canvas();
                let settled = frame.view.viewport.width() == c.width
                    && frame.view.viewport.height() == c.height
                    && frame.exact
                    && !self.render_stale
                    && canvas.is_settled();
                if let Some(p) = self.probe.as_mut() {
                    p.frames_delivered += 1;
                }
                ctx.show_tiled_frame(TiledFrame {
                    surface: frame.surface,
                    transform: frame.view.transform,
                    content: (frame.doc.0, frame.scene_epoch),
                    generation: frame.generation,
                    base: frame.base,
                    covered: frame.covered,
                    fresh: frame.fresh,
                });
                if settled {
                    self.settled_once = true;
                    // With a probe, the capture is of the view it leaves behind.
                    if self.probe.is_none()
                        && let Some(path) = self.screenshot.take()
                    {
                        ctx.capture(path, true);
                    }
                }
            }
        }
        self.show_view(ctx);
        if let Some(r) = ctx.renderer() {
            let label = r.to_string();
            if label != self.renderer_label {
                tracing::info!(renderer = %label, reason = ?r.reason, "canvas renderer");
                self.renderer_label = label;
            }
        }

        // Nothing to render (no document, or it failed to open): capture
        // what there is rather than wait forever — but not the very first
        // frame. On Wayland the compositor's fractional scale arrives after
        // the first frame (measured at 1.25 on GNOME 46: the first frame
        // laid the interface out at 1.0 in a 1.25 surface), so let a few
        // frames and a scale change go by first.
        if self.app.active().is_none() && self.to_open.is_empty() && self.screenshot.is_some() {
            self.empty_frames += 1;
            if self.empty_frames < EMPTY_CAPTURE_FRAMES {
                return FrameRequest::RedrawAfter(Duration::from_millis(50));
            }
            if let Some(path) = self.screenshot.take() {
                ctx.capture(path, true);
            }
        }

        // Wake for the owed Final even when nothing else is animating.
        // The owed Final, or the next autosave, whichever is sooner.
        let due = match (self.final_due, self.autosave_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
        .map(|t| t.saturating_duration_since(Instant::now()));
        let repaint = match (repaint, due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if self.probe.is_some() && self.settled_once {
            return FrameRequest::Redraw;
        }
        if autoscroll {
            return FrameRequest::RedrawAfter(Duration::from_millis(16));
        }
        match repaint {
            Some(d) if d.is_zero() => FrameRequest::Redraw,
            Some(d) if d < Duration::from_secs(3600) => FrameRequest::RedrawAfter(d),
            // The render thread wakes the loop itself when a frame lands.
            _ => FrameRequest::Idle,
        }
    }

    fn on_exit(&mut self, _ctx: &mut ShellCtx<'_>) {
        if let Some(mut canvas) = self.render.take() {
            canvas.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::{PortalEvent, PortalRequestId};
    use xarast_app::{DeviceSize, DocumentId};

    fn session() -> Session {
        let mut s = Session::new_empty(DocumentId(1));
        s.apply(Intent::Resize(DeviceSize::new(900, 700))).unwrap();
        s.apply(Intent::SetDpi(120.0)).unwrap();
        s.apply(Intent::ZoomTo(xarast_app::ZoomTarget::Page))
            .unwrap();
        s.apply(Intent::Pan { dx: 13.0, dy: -7.0 }).unwrap();
        s
    }

    #[test]
    fn the_interface_page_edge_lands_on_the_rendered_page() {
        // The interface draws the page edge from the projection; the render
        // thread fills the page from the viewport. They must agree to a
        // pixel at a fractional scale, or the edge floats off the page.
        let s = session();
        let ppp = 1.25;
        let mut keys = Vec::new();
        let dv = document_view(&s, ppp, &mut keys, &mut Vec::new());
        let rendered = xarast_app::viewport::device_rect_of(
            &s.viewport,
            xarast_app::viewport::page_rect(&s.doc),
        );
        let (l, t, r, b) = dv.page;
        let edge = [
            dv.view.doc_to_view_x(l) * ppp,
            dv.view.doc_to_view_y(t) * ppp,
            dv.view.doc_to_view_x(r) * ppp,
            dv.view.doc_to_view_y(b) * ppp,
        ];
        let want = [rendered.x0, rendered.y0, rendered.x1, rendered.y1].map(f64::from);
        for (e, w) in edge.iter().zip(want) {
            assert!((e - w).abs() <= 1.0, "edge {edge:?} vs rendered {want:?}");
        }
        assert!(edge[1] < edge[3], "top above bottom: {edge:?}");
    }

    #[test]
    fn the_vertical_ruler_reads_document_y_over_the_page() {
        // XARA-T-0026: the ruler used to read negative because the
        // projection negated y. Every tick drawn over the rendered page must
        // now carry a value inside the page's own y range, larger nearer
        // the top, and the pointer read-out must agree with the renderer.
        let s = session();
        let ppp = 1.25;
        let mut keys = Vec::new();
        let dv = document_view(&s, ppp, &mut keys, &mut Vec::new());
        let page = xarast_app::viewport::page_rect(&s.doc);
        let rendered = xarast_app::viewport::device_rect_of(&s.viewport, page);
        let (top, bottom) = (f64::from(rendered.y0) / ppp, f64::from(rendered.y1) / ppp);
        let ticks = xarast_ui::rulers::ticks(
            xarast_ui::Axis::Vertical,
            xarast_ui::Unit::Point,
            &dv.view,
            f64::from(s.viewport.size().height) / ppp,
        );
        let over_page: Vec<_> = ticks
            .iter()
            .filter(|t| t.position > top + 1.0 && t.position < bottom - 1.0)
            .collect();
        assert!(!over_page.is_empty());
        for t in &over_page {
            assert!(
                t.value >= page.lo.y && t.value <= page.hi.y,
                "tick {t:?} outside the page's y range {:?}..{:?}",
                page.lo.y,
                page.hi.y
            );
        }
        assert!(over_page.windows(2).all(|w| w[0].value > w[1].value));
        // The top edge of the rendered page reads as the page's top.
        let read = dv.view.view_to_doc_y(top);
        let tolerance = xarast_geom::Mp::from_pt(1.0 / dv.view.zoom).raw();
        assert!(
            (read.raw() - page.hi.y.raw()).abs() <= tolerance,
            "{read:?} vs {:?}",
            page.hi.y
        );
    }

    #[test]
    fn layers_are_listed_with_keys_that_map_back_to_nodes() {
        let s = session();
        let mut keys = Vec::new();
        let dv = document_view(&s, 1.0, &mut keys, &mut Vec::new());
        assert_eq!(dv.layers.len(), keys.len());
        assert!(!keys.is_empty(), "an empty document still has a layer");
        let mut v = Viewer::new(Vec::new());
        v.layer_keys = keys.clone();
        let intent = v.ui_intent(
            UiCommand::SetLayerVisible {
                layer: dv.layers[0].key,
                visible: false,
            },
            1.0,
        );
        assert_eq!(
            intent,
            Some(Intent::SetLayerVisible {
                layer: keys[0],
                visible: false
            })
        );
    }

    #[test]
    fn interface_navigation_is_scaled_from_points_to_pixels() {
        let v = Viewer::new(Vec::new());
        assert_eq!(
            v.ui_intent(UiCommand::Pan { dx: 2.0, dy: -4.0 }, 1.5),
            Some(Intent::Pan { dx: 3.0, dy: -6.0 })
        );
        assert_eq!(
            v.ui_intent(
                UiCommand::ZoomTo(xarast_ui::model::ZoomTarget::Drawing),
                1.0
            ),
            Some(Intent::ZoomTo(xarast_app::ZoomTarget::Drawing))
        );
        assert_eq!(v.ui_intent(UiCommand::RequestRedraw, 1.0), None);
    }

    #[test]
    fn colour_editor_commands_become_intents_and_the_model_shows_the_editor() {
        let mut v = Viewer::new(Vec::new());
        let op = xarast_app::colour_editor::ColourEditorOp::Commit;
        assert_eq!(
            v.ui_intent(UiCommand::ColourEditor(op.clone()), 1.0),
            Some(Intent::ColourEditor(op))
        );
        assert!(v.ui_model(1.0).colour_editor.is_none(), "no document");
        v.app.new_document();
        let editor = v
            .ui_model(1.0)
            .colour_editor
            .expect("a document has an editor");
        assert_eq!(editor.title, "Fill for new objects");
    }

    #[test]
    fn photo_panel_commands_become_intents_and_the_model_shows_the_panel() {
        let mut v = Viewer::new(Vec::new());
        let op = xarast_app::photo_panel::PhotoPanelOp::Commit;
        assert_eq!(
            v.ui_intent(UiCommand::PhotoPanel(op.clone()), 1.0),
            Some(Intent::PhotoPanel(op))
        );
        assert!(v.ui_model(1.0).photo_panel.is_none(), "no document");
        v.app.new_document();
        let panel = v
            .ui_model(1.0)
            .photo_panel
            .expect("a document has a photo panel");
        assert!(panel.node.is_none(), "nothing selected");
    }

    /// The colour bar (XARA-US-0042): its drag positions become canvas
    /// points or "elsewhere", and the status line says what a drop does.
    #[test]
    fn colour_drags_resolve_against_the_canvas_and_speak_in_the_status_line() {
        use xarast_app::colour_bar::{ColourBarOp, ColourSource, DragPoint};
        let mut v = primed_viewer();
        assert!(v.ui_model(1.0).colour_bar.is_some());
        // Window points at a scale of 2: (60, 60) pt is (120, 120) px, 100
        // px into a canvas that starts at (20, 20).
        assert_eq!(
            v.ui_intent(
                UiCommand::ColourDragAt {
                    x: 60.0,
                    y: 60.0,
                    shift: true
                },
                2.0
            ),
            Some(Intent::ColourBar(ColourBarOp::DragTo(DragPoint::Canvas {
                at: xarast_app::DevicePoint::new(100.0, 100.0),
                shift: true
            })))
        );
        assert_eq!(
            v.ui_intent(
                UiCommand::ColourDragAt {
                    x: 5.0,
                    y: 5.0,
                    shift: false
                },
                1.0
            ),
            Some(Intent::ColourBar(ColourBarOp::DragTo(DragPoint::Elsewhere)))
        );
        assert_eq!(
            v.ui_intent(UiCommand::SetLine(None), 1.0),
            Some(Intent::ColourBar(ColourBarOp::Apply {
                source: ColourSource::NoColour,
                slot: xarast_app::colour_editor::PaintSlot::Stroke
            }))
        );
        v.apply(vec![Intent::ColourBar(ColourBarOp::DragBegin(
            ColourSource::NoColour,
        ))]);
        let m = v.ui_model(1.0);
        let status = m.status.message.expect("the drag speaks");
        assert!(status.contains("Nothing here"), "{status}");
        v.apply(vec![Intent::ColourBar(ColourBarOp::DragCancel)]);
        assert!(v.ui_model(1.0).status.message.is_none());
    }

    /// A viewer with one open document and a primed 800×600 canvas, as
    /// the first frame leaves it.
    fn primed_viewer() -> Viewer {
        let mut v = Viewer::new(Vec::new());
        v.app.new_document();
        let mut intents = Vec::new();
        v.adapter.prime(
            CanvasRegion::new(20, 20, 800, 600),
            crate::scale::ScaleFactor::new(1.0),
            &mut intents,
        );
        v.apply(intents);
        v.primed = true;
        v.render_stale = false;
        v.scene_stale = false;
        v
    }

    fn pointer(phase: crate::input::event::PointerPhase, x: f64, y: f64) -> ShellEvent {
        ShellEvent::Pointer(crate::input::event::PointerEvent {
            id: crate::input::event::PointerId(0),
            phase,
            position: crate::scale::PhysicalPos::new(x, y),
            source: crate::input::tablet::InputSource::Mouse,
            primary: true,
            modifiers: crate::input::keyboard::Modifiers::NONE,
        })
    }

    #[test]
    fn constrain_wheel_over_the_canvas_zooms_the_document_and_owes_a_frame() {
        use crate::input::event::{PointerPhase, ScrollUnit};
        let mut v = primed_viewer();
        let before = v.app.active().unwrap().viewport.zoom();
        v.handle(&ShellEvent::ModifiersChanged(
            crate::input::keyboard::Modifiers::NONE.with_ctrl(),
        ));
        let redraw = v.handle(&pointer(
            PointerPhase::Scroll {
                dx: 0.0,
                dy: 1.0,
                unit: ScrollUnit::Lines,
            },
            400.0,
            300.0,
        ));
        assert!(redraw);
        assert!(v.render_stale, "a zoom owes a canvas frame");
        assert!(!v.scene_stale, "a zoom does not rebuild the scene");
        let after = v.app.active().unwrap().viewport.zoom();
        assert!((after / before - std::f64::consts::SQRT_2).abs() < 1e-9);
    }

    #[test]
    fn a_middle_drag_pans_the_document() {
        use crate::input::event::{PointerButton, PointerPhase};
        let mut v = primed_viewer();
        let c0 = v.app.active().unwrap().viewport.centre();
        v.handle(&pointer(
            PointerPhase::Pressed(PointerButton::Middle),
            300.0,
            300.0,
        ));
        assert!(v.handle(&pointer(PointerPhase::Moved, 340.0, 280.0)));
        v.handle(&pointer(
            PointerPhase::Released(PointerButton::Middle),
            340.0,
            280.0,
        ));
        let c1 = v.app.active().unwrap().viewport.centre();
        // The drawing follows the pointer: right and up on screen moves the
        // centre left and down in document space (y up).
        assert!(c1.x < c0.x && c1.y < c0.y, "{c0:?} -> {c1:?}");
        assert!(v.render_stale);
    }

    #[test]
    fn a_dropped_file_is_queued_for_the_next_frame() {
        let mut v = primed_viewer();
        let dropped = ShellEvent::Drag(DragEvent::Dropped {
            paths: vec![PathBuf::from("/nonexistent/a.xar")],
            at: None,
        });
        assert!(v.handle(&dropped));
        assert_eq!(v.to_open.len(), 1);
        v.open_queued();
        assert!(
            v.message
                .as_deref()
                .is_some_and(|m| m.contains("Could not open"))
        );
    }

    #[test]
    fn view_keys_are_the_view_commands_shortcuts() {
        let map = command_shortcuts();
        let key = |k: Key, m: Modifiers| crate::input::keyboard::KeyEvent {
            key: k,
            location: crate::input::keyboard::KeyLocation::Standard,
            state: KeyState::Pressed,
            repeat: false,
            text: None,
            modifiers: m,
        };
        let none = Modifiers::NONE;
        let ctrl = none.with_ctrl();
        for (k, m, want) in [
            (Key::char('1'), none, Some(AppCommand::Zoom100)),
            (Key::char('0'), none, Some(AppCommand::FitPage)),
            (Key::Named(NamedKey::Home), none, Some(AppCommand::FitPage)),
            (Key::char('d'), none, Some(AppCommand::FitDrawing)),
            (Key::char('+'), none, Some(AppCommand::ZoomIn)),
            (Key::char('+'), none.with_shift(), Some(AppCommand::ZoomIn)),
            (Key::char('-'), none, Some(AppCommand::ZoomOut)),
            (Key::char('o'), ctrl, Some(AppCommand::Open)),
            (Key::char('w'), ctrl, Some(AppCommand::Close)),
            (Key::char('q'), ctrl, Some(AppCommand::Quit)),
            (Key::char('o'), none, None),
            (Key::char('D'), none.with_shift(), None),
            (Key::char('1'), ctrl, None),
        ] {
            assert_eq!(map.resolve(&key(k.clone(), m), false), want, "{k:?} {m:?}");
        }
    }

    // ---- The egui input shim, end to end (XARA-US-0002) ----------------
    //
    // These drive the real path with no window: ShellEvent → EguiInput and
    // IntentAdapter → egui frame → UiCommand/Intent → Session.

    const SIZE: (u32, u32) = (1280, 800);

    fn one() -> ScaleFactor {
        ScaleFactor::new(1.0)
    }

    /// Runs `n` interface frames and returns the last accessibility tree.
    fn ui_frames(v: &mut Viewer, n: usize) -> Option<egui::accesskit::TreeUpdate> {
        let mut tree = None;
        for _ in 0..n {
            let step = v.ui_step(one(), PhysicalSize::new(SIZE.0, SIZE.1));
            if let Some(r) = step.region.filter(|r| r.width > 0 && r.height > 0) {
                v.place_canvas(r, one());
            }
            if step.output.accesskit_update.is_some() {
                tree = step.output.accesskit_update;
            }
        }
        tree
    }

    /// A viewer with a document, laid out by real interface frames.
    fn live_viewer() -> Viewer {
        let mut v = Viewer::new(Vec::new());
        v.app.new_document();
        v.egui.enable_accesskit();
        ui_frames(&mut v, 3);
        v
    }

    /// Delivers events the way `on_event` does.
    fn send(v: &mut Viewer, events: &[ShellEvent]) {
        for e in events {
            v.input.push(e, 1.0, None);
            v.handle(e);
        }
    }

    fn ptr_mod(
        phase: crate::input::event::PointerPhase,
        x: f64,
        y: f64,
        modifiers: crate::input::keyboard::Modifiers,
    ) -> ShellEvent {
        let ShellEvent::Pointer(mut p) = pointer(phase, x, y) else {
            unreachable!()
        };
        p.modifiers = modifiers;
        ShellEvent::Pointer(p)
    }

    fn click(v: &mut Viewer, x: f64, y: f64) {
        use crate::input::event::{PointerButton, PointerPhase};
        send(v, &[pointer(PointerPhase::Moved, x, y)]);
        ui_frames(v, 1);
        send(
            v,
            &[pointer(PointerPhase::Pressed(PointerButton::Primary), x, y)],
        );
        ui_frames(v, 1);
        send(
            v,
            &[pointer(
                PointerPhase::Released(PointerButton::Primary),
                x,
                y,
            )],
        );
        ui_frames(v, 1);
    }

    fn type_key(v: &mut Viewer, key: Key, text: Option<&str>) {
        use crate::input::keyboard::{KeyEvent, KeyLocation, Modifiers};
        for state in [KeyState::Pressed, KeyState::Released] {
            send(
                v,
                &[ShellEvent::Key(KeyEvent {
                    key: key.clone(),
                    location: KeyLocation::Standard,
                    state,
                    repeat: false,
                    text: text
                        .filter(|_| state == KeyState::Pressed)
                        .map(str::to_owned),
                    modifiers: Modifiers::NONE,
                })],
            );
        }
    }

    /// The centre of the first accessible node whose name matches.
    fn centre_of(tree: &egui::accesskit::TreeUpdate, want: impl Fn(&str) -> bool) -> (f64, f64) {
        tree.nodes
            .iter()
            .find_map(|(_, n)| {
                let label = n.label()?;
                if !want(label) {
                    return None;
                }
                let b = n.bounds()?;
                Some(((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0))
            })
            .expect("the node is in the accessibility tree")
    }

    fn canvas_centre(v: &Viewer) -> (f64, f64) {
        let c = v.adapter.canvas();
        (
            f64::from(c.x) + f64::from(c.width) / 2.0,
            f64::from(c.y) + f64::from(c.height) / 2.0,
        )
    }

    fn zoom(v: &Viewer) -> f64 {
        v.app.active().unwrap().viewport.zoom()
    }

    #[test]
    fn one_ctrl_wheel_notch_over_the_canvas_zooms_exactly_one_step() {
        use crate::input::event::{PointerPhase, ScrollUnit};
        let mut v = live_viewer();
        let (x, y) = canvas_centre(&v);
        let ctrl = crate::input::keyboard::Modifiers::NONE.with_ctrl();
        send(
            &mut v,
            &[
                ShellEvent::ModifiersChanged(ctrl),
                ptr_mod(PointerPhase::Moved, x, y, ctrl),
            ],
        );
        ui_frames(&mut v, 2);
        let before = zoom(&v);
        send(
            &mut v,
            &[ptr_mod(
                PointerPhase::Scroll {
                    dx: 0.0,
                    dy: 1.0,
                    unit: ScrollUnit::Lines,
                },
                x,
                y,
                ctrl,
            )],
        );
        // egui spreads one wheel notch over about ten frames; run past it,
        // so a second, smoothed zoom would have had time to land.
        ui_frames(&mut v, 20);
        let ratio = zoom(&v) / before;
        assert!(
            (ratio - std::f64::consts::SQRT_2).abs() < 1e-9,
            "one notch zoomed by {ratio}, not √2"
        );
    }

    #[test]
    fn a_plain_wheel_notch_pans_exactly_once() {
        use crate::input::event::{PointerPhase, ScrollUnit};
        let mut v = live_viewer();
        let (x, y) = canvas_centre(&v);
        send(&mut v, &[pointer(PointerPhase::Moved, x, y)]);
        ui_frames(&mut v, 2);
        let before = v.app.active().unwrap().viewport.centre();
        send(
            &mut v,
            &[pointer(
                PointerPhase::Scroll {
                    dx: 0.0,
                    dy: 1.0,
                    unit: ScrollUnit::Lines,
                },
                x,
                y,
            )],
        );
        let after_adapter = v.app.active().unwrap().viewport.centre();
        ui_frames(&mut v, 20);
        let after_egui = v.app.active().unwrap().viewport.centre();
        assert_ne!(before, after_adapter, "the adapter pans");
        assert_eq!(after_adapter, after_egui, "and nothing pans a second time");
    }

    #[test]
    fn a_wheel_over_the_dock_does_not_move_the_document() {
        use crate::input::event::{PointerPhase, ScrollUnit};
        let mut v = live_viewer();
        let at = (f64::from(SIZE.0) - 40.0, 400.0);
        assert!(
            !v.adapter
                .canvas()
                .contains(crate::scale::PhysicalPos::new(at.0, at.1))
        );
        send(&mut v, &[pointer(PointerPhase::Moved, at.0, at.1)]);
        ui_frames(&mut v, 2);
        let before = v.app.active().unwrap().viewport.centre();
        send(
            &mut v,
            &[pointer(
                PointerPhase::Scroll {
                    dx: 0.0,
                    dy: 3.0,
                    unit: ScrollUnit::Lines,
                },
                at.0,
                at.1,
            )],
        );
        ui_frames(&mut v, 20);
        assert_eq!(before, v.app.active().unwrap().viewport.centre());
    }

    #[test]
    fn clicking_a_layer_toggle_in_the_panel_hides_the_layer() {
        let mut v = live_viewer();
        let tree = ui_frames(&mut v, 1).expect("accessibility is on");
        let (x, y) = centre_of(&tree, |l| l.starts_with("Hide layer"));
        click(&mut v, x, y);
        let layers = v.ui_model(1.0).document.unwrap().layers;
        assert!(
            layers.iter().any(|l| !l.visible),
            "a click on the toggle reached the document: {layers:?}"
        );
    }

    #[test]
    fn typing_in_a_rename_field_does_not_trigger_view_keys() {
        let mut v = live_viewer();
        let tree = ui_frames(&mut v, 1).expect("accessibility is on");
        let original = v.ui_model(1.0).document.unwrap().layers[0].name.clone();
        let (x, y) = centre_of(&tree, |l| l.starts_with(&format!("{original}, ")));
        // Double-click the name to rename it.
        click(&mut v, x, y);
        click(&mut v, x, y);
        ui_frames(&mut v, 2);
        assert!(v.text_input, "the rename field has the keyboard");

        let before = zoom(&v);
        type_key(&mut v, Key::char('1'), Some("1"));
        ui_frames(&mut v, 2);
        assert!(
            (zoom(&v) - before).abs() < 1e-12,
            "'1' typed into a text field zoomed the view"
        );
        type_key(&mut v, Key::Named(NamedKey::Enter), None);
        ui_frames(&mut v, 3);
        let renamed = v.ui_model(1.0).document.unwrap().layers[0].name.clone();
        assert!(
            renamed.contains('1') && renamed != original,
            "{original} -> {renamed}"
        );
        assert!(!v.text_input, "Enter gave the keyboard back");

        // And with no text field, the same key is a view key again.
        type_key(&mut v, Key::char('1'), Some("1"));
        assert!(
            (zoom(&v) - before).abs() > 1e-9 || (before - 1.0).abs() < 1e-9,
            "'1' outside a text field zooms to 100 %"
        );
    }

    // ---- Menus, shortcuts, the open dialog (XARA-US-0082) ---------------

    /// A small `.xar` the importer accepts, written to a scratch directory.
    fn tiny_xar(name: &str) -> PathBuf {
        let mut spread = Vec::new();
        for v in [600_000i32, 450_000, 0, 0] {
            spread.extend_from_slice(&v.to_le_bytes());
        }
        spread.push(2);
        let mut layer = vec![0x01 | 0x04 | 0x08];
        for u in "Layer 1".encode_utf16().chain([0]) {
            layer.extend_from_slice(&u.to_le_bytes());
        }
        let bytes = xarast_xar::synth::XarBuilder::new()
            .record(40, &[])
            .down()
            .record(41, &[])
            .down()
            .record(42, &[])
            .down()
            .record(45, &spread)
            .record(43, &[])
            .down()
            .record(48, &layer)
            .up()
            .up()
            .up()
            .up()
            .end_of_file()
            .finish();
        let dir = std::env::temp_dir().join(format!("xarast-viewer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// Runs `f` with a shell context whose portal is an offline service
    /// (every request fails at once, no D-Bus is touched) and returns
    /// whether `f` asked the shell to exit, and what the portal answered.
    fn with_ctx(f: impl FnOnce(&mut ShellCtx<'_>)) -> (bool, Vec<PortalEvent>) {
        let mut clipboard = crate::clipboard::NullClipboard::new("tests");
        with_clipboard(&mut clipboard, f)
    }

    /// [`with_ctx`] with the given clipboard.
    fn with_clipboard(
        clipboard: &mut dyn crate::clipboard::Clipboard,
        f: impl FnOnce(&mut ShellCtx<'_>),
    ) -> (bool, Vec<PortalEvent>) {
        let service = crate::portal::PortalService::offline("no portal in tests");
        let mut frame = crate::window::PendingFrame::default();
        let waker = crate::ShellWaker::none();
        let mut ctx = ShellCtx {
            window: None,
            scale: one(),
            size: PhysicalSize::new(SIZE.0, SIZE.1),
            clipboard,
            portal: service.handle(),
            capabilities: crate::display::PlatformCapabilities::HEADLESS,
            frame: &mut frame,
            waker: &waker,
            exit: false,
            renderer: None,
            last_present: None,
            parent_window: Some("wayland:test-handle"),
        };
        f(&mut ctx);
        let exit = ctx.exit;
        let mut answers = Vec::new();
        for _ in 0..200 {
            service.poll(&mut answers);
            if !answers.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        (exit, answers)
    }

    fn press(v: &mut Viewer, key: Key, modifiers: crate::input::keyboard::Modifiers) {
        use crate::input::keyboard::{KeyEvent, KeyLocation};
        for state in [KeyState::Pressed, KeyState::Released] {
            send(
                v,
                &[ShellEvent::Key(KeyEvent {
                    key: key.clone(),
                    location: KeyLocation::Standard,
                    state,
                    repeat: false,
                    text: None,
                    modifiers,
                })],
            );
        }
    }

    fn key_state(
        v: &mut Viewer,
        key: Key,
        modifiers: crate::input::keyboard::Modifiers,
        state: KeyState,
    ) {
        use crate::input::keyboard::{KeyEvent, KeyLocation};
        send(
            v,
            &[ShellEvent::Key(KeyEvent {
                key,
                location: KeyLocation::Standard,
                state,
                repeat: false,
                text: None,
                modifiers,
            })],
        );
    }

    #[test]
    fn held_switch_keys_change_the_tool_until_released() {
        let (mut v, _) = viewer_with_square();
        press(
            &mut v,
            Key::Named(NamedKey::Function(3)),
            Modifiers::NONE.with_shift(),
        );
        assert_eq!(tool(&v), xarast_app::ToolId::Rectangle);
        let alt = Modifiers::NONE.with_alt();
        for (key, mods, want) in [
            (
                Key::Named(NamedKey::Space),
                Modifiers::NONE,
                xarast_app::ToolId::Selector,
            ),
            (Key::char('s'), alt, xarast_app::ToolId::Selector),
            (Key::char('z'), alt, xarast_app::ToolId::Zoom),
            (Key::char('x'), alt, xarast_app::ToolId::Pan),
        ] {
            key_state(&mut v, key.clone(), mods, KeyState::Pressed);
            assert_eq!(tool(&v), want, "{key:?} held");
            // Alt let go before the letter: the switch still ends.
            key_state(&mut v, key.clone(), Modifiers::NONE, KeyState::Released);
            assert_eq!(tool(&v), xarast_app::ToolId::Rectangle, "{key:?} released");
        }
        // Focus loss while held restores too.
        key_state(&mut v, Key::char('z'), alt, KeyState::Pressed);
        send(&mut v, &[ShellEvent::Focused(false)]);
        assert_eq!(tool(&v), xarast_app::ToolId::Rectangle);
    }

    /// Clicks the first accessible node with this label, as a screen
    /// reader would.
    fn activate(v: &mut Viewer, label: &str) {
        let tree = ui_frames(v, 1).expect("accessibility is on");
        let target = tree
            .nodes
            .iter()
            .find_map(|(id, n)| (n.label() == Some(label)).then_some(*id))
            .unwrap_or_else(|| panic!("no node named {label:?}"));
        send(
            v,
            &[ShellEvent::AccessibilityAction(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Click,
                    target,
                    data: None,
                },
            )],
        );
        ui_frames(v, 2);
    }

    fn empty_viewer() -> Viewer {
        let mut v = Viewer::new(Vec::new());
        v.egui.enable_accesskit();
        ui_frames(&mut v, 3);
        v
    }

    #[test]
    fn the_menu_bar_is_in_the_first_frame_with_and_without_a_document() {
        for mut v in [empty_viewer(), live_viewer()] {
            let tree = ui_frames(&mut v, 1).expect("accessibility is on");
            for title in ["File", "View", "Help"] {
                let (_, y) = centre_of(&tree, |l| l == title);
                assert!(y < 40.0, "{title} is not at the top: {y}");
            }
        }
    }

    #[test]
    fn the_gallery_keys_and_the_window_menu_show_their_panes() {
        use crate::input::keyboard::Modifiers;
        use xarast_app::DockPane;
        let perform = |v: &mut Viewer| {
            let _ = with_ctx(|ctx| v.perform_requests(ctx));
            ui_frames(v, 2);
        };
        let mut v = live_viewer();
        // Window › Photo: the menu item, not the photo panel's tab.
        activate(&mut v, "Window");
        let tree = ui_frames(&mut v, 1).expect("accessibility is on");
        let target = tree
            .nodes
            .iter()
            .find_map(|(id, n)| {
                (n.role() == egui::accesskit::Role::MenuItem && n.label() == Some("Photo"))
                    .then_some(*id)
            })
            .expect("Window › Photo");
        send(
            &mut v,
            &[ShellEvent::AccessibilityAction(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Click,
                    target,
                    data: None,
                },
            )],
        );
        ui_frames(&mut v, 2);
        assert!(!v.workspace.is_pane_showing(DockPane::Photo));
        perform(&mut v);
        assert!(v.workspace.is_pane_showing(DockPane::Photo));
        assert!(!v.workspace.is_pane_showing(DockPane::BitmapGallery));

        for (key, pane) in [
            (11, DockPane::BitmapGallery),
            (9, DockPane::ColourGallery),
            (10, DockPane::Layers),
        ] {
            press(&mut v, Key::Named(NamedKey::Function(key)), Modifiers::NONE);
            perform(&mut v);
            assert!(v.workspace.is_pane_showing(pane), "F{key}");
        }
        assert!(!v.workspace.is_pane_showing(DockPane::Photo));

        // No document is needed to reach a pane.
        let mut v = empty_viewer();
        press(&mut v, Key::Named(NamedKey::Function(9)), Modifiers::NONE);
        assert_eq!(
            v.requests,
            [PlatformRequest::ShowDialog(xarast_app::Dialog::Pane(
                DockPane::ColourGallery
            ))]
        );
        perform(&mut v);
        assert!(v.workspace.is_pane_showing(DockPane::ColourGallery));
    }

    #[test]
    fn file_open_asks_the_platform_for_a_dialog_and_a_failure_is_shown() {
        let mut v = empty_viewer();
        activate(&mut v, "File");
        activate(&mut v, "Open…");
        assert_eq!(v.requests, [PlatformRequest::ShowOpenDialog]);

        let (exit, answers) = with_ctx(|ctx| v.perform_requests(ctx));
        assert!(!exit);
        assert!(v.open_dialog.is_some(), "a dialog is outstanding");
        assert!(v.requests.is_empty());
        // The offline portal answers at once with a reason.
        assert_eq!(answers.len(), 1, "{answers:?}");
        for a in answers {
            v.handle(&ShellEvent::Portal(a));
        }
        assert!(v.open_dialog.is_none());
        let message = v.message.clone().unwrap_or_default();
        assert!(
            message.starts_with("Could not show the file chooser"),
            "{message}"
        );
        assert!(
            v.app
                .diagnostics
                .entries()
                .iter()
                .any(|e| e.message == message),
            "the failure is in the problem list too"
        );
    }

    #[test]
    fn the_empty_state_open_button_asks_for_the_dialog_too() {
        let mut v = empty_viewer();
        let tree = ui_frames(&mut v, 1).expect("accessibility is on");
        // The empty state's button is the only "Open…" while no menu is open.
        let (x, y) = centre_of(&tree, |l| l == "Open…");
        click(&mut v, x, y);
        assert_eq!(v.requests, [PlatformRequest::ShowOpenDialog]);
    }

    #[test]
    fn a_chosen_file_replaces_the_document_and_retitles_the_window() {
        let mut v = live_viewer();
        v.open_dialog = Some(PortalRequestId(7));
        let path = tiny_xar("chosen.xar");
        // An answer to some other request is not ours.
        assert!(!v.portal_answer(&PortalEvent::Cancelled {
            request: PortalRequestId(3)
        }));
        v.handle(&ShellEvent::Portal(PortalEvent::FilesChosen {
            request: PortalRequestId(7),
            paths: vec![path.clone()],
        }));
        assert!(v.open_dialog.is_none());
        v.open_queued();
        assert_eq!(v.app.docs.len(), 1, "the document was replaced");
        assert_eq!(
            v.app.active().unwrap().path.as_deref(),
            Some(path.as_path())
        );
        assert!(v.title_stale && v.fit_pending && !v.primed);
        assert_eq!(v.window_title(), "chosen.xar — Xarast");
        assert_eq!(v.app.recent.paths(), [path]);
    }

    #[test]
    fn a_cancelled_dialog_changes_nothing() {
        let mut v = live_viewer();
        v.open_dialog = Some(PortalRequestId(1));
        v.handle(&ShellEvent::Portal(PortalEvent::Cancelled {
            request: PortalRequestId(1),
        }));
        assert!(v.open_dialog.is_none() && v.to_open.is_empty() && v.message.is_none());
        assert_eq!(v.app.docs.len(), 1);
    }

    #[test]
    fn only_one_dialog_is_asked_for_at_a_time() {
        let mut v = empty_viewer();
        v.open_dialog = Some(PortalRequestId(1));
        v.run_command(AppCommand::Open);
        let (_, answers) = with_ctx(|ctx| v.perform_requests(ctx));
        assert!(answers.is_empty(), "no second dialog: {answers:?}");
        assert_eq!(v.open_dialog, Some(PortalRequestId(1)));
    }

    #[test]
    fn a_file_that_fails_to_open_keeps_the_document_and_says_why() {
        let mut v = live_viewer();
        let before = v.app.active().unwrap().id;
        v.to_open.push(PathBuf::from("/nonexistent/broken.xar"));
        v.open_queued();
        assert_eq!(v.app.active().unwrap().id, before);
        let message = v.message.clone().unwrap_or_default();
        assert!(
            message.starts_with("Could not open /nonexistent/broken.xar"),
            "{message}"
        );
    }

    #[test]
    fn ctrl_o_asks_for_the_dialog_and_ctrl_q_quits() {
        let ctrl = crate::input::keyboard::Modifiers::NONE.with_ctrl();
        let mut v = empty_viewer();
        press(&mut v, Key::char('o'), ctrl);
        assert_eq!(v.requests, [PlatformRequest::ShowOpenDialog]);
        v.requests.clear();
        press(&mut v, Key::char('q'), ctrl);
        let (exit, _) = with_ctx(|ctx| v.perform_requests(ctx));
        assert!(exit, "Ctrl+Q ends the application");
    }

    #[test]
    fn ctrl_w_closes_the_document_and_the_empty_state_comes_back() {
        let ctrl = crate::input::keyboard::Modifiers::NONE.with_ctrl();
        let mut v = live_viewer();
        press(&mut v, Key::char('w'), ctrl);
        assert!(v.app.active().is_none());
        assert_eq!(v.window_title(), "Xarast");
        // The interface has no canvas any more: the empty state is back.
        let step = v.ui_step(one(), PhysicalSize::new(SIZE.0, SIZE.1));
        assert!(step.region.is_none());
        assert!(v.ui_model(1.0).document.is_none());
        // View shortcuts with nothing open do nothing, and do not panic.
        press(
            &mut v,
            Key::char('1'),
            crate::input::keyboard::Modifiers::NONE,
        );
        assert!(v.app.active().is_none());
    }

    #[test]
    fn view_menu_items_and_their_keys_do_the_same_thing() {
        let mut by_menu = live_viewer();
        activate(&mut by_menu, "View");
        activate(&mut by_menu, "Zoom in");
        let mut by_key = live_viewer();
        let before = zoom(&by_key);
        press(
            &mut by_key,
            Key::char('+'),
            crate::input::keyboard::Modifiers::NONE,
        );
        assert!((zoom(&by_key) / before - xarast_app::command::ZOOM_STEP).abs() < 1e-9);
        assert!((zoom(&by_menu) - zoom(&by_key)).abs() < 1e-9);
    }

    #[test]
    fn no_shortcut_fires_while_a_text_field_has_the_keyboard() {
        let ctrl = crate::input::keyboard::Modifiers::NONE.with_ctrl();
        let mut v = live_viewer();
        let tree = ui_frames(&mut v, 1).expect("accessibility is on");
        let original = v.ui_model(1.0).document.unwrap().layers[0].name.clone();
        let (x, y) = centre_of(&tree, |l| l.starts_with(&format!("{original}, ")));
        click(&mut v, x, y);
        click(&mut v, x, y);
        ui_frames(&mut v, 2);
        assert!(v.text_input, "the rename field has the keyboard");
        let before = zoom(&v);
        for (key, m) in [
            (Key::char('o'), ctrl),
            (Key::char('w'), ctrl),
            (Key::char('q'), ctrl),
            (Key::char('d'), crate::input::keyboard::Modifiers::NONE),
            (Key::char('+'), crate::input::keyboard::Modifiers::NONE),
        ] {
            press(&mut v, key, m);
        }
        assert!(v.requests.is_empty(), "{:?}", v.requests);
        assert!(v.app.active().is_some(), "Ctrl+W did not close");
        assert!((zoom(&v) - before).abs() < 1e-12);
    }

    #[test]
    fn open_recent_opens_the_file_and_clear_forgets_them() {
        let mut v = empty_viewer();
        let path = tiny_xar("recent.xar");
        v.app.recent.remember(&path);
        let label = xarast_ui::menus::recent_label(&path);
        activate(&mut v, &label);
        v.open_queued();
        assert_eq!(
            v.app.active().unwrap().path.as_deref(),
            Some(path.as_path())
        );
        activate(&mut v, "File");
        activate(&mut v, "Open Recent");
        activate(&mut v, "Clear Recent Files");
        assert!(v.app.recent.is_empty());
    }

    // ---- AccessKit (XARA-US-0003) ---------------------------------------

    #[test]
    fn an_assistive_technology_gets_a_tree_only_once_it_listens() {
        let mut v = Viewer::new(Vec::new());
        v.app.new_document();
        assert!(
            ui_frames(&mut v, 2).is_none(),
            "no tree while nobody listens"
        );
        v.handle(&ShellEvent::AccessibilityActivated);
        let tree = ui_frames(&mut v, 1).expect("a tree once activated");
        assert!(tree.tree.is_some(), "the first update is a full tree");
        let mut named = tree.clone();
        name_the_tree(&mut named, "a.xar — Xarast");
        let root = named.tree.as_ref().unwrap().root;
        let root_label = named
            .nodes
            .iter()
            .find(|(id, _)| *id == root)
            .and_then(|(_, n)| n.label());
        assert_eq!(root_label, Some("a.xar — Xarast"));
        let labels: Vec<_> = tree.nodes.iter().filter_map(|(_, n)| n.label()).collect();
        assert!(
            labels.iter().any(|l| l.starts_with("Hide layer")),
            "{labels:?}"
        );
        v.handle(&ShellEvent::AccessibilityDeactivated);
        assert!(ui_frames(&mut v, 2).is_none(), "and none after it leaves");
    }

    #[test]
    fn a_screen_reader_click_operates_a_panel_control() {
        let mut v = Viewer::new(Vec::new());
        v.app.new_document();
        v.handle(&ShellEvent::AccessibilityActivated);
        let tree = ui_frames(&mut v, 2).expect("a tree once activated");
        let target = tree
            .nodes
            .iter()
            .find_map(|(id, n)| {
                n.label()
                    .filter(|l| l.starts_with("Hide layer"))
                    .map(|_| *id)
            })
            .expect("the visibility toggle is published");
        send(
            &mut v,
            &[ShellEvent::AccessibilityAction(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Click,
                    target,
                    data: None,
                },
            )],
        );
        ui_frames(&mut v, 2);
        let layers = v.ui_model(1.0).document.unwrap().layers;
        assert!(layers.iter().any(|l| !l.visible), "{layers:?}");
    }

    // ---- Tools and undo, end to end (XARA-US-0029, XARA-US-0030) --------

    /// Adds one 100 pt square at (100 pt, 100 pt) to the active layer.
    #[derive(Debug)]
    struct AddSquare;

    impl xarast_doc::Command for AddSquare {
        fn label(&self) -> &'static str {
            "Fixture"
        }
        fn run(&self, tx: &mut xarast_doc::Tx<'_>) -> Result<(), xarast_doc::EditError> {
            let spread = tx.doc().active_spread();
            let layer = tx.doc().active_layer(spread).expect("a layer");
            let n = tx.create(xarast_doc::NodeKind::Shape(Box::new(
                xarast_doc::ShapeNode {
                    shape: xarast_doc::ShapeKind::Rect,
                    origin: xarast_geom::Point::raw(100_000, 100_000),
                    major: xarast_geom::Vector::raw(100_000, 0),
                    minor: xarast_geom::Vector::raw(0, 100_000),
                },
            )))?;
            tx.attach(n, layer, xarast_doc::Attach::LastChild)?;
            // Filled (with the default line colour, black), so a click on
            // its interior picks it: a transparent interior does not.
            let black = match xarast_doc::default_for(xarast_doc::AttrSlot::StrokeColour) {
                xarast_doc::AttrValue::StrokeColour(p) => p,
                _ => unreachable!("the stroke colour slot holds a stroke colour"),
            };
            let fill = tx.create(xarast_doc::NodeKind::Attr(Box::new(
                xarast_doc::AttrNode::new(xarast_doc::AttrValue::Fill(black)),
            )))?;
            tx.attach(fill, n, xarast_doc::Attach::LastChild)
        }
    }

    /// A live viewer whose document holds one square, the history empty.
    fn viewer_with_square() -> (Viewer, xarast_doc::NodeId) {
        let mut v = live_viewer();
        let s = v.app.active_mut().unwrap();
        s.dispatch(&AddSquare).expect("fixture");
        s.bus.history_mut().clear(&mut s.doc);
        let n = xarast_app::edit::selectable_objects(&s.doc).next().unwrap();
        ui_frames(&mut v, 1);
        (v, n)
    }

    /// A document point in window pixels.
    fn window_at(v: &Viewer, x: i32, y: i32) -> (f64, f64) {
        let s = v.app.active().unwrap();
        let d = s.viewport.doc_to_device(xarast_geom::Point::raw(x, y));
        let c = v.adapter.canvas();
        (f64::from(c.x) + d.x, f64::from(c.y) + d.y)
    }

    fn square_origin(v: &Viewer, n: xarast_doc::NodeId) -> xarast_geom::Point {
        match v.app.active().unwrap().doc.tree.kind(n) {
            Some(xarast_doc::NodeKind::Shape(s)) => s.origin,
            _ => panic!("not a shape"),
        }
    }

    fn tool(v: &Viewer) -> xarast_app::ToolId {
        v.app.active().unwrap().tools().current()
    }

    /// XARA-T-0220: with a fill handle selected the arrows nudge it (one
    /// undo step per run) instead of panning, and the status line speaks
    /// for the fill tool.
    #[test]
    fn arrows_nudge_a_selected_fill_handle_and_pan_otherwise() {
        use xarast_app::{PointerButton, PointerSample};
        let (mut v, n) = viewer_with_square();
        let s = v.app.active_mut().unwrap();
        s.apply(Intent::Select {
            nodes: vec![n],
            mode: xarast_app::SelectMode::Replace,
        })
        .unwrap();
        s.apply(Intent::ChooseTool(xarast_app::ToolId::Fill))
            .unwrap();
        let at = |s: &Session, x: i32| {
            s.viewport
                .doc_to_device(xarast_geom::Point::raw(x, 150_000))
        };
        let sample = |at, time_ms| PointerSample {
            at,
            pressure: None,
            time_ms,
        };
        let (a, b) = (at(s, 120_000), at(s, 180_000));
        s.apply(Intent::PointerMove(sample(a, 0))).unwrap();
        s.apply(Intent::PointerDown {
            button: PointerButton::Primary,
            sample: sample(a, 0),
        })
        .unwrap();
        s.apply(Intent::PointerMove(sample(b, 10))).unwrap();
        s.apply(Intent::PointerUp {
            button: PointerButton::Primary,
            sample: sample(b, 20),
        })
        .unwrap();
        assert_eq!(s.undo_label(), Some("Set Fill"));
        assert!(s.takes_nudge(), "the end handle is selected");
        // Hovering the end handle: the status line says what a drag does.
        s.apply(Intent::PointerMove(sample(b, 30))).unwrap();
        let status = v.ui_model(1.0).status.message.unwrap_or_default();
        assert!(status.contains("Drag to move"), "{status}");
        let len = v.app.active().unwrap().bus.history().len();
        let centre = v.app.active().unwrap().viewport.centre();
        for _ in 0..3 {
            press(&mut v, Key::Named(NamedKey::ArrowRight), Modifiers::NONE);
        }
        press(
            &mut v,
            Key::Named(NamedKey::ArrowUp),
            Modifiers::NONE.with_ctrl(),
        );
        let s = v.app.active().unwrap();
        assert_eq!(s.bus.history().len(), len + 1, "one step for the run");
        assert_eq!(s.undo_label(), Some("Move Fill Handle"));
        assert_eq!(s.viewport.centre(), centre, "the view did not pan");
        // With no handle selected the arrows pan again.
        v.apply(vec![Intent::Cancel]);
        assert!(!v.app.active().unwrap().takes_nudge());
        press(&mut v, Key::Named(NamedKey::ArrowRight), Modifiers::NONE);
        let s = v.app.active().unwrap();
        assert_eq!(s.bus.history().len(), len + 1);
        assert_ne!(s.viewport.centre(), centre, "the arrow panned");
    }

    /// XARA-T-0170, XARA-T-0186: the arrows nudge the selector's selection
    /// and the shape editor's points; Tab and Home go to the shape editor
    /// while it has a path, and Home fits the page otherwise; NumPad +
    /// drops a copy only during a drag.
    #[test]
    fn context_keys_go_to_the_tool_that_claims_them() {
        use crate::input::keyboard::{KeyEvent, KeyLocation};
        let (mut v, n) = viewer_with_square();
        let s = v.app.active_mut().unwrap();
        s.apply(Intent::Select {
            nodes: vec![n],
            mode: xarast_app::SelectMode::Replace,
        })
        .unwrap();
        // The selector: an arrow nudges, and does not pan.
        let origin = square_origin(&v, n);
        let centre = v.app.active().unwrap().viewport.centre();
        press(&mut v, Key::Named(NamedKey::ArrowRight), Modifiers::NONE);
        let s = v.app.active().unwrap();
        assert_eq!(s.undo_label(), Some("Move"));
        assert_eq!(s.viewport.centre(), centre, "no pan");
        assert_eq!(
            square_origin(&v, n),
            origin + xarast_geom::Vector::raw(2835, 0)
        );
        // NumPad + outside a drag zooms in, as before.
        let zoom = v.app.active().unwrap().viewport.zoom();
        for state in [KeyState::Pressed, KeyState::Released] {
            send(
                &mut v,
                &[ShellEvent::Key(KeyEvent {
                    key: Key::char('+'),
                    location: KeyLocation::Numpad,
                    state,
                    repeat: false,
                    text: None,
                    modifiers: Modifiers::NONE,
                })],
            );
        }
        let s = v.app.active_mut().unwrap();
        assert!(s.viewport.zoom() > zoom, "NumPad + zoomed in");
        // The shape editor on the square made a path.
        s.apply(Intent::ConvertToShapes).unwrap();
        s.apply(Intent::ChooseTool(xarast_app::ToolId::ShapeEditor))
            .unwrap();
        let zoom = s.viewport.zoom();
        press(&mut v, Key::Named(NamedKey::Home), Modifiers::NONE);
        let s = v.app.active().unwrap();
        assert_eq!(s.viewport.zoom(), zoom, "Home did not fit the page");
        let first: Vec<u32> = s.edit.control_points(n).unwrap().iter().collect();
        assert_eq!(first.len(), 1, "Home selected the first point");
        press(&mut v, Key::Named(NamedKey::Tab), Modifiers::NONE);
        let s = v.app.active().unwrap();
        let next: Vec<u32> = s.edit.control_points(n).unwrap().iter().collect();
        assert_eq!(next.len(), 1);
        assert_ne!(next, first, "Tab moved the selection on");
        // Without a path Home fits the page again.
        v.apply(vec![Intent::Select {
            nodes: Vec::new(),
            mode: xarast_app::SelectMode::Replace,
        }]);
        press(&mut v, Key::Named(NamedKey::Home), Modifiers::NONE);
        assert_ne!(
            v.app.active().unwrap().viewport.zoom(),
            zoom,
            "Home fitted the page"
        );
    }

    #[test]
    fn the_palette_and_the_tool_keys_choose_the_same_tools() {
        let (mut v, _) = viewer_with_square();
        assert_eq!(tool(&v), xarast_app::ToolId::Selector);
        // Shift+F8: the push tool, by key.
        press(
            &mut v,
            Key::Named(NamedKey::Function(8)),
            Modifiers::NONE.with_shift(),
        );
        assert_eq!(tool(&v), xarast_app::ToolId::Pan);
        // The palette button, as a screen reader clicks it.
        activate(&mut v, "Rectangle");
        assert_eq!(tool(&v), xarast_app::ToolId::Rectangle);
        let infobar = v.ui_model(1.0).editing.unwrap().infobar;
        assert!(format!("{infobar:?}").contains("draw a rectangle"));
        activate(&mut v, "Pen");
        assert_eq!(tool(&v), xarast_app::ToolId::Pen);
        let infobar = v.ui_model(1.0).editing.unwrap().infobar;
        assert!(format!("{infobar:?}").contains("drag for a smooth point"));
        activate(&mut v, "Freehand");
        assert_eq!(tool(&v), xarast_app::ToolId::Freehand);
        let infobar = v.ui_model(1.0).editing.unwrap().infobar;
        assert!(format!("{infobar:?}").contains("rub it out"));
        press(&mut v, Key::Named(NamedKey::Function(2)), Modifiers::NONE);
        assert_eq!(tool(&v), xarast_app::ToolId::Selector);
        activate(&mut v, "Text");
        assert_eq!(tool(&v), xarast_app::ToolId::Text);
        press(&mut v, Key::Named(NamedKey::Function(2)), Modifiers::NONE);
        // A tool of a later phase is published, greyed out, and inert.
        if let Some(later) = xarast_app::ToolId::ALL
            .into_iter()
            .find(|t| !t.is_available())
        {
            activate(&mut v, later.label());
            assert_eq!(tool(&v), xarast_app::ToolId::Selector);
        }
    }

    #[test]
    fn a_text_caret_takes_the_navigation_and_character_keys() {
        let (mut v, _) = viewer_with_square();
        let at = |v: &Viewer| {
            v.app
                .active()
                .unwrap()
                .viewport
                .doc_to_device(xarast_geom::Point::raw(0, 0))
        };
        // F8: the text tool. A click on empty canvas puts up the caret of
        // a new story.
        press(&mut v, Key::Named(NamedKey::Function(8)), Modifiers::NONE);
        assert_eq!(tool(&v), xarast_app::ToolId::Text);
        let (x, y) = window_at(&v, 400_000, 500_000);
        click(&mut v, x, y);
        assert!(v.app.active().unwrap().text_editing());
        let view = at(&v);
        // The arrows no longer pan, and "d" is not Fit drawing: both are
        // the text's.
        press(&mut v, Key::Named(NamedKey::ArrowLeft), Modifiers::NONE);
        press(&mut v, Key::char('d'), Modifiers::NONE);
        press(&mut v, Key::Named(NamedKey::Home), Modifiers::NONE);
        assert_eq!(at(&v), view);
        assert_eq!(tool(&v), xarast_app::ToolId::Text);
        // Keys with text type it (T9.4.6): the first creates the story;
        // Enter breaks the paragraph, Backspace deletes, Tab is a tab;
        // Delete never reaches the object under the caret.
        xarast_app::fonts::set_shared(xarast_app::fonts::FontService::from_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../xarast-text/tests/fonts"),
        ));
        let type_key = |v: &mut Viewer, key: Key, text: Option<&str>| {
            use crate::input::keyboard::{KeyEvent, KeyLocation};
            send(
                v,
                &[ShellEvent::Key(KeyEvent {
                    key,
                    location: KeyLocation::Standard,
                    state: KeyState::Pressed,
                    repeat: false,
                    text: text.map(str::to_owned),
                    modifiers: Modifiers::NONE,
                })],
            );
        };
        type_key(&mut v, Key::char('H'), Some("H"));
        type_key(&mut v, Key::char('i'), Some("i"));
        type_key(&mut v, Key::char('x'), Some("x"));
        type_key(&mut v, Key::Named(NamedKey::Backspace), Some("\u{8}"));
        type_key(&mut v, Key::Named(NamedKey::Enter), Some("\r"));
        type_key(&mut v, Key::Named(NamedKey::Tab), Some("\t"));
        type_key(&mut v, Key::Named(NamedKey::Delete), Some("\u{7f}"));
        let s = v.app.active().unwrap();
        let story = s
            .edit
            .selection()
            .next()
            .expect("the new story is selected");
        let text = xarast_doc::StoryText::collect_simple(&s.doc.tree, &s.doc.defaults, story)
            .unwrap()
            .text;
        assert_eq!(text, "Hi\n\t\n");
        assert_eq!(at(&v), view, "Tab and Space are text, not tools");
        // Esc leaves the text; then the arrows pan again.
        press(&mut v, Key::Named(NamedKey::Escape), Modifiers::NONE);
        assert!(!v.app.active().unwrap().text_editing());
        press(&mut v, Key::Named(NamedKey::ArrowLeft), Modifiers::NONE);
        assert_ne!(at(&v), view);
    }

    /// A clipboard in memory: what the system clipboard would hold.
    #[derive(Debug, Default)]
    struct FakeClipboard {
        text: Option<String>,
        image: Option<crate::clipboard::ClipboardImage>,
    }

    impl crate::clipboard::Clipboard for FakeClipboard {
        fn text(&mut self) -> Result<String, crate::clipboard::ClipboardError> {
            self.text
                .clone()
                .ok_or(crate::clipboard::ClipboardError::Empty("text"))
        }

        fn set_text(&mut self, text: &str) -> Result<(), crate::clipboard::ClipboardError> {
            self.text = Some(text.to_owned());
            Ok(())
        }

        fn image(
            &mut self,
        ) -> Result<crate::clipboard::ClipboardImage, crate::clipboard::ClipboardError> {
            self.image
                .clone()
                .ok_or(crate::clipboard::ClipboardError::Empty("image"))
        }

        fn set_image(
            &mut self,
            _: &crate::clipboard::ClipboardImage,
        ) -> Result<(), crate::clipboard::ClipboardError> {
            Ok(())
        }

        fn persists_after_exit(&self) -> bool {
            false
        }
    }

    /// A viewer with the text tool's caret up at a pending point story,
    /// and the pinned fonts.
    fn viewer_with_text_caret() -> (Viewer, xarast_doc::NodeId) {
        let (mut v, square) = viewer_with_square();
        xarast_app::fonts::set_shared(xarast_app::fonts::FontService::from_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../xarast-text/tests/fonts"),
        ));
        press(&mut v, Key::Named(NamedKey::Function(8)), Modifiers::NONE);
        let (x, y) = window_at(&v, 400_000, 500_000);
        click(&mut v, x, y);
        assert!(v.app.active().unwrap().text_editing());
        (v, square)
    }

    fn type_text(v: &mut Viewer, text: &str) {
        use crate::input::keyboard::{KeyEvent, KeyLocation};
        for c in text.chars() {
            send(
                v,
                &[ShellEvent::Key(KeyEvent {
                    key: Key::char(c),
                    location: KeyLocation::Standard,
                    state: KeyState::Pressed,
                    repeat: false,
                    text: Some(c.to_string()),
                    modifiers: Modifiers::NONE,
                })],
            );
        }
    }

    /// The text of the story the caret is in.
    fn caret_story_text(v: &Viewer) -> String {
        let s = v.app.active().unwrap();
        let Some(xarast_app::text_tool::TextEditing::Story(sel)) = s.text_state() else {
            panic!("no caret in a story");
        };
        xarast_doc::StoryText::collect_simple(&s.doc.tree, &s.doc.defaults, sel.story)
            .unwrap()
            .text
    }

    /// A `winit` input-method event, through the real translation.
    fn ime(v: &mut Viewer, event: winit::event::Ime) {
        let e = ShellEvent::Ime(crate::input::translate::translate_ime(&event));
        send(v, &[e]);
    }

    #[test]
    fn the_input_method_follows_the_text_caret_and_composes_in_the_story() {
        use winit::event::Ime;
        let (v, _) = viewer_with_square();
        // No caret, no text field: the input method stays off.
        assert_eq!(v.ime_request(None), None);
        // A focused egui text field wants it under its cursor.
        let field = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(1.0, 14.0));
        assert_eq!(v.ime_request(Some(field)), Some([10, 20, 1, 14]));

        let (mut v, _) = viewer_with_text_caret();
        let pending = v.ime_request(None).expect("on while a caret is up");
        let (x, _) = window_at(&v, 400_000, 500_000);
        assert!(
            (f64::from(pending[0]) - x).abs() <= 1.0,
            "{pending:?} at {x}"
        );
        assert!(pending[3] > 5, "as tall as the caret");

        // A commit at the pending caret types (and creates) the story.
        ime(&mut v, Ime::Enabled);
        ime(&mut v, Ime::Preedit("h".into(), Some((1, 1))));
        ime(&mut v, Ime::Preedit(String::new(), None));
        ime(&mut v, Ime::Commit("Hi".into()));
        assert_eq!(
            caret_story_text(&v),
            "Hi
"
        );
        let at_end = v.ime_request(None).unwrap();

        // A composition shows in the story, not in the document, and the
        // candidate window moves with the caret inside it.
        let digest = v.app.active().unwrap().doc.canonical_digest();
        ime(&mut v, Ime::Preedit("abc".into(), Some((3, 3))));
        let s = v.app.active().unwrap();
        assert!(s.preview().text.is_some());
        assert_eq!(s.doc.canonical_digest(), digest);
        let composing = v.ime_request(None).unwrap();
        assert!(composing[0] > at_end[0], "{composing:?} vs {at_end:?}");
        // Keys during a composition are the input method's: no tool
        // switch, no zoom, no typing.
        type_text(&mut v, "d1");
        press(&mut v, Key::Named(NamedKey::ArrowLeft), Modifiers::NONE);
        assert_eq!(v.app.active().unwrap().doc.canonical_digest(), digest);
        assert_eq!(tool(&v), xarast_app::ToolId::Text);

        // Cancelled by the input method: nothing written.
        ime(&mut v, Ime::Disabled);
        assert!(v.app.active().unwrap().preview().text.is_none());
        assert_eq!(v.app.active().unwrap().doc.canonical_digest(), digest);
        // Committed: typed where the caret is.
        ime(&mut v, Ime::Enabled);
        ime(&mut v, Ime::Preedit("にほ".into(), Some((6, 6))));
        ime(&mut v, Ime::Commit("日本".into()));
        assert_eq!(
            caret_story_text(&v),
            "Hi日本
"
        );
        assert!(v.app.active().unwrap().preview().text.is_none());

        // Esc leaves the text: the input method goes off.
        press(&mut v, Key::Named(NamedKey::Escape), Modifiers::NONE);
        assert_eq!(v.ime_request(None), None);
    }

    #[test]
    fn ctrl_c_x_v_at_a_text_caret_move_text_not_objects() {
        let (mut v, square) = viewer_with_text_caret();
        let mut clipboard = FakeClipboard::default();
        type_text(&mut v, "Hello");
        // Shift+Home selects it; Ctrl+C copies the text.
        press(
            &mut v,
            Key::Named(NamedKey::Home),
            Modifiers::NONE.with_shift(),
        );
        press(&mut v, Key::char('c'), Modifiers::NONE.with_ctrl());
        with_clipboard(&mut clipboard, |ctx| v.perform_requests(ctx));
        assert_eq!(clipboard.text.as_deref(), Some("Hello"));
        // End, Ctrl+V: pasted from the system clipboard.
        press(&mut v, Key::Named(NamedKey::End), Modifiers::NONE);
        press(&mut v, Key::char('v'), Modifiers::NONE.with_ctrl());
        with_clipboard(&mut clipboard, |ctx| v.perform_requests(ctx));
        assert_eq!(
            caret_story_text(&v),
            "HelloHello
"
        );
        // Text another application copied is pasted as text.
        clipboard.text = Some(" and more".to_owned());
        press(&mut v, Key::char('v'), Modifiers::NONE.with_ctrl());
        with_clipboard(&mut clipboard, |ctx| v.perform_requests(ctx));
        assert_eq!(
            caret_story_text(&v),
            "HelloHello and more
"
        );
        // Ctrl+X cuts the selected text, never the objects.
        press(
            &mut v,
            Key::Named(NamedKey::Home),
            Modifiers::NONE.with_shift(),
        );
        press(&mut v, Key::char('x'), Modifiers::NONE.with_ctrl());
        with_clipboard(&mut clipboard, |ctx| v.perform_requests(ctx));
        assert_eq!(clipboard.text.as_deref(), Some("HelloHello and more"));
        // The story it emptied goes with it, in the same step (XARA-T-0237);
        // the caret waits where the story began.
        let s = v.app.active().unwrap();
        assert!(matches!(
            s.text_state(),
            Some(xarast_app::text_tool::TextEditing::Pending { .. })
        ));
        assert!(s.doc.tree.contains(square) && s.doc.tree.is_reachable(square));
        assert_eq!(s.undo_label(), Some("Cut"));
    }

    #[test]
    fn select_drag_undo_and_redo_in_the_whole_viewer() {
        use crate::input::event::{PointerButton, PointerPhase};
        let (mut v, n) = viewer_with_square();
        let before = square_origin(&v, n);

        let (x, y) = window_at(&v, 150_000, 150_000);
        click(&mut v, x, y);
        assert_eq!(
            v.app.active().unwrap().edit.selection().collect::<Vec<_>>(),
            vec![n]
        );
        // The selection's box and handles are drawn over the canvas.
        assert!(overlay_items(v.app.active().unwrap()).len() >= 9);

        // Drag it 60 px right, 30 px down, in small steps.
        send(
            &mut v,
            &[pointer(PointerPhase::Pressed(PointerButton::Primary), x, y)],
        );
        for i in 1..=30 {
            let t = f64::from(i);
            send(&mut v, &[pointer(PointerPhase::Moved, x + 2.0 * t, y + t)]);
        }
        ui_frames(&mut v, 1);
        assert_eq!(square_origin(&v, n), before, "nothing commits mid-drag");
        assert!(v.scene_stale, "the preview owes a scene");
        send(
            &mut v,
            &[pointer(
                PointerPhase::Released(PointerButton::Primary),
                x + 60.0,
                y + 30.0,
            )],
        );
        ui_frames(&mut v, 1);
        let moved = square_origin(&v, n);
        assert!(moved.x > before.x && moved.y < before.y, "{moved:?}");
        let editing = v.ui_model(1.0).editing.unwrap();
        assert_eq!(editing.undo.as_deref(), Some("Move"));

        // Edit › Undo Move, through the menu.
        activate(&mut v, "Edit");
        activate(&mut v, "Undo Move");
        assert_eq!(square_origin(&v, n), before);
        // Ctrl+Shift+Z redoes (the layout reports the capital).
        press(
            &mut v,
            Key::char('Z'),
            Modifiers::NONE.with_ctrl().with_shift(),
        );
        assert_eq!(square_origin(&v, n), moved);
        // Ctrl+Z undoes, Ctrl+Y redoes.
        press(&mut v, Key::char('z'), Modifiers::NONE.with_ctrl());
        assert_eq!(square_origin(&v, n), before);
        press(&mut v, Key::char('y'), Modifiers::NONE.with_ctrl());
        assert_eq!(square_origin(&v, n), moved);
    }

    #[test]
    fn escape_mid_drag_cancels_even_though_shortcuts_are_off_in_a_drag() {
        use crate::input::event::{PointerButton, PointerPhase};
        let (mut v, n) = viewer_with_square();
        let before = square_origin(&v, n);
        let (x, y) = window_at(&v, 150_000, 150_000);
        send(&mut v, &[pointer(PointerPhase::Moved, x, y)]);
        send(
            &mut v,
            &[pointer(PointerPhase::Pressed(PointerButton::Primary), x, y)],
        );
        send(&mut v, &[pointer(PointerPhase::Moved, x + 80.0, y)]);
        assert!(!v.app.active().unwrap().preview().is_empty());
        press(&mut v, Key::Named(NamedKey::Escape), Modifiers::NONE);
        assert!(v.app.active().unwrap().preview().is_empty());
        send(
            &mut v,
            &[pointer(
                PointerPhase::Released(PointerButton::Primary),
                x + 80.0,
                y,
            )],
        );
        assert_eq!(square_origin(&v, n), before);
        assert_eq!(v.app.active().unwrap().bus.history().len(), 0);
    }

    #[test]
    fn delete_removes_the_selection_and_undo_brings_it_back() {
        let (mut v, n) = viewer_with_square();
        let (x, y) = window_at(&v, 150_000, 150_000);
        click(&mut v, x, y);
        press(&mut v, Key::Named(NamedKey::Delete), Modifiers::NONE);
        assert!(!v.app.active().unwrap().doc.tree.is_reachable(n));
        press(&mut v, Key::char('z'), Modifiers::NONE.with_ctrl());
        assert!(v.app.active().unwrap().doc.tree.is_reachable(n));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("xarast-viewer-save-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn every_chord_of_the_command_table_reaches_its_command() {
        use crate::input::keyboard::{KeyEvent, KeyLocation};
        let map = command_shortcuts();
        for command in AppCommand::ALL {
            for chord in command.shortcuts() {
                let key = match chord.key {
                    ChordKey::Char(c) if chord.shift && c.is_ascii_alphabetic() => {
                        Key::char(c.to_ascii_uppercase())
                    }
                    other => chord_key(other),
                };
                let location = if matches!(chord.key, ChordKey::NumPad(_)) {
                    KeyLocation::Numpad
                } else {
                    KeyLocation::Standard
                };
                let mut modifiers = Modifiers::NONE;
                if chord.ctrl {
                    modifiers = modifiers.with_ctrl();
                }
                if chord.shift {
                    modifiers = modifiers.with_shift();
                }
                let event = KeyEvent {
                    key,
                    location,
                    state: KeyState::Pressed,
                    repeat: false,
                    text: None,
                    modifiers,
                };
                assert_eq!(
                    map.resolve(&event, false),
                    Some(command),
                    "{chord} does not run {command:?}"
                );
            }
        }
    }

    #[test]
    fn ctrl_s_asks_for_a_xarast_name_then_saves_and_clears_the_marker() {
        let dir = scratch("ctrl-s");
        let (mut v, _) = viewer_with_square();
        assert!(
            v.window_title().starts_with('\u{2022}'),
            "{}",
            v.window_title()
        );
        press(&mut v, Key::char('s'), Modifiers::NONE.with_ctrl());
        assert!(matches!(
            v.requests.as_slice(),
            [PlatformRequest::ShowSaveDialog { file_name, .. }] if file_name == "Untitled.xarast"
        ));
        // The offline portal fails at once: the status bar says so and
        // nothing waits for a name any more.
        let (exit, answers) = with_ctx(|ctx| v.perform_requests(ctx));
        assert!(!exit && v.save_dialog.is_some());
        for a in answers {
            v.handle(&ShellEvent::Portal(a));
        }
        assert!(v.save_dialog.is_none());
        let message = v.message.clone().unwrap_or_default();
        assert!(
            message.starts_with("Could not show the save dialog"),
            "{message}"
        );

        // A chosen name: saved off the interface thread, then applied.
        press(
            &mut v,
            Key::char('S'),
            Modifiers::NONE.with_ctrl().with_shift(),
        );
        assert!(matches!(
            v.requests.as_slice(),
            [PlatformRequest::ShowSaveDialog { .. }]
        ));
        v.requests.clear();
        v.save_dialog = Some(PortalRequestId(42));
        v.handle(&ShellEvent::Portal(PortalEvent::SaveChosen {
            request: PortalRequestId(42),
            path: dir.join("square"),
        }));
        assert!(v.app.is_saving());
        v.app.join_saves();
        let _ = with_ctx(|ctx| v.housekeeping(ctx));
        let message = v.message.clone().unwrap_or_default();
        assert!(message.starts_with("Saved square.xarast"), "{message}");
        assert_eq!(v.window_title(), "square.xarast — Xarast");
        assert!(dir.join("square.xarast").is_file());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ctrl_shift_e_exports_through_the_dialog_and_the_chooser() {
        let dir = scratch("export");
        let (mut v, _) = viewer_with_square();
        press(
            &mut v,
            Key::char('E'),
            Modifiers::NONE.with_ctrl().with_shift(),
        );
        let model = v.ui_model(1.0);
        let view = model.export_dialog.expect("the dialog is shown");
        assert_eq!(view.format, xarast_app::export_dialog::FormatId::Png);
        // The dialog's Export… asks for a file, filtered to the format.
        assert_eq!(
            v.ui_intent(
                UiCommand::ExportDialog(xarast_app::export_dialog::ExportDialogOp::Export),
                1.0
            ),
            Some(Intent::ExportDialog(
                xarast_app::export_dialog::ExportDialogOp::Export
            ))
        );
        v.apply(vec![Intent::ExportDialog(
            xarast_app::export_dialog::ExportDialogOp::Export,
        )]);
        assert!(matches!(
            v.requests.as_slice(),
            [PlatformRequest::ShowExportFileDialog { file_name, extensions, .. }]
                if file_name == "Untitled.png" && extensions.contains(&"png")
        ));
        v.requests.clear();
        v.export_dialog = Some(PortalRequestId(7));
        v.handle(&ShellEvent::Portal(PortalEvent::SaveChosen {
            request: PortalRequestId(7),
            path: dir.join("square"),
        }));
        assert!(v.export_dialog.is_none());
        v.app.wait_exports();
        assert!(dir.join("square.png").is_file());
        assert!(matches!(
            v.ui_model(1.0).export_dialog.map(|d| d.phase),
            Some(xarast_app::export_dialog::ExportPhase::Done { .. })
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn closing_the_window_asks_first_and_quits_after_discard() {
        let (mut v, _) = viewer_with_square();
        v.handle(&ShellEvent::CloseRequested);
        assert!(v.app.prompt().is_some());
        assert!(v.ui_model(1.0).prompt.is_some(), "the interface shows it");
        let (exit, _) = with_ctx(|ctx| v.perform_requests(ctx));
        assert!(!exit, "nothing quits while it asks");
        let intent = v
            .ui_intent(
                UiCommand::AnswerPrompt(xarast_app::PromptAnswer::Discard),
                1.0,
            )
            .unwrap();
        v.apply(vec![intent]);
        let (exit, _) = with_ctx(|ctx| v.perform_requests(ctx));
        assert!(exit);
        assert!(v.handles_close());
    }

    #[test]
    fn a_signal_autosaves_the_work_and_exits() {
        let dir = scratch("signal");
        let (v, _) = viewer_with_square();
        let watch = crate::signals::SignalWatch::inert();
        let mut v = v.with_signals(watch.clone());
        v.app = std::mem::take(&mut v.app)
            .with_autosave(dir.clone(), xarast_app::autosave::AutosavePolicy::default());
        let (exit, _) = with_ctx(|ctx| v.housekeeping(ctx));
        assert!(!exit);
        watch.simulate();
        let (exit, _) = with_ctx(|ctx| v.housekeeping(ctx));
        assert!(exit);
        assert!(v.app.docs.is_empty());
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(entries.len(), 1, "one autosave kept for recovery");
        assert!(entries[0].path().join("snapshot.xarast").is_file());
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- Crashes (XARA-US-0064) -----------------------------------------

    #[test]
    fn a_fatal_worker_panic_autosaves_the_work_and_exits() {
        let dir = scratch("fatal");
        let (v, _) = viewer_with_square();
        let fatal = xarast_app::crash::FatalWatch::new();
        let mut v = v.with_fatal_watch(fatal.clone());
        v.app = std::mem::take(&mut v.app)
            .with_autosave(dir.clone(), xarast_app::autosave::AutosavePolicy::default());
        let (exit, _) = with_ctx(|ctx| v.housekeeping(ctx));
        assert!(!exit);
        fatal.raise("xarast-render");
        let (exit, _) = with_ctx(|ctx| v.housekeeping(ctx));
        assert!(exit);
        assert!(v.app.docs.is_empty());
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(entries.len(), 1, "one autosave kept for recovery");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_panic_on_the_interface_thread_autosaves_while_unwinding() {
        let dir = scratch("unwind");
        let (mut v, _) = viewer_with_square();
        v.app = std::mem::take(&mut v.app)
            .with_autosave(dir.clone(), xarast_app::autosave::AutosavePolicy::default());
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _viewer = v;
            panic!("a bug in an interface frame");
        }));
        assert!(r.is_err());
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(entries.len(), 1, "the modified document was autosaved");
        assert!(entries[0].path().join("snapshot.xarast").is_file());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn choosing_safe_mode_after_a_crash_reaches_the_shell() {
        let check = xarast_app::crash::StartupCheck {
            crashed: 1,
            recent_crashes: 1,
            report: Some(PathBuf::from("/state/xarast/crashes/crash-1-2.toml")),
            safe_mode: xarast_app::crash::SafeMode::Offered,
        };
        let mut v = Viewer::new(Vec::new()).with_startup_check(&check);
        let _ = with_ctx(|ctx| v.housekeeping(ctx));
        let prompt = v.app.prompt().expect("the crash is reported").clone();
        assert!(prompt.offers(xarast_app::PromptAnswer::SafeMode));
        assert!(v.ui_model(1.0).prompt.is_some(), "the interface shows it");
        let intent = v
            .ui_intent(
                UiCommand::AnswerPrompt(xarast_app::PromptAnswer::SafeMode),
                1.0,
            )
            .unwrap();
        v.apply(vec![intent]);
        let mut asked = false;
        let (exit, _) = with_ctx(|ctx| {
            v.perform_requests(ctx);
            asked = ctx.frame.safe_mode;
        });
        assert!(!exit);
        assert!(asked, "the shell was asked to rebuild in safe mode");
        assert!(v.app.safe_mode());
        v.handle(&ShellEvent::GpuRebuilt(crate::gpu_errors::DeviceRebuilt {
            renderer: "CPU · llvmpipe (Vulkan), software".to_owned(),
            cause: crate::gpu_errors::RebuildCause::SafeMode,
        }));
        assert_eq!(
            v.message.as_deref(),
            Some("Safe mode: CPU · llvmpipe (Vulkan), software")
        );
    }

    // ---- Placing bitmaps by drop and paste (XARA-T-0272) ---------------

    /// The bitmap objects of the active document, in tree order.
    fn bitmaps(v: &Viewer) -> Vec<xarast_doc::BitmapNode> {
        let s = v.app.active().unwrap();
        s.doc
            .tree
            .preorder(s.doc.tree.root())
            .filter_map(|n| match s.doc.tree.kind(n) {
                Some(xarast_doc::NodeKind::Bitmap(b)) => Some((**b).clone()),
                _ => None,
            })
            .collect()
    }

    fn bitmap_centre(b: &xarast_doc::BitmapNode) -> (f64, f64) {
        let far = b.origin + b.major + b.minor;
        let (a, c) = (b.origin.to_f64(), far.to_f64());
        ((a.0 + c.0) / 2.0, (a.1 + c.1) / 2.0)
    }

    /// A PNG of `w × h` pixels on disk, in a directory of its own.
    fn png_file(tag: &str, w: u32, h: u32) -> (PathBuf, PathBuf) {
        let rgba = [0u8, 128, 255, 255].repeat((w * h) as usize);
        let img = xarast_app::place::image_from_rgba(w, h, &rgba).unwrap();
        let dir = std::env::temp_dir().join(format!("xarast-t0272-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dropped.png");
        std::fs::write(&path, &*img.resource.original.as_ref().unwrap().bytes).unwrap();
        (dir, path)
    }

    #[test]
    fn an_image_dropped_on_the_canvas_is_placed_at_the_drop_point() {
        let (mut v, _) = viewer_with_square();
        let (dir, png) = png_file("drop", 96, 48);
        let history = v.app.active().unwrap().bus.history().len();
        let (x, y) = window_at(&v, 300_000, 450_000);
        #[allow(clippy::cast_possible_truncation)]
        let at = crate::input::event::PhysicalPos2::new(x.round() as i32, y.round() as i32);
        let dropped = ShellEvent::Drag(DragEvent::Dropped {
            paths: vec![png.clone(), PathBuf::from("/nonexistent/other.xar")],
            at: Some(at),
        });
        assert!(v.handle(&dropped));
        // The image is placed, the document is queued to open as before.
        assert_eq!(v.to_open, vec![PathBuf::from("/nonexistent/other.xar")]);
        let s = v.app.active().unwrap();
        assert_eq!(s.bus.history().len(), history + 1);
        assert_eq!(s.undo_label(), Some("Import Bitmap"));
        let b = bitmaps(&v);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].major.dx, xarast_geom::Mp::new(72_000));
        let c = bitmap_centre(&b[0]);
        // One device pixel at the zoom in force.
        let px = 1.0 / v.app.active().unwrap().viewport.zoom() * 750.0 + 1.0;
        assert!(
            (c.0 - 300_000.0).abs() <= px && (c.1 - 450_000.0).abs() <= px,
            "{c:?} within {px}"
        );
        // Dropped outside the canvas: centred in the view instead.
        let outside = ShellEvent::Drag(DragEvent::Dropped {
            paths: vec![png],
            at: Some(crate::input::event::PhysicalPos2::new(1, 1)),
        });
        v.handle(&outside);
        let b = bitmaps(&v);
        assert_eq!(b.len(), 2);
        let view = v
            .app
            .active()
            .unwrap()
            .viewport
            .visible_doc_rect()
            .centre()
            .to_f64();
        let c = bitmap_centre(&b[1]);
        assert!((c.0 - view.0).abs() <= 1.0 && (c.1 - view.1).abs() <= 1.0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_dropped_file_that_is_not_an_image_leaves_the_document_alone() {
        let (mut v, _) = viewer_with_square();
        let dir = std::env::temp_dir().join(format!("xarast-t0272-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("fake.png");
        std::fs::write(&fake, b"GIF? no").unwrap();
        let digest = v.app.active().unwrap().doc.canonical_digest();
        v.handle(&ShellEvent::Drag(DragEvent::Dropped {
            paths: vec![fake],
            at: None,
        }));
        assert_eq!(v.app.active().unwrap().doc.canonical_digest(), digest);
        assert!(
            v.message
                .as_deref()
                .is_some_and(|m| m.contains("Could not place")),
            "{:?}",
            v.message
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ctrl_v_pastes_a_picture_from_the_clipboard_as_one_step() {
        let (mut v, _) = viewer_with_square();
        let mut clipboard = FakeClipboard {
            text: None,
            image: crate::clipboard::ClipboardImage::new(4, 2, [255u8, 0, 0, 255].repeat(8)),
        };
        let history = v.app.active().unwrap().bus.history().len();
        press(&mut v, Key::char('v'), Modifiers::NONE.with_ctrl());
        with_clipboard(&mut clipboard, |ctx| v.perform_requests(ctx));
        let s = v.app.active().unwrap();
        assert_eq!(s.bus.history().len(), history + 1);
        assert_eq!(s.undo_label(), Some("Paste"));
        let b = bitmaps(&v);
        assert_eq!(b.len(), 1);
        // 4 × 2 px at 96 dpi: 3 × 1.5 pt.
        assert_eq!(b[0].major.dx, xarast_geom::Mp::new(3_000));
        assert_eq!(b[0].minor.dy, xarast_geom::Mp::new(-1_500));
        let digest_after = v.app.active().unwrap().doc.canonical_digest();
        press(&mut v, Key::char('z'), Modifiers::NONE.with_ctrl());
        assert!(bitmaps(&v).is_empty(), "one undo removes it");
        press(
            &mut v,
            Key::char('z'),
            Modifiers::NONE.with_ctrl().with_shift(),
        );
        assert_eq!(v.app.active().unwrap().doc.canonical_digest(), digest_after);
    }

    #[test]
    fn our_own_copy_wins_over_a_picture_on_the_clipboard() {
        let (mut v, _) = viewer_with_square();
        let mut clipboard = FakeClipboard::default();
        press(&mut v, Key::char('a'), Modifiers::NONE.with_ctrl());
        press(&mut v, Key::char('c'), Modifiers::NONE.with_ctrl());
        with_clipboard(&mut clipboard, |ctx| v.perform_requests(ctx));
        assert!(clipboard.text.is_some());
        clipboard.image = crate::clipboard::ClipboardImage::new(1, 1, vec![0, 0, 0, 255]);
        press(&mut v, Key::char('v'), Modifiers::NONE.with_ctrl());
        with_clipboard(&mut clipboard, |ctx| v.perform_requests(ctx));
        assert!(
            bitmaps(&v).is_empty(),
            "the objects were pasted, not the picture"
        );
        assert_eq!(v.app.active().unwrap().undo_label(), Some("Paste"));
    }

    #[test]
    fn file_import_asks_for_a_chooser_of_several_images_and_places_each() {
        let (mut v, _) = viewer_with_square();
        v.run_command(AppCommand::Import);
        assert_eq!(v.requests, [PlatformRequest::ShowImportDialog]);
        let r = import_request(Some("wayland:x".to_owned()));
        assert!(r.multiple, "several files at once");
        assert_eq!(r.title, "Import");
        assert!(r.filters[0].extensions.iter().any(|e| e == "webp"));
        let (_, answers) = with_ctx(|ctx| v.perform_requests(ctx));
        assert!(v.import_dialog.is_some() && v.open_dialog.is_none());
        // The offline portal fails at once: said, and nothing changes.
        for a in answers {
            v.handle(&ShellEvent::Portal(a));
        }
        assert!(v.import_dialog.is_none());
        assert!(
            v.message
                .as_deref()
                .is_some_and(|m| m.starts_with("Could not show the file chooser"))
        );
        // A chooser that answers with two files places both, in the view.
        let (dir, png) = png_file("import", 96, 48);
        let second = dir.join("second.png");
        std::fs::copy(&png, &second).unwrap();
        v.import_dialog = Some(PortalRequestId(9));
        let history = v.app.active().unwrap().bus.history().len();
        assert!(v.handle(&ShellEvent::Portal(PortalEvent::FilesChosen {
            request: PortalRequestId(9),
            paths: vec![png, second],
        })));
        assert!(v.import_dialog.is_none());
        let s = v.app.active().unwrap();
        assert_eq!(s.bus.history().len(), history + 2, "one step each");
        assert_eq!(s.undo_label(), Some("Import Bitmap"));
        assert_eq!(bitmaps(&v).len(), 2);
        assert!(v.to_open.is_empty(), "nothing is opened as a document");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_gallerys_replace_and_save_a_copy_use_their_own_choosers() {
        use xarast_app::bitmap_gallery::BitmapGalleryOp;
        let (mut v, _) = viewer_with_square();
        let (dir, png) = png_file("replace", 96, 48);
        let img = xarast_app::place::image_from_file(&png).unwrap();
        let id = v
            .app
            .active_mut()
            .unwrap()
            .doc
            .resources
            .insert_bitmap(img.resource);
        // Replace: the Import chooser, for one file.
        let r = replace_request(Some("wayland:x".to_owned()));
        assert!(!r.multiple, "one file");
        assert_eq!(r.title, "Replace bitmap");
        assert!(r.filters[0].extensions.iter().any(|e| e == "png"));
        v.apply(vec![Intent::BitmapGallery(BitmapGalleryOp::Replace(id))]);
        assert_eq!(v.requests, [PlatformRequest::ShowReplaceBitmapDialog]);
        let (_, answers) = with_ctx(|ctx| v.perform_requests(ctx));
        assert!(v.replace_dialog.is_some() && v.import_dialog.is_none());
        // An answer to another chooser is not taken.
        assert!(!v.replace_answer(&PortalEvent::Cancelled {
            request: PortalRequestId(1_000),
        }));
        for a in answers {
            v.handle(&ShellEvent::Portal(a));
        }
        assert!(
            v.replace_dialog.is_none(),
            "the offline portal fails at once"
        );
        // The chosen file takes the bitmap's place.
        let (dir2, other) = png_file("replace-with", 32, 32);
        v.apply(vec![Intent::BitmapGallery(BitmapGalleryOp::Replace(id))]);
        v.requests.clear();
        v.replace_dialog = Some(PortalRequestId(11));
        assert!(v.handle(&ShellEvent::Portal(PortalEvent::FilesChosen {
            request: PortalRequestId(11),
            paths: vec![other],
        })));
        let s = v.app.active().unwrap();
        assert_eq!(s.undo_label(), Some("Replace Bitmap"));
        assert_eq!(s.doc.resources.bitmap(id).unwrap().info.width, 32);
        // Save a copy: a save chooser filtered to the bitmap's format.
        v.apply(vec![Intent::BitmapGallery(BitmapGalleryOp::SaveCopy(id))]);
        let [
            PlatformRequest::ShowSaveCopyDialog {
                file_name,
                extensions,
                ..
            },
        ] = v.requests.as_slice()
        else {
            panic!("{:?}", v.requests);
        };
        assert_eq!(file_name, "dropped.png");
        assert_eq!(extensions, &["png"]);
        v.requests.clear();
        v.copy_dialog = Some(PortalRequestId(12));
        let target = dir.join("copy.png");
        assert!(v.handle(&ShellEvent::Portal(PortalEvent::SaveChosen {
            request: PortalRequestId(12),
            path: target.clone(),
        })));
        assert!(v.copy_dialog.is_none());
        let stored = v.app.active().unwrap().doc.resources.bitmap(id).unwrap();
        assert_eq!(
            std::fs::read(&target).unwrap(),
            stored.original.as_ref().unwrap().bytes.as_ref()
        );
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    #[test]
    fn ctrl_v_of_a_file_managers_copy_imports_the_files_not_a_picture() {
        let (mut v, _) = viewer_with_square();
        let (dir, png) = png_file("paste-files", 8, 4);
        let mut clipboard = FakeClipboard {
            text: Some(format!("copy\nfile://{}\n", png.display())),
            // A picture beside it must not win over the files.
            image: crate::clipboard::ClipboardImage::new(1, 1, vec![0, 0, 0, 255]),
        };
        press(&mut v, Key::char('v'), Modifiers::NONE.with_ctrl());
        with_clipboard(&mut clipboard, |ctx| v.perform_requests(ctx));
        let s = v.app.active().unwrap();
        assert_eq!(s.undo_label(), Some("Import Bitmap"));
        let b = bitmaps(&v);
        assert_eq!(b.len(), 1);
        // 8 × 4 px at 96 dpi: the file, not the 1 × 1 picture.
        assert_eq!(b[0].major.dx, xarast_geom::Mp::new(6_000));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bitmap_dragged_from_the_gallery_lands_on_the_canvas_point() {
        use xarast_app::bitmap_gallery::{BitmapDragPoint, BitmapGalleryOp};
        let (mut v, _) = viewer_with_square();
        let img = xarast_app::place::image_from_rgba(4, 4, &[9u8, 9, 9, 255].repeat(16)).unwrap();
        let id = v
            .app
            .active_mut()
            .unwrap()
            .doc
            .resources
            .insert_bitmap(img.resource);
        let m = v.ui_model(1.0);
        let gallery = m.bitmap_gallery.expect("the gallery is in the model");
        assert_eq!(gallery.entries.len(), 1);
        assert_eq!(gallery.entries[0].id, id);
        // A pointer over the canvas maps to canvas pixels; elsewhere not.
        let (x, y) = window_at(&v, 300_000, 450_000);
        #[allow(clippy::cast_possible_truncation)]
        let over = UiCommand::BitmapDragAt {
            x: x as f32,
            y: y as f32,
        };
        let Some(Intent::BitmapGallery(BitmapGalleryOp::DragTo(BitmapDragPoint::Canvas(at)))) =
            v.ui_intent(over.clone(), 1.0)
        else {
            panic!("over the canvas");
        };
        assert_eq!(
            v.ui_intent(UiCommand::BitmapDragAt { x: 1.0, y: 1.0 }, 1.0),
            Some(Intent::BitmapGallery(BitmapGalleryOp::DragTo(
                BitmapDragPoint::Elsewhere
            )))
        );
        assert_eq!(
            v.ui_intent(UiCommand::CancelImports, 1.0),
            Some(Intent::CancelImports)
        );
        let intents = vec![
            Intent::BitmapGallery(BitmapGalleryOp::DragBegin(id)),
            Intent::BitmapGallery(BitmapGalleryOp::DragTo(BitmapDragPoint::Canvas(at))),
        ];
        v.apply(intents);
        // While it is dragged the status line says what a drop does.
        let status = v.ui_model(1.0).status.message.unwrap_or_default();
        assert!(status.starts_with("Drop to"), "{status}");
        v.apply(vec![Intent::BitmapGallery(BitmapGalleryOp::DragDrop)]);
        let b = bitmaps(&v);
        assert_eq!(b.len(), 1);
        let c = bitmap_centre(&b[0]);
        let px = 1.0 / v.app.active().unwrap().viewport.zoom() * 750.0 + 1.0;
        assert!(
            (c.0 - 300_000.0).abs() <= px && (c.1 - 450_000.0).abs() <= px,
            "{c:?}"
        );
    }
}
