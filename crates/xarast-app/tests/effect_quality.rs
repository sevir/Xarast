//! Draft and Final for live effects (phase 13 J3, XARA-T-0315): a drag on
//! a shadowed object's slider is drawn at `Draft`, where the effect pass
//! computes the shadow on a reduced grid, and the `Final` the scheduler
//! owes once input stops is exactly the frame a fresh `Final` render gives.

use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use xarast_app::schedule::{Backdrop, Canvas, QualityScheduler};
use xarast_app::shapes::rectangle;
use xarast_app::{
    DeviceSize, DocumentId, EditCommand, InfobarDrag, InfobarField, InfobarValue, Intent,
    RenderThread, RenderedFrame, Session, ToolId, ZoomTarget,
};
use xarast_color::{Colour, ColourValue};
use xarast_doc::AttrValue;
use xarast_doc::fill::FillGeometry;
use xarast_geom::{Mp, Point};
use xarast_render::RenderQuality;

const BACKDROP: Backdrop = Backdrop {
    pasteboard: [0x80, 0x80, 0x84, 0xff],
    page: [0xff; 4],
};
const WAIT: Duration = Duration::from_secs(60);

/// A selected red rectangle with a wall shadow of a 30 pt penumbra, zoomed
/// to fill a 400 × 300 view: the shadow's blur radius is far above
/// `DRAFT_RADIUS_PX` and below the `Final` ceiling.
fn session() -> Session {
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::Resize(DeviceSize::new(400, 300))).unwrap();
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).unwrap();
    s.apply_edit(EditCommand::CreateShape {
        layer,
        shape: Box::new(rectangle(Point::raw(200_000, 200_000), 50_000.0, 30_000.0)),
        attrs: vec![AttrValue::Fill(FillGeometry::Flat {
            value: Colour::Direct(ColourValue::Rgbt {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                t: 0.0,
            }),
        })],
    })
    .unwrap();
    let nodes: Vec<_> = xarast_app::edit::selectable_objects(&s.doc).collect();
    s.apply(Intent::Select {
        nodes,
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    s.apply(Intent::ChooseTool(ToolId::Shadow)).unwrap();
    s.apply(Intent::InfobarEdit {
        field: InfobarField::ShadowType,
        value: InfobarValue::Choice(1),
    })
    .unwrap();
    s.apply(Intent::InfobarEdit {
        field: InfobarField::ShadowBlur,
        value: InfobarValue::Length(Mp::new(30_000)),
    })
    .unwrap();
    s.apply(Intent::ZoomTo(ZoomTarget::Drawing)).unwrap();
    s.rebuild_scene(None).unwrap();
    s
}

fn canvas() -> (Canvas, mpsc::Receiver<()>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let rt = RenderThread::spawn(Box::new(move || {
        let _ = tx.lock().map(|t| t.send(()));
    }))
    .unwrap();
    (
        Canvas::new(rt, BACKDROP).with_scheduler(QualityScheduler::default()),
        rx,
    )
}

/// Applies `intent` at `now` (noting it as interaction), pumps, and waits
/// for the frame.
fn step(
    s: &mut Session,
    c: &mut Canvas,
    woken: &mpsc::Receiver<()>,
    now: Instant,
    intent: Option<Intent>,
) -> RenderedFrame {
    if let Some(i) = intent {
        let changed = s.apply(i).unwrap();
        c.note(now, changed);
    }
    c.pump(now, s).unwrap();
    woken.recv_timeout(WAIT).expect("a frame");
    c.take_latest().expect("a frame after the wake")
}

fn gain(v: f64) -> Intent {
    Intent::InfobarDrag(InfobarDrag::Preview {
        field: InfobarField::ShadowGain,
        value: InfobarValue::Real(v),
    })
}

#[test]
fn a_slider_drag_draws_draft_effects_and_settles_on_the_exact_final() {
    let mut s = session();
    let scale = s.viewport.transform().max_scale();
    // The penumbra is the blur's diameter: its radius in device pixels.
    let r = 30_000.0 * scale / 2.0;
    assert!(
        r > f64::from(xarast_render::DRAFT_RADIUS_PX)
            && r <= f64::from(xarast_render::MAX_RADIUS_PX),
        "a {r:.1} px blur: reduced at Draft only"
    );

    let (mut c, woken) = canvas();
    let t0 = Instant::now();
    let first = step(&mut s, &mut c, &woken, t0, None);
    assert_eq!(
        first.view.quality,
        RenderQuality::Final,
        "a document's first frame"
    );

    // The drag: every frame inside the idle delay is a Draft.
    let mut clock = t0;
    let mut last = None;
    for v in [0.1, 0.2, 0.3, 0.4] {
        clock += Duration::from_millis(16);
        let f = step(&mut s, &mut c, &woken, clock, Some(gain(v)));
        assert_eq!(f.view.quality, RenderQuality::Draft, "gain {v} mid-drag");
        last = Some(f);
    }
    let draft = last.expect("drag frames");

    // Input stops: the scheduler owes a Final, drawn once the idle delay
    // has passed.
    clock += Duration::from_millis(500);
    let fin = step(&mut s, &mut c, &woken, clock, None);
    assert_eq!(
        fin.view.quality,
        RenderQuality::Final,
        "after the drag settles"
    );
    assert!(fin.exact);
    assert!(c.is_settled());

    // The settled Final is the frame a fresh render thread draws of the
    // same state: nothing of the Draft effects survives in it (the effect
    // cache keys on the quality).
    let (mut fresh, fresh_woken) = canvas();
    let reference = step(&mut s, &mut fresh, &fresh_woken, Instant::now(), None);
    assert_eq!(reference.view.quality, RenderQuality::Final);
    assert!(
        fin.surface == reference.surface,
        "the settled Final differs from a fresh one"
    );
    // And the Draft really was computed differently, though close.
    assert!(
        draft.surface != fin.surface,
        "the Draft shadow is computed on its own grid"
    );
    let n = fin.surface.data().len() as f64;
    let diff: u64 = draft
        .surface
        .data()
        .iter()
        .zip(fin.surface.data())
        .map(|(a, b)| u64::from(a.abs_diff(*b)))
        .sum();
    assert!(
        (diff as f64 / n) < 2.0,
        "Draft against Final: mean {:.2}",
        diff as f64 / n
    );
    c.shutdown();
    fresh.shutdown();
}
