//! Non-destructive photo adjustments (phase 10 W10.6, T10.6.1 and the
//! command half of T10.6.5).
//!
//! A placed bitmap ([`BitmapNode`]) may carry a [`PhotoOps`] chain. The
//! resource it names stays the **master**: nothing here or downstream ever
//! changes its pixels. What the object shows is the master put through the
//! chain — a *derived* image the renderer makes and caches (`xarast-app`'s
//! walker, `docs/memory/image.md`, "Photo adjustments").
//!
//! # The chain is a set of settings with one canonical order
//!
//! Each kind of operation appears at most once (levels: at most once per
//! channel) and is evaluated in a fixed order, whatever order the user set
//! them in:
//!
//! 1. [`PhotoOp::Crop`], a rectangle in **master** pixels;
//! 2. [`PhotoOp::Orient`], quarter turns and a mirror of the cropped image;
//! 3. the per-channel point operations, fused into one lookup table per
//!    channel (T10.6.4): [`PhotoOp::Levels`] for red, green, blue, then
//!    for all three; [`PhotoOp::Gamma`]; [`PhotoOp::Brightness`];
//!    [`PhotoOp::Contrast`];
//! 4. [`PhotoOp::Saturation`], then [`PhotoOp::Greyscale`], which mix
//!    channels and so cannot be part of the table.
//!
//! [`PhotoOps::normalise`] sorts into that order, keeps the **last** value
//! set for each kind and drops operations that do nothing, so that two
//! chains that look the same hash the same ([`PhotoOps::hash`]).
//!
//! # Unknown operations
//!
//! An operation this version cannot evaluate (read from a newer file) is a
//! [`PhotoOp::Unknown`] holding its element's text verbatim; it is written
//! back character for character (`research/06 §8`). It renders as if absent,
//! and it makes the chain non-editable ([`PhotoOps::is_editable`]): the
//! chain's order could matter to the operation, so normalising or editing
//! it is refused ([`SetPhotoOps`]).

use std::sync::Arc;

use sha2::{Digest, Sha256};
use xarast_geom::{Mp, Point, Vector};

use crate::history::{Command, EditError, Tx};
use crate::kind::{BitmapNode, NodeKind};
use crate::tree::NodeId;

/// A rectangle in master pixels: `x`, `y` from the top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PixelRect {
    /// Left column.
    pub x: u32,
    /// Top row.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl PixelRect {
    /// The part of the rectangle inside a `width` × `height` image, or
    /// `None` when nothing is.
    #[must_use]
    pub fn clamped(self, width: u32, height: u32) -> Option<PixelRect> {
        let x1 = self.x.saturating_add(self.width).min(width);
        let y1 = self.y.saturating_add(self.height).min(height);
        (self.x < x1 && self.y < y1).then(|| PixelRect {
            x: self.x,
            y: self.y,
            width: x1 - self.x,
            height: y1 - self.y,
        })
    }
}

/// A lossless reorientation: first mirrored left to right when `flip`,
/// then turned `turns` quarter turns clockwise. The eight values are the
/// eight orientations of a rectangle.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct PhotoOrient {
    /// Quarter turns clockwise, `0..=3`.
    pub turns: u8,
    /// Mirrored left to right before turning.
    pub flip: bool,
}

impl PhotoOrient {
    /// The orientation that changes nothing.
    pub const IDENTITY: PhotoOrient = PhotoOrient {
        turns: 0,
        flip: false,
    };

    /// A clockwise quarter turn.
    pub const CW: PhotoOrient = PhotoOrient {
        turns: 1,
        flip: false,
    };

    /// A mirror left to right.
    pub const FLIP_H: PhotoOrient = PhotoOrient {
        turns: 0,
        flip: true,
    };

    /// A mirror top to bottom.
    pub const FLIP_V: PhotoOrient = PhotoOrient {
        turns: 2,
        flip: true,
    };

    /// With `turns` reduced to `0..=3`.
    #[must_use]
    pub const fn normalised(self) -> PhotoOrient {
        PhotoOrient {
            turns: self.turns % 4,
            flip: self.flip,
        }
    }

