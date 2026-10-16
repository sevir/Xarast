//! The attribute scope stack and its snapshot.
//!
//! A dense table of the values in force, an unwind log of
//! `(slot, previous value)` and a vector of scope marks. About sixty lines,
//! and it replaces the original's render stack, its current-attribute table
//! and its attribute-node dispatch entirely.

use std::sync::Arc;

use xarast_geom::Mp;

use super::{ATTR_SLOT_COUNT, AttrSlot, AttrValue, DefaultAttrs};

#[derive(Copy, Clone, Debug)]
struct ScopeMark {
    undo_len: usize,
    multi_len: usize,
}

/// The attribute state in force, plus the scopes that will restore it.
#[derive(Clone, Debug)]
pub struct AttrStack {
    current: Box<[Arc<AttrValue>; ATTR_SLOT_COUNT]>,
    multi: Vec<Arc<AttrValue>>,
    undo: Vec<(u16, Arc<AttrValue>)>,
    scopes: Vec<ScopeMark>,
}

impl AttrStack {
    /// Starts from the document's defaults.
    #[must_use]
    pub fn with_defaults(defaults: &DefaultAttrs) -> AttrStack {
        AttrStack {
            current: Box::new(defaults.slots().clone()),
            multi: Vec::new(),
            undo: Vec::new(),
            scopes: Vec::new(),
        }
    }

    /// The value in force for a slot.
    #[inline]
    #[must_use]
    pub fn get(&self, slot: AttrSlot) -> &AttrValue {
        &self.current[slot as usize]
    }

    /// The multi-applicable attributes in force, in the order they were
    /// applied.
    #[inline]
    #[must_use]
    pub fn multi(&self) -> &[Arc<AttrValue>] {
        &self.multi
    }

    /// Applies a value: replaces its slot, or accumulates when it has none.
    pub fn push(&mut self, value: Arc<AttrValue>) {
        match value.slot() {
            Some(slot) => {
                let i = slot as usize;
                let prev = std::mem::replace(&mut self.current[i], value);
                self.undo.push((i as u16, prev));
            }
            None => self.multi.push(value),
        }
    }

    /// Call on [`WalkEvent::EnterScope`](crate::WalkEvent::EnterScope).
    #[inline]
    pub fn push_scope(&mut self) {
        self.scopes.push(ScopeMark {
            undo_len: self.undo.len(),
            multi_len: self.multi.len(),
        });
    }

    /// Call on [`WalkEvent::LeaveScope`](crate::WalkEvent::LeaveScope).
    ///
    /// Unwinding an unbalanced `pop_scope` is a no-op rather than a panic: a
    /// truncated file is a real thing, and the model must survive one.
    pub fn pop_scope(&mut self) {
        let Some(mark) = self.scopes.pop() else {
            return;
        };
        while self.undo.len() > mark.undo_len {
            let Some((i, prev)) = self.undo.pop() else {
                break;
            };
            self.current[i as usize] = prev;
        }
        self.multi.truncate(mark.multi_len);
    }

    /// How deep the scope stack is.
    #[inline]
    #[must_use]
    pub fn depth(&self) -> usize {
        self.scopes.len()
    }

    /// How far the painted outline reaches beyond the geometry, without
    /// taking a snapshot first.
    ///
    /// The whole-document bounds pass asks this for every node, so it must
    /// not allocate.
    #[must_use]
    pub fn stroke_extent(&self) -> Mp {
        stroke_extent_of(|s| &self.current[s as usize])
    }

    /// A photograph of the state in force.
    #[must_use]
    pub fn snapshot(&self) -> ResolvedAttrs {
        ResolvedAttrs {
            slots: Arc::new(*self.current.clone()),
            multi: Arc::from(self.multi.clone()),
        }
    }
}

/// Immutable resolved attribute state.
///
/// Cloning is two atomic increments: the slot table and the multi list are
/// each behind one [`Arc`], which matters because the resolver hands these out
/// per node.
#[derive(Clone, Debug)]
pub struct ResolvedAttrs {
    slots: Arc<[Arc<AttrValue>; ATTR_SLOT_COUNT]>,
    multi: Arc<[Arc<AttrValue>]>,
}

impl ResolvedAttrs {
    /// The value in force for a slot.
    #[inline]
    #[must_use]
    pub fn get(&self, slot: AttrSlot) -> &AttrValue {
        &self.slots[slot as usize]
    }

    /// The multi-applicable attributes in force.
    #[inline]
    #[must_use]
    pub fn multi(&self) -> &[Arc<AttrValue>] {
        &self.multi
    }

    /// How far the painted outline reaches beyond the geometry.
    ///
    /// Half the line width, widened by the mitre limit at a mitred join and by
    /// the feather size, because both enlarge the bounding box.
    #[must_use]
    pub fn stroke_extent(&self) -> Mp {
        stroke_extent_of(|s| self.get(s))
    }

    /// [`stroke_extent`](Self::stroke_extent) once `over` is applied on
    /// top (the last value of a slot wins), without building a snapshot.
    #[must_use]
    pub fn stroke_extent_over(&self, over: &[AttrValue]) -> Mp {
        stroke_extent_of(|s| {
            over.iter()
                .rev()
                .find(|v| v.slot() == Some(s))
                .unwrap_or_else(|| self.get(s))
        })
    }

    /// The colour fill in force.
    #[must_use]
    pub fn fill(&self) -> &crate::fill::Paint {
        match self.get(AttrSlot::FillGeometry) {
            AttrValue::Fill(p) => p,
            _ => unreachable!("slot FillGeometry always holds AttrValue::Fill"),
        }
    }

    /// The outline colour in force.
    #[must_use]
    pub fn stroke(&self) -> &crate::fill::Paint {
        match self.get(AttrSlot::StrokeColour) {
            AttrValue::StrokeColour(p) => p,
            _ => unreachable!("slot StrokeColour always holds AttrValue::StrokeColour"),
        }
    }
}

impl PartialEq for ResolvedAttrs {
    fn eq(&self, other: &ResolvedAttrs) -> bool {
        if Arc::ptr_eq(&self.slots, &other.slots) && Arc::ptr_eq(&self.multi, &other.multi) {
            return true;
        }
        self.slots
            .iter()
            .zip(other.slots.iter())
            .all(|(a, b)| a == b)
            && self.multi.len() == other.multi.len()
            && self
                .multi
                .iter()
                .zip(other.multi.iter())
                .all(|(a, b)| a == b)
    }
}

/// The one place the stroke extent is worked out, so that the stack and a
/// snapshot cannot disagree about how wide a stroke paints.
fn stroke_extent_of<'a>(get: impl Fn(AttrSlot) -> &'a AttrValue) -> Mp {
    let width = match get(AttrSlot::LineWidth) {
        AttrValue::LineWidth(w) => *w,
        _ => Mp::ZERO,
    };
    let mut extent = Mp::new(width.raw() / 2);
    if let AttrValue::JoinType(xarast_geom::Join::Mitre) = get(AttrSlot::JoinType)
        && let AttrValue::MitreLimit(limit) = get(AttrSlot::MitreLimit)
    {
        let scaled = extent.to_f64() * (limit.to_f64() / 1000.0).max(1.0);
        extent = Mp::from_f64_round(scaled);
    }
    if let AttrValue::Feather { size, .. } = get(AttrSlot::Feather) {
        extent = extent.saturating_add(*size);
    }
    extent
}
