//! Editing text on a path (phase 9, T9.5.5, XARA-T-0251): fitting a story
//! to a path, taking it off again, reversing it along the path, and
//! setting where it starts and ends on the path (the left and right
//! indents).
//!
//! Facts about the original are in `docs/memory/text.md`, "Editing text
//! on a path"; this is our own implementation of them:
//!
//! * **Fit** ([`TextPathEdit::Fit`]): the path (a shape or a quick shape
//!   becomes a path first) moves into the story, after the story's own
//!   attributes, keeping the attributes it was drawn with. The story's
//!   matrix is split ([`fit_placement`]): its rotation, shear and mirror go
//!   into the layout's [`CharsTransform`], and what is left stays in the
//!   matrix (a translation, plus any scale). A story turned past ±90° is
//!   reversed on the path instead, and its rotation turned by half a turn,
//!   so that its text reads upright along the path.
//! * **Remove** ([`TextPathEdit::Remove`]): the exact inverse
//!   ([`removed_placement`]): the characters' transform goes back into the
//!   matrix, the story becomes a point story, and the path moves out to sit
//!   just in front of it, keeping the attributes it was drawn with.
//! * **Reverse** ([`TextPathEdit::Reverse`]): toggles the layout's
//!   `reversed` flag: the text runs from the path's other end and hangs on
//!   its other side.
//! * **Indents** ([`TextPathEdit::Indents`]): sets both indents at once,
//!   the text tool's handle drag.
//!
//! Each is one undo step through the command bus, so the operation journal
//! records it too.

use xarast_doc::attr::{ALL_ATTR_SLOTS, resolve_uncached};
use xarast_doc::fill_edit::set_own_attr;
use xarast_doc::{
    Attach, CharsTransform, Document, EditError, NodeId, NodeKind, PathNode, TextLayout,
    TextStoryNode, Tx, is_text_slot,
};
use xarast_geom::{Matrix, Mp};

/// An edit of text on a path.
#[derive(Debug, Clone, PartialEq)]
pub enum TextPathEdit {
    /// Fits `story` (not on a path) to `path` (a path, a shape or a quick
    /// shape outside the story).
    Fit {
        /// The `TextStory`.
        story: NodeId,
        /// The object it is fitted to.
        path: NodeId,
    },
    /// Takes every story in the list off its path.
    Remove {
        /// The stories on a path.
        stories: Vec<NodeId>,
    },
    /// Reverses every story in the list along its path.
    Reverse {
        /// The stories on a path.
        stories: Vec<NodeId>,
    },
    /// Sets a story's left and right indents on its path, in story space.
    Indents {
        /// The story on a path.
        story: NodeId,
        /// The new left indent.
        left: Mp,
        /// The new right indent.
        right: Mp,
    },
}

/// The undo label of a fit.
pub const FIT_LABEL: &str = "Fit Text to Path";
/// The undo label of a removal.
pub const REMOVE_LABEL: &str = "Remove Text from Path";
/// The undo label of a reversal.
pub const REVERSE_LABEL: &str = "Reverse Text on Path";
/// The undo label of an indent drag.
pub const INDENT_LABEL: &str = "Path Indent";

impl TextPathEdit {
    /// The undo label.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            TextPathEdit::Fit { .. } => FIT_LABEL,
            TextPathEdit::Remove { .. } => REMOVE_LABEL,
            TextPathEdit::Reverse { .. } => REVERSE_LABEL,
            TextPathEdit::Indents { .. } => INDENT_LABEL,
        }
    }

    /// Whether the edit has nothing to act on.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        match self {
            TextPathEdit::Remove { stories } | TextPathEdit::Reverse { stories } => {
                stories.is_empty()
            }
            TextPathEdit::Fit { .. } | TextPathEdit::Indents { .. } => false,
        }
    }

    /// Every node it edits (for the locked-layer check).
    fn nodes(&self) -> Vec<NodeId> {
        match self {
            TextPathEdit::Fit { story, path } => vec![*story, *path],
            TextPathEdit::Remove { stories } | TextPathEdit::Reverse { stories } => stories.clone(),
            TextPathEdit::Indents { story, .. } => vec![*story],
        }
    }

    /// Applies the edit.
    pub(crate) fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        crate::ops::check_layers(tx, &self.nodes())?;
        match self {
            TextPathEdit::Fit { story, path } => fit(tx, *story, *path),
            TextPathEdit::Remove { stories } => {
                for &s in stories {
                    remove(tx, s)?;
                }
                Ok(())
            }
            TextPathEdit::Reverse { stories } => {
                for &s in stories {
                    let mut node = on_path(tx.doc(), s)?;
                    if let TextLayout::OnPath { reversed, .. } = &mut node.layout {
                        *reversed = !*reversed;
                    }
                    tx.set_kind(s, NodeKind::TextStory(Box::new(node)))?;
                }
                Ok(())
            }
            TextPathEdit::Indents { story, left, right } => {
                let mut node = on_path(tx.doc(), *story)?;
                if let TextLayout::OnPath {
                    left_indent,
                    right_indent,
                    ..
                } = &mut node.layout
                {
                    *left_indent = (*left).max(Mp::ZERO);
                    *right_indent = (*right).max(Mp::ZERO);
                }
                tx.set_kind(*story, NodeKind::TextStory(Box::new(node)))
            }
        }
    }
}