    /// `self` followed by `next`: what the user gets pressing "rotate" or
    /// "flip" on an image already oriented by `self`.
    #[must_use]
    pub const fn then(self, next: PhotoOrient) -> PhotoOrient {
        let a = self.normalised();
        let b = next.normalised();
        // A mirror before a turn of `t` equals a turn of `-t` before the
        // mirror, so `b`'s mirror moves in front of `a`'s turns.
        if b.flip {
            PhotoOrient {
                turns: (4 - a.turns + b.turns) % 4,
                flip: !a.flip,
            }
        } else {
            PhotoOrient {
                turns: (a.turns + b.turns) % 4,
                flip: a.flip,
            }
        }
    }

    /// Whether width and height swap.
    #[must_use]
    pub const fn swaps_axes(self) -> bool {
        self.turns % 2 == 1
    }

    /// The size of a `w` × `h` image once oriented.
    #[must_use]
    pub const fn size(self, w: u32, h: u32) -> (u32, u32) {
        if self.swaps_axes() { (h, w) } else { (w, h) }
    }
}

/// The channel a levels operation applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LevelsChannel {
    /// Red, green and blue alike (applied after the single channels).
    All,
    /// Red only.
    Red,
    /// Green only.
    Green,
    /// Blue only.
    Blue,
}

impl LevelsChannel {
    /// The name the file format uses.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            LevelsChannel::All => "rgb",
            LevelsChannel::Red => "red",
            LevelsChannel::Green => "green",
            LevelsChannel::Blue => "blue",
        }
    }

    /// The channel a name spells.
    #[must_use]
    pub fn from_name(s: &str) -> Option<LevelsChannel> {
        Some(match s {
            "rgb" => LevelsChannel::All,
            "red" => LevelsChannel::Red,
            "green" => LevelsChannel::Green,
            "blue" => LevelsChannel::Blue,
            _ => return None,
        })
    }
}

/// Input and output ranges of a levels operation, in 8-bit levels: the
/// input range `in_lo..=in_hi` is stretched onto `out_lo..=out_hi`, and
/// what is outside it clips (the original's `SetInputRange` /
/// `SetOutputRange` pair).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Levels {
    /// Which channel.
    pub channel: LevelsChannel,
    /// Input black point.
    pub in_lo: u8,
    /// Input white point.
    pub in_hi: u8,
    /// Output black point.
    pub out_lo: u8,
    /// Output white point.
    pub out_hi: u8,
}

impl Levels {
    /// Whether it changes nothing.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        self.in_lo == 0 && self.in_hi == 255 && self.out_lo == 0 && self.out_hi == 255
    }
}

/// One non-destructive operation.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PhotoOp {
    /// Keep a rectangle of the master (master pixels).
    Crop(PixelRect),
    /// Turn and mirror.
    Orient(PhotoOrient),
    /// Stretch an input range of levels onto an output range.
    Levels(Levels),
    /// `x^(1/γ)`; `(0, 8]`, 1 neutral.
    Gamma(f32),
    /// Added to every channel, `[-1, 1]` of full scale, 0 neutral.
    Brightness(f32),
    /// Slope about mid-grey, `[-1, 1]`, 0 neutral, −1 flat grey.
    Contrast(f32),
    /// Distance from the pixel's luma scaled by `1 + s`, `[-1, 1]`, 0
    /// neutral, −1 grey.
    Saturation(f32),
    /// Every channel set to the pixel's luma.
    Greyscale,
    /// An operation from a newer version, preserved verbatim: `kind` is
    /// its `xarast:kind`, `raw` the whole element as it was read.
    Unknown {
        /// The `xarast:kind` it declared.
        kind: Arc<str>,
        /// The element's text, written back unchanged.
        raw: Arc<str>,
    },
}

/// Gamma's accepted range.
pub const GAMMA_RANGE: (f32, f32) = (0.05, 8.0);

