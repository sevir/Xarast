//! Guides: dragged out of a ruler, moved, and dropped back to delete.
//!
//! A guide is a document coordinate, not a screen one, so it stays where
//! the user put it across a pan, a zoom and a scale-factor change. The
//! interactions here are the ones `research/04 §1.18` describes: drag out of
//! a ruler to create, drag to move, drag back onto the ruler to remove.

use xarast_geom::Mp;

use crate::model::ViewTransform;

/// Which ruler a guide came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Axis {
    /// A vertical guide, dragged from the left ruler, fixed in x.
    Vertical,
    /// A horizontal guide, dragged from the top ruler, fixed in y.
    Horizontal,
}

impl Axis {
    /// The other axis.
    pub fn other(self) -> Axis {
        match self {
            Axis::Vertical => Axis::Horizontal,
            Axis::Horizontal => Axis::Vertical,
        }
    }
}

/// One guide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Guide {
    /// Which way it runs.
    pub axis: Axis,
    /// Where it sits along its fixed axis, in document coordinates.
    #[serde(with = "crate::serde_mp")]
    pub position: Mp,
}

impl Guide {
    /// A vertical guide at a document x.
    pub fn vertical(position: Mp) -> Guide {
        Guide {
            axis: Axis::Vertical,
            position,
        }
    }

    /// A horizontal guide at a document y.
    pub fn horizontal(position: Mp) -> Guide {
        Guide {
            axis: Axis::Horizontal,
            position,
        }
    }

    /// Where the guide is drawn, in canvas-relative logical points.
    pub fn view_position(&self, view: &ViewTransform) -> f64 {
        match self.axis {
            Axis::Vertical => view.doc_to_view_x(self.position),
            Axis::Horizontal => view.doc_to_view_y(self.position),
        }
    }
}

/// How close, in logical points, the pointer has to be to grab a guide.
///
/// Wide enough to hit with a mouse on a hairline, narrow enough not to
/// steal a click meant for an object underneath.
pub const GRAB_TOLERANCE: f64 = 4.0;

/// Finds the guide under a pointer, if any.
///
/// The pointer is in canvas-relative logical points. When two guides
/// overlap the later one wins, because it is the one drawn on top and the
/// one the user just placed.
pub fn guide_at(
    guides: &[Guide],
    view: &ViewTransform,
    pointer_x: f64,
    pointer_y: f64,
) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, g) in guides.iter().enumerate() {
        let pos = g.view_position(view);
        let distance = match g.axis {
            Axis::Vertical => (pointer_x - pos).abs(),
            Axis::Horizontal => (pointer_y - pos).abs(),
        };
        if distance <= GRAB_TOLERANCE && best.is_none_or(|(_, d)| distance <= d) {
            best = Some((i, distance));
        }
    }
    best.map(|(i, _)| i)
}

/// How close, in logical points, a guide has to be to a grid line to
/// land on it (the application's snap radius, 8 device pixels).
pub const GRID_SNAP_REACH: f64 = 8.0;

/// Snaps the position of a guide being dragged to the nearest line of the
/// grid, when one is within [`GRID_SNAP_REACH`] at this zoom. The grid
/// lines are the minor ones, the same as every other snap.
pub fn snap_to_grid(axis: Axis, position: Mp, grid: &crate::grid::GridSettings, zoom: f64) -> Mp {
    if !(zoom.is_finite() && zoom > 0.0) {
        return position;
    }
    let line = match axis {
        Axis::Vertical => grid.snap_x(position),
        Axis::Horizontal => grid.snap_y(position),
    };
    let reach = f64::from(Mp::from_pt(GRID_SNAP_REACH / zoom).raw());
    if (i64::from(line.raw()) - i64::from(position.raw())).abs() as f64 <= reach {
        line
    } else {
        position
    }
}

/// What a guide drag has turned into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuideDragOutcome {
    /// The guide moves to this document position.
    Move(Mp),
    /// The guide was dragged back over its ruler and should be removed.
    Remove,
}