/// The story node of `story` when it is on a path.
fn on_path(doc: &Document, story: NodeId) -> Result<TextStoryNode, EditError> {
    match doc.tree.kind(story) {
        Some(NodeKind::TextStory(t)) if matches!(t.layout, TextLayout::OnPath { .. }) => {
            Ok((**t).clone())
        }
        _ => Err(EditError::WrongKind(story)),
    }
}

/// Whether `node` is a story on a path.
#[must_use]
pub fn is_on_path(doc: &Document, node: NodeId) -> bool {
    on_path(doc, node).is_ok()
}

/// What a fit needs from the selection: exactly one story that is not on
/// a path, and exactly one other object that can become a path (a path, a
/// shape or a quick shape) and is not inside the story. `None` otherwise.
#[must_use]
pub fn fit_target(doc: &Document, selection: &[NodeId]) -> Option<(NodeId, NodeId)> {
    let mut story = None;
    let mut path = None;
    for &n in selection {
        match doc.tree.kind(n) {
            Some(NodeKind::TextStory(t)) => {
                if story.is_some() || matches!(t.layout, TextLayout::OnPath { .. }) {
                    return None;
                }
                story = Some(n);
            }
            Some(NodeKind::Path(_) | NodeKind::Shape(_) | NodeKind::QuickShape(_)) => {
                if path.is_some() {
                    return None;
                }
                path = Some(n);
            }
            _ => return None,
        }
    }
    let (story, path) = (story?, path?);
    if doc.tree.is_ancestor(story, path) {
        return None;
    }
    Some((story, path))
}

/// The selected stories that are on a path.
#[must_use]
pub fn stories_on_path(doc: &Document, selection: &[NodeId]) -> Vec<NodeId> {
    selection
        .iter()
        .copied()
        .filter(|&n| is_on_path(doc, n))
        .collect()
}

/// The path a story on a path follows: its first path child.
#[must_use]
pub fn story_path(doc: &Document, story: NodeId) -> Option<NodeId> {
    doc.tree
        .children(story)
        .find(|&c| matches!(doc.tree.kind(c), Some(NodeKind::Path(_))))
}

// ── the matrix ────────────────────────────────────────────────────────

/// The linear part of a matrix as columns: `[a, b, c, d]` maps x to
/// `(a, b)` and y to `(c, d)`.
type Lin = [f64; 4];

/// `p · q`: `q` first, then `p`.
fn mul(p: Lin, q: Lin) -> Lin {
    [
        p[0] * q[0] + p[2] * q[1],
        p[1] * q[0] + p[3] * q[1],
        p[0] * q[2] + p[2] * q[3],
        p[1] * q[2] + p[3] * q[3],
    ]
}

fn inverse(p: Lin) -> Option<Lin> {
    let det = p[0] * p[3] - p[1] * p[2];
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    Some([p[3] / det, -p[1] / det, -p[2] / det, p[0] / det])
}

fn rotation(r: f64) -> Lin {
    let (s, c) = r.sin_cos();
    [c, s, -s, c]
}

/// The characters' transform before the fit, as the fit applies it
/// (`xarast_text::PathFit`): a shear, then a reflection about the
/// baseline.
fn chars_matrix(chars: CharsTransform) -> Lin {
    let k = if chars.reflected { -1.0 } else { 1.0 };
    let t = CharsTransform::radians(chars.shear).tan();
    [1.0, 0.0, k * t, k]
}

/// The rotation the characters had before the fit, from the stored one:
/// a reversed story carries it turned by half a turn (the original's
/// rule, `text.md`).
fn unreversed_rotation(chars: CharsTransform, reversed: bool) -> f64 {
    let r = CharsTransform::radians(chars.rotation);
    if !reversed {
        r
    } else if r > 0.0 {
        r - std::f64::consts::PI
    } else {
        r + std::f64::consts::PI
    }
}

/// What the characters' transform and the rotation make together: the
/// part of the story's matrix a fit takes out of it.
fn taken(chars: CharsTransform, reversed: bool) -> Lin {
    mul(
        rotation(unreversed_rotation(chars, reversed)),
        chars_matrix(chars),
    )
}

