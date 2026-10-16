//! Placing a bitmap object (phase 10, T10.3.6): a [`BitmapNode`] with the
//! attributes every new one gets.
//!
//! A placed bitmap is a parallelogram showing an image (`research/02
//! §8.6`). The original gives every new one the same three attributes as
//! its own children (`NodeBitmap::ApplyDefaultBitmapAttrs`,
//! `Kernel/nodebmp.cpp:997-1055`, facts only): **no line colour, no fill
//! colour and a zero line width**. The two colours are "none" because a
//! bitmap object reads them as its contone pair, and none means the image
//! shows as it is; the zero width keeps an outline from appearing if a
//! colour is given later. So the object does *not* inherit the layer's
//! black outline, and a colour dropped on it later is a deliberate edit.
//! (`research/02 §8.6` says the defaults apply a bitmap *fill*; the code
//! applies the three attributes above — the object draws its image
//! itself.)
//!
//! Its natural size is `pixels × 72 000 / dpi` millipoints per axis
//! ([`crate::bitmap_fill::natural_length`]).

use xarast_color::Colour;
use xarast_geom::{Mp, Point, Vector};

use crate::attr::{AttrNode, AttrValue};
use crate::fill::FillGeometry;
use crate::history::{Command, EditError, Tx};
use crate::kind::{BitmapNode, NodeKind};
use crate::resources::BitmapId;
use crate::tree::{Attach, NodeId};

/// "No colour": paint nothing.
fn no_colour() -> Colour {
    Colour::None
}

/// The attributes every new bitmap object carries as its own children: no
/// line colour, no fill colour, a zero line width.
#[must_use]
pub fn default_bitmap_attrs() -> Vec<AttrValue> {
    vec![
        AttrValue::StrokeColour(FillGeometry::Flat { value: no_colour() }),
        AttrValue::Fill(FillGeometry::Flat { value: no_colour() }),
        AttrValue::LineWidth(Mp::ZERO),
    ]
}

/// A bitmap object `width` × `height` millipoints, upright, centred on
/// `centre`. As the importer builds one, `origin` is the image's top-left
/// corner, `major` runs along its top edge and `minor` down its left edge.
#[must_use]
pub fn bitmap_node_centred(image: BitmapId, centre: Point, width: Mp, height: Mp) -> BitmapNode {
    let (w, h) = (width.raw().max(0), height.raw().max(0));
    let origin = Point::raw(
        centre.x.raw().saturating_sub(w / 2),
        centre.y.raw().saturating_add(h - h / 2),
    );
    BitmapNode {
        image,
        origin,
        major: Vector::new(Mp::new(w), Mp::ZERO),
        minor: Vector::new(Mp::ZERO, Mp::new(h.saturating_neg())),
        photo_ops: Default::default(),
    }
}

/// Places a bitmap object as the last object of a layer, carrying
/// [`default_bitmap_attrs`] — a drop or a paste of an image. One undo step;
/// the image resource itself is added to the document before, outside the
/// history, as a pasted fragment's bitmaps are.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaceBitmap {
    /// The layer it goes onto.
    pub layer: NodeId,
    /// The object.
    pub bitmap: BitmapNode,
    /// What the Edit menu calls the step ("Import Bitmap", "Paste").
    pub label: &'static str,
}

impl Command for PlaceBitmap {
    fn label(&self) -> &'static str {
        self.label
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        match tx.doc().tree.kind(self.layer) {
            Some(NodeKind::Layer(l)) if !l.locked && !l.guide => {}
            _ => return Err(EditError::NotPermitted(self.layer)),
        }
        if tx.doc().resources.bitmap(self.bitmap.image).is_none() {
            return Err(EditError::MissingResource(
                crate::resources::ResourceRef::Bitmap(self.bitmap.image),
            ));
        }
        let node = tx.create(NodeKind::Bitmap(Box::new(self.bitmap.clone())))?;
        tx.attach(node, self.layer, Attach::LastChild)?;
        for a in default_bitmap_attrs() {
            let attr = tx.create(NodeKind::Attr(Box::new(AttrNode::new(a))))?;
            tx.attach(attr, node, Attach::LastChild)?;
        }
        Ok(())
    }
}

/// Replaces the resource behind one [`BitmapId`] (the bitmap gallery's
/// Replace): one undo step, exact on undo. Every bitmap object, bitmap fill
/// and bitmap transparency that names the id keeps its geometry and shows
/// the new image; only those objects are marked changed
/// ([`crate::Action::ReplaceBitmap`]). Unlike inserting a resource this is
/// in the history.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplaceBitmap {
    /// The bitmap whose resource is swapped.
    pub image: BitmapId,
    /// What takes its place.
    pub with: crate::resources::BitmapResource,
}

impl Command for ReplaceBitmap {
    fn label(&self) -> &'static str {
        "Replace Bitmap"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        tx.replace_bitmap(self.image, self.with.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_centred_bitmap_is_upright_and_centred() {
        let b = bitmap_node_centred(
            BitmapId::default(),
            Point::raw(100_000, 50_000),
            Mp::new(72_000),
            Mp::new(36_001),
        );
        assert_eq!(b.origin, Point::raw(64_000, 68_001));
        assert_eq!(b.major, Vector::new(Mp::new(72_000), Mp::ZERO));
        assert_eq!(b.minor, Vector::new(Mp::ZERO, Mp::new(-36_001)));
        // The far corner is 36 001 mp below: the centre is half a
        // millipoint off, never more.
        let far = b.origin + b.major + b.minor;
        assert_eq!(far, Point::raw(136_000, 32_000));
    }
}
