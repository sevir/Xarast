//! Geometry primitives shared by every other Xarast crate.
//!
//! # The coordinate system
//!
//! Coordinates are **integer millipoints** ([`Mp`]), 1/1000 of a PostScript
//! point, which is what the `.xar` format stores and what gives Xarast exact
//! equality, deterministic output and lossless round-tripping. Continuous
//! operations — flattening, offsetting, boolean operations — work in `f64`
//! and quantise on the way back.
//!
//! **Y is up.** The origin is the bottom-left of the spread's page bounding
//! rectangle, [`Rect::lo`] is the bottom-left corner, and the flip to a
//! Y-down frame belongs to whichever consumer wants one — SVG, PDF or the
//! screen — not to this crate. See the [`point`] module.
//!
//! The overflow behaviour of [`Mp`] is a specified, tested contract rather
//! than an accident of the types; read [`mp`] before doing arithmetic on
//! coordinates.
//!
//! # What lives here and what does not
//!
//! This crate is pure geometry. Fill and stroke *intent*, attribute nodes and
//! anything that knows what a document is belong to `xarast-doc`; keeping
//! them out is what lets `Path: Eq` mean "the same shape".
//!
//! # Dependencies
//!
//! [`kurbo`] supplies the `f64` curve algebra — flattening, stroking,
//! dashing, offsetting, arc length, nearest point — and `i_overlay` supplies
//! the boolean engine. `lyon_algorithms` was considered, per
//! `docs/10-architecture.md`, and is not used: it is `f32`, and everything
//! this crate needs from it `kurbo` already provides in `f64`, so taking it
//! would add a second curve vocabulary and a precision boundary for nothing.
//!
//! See `docs/phases/phase-01-geometry-and-colour.md` for the specification
//! and `docs/memory/geometry.md` for the decisions behind it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![doc(html_no_source)]

pub mod bevel;
pub mod boolean;
pub mod contour;
pub mod fit;
pub mod fixed;
pub mod flatten;
pub mod hit;
pub mod hit_index;
pub mod matrix;
pub mod measure;
pub mod mp;
pub mod path;
pub mod path_edit;
pub mod point;
pub mod profile;
pub mod rect;
pub mod regular;
pub mod stroke;

pub use boolean::{BoolOp, boolean, self_union};
pub use fit::{fit_stroke, fit_stroke_indexed, max_sample_deviation};
pub use fixed::Fixed16;
pub use flatten::{
    Polyline, SegmentTrace, Tolerance, VertexSource, flatten, flatten_traced, max_deviation,
};
pub use hit::{
    HitShape, HitTolerance, ShapeHit, hit_fill, hit_fill_transformed, hit_stroke,
    hit_stroke_transformed,
};
pub use hit_index::{Candidates, HitIndex, HitIndexStats, RectMode};
pub use matrix::Matrix;
pub use measure::{
    Nearest, PathHitIndex, arclen, fill_contains, nearest_in_index, nearest_point,
    nearest_point_transformed, point_at_arclen,
};
pub use mp::{Mp, ParseMpError};
pub use path::{Path, PathBuilder, PathError, PointFlags, Segment, SubPathRef, Verb};
pub use path_edit::{
    Control, EditNode, EditPath, EditSubpath, NodeRef, PointRole, SegRef, SegmentHit, Side,
};
pub use point::{Point, Vector};
pub use profile::BiasGain;
pub use rect::Rect;
pub use regular::{MAX_REGULAR_SIDES, RegularShapeSpec, regular_shape_outline};
pub use stroke::{
    Cap, DashPattern, FillRule, Join, StrokeError, StrokeStyle, dash, offset, reduced_dash_offset,
    stroke_to_path,
};