fn with_lin(m: Matrix, l: Lin) -> Matrix {
    Matrix {
        a: l[0],
        b: l[1],
        c: l[2],
        d: l[3],
        ..m
    }
}

/// Splits a straight story's matrix for fitting it to a path: returns the
/// matrix that stays on the story, whether the text goes on reversed, and
/// the characters' transform (mirror, rotation, shear).
///
/// The rotation is the angle of the matrix's x axis; text turned past
/// ±90° is reversed instead and the rotation turned by half a turn. The
/// mirror is the sign of the determinant, and the shear is what remains
/// above the diagonal once the rotation is taken out, as the fit applies
/// it. The remaining matrix is the original one with the rotation and the
/// characters' transform (as stored, rounded to 16.16) taken off, so
/// [`removed_placement`] gives the original back to rounding error: the
/// translation is kept exactly, and a uniform scale stays as a scale.
/// `None` for a singular matrix.
#[must_use]
pub fn fit_placement(m: Matrix) -> Option<(Matrix, bool, CharsTransform)> {
    let lin: Lin = [m.a, m.b, m.c, m.d];
    let len = m.a.hypot(m.b);
    let det = m.a * m.d - m.b * m.c;
    if !len.is_finite() || len < 1e-12 || !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let theta = m.b.atan2(m.a);
    // The matrix with its rotation taken out is upper triangular:
    // [[len, p], [0, s]], s the signed scale, p the shear term.
    let s = det / len;
    let (sin, cos) = theta.sin_cos();
    let p = cos * m.c + sin * m.d;
    let shear = (p / s).atan();
    let half = std::f64::consts::FRAC_PI_2;
    let (reversed, rot) = if theta < -half || theta > half {
        let turned = if theta > 0.0 {
            theta - std::f64::consts::PI
        } else {
            theta + std::f64::consts::PI
        };
        (true, turned)
    } else {
        (false, theta)
    };
    let chars = CharsTransform {
        reflected: s < 0.0,
        rotation: CharsTransform::fixed(rot),
        shear: CharsTransform::fixed(shear),
    };
    let rest = mul(lin, inverse(taken(chars, reversed))?);
    if rest.iter().any(|v| !v.is_finite()) {
        return None;
    }
    Some((with_lin(m, rest), reversed, chars))
}

/// The matrix of a story taken off its path: its matrix with the
/// characters' transform and their rotation put back (the inverse of
/// [`fit_placement`]).
#[must_use]
pub fn removed_placement(m: Matrix, reversed: bool, chars: CharsTransform) -> Matrix {
    with_lin(m, mul([m.a, m.b, m.c, m.d], taken(chars, reversed)))
}

// ── the edits ─────────────────────────────────────────────────────────

/// Moves `node` to `anchor`/`how`, giving it as its own attributes those
/// (other than text attributes) it was drawn with and would no longer
/// inherit there, so that it looks the same.
fn move_keeping_look(
    tx: &mut Tx<'_>,
    node: NodeId,
    anchor: NodeId,
    how: Attach,
) -> Result<(), EditError> {
    let before = resolve_uncached(&tx.doc().tree, node, &tx.doc().defaults);
    tx.move_node(node, anchor, how)?;
    let after = resolve_uncached(&tx.doc().tree, node, &tx.doc().defaults);
    for &slot in &ALL_ATTR_SLOTS {
        if is_text_slot(slot) {
            continue;
        }
        let (was, now) = (before.get(slot), after.get(slot));
        if was != now {
            set_own_attr(tx, node, was.clone())?;
        }
    }
    Ok(())
}

fn fit(tx: &mut Tx<'_>, story: NodeId, path: NodeId) -> Result<(), EditError> {
    let node = match tx.doc().tree.kind(story) {
        Some(NodeKind::TextStory(t)) if !matches!(t.layout, TextLayout::OnPath { .. }) => {
            (**t).clone()
        }
        _ => return Err(EditError::WrongKind(story)),
    };
    if tx.doc().tree.is_ancestor(story, path) || path == story {
        return Err(EditError::WrongKind(path));
    }
    // A shape or a quick shape becomes the path of its outline first (the
    // original converts whatever it fits to into a path).
    match tx.doc().tree.kind(path) {
        Some(NodeKind::Path(_)) => {}
        Some(kind @ (NodeKind::Shape(_) | NodeKind::QuickShape(_))) => {
            let outline = crate::picking::geometry_of(kind).ok_or(EditError::WrongKind(path))?;
            tx.set_kind(
                path,
                NodeKind::Path(Box::new(PathNode::new((*outline).clone()))),
            )?;
        }
        _ => return Err(EditError::WrongKind(path)),
    }
    let (matrix, reversed, chars) =
        fit_placement(node.transform).ok_or(EditError::WrongKind(story))?;
    // After the story's own attributes, before its lines.
    let tree = &tx.doc().tree;
    let first_ink = tree
        .children(story)
        .find(|&c| !matches!(tree.kind(c), Some(NodeKind::Attr(_))));
    match first_ink {
        Some(c) => move_keeping_look(tx, path, c, Attach::Prev)?,
        None => move_keeping_look(tx, path, story, Attach::LastChild)?,
    }
    let new = TextStoryNode {
        transform: matrix,
        layout: TextLayout::OnPath {
            reversed,
            tangential: true,
            left_indent: Mp::ZERO,
            right_indent: Mp::ZERO,
            chars,
        },
        ..node
    };
    tx.set_kind(story, NodeKind::TextStory(Box::new(new)))
}