impl PhotoOp {
    /// The canonical position of the operation's kind (see the module
    /// docs); `None` for an unknown one.
    #[must_use]
    pub const fn rank(&self) -> Option<u8> {
        Some(match self {
            PhotoOp::Crop(_) => 0,
            PhotoOp::Orient(_) => 1,
            PhotoOp::Levels(l) => match l.channel {
                LevelsChannel::Red => 2,
                LevelsChannel::Green => 3,
                LevelsChannel::Blue => 4,
                LevelsChannel::All => 5,
            },
            PhotoOp::Gamma(_) => 6,
            PhotoOp::Brightness(_) => 7,
            PhotoOp::Contrast(_) => 8,
            PhotoOp::Saturation(_) => 9,
            PhotoOp::Greyscale => 10,
            PhotoOp::Unknown { .. } => return None,
        })
    }

    /// The name the file format writes in `xarast:kind`.
    #[must_use]
    pub fn kind_name(&self) -> &str {
        match self {
            PhotoOp::Crop(_) => "crop",
            PhotoOp::Orient(_) => "orient",
            PhotoOp::Levels(_) => "levels",
            PhotoOp::Gamma(_) => "gamma",
            PhotoOp::Brightness(_) => "brightness",
            PhotoOp::Contrast(_) => "contrast",
            PhotoOp::Saturation(_) => "saturation",
            PhotoOp::Greyscale => "greyscale",
            PhotoOp::Unknown { kind, .. } => kind,
        }
    }

    /// Whether the operation is one of the per-channel point operations
    /// the lookup table fuses.
    #[must_use]
    pub const fn is_point_op(&self) -> bool {
        matches!(
            self,
            PhotoOp::Levels(_) | PhotoOp::Gamma(_) | PhotoOp::Brightness(_) | PhotoOp::Contrast(_)
        )
    }

    /// The operation with its parameter clamped to its range, `-0.0`
    /// made `0.0`; `None` when it does nothing (or is not a number).
    /// A crop is kept whatever it is: whether it covers the whole image
    /// depends on the master.
    #[must_use]
    pub fn canonical(&self) -> Option<PhotoOp> {
        let unit = |v: f32| -> Option<f32> {
            if v.is_nan() {
                return None;
            }
            let v = v.clamp(-1.0, 1.0);
            (v != 0.0).then_some(v)
        };
        Some(match self {
            PhotoOp::Crop(r) => {
                if r.width == 0 || r.height == 0 {
                    return None;
                }
                PhotoOp::Crop(*r)
            }
            PhotoOp::Orient(o) => {
                let o = o.normalised();
                if o == PhotoOrient::IDENTITY {
                    return None;
                }
                PhotoOp::Orient(o)
            }
            PhotoOp::Levels(l) => {
                if l.is_identity() || l.in_lo >= l.in_hi {
                    return None;
                }
                PhotoOp::Levels(*l)
            }
            PhotoOp::Gamma(g) => {
                if g.is_nan() {
                    return None;
                }
                let g = g.clamp(GAMMA_RANGE.0, GAMMA_RANGE.1);
                if g == 1.0 {
                    return None;
                }
                PhotoOp::Gamma(g)
            }
            PhotoOp::Brightness(v) => PhotoOp::Brightness(unit(*v)?),
            PhotoOp::Contrast(v) => PhotoOp::Contrast(unit(*v)?),
            PhotoOp::Saturation(v) => PhotoOp::Saturation(unit(*v)?),
            PhotoOp::Greyscale => PhotoOp::Greyscale,
            PhotoOp::Unknown { .. } => self.clone(),
        })
    }
}

/// A chain of operations on a placed bitmap. Empty: the master as it is.
#[derive(Clone, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct PhotoOps {
    /// The operations, in order.
    pub ops: Vec<PhotoOp>,
}

impl PhotoOps {
    /// No operations.
    #[must_use]
    pub const fn new() -> PhotoOps {
        PhotoOps { ops: Vec::new() }
    }

    /// Whether there is nothing to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// False when any operation is unknown: the UI shows the chain but
    /// does not edit it, and [`SetPhotoOps`] refuses to replace it.
    #[must_use]
    pub fn is_editable(&self) -> bool {
        self.ops.iter().all(|o| o.rank().is_some())
    }