/// Decides what a guide drag means when the pointer is released.
///
/// `ruler_depth` is how far into the canvas the ruler strip reaches, in
/// logical points: dropping a guide inside that band is how a guide is
/// deleted, exactly as dragging one out of it is how a guide is made.
pub fn resolve_guide_drag(
    axis: Axis,
    view: &ViewTransform,
    pointer_x: f64,
    pointer_y: f64,
    ruler_depth: f64,
) -> GuideDragOutcome {
    let along = match axis {
        Axis::Vertical => pointer_x,
        Axis::Horizontal => pointer_y,
    };
    if along < ruler_depth {
        return GuideDragOutcome::Remove;
    }
    GuideDragOutcome::Move(match axis {
        Axis::Vertical => view.view_to_doc_x(pointer_x),
        Axis::Horizontal => view.view_to_doc_y(pointer_y),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> ViewTransform {
        ViewTransform {
            zoom: 2.0,
            origin_x: 50.0,
            origin_y: 30.0,
            y_up: false,
        }
    }

    #[test]
    fn a_guide_keeps_its_document_position_across_pan_and_zoom() {
        let g = Guide::vertical(Mp::from_pt(100.0));
        let a = g.view_position(&view());
        let b = g.view_position(&view().panned(10.0, 0.0));
        assert!((b - a - 10.0).abs() < 1e-9);
        let zoomed = view().zoomed_about(2.0, 0.0, 0.0);
        assert!((g.view_position(&zoomed) - a * 2.0).abs() < 1e-9);
    }

    #[test]
    fn the_pointer_grabs_the_nearest_guide_within_tolerance() {
        let v = view();
        let guides = vec![
            Guide::vertical(Mp::from_pt(0.0)),
            Guide::horizontal(Mp::from_pt(0.0)),
        ];
        assert_eq!(guide_at(&guides, &v, 50.0, 500.0), Some(0));
        assert_eq!(guide_at(&guides, &v, 500.0, 30.0), Some(1));
        assert_eq!(guide_at(&guides, &v, 500.0, 500.0), None);
        assert_eq!(
            guide_at(&guides, &v, 50.0 + GRAB_TOLERANCE + 0.5, 500.0),
            None
        );
    }

    #[test]
    fn the_last_of_two_overlapping_guides_wins() {
        let v = ViewTransform::default();
        let guides = vec![Guide::vertical(Mp::ZERO), Guide::vertical(Mp::ZERO)];
        assert_eq!(guide_at(&guides, &v, 0.0, 10.0), Some(1));
    }

    #[test]
    fn dropping_a_guide_on_its_ruler_removes_it() {
        let v = view();
        assert_eq!(
            resolve_guide_drag(Axis::Vertical, &v, 5.0, 300.0, 18.0),
            GuideDragOutcome::Remove
        );
        assert_eq!(
            resolve_guide_drag(Axis::Horizontal, &v, 300.0, 2.0, 18.0),
            GuideDragOutcome::Remove
        );
    }

    #[test]
    fn dropping_a_guide_on_the_canvas_moves_it_there() {
        let v = view();
        match resolve_guide_drag(Axis::Vertical, &v, 250.0, 300.0, 18.0) {
            GuideDragOutcome::Move(p) => {
                assert_eq!(p, v.view_to_doc_x(250.0));
            }
            other => panic!("expected a move, got {other:?}"),
        }
    }

    #[test]
    fn guides_round_trip_through_serde() {
        let g = Guide::horizontal(Mp::from_mm(12.5));
        let json = serde_json::to_string(&g).unwrap();
        assert_eq!(serde_json::from_str::<Guide>(&json).unwrap(), g);
        assert_eq!(Axis::Vertical.other(), Axis::Horizontal);
    }

    #[test]
    fn a_guide_snaps_to_a_grid_line_within_reach_and_only_then() {
        let grid = crate::grid::GridSettings {
            spacing: Mp::from_pt(50.0),
            subdivisions: 1,
            origin: (Mp::from_pt(10.0), Mp::ZERO),
            ..Default::default()
        };
        // The vertical lines sit at 10, 60, 110 pt: 4 pt away snaps at
        // zoom 1, 4 pt away does not at zoom 4 (reach 2 pt).
        let near = Mp::from_pt(64.0);
        assert_eq!(
            snap_to_grid(Axis::Vertical, near, &grid, 1.0),
            Mp::from_pt(60.0)
        );
        assert_eq!(snap_to_grid(Axis::Vertical, near, &grid, 4.0), near);
        // Horizontal guides use the other origin.
        assert_eq!(
            snap_to_grid(Axis::Horizontal, Mp::from_pt(103.0), &grid, 1.0),
            Mp::from_pt(100.0)
        );
        assert_eq!(snap_to_grid(Axis::Vertical, near, &grid, 0.0), near);
    }
}