fn remove(tx: &mut Tx<'_>, story: NodeId) -> Result<(), EditError> {
    let node = on_path(tx.doc(), story)?;
    let TextLayout::OnPath {
        reversed, chars, ..
    } = node.layout
    else {
        return Err(EditError::WrongKind(story));
    };
    if let Some(path) = story_path(tx.doc(), story) {
        move_keeping_look(tx, path, story, Attach::Next)?;
    }
    let new = TextStoryNode {
        transform: removed_placement(node.transform, reversed, chars),
        layout: TextLayout::AtPoint,
        ..node
    };
    tx.set_kind(story, NodeKind::TextStory(Box::new(new)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use xarast_geom::Vector;

    fn close(a: Matrix, b: Matrix) -> bool {
        let eq = |x: f64, y: f64| (x - y).abs() < 1e-4;
        eq(a.a, b.a) && eq(a.b, b.b) && eq(a.c, b.c) && eq(a.d, b.d) && a.e == b.e && a.f == b.f
    }

    fn at(x: i32, y: i32) -> Matrix {
        Matrix::translate(Vector::new(Mp::new(x), Mp::new(y)))
    }

    #[test]
    fn a_translation_stays_and_nothing_goes_into_the_characters() {
        let m = at(1000, 2000);
        let (rest, reversed, chars) = fit_placement(m).unwrap();
        assert!(close(rest, m));
        assert!(!reversed);
        assert!(chars.is_identity());
        assert!(close(removed_placement(rest, reversed, chars), m));
    }

    #[test]
    fn rotation_shear_and_mirror_go_into_the_characters_and_come_back() {
        for (deg, shear, mirror) in [
            (30.0, 0.0, false),
            (-60.0, 0.2, false),
            (10.0, -0.3, true),
            (89.0, 0.1, true),
        ] {
            let r = f64::to_radians(deg);
            let m = Matrix::skew(shear, 0.0)
                .then(Matrix::scale(1.0, if mirror { -1.0 } else { 1.0 }))
                .then(Matrix::rotate(r))
                .then(at(5000, -7000));
            let (rest, reversed, chars) = fit_placement(m).unwrap();
            assert!(!reversed, "{deg}");
            assert_eq!(chars.reflected, mirror);
            assert!((CharsTransform::radians(chars.rotation) - r).abs() < 1e-4);
            // What stays is the translation alone.
            assert!(close(rest, at(5000, -7000)), "{deg}: {rest:?}");
            assert!(close(removed_placement(rest, reversed, chars), m), "{deg}");
        }
    }

    #[test]
    fn past_a_right_angle_the_text_is_reversed_and_turned_half_a_turn() {
        for deg in [120.0, -135.0, 180.0, -91.0] {
            let r = f64::to_radians(deg);
            let m = Matrix::rotate(r).then(at(100, 200));
            let (rest, reversed, chars) = fit_placement(m).unwrap();
            assert!(reversed, "{deg}");
            let stored = CharsTransform::radians(chars.rotation);
            assert!(stored.abs() <= std::f64::consts::FRAC_PI_2 + 1e-9, "{deg}");
            assert!(close(rest, at(100, 200)), "{deg}: {rest:?}");
            assert!(close(removed_placement(rest, reversed, chars), m), "{deg}");
        }
    }

    #[test]
    fn a_uniform_scale_stays_in_the_matrix() {
        let m = Matrix::scale(2.0, 2.0)
            .then(Matrix::rotate(0.5))
            .then(at(10, 20));
        let (rest, _, chars) = fit_placement(m).unwrap();
        assert!(close(rest, Matrix::scale(2.0, 2.0).then(at(10, 20))));
        assert!(close(removed_placement(rest, false, chars), m));
        assert!(fit_placement(Matrix::scale(0.0, 1.0)).is_none());
    }
}