    /// The canonical form (see the module docs): sorted, last value per
    /// kind, no operation that does nothing, parameters clamped.
    /// Idempotent. A chain with an unknown operation is left exactly as
    /// it is.
    pub fn normalise(&mut self) {
        if !self.is_editable() {
            return;
        }
        let mut slots: [Option<PhotoOp>; 11] = Default::default();
        for op in &self.ops {
            if let Some(r) = op.rank() {
                // Last one wins, including a neutral one that clears it.
                slots[usize::from(r)] = op.canonical();
            }
        }
        self.ops = slots.into_iter().flatten().collect();
    }

    /// The normalised chain.
    #[must_use]
    pub fn normalised(&self) -> PhotoOps {
        let mut c = self.clone();
        c.normalise();
        c
    }

    /// The chain with its unknown operations left out, normalised: what
    /// is evaluated (unknown operations render as if absent).
    #[must_use]
    pub fn evaluable(&self) -> PhotoOps {
        let mut c = PhotoOps {
            ops: self
                .ops
                .iter()
                .filter(|o| o.rank().is_some())
                .cloned()
                .collect(),
        };
        c.normalise();
        c
    }

    /// Replaces (or with `None` clears) the operation of `op`'s kind, and
    /// normalises: how a control in the photo panel edits the chain.
    #[must_use]
    pub fn with(&self, op: PhotoOp) -> PhotoOps {
        let mut c = self.clone();
        c.ops.push(op);
        c.normalise();
        c
    }

    /// The crop, if any.
    #[must_use]
    pub fn crop(&self) -> Option<PixelRect> {
        self.ops.iter().rev().find_map(|o| match o {
            PhotoOp::Crop(r) => Some(*r),
            _ => None,
        })
    }

    /// The orientation (identity when none).
    #[must_use]
    pub fn orient(&self) -> PhotoOrient {
        self.ops
            .iter()
            .rev()
            .find_map(|o| match o {
                PhotoOp::Orient(r) => Some(r.normalised()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// A SHA-256 over the canonical encoding of the **normalised** chain:
    /// equal for chains that evaluate the same whatever order they were
    /// built in. Unknown operations hash by their text.
    #[must_use]
    pub fn hash(&self) -> [u8; 32] {
        let n = self.normalised();
        let mut h = Sha256::new();
        h.update(b"xarast.photo-ops.v1");
        for op in &n.ops {
            h.update([op.rank().unwrap_or(0xff)]);
            match op {
                PhotoOp::Crop(r) => {
                    for v in [r.x, r.y, r.width, r.height] {
                        h.update(v.to_le_bytes());
                    }
                }
                PhotoOp::Orient(o) => h.update([o.turns, u8::from(o.flip)]),
                PhotoOp::Levels(l) => h.update([l.in_lo, l.in_hi, l.out_lo, l.out_hi]),
                PhotoOp::Gamma(v)
                | PhotoOp::Brightness(v)
                | PhotoOp::Contrast(v)
                | PhotoOp::Saturation(v) => h.update(v.to_bits().to_le_bytes()),
                PhotoOp::Greyscale => {}
                PhotoOp::Unknown { raw, .. } => {
                    h.update((raw.len() as u64).to_le_bytes());
                    h.update(raw.as_bytes());
                }
            }
        }
        h.finalize().into()
    }

    /// The size of the image the chain makes of a `w` × `h` master.
    #[must_use]
    pub fn derived_size(&self, w: u32, h: u32) -> (u32, u32) {
        let (cw, ch) = self
            .crop()
            .and_then(|r| r.clamped(w, h))
            .map_or((w, h), |r| (r.width, r.height));
        self.orient().size(cw, ch)
    }

    /// The affine map from the derived image's unit square (`u` right,
    /// `v` down, from its top-left corner) to master pixel coordinates,
    /// as `[a, b, c, d, e, f]` with `x = a·u + c·v + e`,
    /// `y = b·u + d·v + f`. `None` when the master is empty.
    #[must_use]
    pub fn unit_to_master(&self, w: u32, h: u32) -> Option<[f64; 6]> {
        if w == 0 || h == 0 {
            return None;
        }
        let r = self
            .crop()
            .and_then(|r| r.clamped(w, h))
            .unwrap_or(PixelRect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            });
        // The derived unit square to the cropped image's unit square: undo
        // the turns (each a quarter turn clockwise), then the mirror.
        let o = self.orient();
        // Start with the identity on (u, v) and apply the inverse turns.
        // One clockwise turn maps source (s, t) to derived (1 - t, s), so
        // its inverse maps derived (u, v) to source (v, 1 - u).
        let mut m = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        for _ in 0..o.turns {
            m = compose([0.0, -1.0, 1.0, 0.0, 0.0, 1.0], m);
        }
        if o.flip {
            m = compose([-1.0, 0.0, 0.0, 1.0, 1.0, 0.0], m);
        }
        let scale = [
            f64::from(r.width),
            0.0,
            0.0,
            f64::from(r.height),
            f64::from(r.x),
            f64::from(r.y),
        ];
        Some(compose(scale, m))
    }
}

/// `outer ∘ inner` for maps written `[a, b, c, d, e, f]`.
fn compose(outer: [f64; 6], inner: [f64; 6]) -> [f64; 6] {
    let [a, b, c, d, e, f] = outer;
    let [a2, b2, c2, d2, e2, f2] = inner;
    [
        a * a2 + c * b2,
        b * a2 + d * b2,
        a * c2 + c * d2,
        b * c2 + d * d2,
        a * e2 + c * f2 + e,
        b * e2 + d * f2 + f,
    ]
}

fn invert(m: [f64; 6]) -> Option<[f64; 6]> {
    let [a, b, c, d, e, f] = m;
    let det = a * d - b * c;
    if det.abs() < 1e-12 || !det.is_finite() {
        return None;
    }
    let (ia, ib, ic, id) = (d / det, -b / det, -c / det, a / det);
    Some([ia, ib, ic, id, -(ia * e + ic * f), -(ib * e + id * f)])
}

/// The placement of a bitmap object after its chain changes from `old`
/// to `new`, for a master of `w` × `h` pixels.
///
/// A crop keeps the pixels that stay where they were on the page: the
/// object shrinks (or grows) around them. A change of orientation turns
/// the picture inside the object: the object keeps its centre, the
/// directions of its two edges and its size per pixel along each, and
/// takes the new width and height. `None` when the master is empty or
/// the placement degenerate; the caller then keeps the old placement.
#[must_use]
pub fn replaced_placement(
    node: &BitmapNode,
    old: &PhotoOps,
    new: &PhotoOps,
    (w, h): (u32, u32),
) -> Option<(Point, Vector, Vector)> {
    let old = old.evaluable();
    let new = new.evaluable();
    let o = |p: Point| p.to_f64();
    let (ox, oy) = o(node.origin);
    let (ux, uy) = node.major.to_f64();
    let (vx, vy) = node.minor.to_f64();
    // Unit square of the derived image → page.
    let place = [ux, uy, vx, vy, ox, oy];
    // Step 1: the new crop under the old orientation, pixels held still.
    let mut mid = new.clone();
    mid.ops.retain(|op| !matches!(op, PhotoOp::Orient(_)));
    if old.orient() != PhotoOrient::IDENTITY {
        mid.ops.insert(
            usize::from(new.crop().is_some()),
            PhotoOp::Orient(old.orient()),
        );
    }
    let m_old = old.unit_to_master(w, h)?;
    let m_mid = mid.unit_to_master(w, h)?;
    let p = compose(place, compose(invert(m_old)?, m_mid));
    // Step 2: turn the picture inside the object.
    let (mw, mh) = mid.derived_size(w, h);
    let (nw, nh) = new.derived_size(w, h);
    let (sx, sy) = (f64::from(nw) / f64::from(mw), f64::from(nh) / f64::from(mh));
    let (ux, uy, vx, vy) = (p[0] * sx, p[1] * sx, p[2] * sy, p[3] * sy);
    let (cx, cy) = (p[4] + (p[0] + p[2]) / 2.0, p[5] + (p[1] + p[3]) / 2.0);
    let origin = (cx - (ux + vx) / 2.0, cy - (uy + vy) / 2.0);
    let finite = [origin.0, origin.1, ux, uy, vx, vy]
        .iter()
        .all(|v| v.is_finite() && v.abs() < f64::from(i32::MAX));
    if !finite {
        return None;
    }
    let v = |x: f64, y: f64| Vector::new(Mp::new(x.round() as i32), Mp::new(y.round() as i32));
    Some((
        Point::from_f64_round(origin.0, origin.1),
        v(ux, uy),
        v(vx, vy),
    ))
}

/// Replaces a placed bitmap's photo operations. One undo step.
///
/// The chain is stored normalised. With `master` (the master's size in
/// pixels) the object's placement follows the change
/// ([`replaced_placement`]); without it only the chain changes.
#[derive(Clone, Debug, PartialEq)]
pub struct SetPhotoOps {
    /// The bitmap object.
    pub node: NodeId,
    /// The new chain.
    pub ops: PhotoOps,
    /// The master's pixel size, when known.
    pub master: Option<(u32, u32)>,
    /// What the Edit menu calls the step.
    pub label: &'static str,
}

impl Command for SetPhotoOps {
    fn label(&self) -> &'static str {
        self.label
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let Some(NodeKind::Bitmap(b)) = tx.doc().tree.kind(self.node) else {
            return Err(EditError::WrongKind(self.node));
        };
        if !b.photo_ops.is_editable() {
            return Err(EditError::NotPermitted(self.node));
        }
        let new = self.ops.normalised();
        if new == b.photo_ops {
            return Ok(());
        }
        let mut next: BitmapNode = (**b).clone();
        if let Some(size) = self.master
            && let Some((origin, major, minor)) = replaced_placement(b, &b.photo_ops, &new, size)
        {
            next.origin = origin;
            next.major = major;
            next.minor = minor;
        }
        next.photo_ops = new;
        tx.set_kind(self.node, NodeKind::Bitmap(Box::new(next)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(v: f32) -> PhotoOp {
        PhotoOp::Brightness(v)
    }

    #[test]
    fn normalising_sorts_keeps_the_last_and_drops_neutral_ops() {
        let mut c = PhotoOps {
            ops: vec![
                PhotoOp::Contrast(0.2),
                b(0.1),
                PhotoOp::Gamma(1.0),
                b(0.3),
                PhotoOp::Orient(PhotoOrient {
                    turns: 5,
                    flip: false,
                }),
                PhotoOp::Saturation(-0.0),
            ],
        };
        c.normalise();
        assert_eq!(
            c.ops,
            vec![
                PhotoOp::Orient(PhotoOrient::CW),
                b(0.3),
                PhotoOp::Contrast(0.2)
            ]
        );
        let again = c.normalised();
        assert_eq!(again, c, "idempotent");
    }

    #[test]
    fn the_hash_ignores_the_order_ops_were_set_in() {
        let ops = [b(0.1), PhotoOp::Contrast(0.25), PhotoOp::Gamma(1.4)];
        let orders = [[0, 1, 2], [2, 0, 1], [1, 2, 0]];
        let hashes: Vec<[u8; 32]> = orders
            .iter()
            .map(|o| {
                PhotoOps {
                    ops: o.iter().map(|&i| ops[i].clone()).collect(),
                }
                .hash()
            })
            .collect();
        assert!(hashes.windows(2).all(|w| w[0] == w[1]));
        assert_ne!(hashes[0], PhotoOps::new().hash());
        assert_eq!(
            PhotoOps { ops: vec![b(0.0)] }.hash(),
            PhotoOps::new().hash(),
            "a neutral op hashes like none"
        );
    }

    #[test]
    fn unknown_ops_freeze_the_chain() {
        let unknown = PhotoOp::Unknown {
            kind: Arc::from("curves"),
            raw: Arc::from("<xarast:op xarast:kind=\"curves\"/>"),
        };
        let mut c = PhotoOps {
            ops: vec![PhotoOp::Contrast(0.2), unknown.clone(), b(0.0)],
        };
        let before = c.clone();
        c.normalise();
        assert_eq!(c, before);
        assert!(!c.is_editable());
        assert_eq!(c.evaluable().ops, vec![PhotoOp::Contrast(0.2)]);
    }

    #[test]
    fn orientations_compose_like_the_dihedral_group() {
        let all: Vec<PhotoOrient> = (0..8)
            .map(|i| PhotoOrient {
                turns: i % 4,
                flip: i >= 4,
            })
            .collect();
        // Check against the action on the corners of a unit square.
        let act = |o: PhotoOrient, (x, y): (i32, i32)| {
            let (mut x, y0) = if o.flip { (-x, y) } else { (x, y) };
            let mut y = y0;
            for _ in 0..o.turns {
                (x, y) = (-y, x);
            }
            (x, y)
        };
        let pts = [(1, 2), (-3, 1)];
        for &a in &all {
            for &c in &all {
                let ab = a.then(c);
                for p in pts {
                    assert_eq!(act(ab, p), act(c, act(a, p)), "{a:?} then {c:?}");
                }
            }
        }
        assert_eq!(
            PhotoOrient::CW
                .then(PhotoOrient::CW)
                .then(PhotoOrient::CW)
                .then(PhotoOrient::CW),
            PhotoOrient::IDENTITY
        );
    }

    #[test]
    fn unit_to_master_maps_the_derived_corners() {
        let ops = PhotoOps {
            ops: vec![
                PhotoOp::Crop(PixelRect {
                    x: 10,
                    y: 20,
                    width: 30,
                    height: 40,
                }),
                PhotoOp::Orient(PhotoOrient::CW),
            ],
        };
        assert_eq!(ops.derived_size(100, 100), (40, 30));
        let m = ops.unit_to_master(100, 100).unwrap();
        let at = |u: f64, v: f64| (m[0] * u + m[2] * v + m[4], m[1] * u + m[3] * v + m[5]);
        // Turned clockwise: the derived top-left is the crop's bottom-left.
        assert_eq!(at(0.0, 0.0), (10.0, 60.0));
        assert_eq!(at(1.0, 0.0), (10.0, 20.0));
        assert_eq!(at(0.0, 1.0), (40.0, 60.0));
    }

    #[test]
    fn a_crop_keeps_the_kept_pixels_where_they_were() {
        let node = BitmapNode {
            image: crate::resources::BitmapId::default(),
            origin: Point::raw(0, 100_000),
            major: Vector::new(Mp::new(100_000), Mp::ZERO),
            minor: Vector::new(Mp::ZERO, Mp::new(-100_000)),
            photo_ops: PhotoOps::new(),
        };
        let crop = PhotoOps {
            ops: vec![PhotoOp::Crop(PixelRect {
                x: 25,
                y: 50,
                width: 50,
                height: 25,
            })],
        };
        let (o, u, v) = replaced_placement(&node, &PhotoOps::new(), &crop, (100, 100)).unwrap();
        assert_eq!(o, Point::raw(25_000, 50_000));
        assert_eq!(u, Vector::new(Mp::new(50_000), Mp::ZERO));
        assert_eq!(v, Vector::new(Mp::ZERO, Mp::new(-25_000)));
        // Turning it keeps the centre and swaps the size.
        let turned = crop.with(PhotoOp::Orient(PhotoOrient::CW));
        let n2 = BitmapNode {
            origin: o,
            major: u,
            minor: v,
            photo_ops: Default::default(),
            ..node
        };
        let (o2, u2, v2) = replaced_placement(&n2, &crop, &turned, (100, 100)).unwrap();
        assert_eq!(u2, Vector::new(Mp::new(25_000), Mp::ZERO));
        assert_eq!(v2, Vector::new(Mp::ZERO, Mp::new(-50_000)));
        assert_eq!(o2, Point::raw(37_500, 62_500));
    }
}
