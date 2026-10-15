//! Where the `.xarast` writer places text for browsers (XARA-T-0172,
//! `research/06 §6.7`).
//!
//! The format crate knows no fonts; it asks a
//! [`TextPlacer`] for the position of
//! every character so that the SVG base shows a story where Xarast draws
//! it. This one lays the story out exactly as the walker does (the same
//! attribute bridge, the same shaper, the same fit onto a path) and
//! reports, per character item, the left edge of its cluster box on its
//! baseline. A cluster of several characters (a ligature, a base with
//! marks) shares its box evenly: a browser draws each character on its
//! own, so that is the nearest it can get.
//!
//! A story on a path whose fit is a rotation per character
//! ([`PathFit::is_plain`]) is placed along the path instead (T9.5.6):
//! each character's glyph origin carried onto the path by
//! [`PathFit::cluster_transform`], and the angle it turns there.
//!
//! It also says which faces each story is drawn with and which characters
//! each draws, and makes their WOFF2 subsets for the writer's `@font-face`
//! rules (`research/06 §6.7` rules 2–3), through the process's shared
//! embedding layer (`xarast_text::embed`, the same one PDF export uses):
//! a face whose `OS/2.fsType` forbids embedding gets no file, and the
//! families that asked for it are listed so that their runs say
//! `xarast:font-embed="denied"`.
//!
//! A document that embeds faces is placed with its overlay
//! ([`fonts::for_document`]), so a re-save of a document opened on a
//! machine without its faces embeds them again, from its own subsets.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use xarast_doc::{AttrStack, Document, NodeId, NodeKind, StoryText};
use xarast_format::svg::{FontFile, PlacedFace, Placer, StoryPlacement, TextPlacer};
use xarast_geom::Mp;
use xarast_text::{EmbedError, FaceId, FontStyle, PathFit, StoryInput};

use crate::fonts::{self, FontService};
use crate::text::{layout_text, path_fit, story_input};

/// Lays stories out with a [`FontService`] for the SVG writer.
pub struct SvgTextPlacer {
    /// The base service; a document's stories are placed with
    /// [`fonts::for_document`] over it.
    fonts: Arc<FontService>,
    /// The faces handed out as [`PlacedFace::key`]s, with the service
    /// they belong to.
    faces: Mutex<HashMap<u64, (Arc<FontService>, FaceId)>>,
}

impl std::fmt::Debug for SvgTextPlacer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SvgTextPlacer")
    }
}

impl SvgTextPlacer {
    /// A placer over `fonts`.
    #[must_use]
    pub fn new(fonts: Arc<FontService>) -> SvgTextPlacer {
        SvgTextPlacer {
            fonts,
            faces: Mutex::default(),
        }
    }

    /// The face as the writer knows it, remembered for [`TextPlacer::font_file`].
    fn placed_face(&self, fonts: &Arc<FontService>, face: FaceId) -> Option<PlacedFace> {
        let info = fonts.db().face_info(face)?;
        let mut faces = self.faces.lock().unwrap_or_else(PoisonError::into_inner);
        // Faces of one service keep their index; another service's (a
        // second document written with this placer) are numbered after.
        let mut key = u64::from(face.index());
        while let Some((f, id)) = faces.get(&key) {
            if Arc::ptr_eq(f, fonts) && *id == face {
                break;
            }
            key = key.wrapping_add(1 << 32);
        }
        faces.insert(key, (Arc::clone(fonts), face));
        drop(faces);
        Some(PlacedFace {
            family: info.family,
            weight: info.weight,
            italic: info.style != FontStyle::Normal,
            key,
        })
    }
}

/// The placer over the process's font service ([`fonts::shared`]), ready
/// for [`xarast_format::svg::SvgOptions::text`].
#[must_use]
pub fn placer() -> Placer {
    Placer(Arc::new(SvgTextPlacer::new(fonts::shared())))
}

/// A millipoint value, clamped to what an `Mp` holds.
fn mp_of(v: f64) -> Mp {
    let v = v.round();
    if v.is_nan() {
        return Mp::ZERO;
    }
    Mp::new(v.clamp(f64::from(i32::MIN + 1), f64::from(i32::MAX)) as i32)
}

impl TextPlacer for SvgTextPlacer {
    fn place(
        &self,
        doc: &Document,
        story: NodeId,
        attrs: &mut AttrStack,
    ) -> Option<StoryPlacement> {
        let Some(NodeKind::TextStory(node)) = doc.tree.kind(story) else {
            return None;
        };
        let st = StoryText::collect(&doc.tree, story, attrs, &mut |_, a| {
            Arc::new(a.value.clone())
        })?;
        let mut input = story_input(&doc.tree, &st, node);
        // Along a path the story is laid out as the walker lays it out: a
        // column as long as the path, then carried onto it. A fit SVG
        // cannot say per character (reflected or sheared) stays straight.
        let fit = path_fit(&doc.tree, story, node).filter(PathFit::is_plain);
        if let Some(f) = &fit {
            input.mode = f.story_mode();
        }
        let fonts = fonts::for_document(&self.fonts, doc);
        let layout = fonts.ready().layout(&StoryInput {
            text: layout_text(&st),
            runs: &input.runs,
            paragraphs: &input.paragraphs,
            kerns: &input.kerns,
            mode: input.mode,
        });
        // Byte offset of each character item.
        let mut item_at: HashMap<usize, NodeId> = HashMap::new();
        for e in &st.items {
            if e.len > 0 {
                item_at.insert(e.byte as usize, e.node);
            }
        }
        let mut placement = StoryPlacement {
            along_path: fit.is_some(),
            ..StoryPlacement::default()
        };
        for line in &layout.lines {
            // The baseline of each cluster, shifts included: its first
            // glyph's y.
            let mut ys: HashMap<usize, Mp> = HashMap::new();
            for run in &line.runs {
                for g in &run.glyphs {
                    ys.entry(g.cluster).or_insert(g.y);
                }
            }
            for c in &line.clusters {
                let Some(text) = st.text.get(c.range.clone()) else {
                    continue;
                };
                let n = text.chars().count().max(1) as f64;
                let y = ys.get(&c.range.start).copied().unwrap_or(line.baseline_y);
                // Along a path: where the cluster's glyphs start, carried
                // onto the path, and the angle they turn by there.
                let onto = fit.as_ref().map(|f| {
                    let xf = f.cluster_transform(line, c);
                    let [a, b, ..] = xf.as_coeffs();
                    (xf, b.atan2(a).to_degrees())
                });
                for (k, (off, _)) in text.char_indices().enumerate() {
                    let Some(node) = item_at.get(&(c.range.start + off)) else {
                        continue;
                    };
                    let (x, y) = match onto {
                        Some((xf, turn)) => {
                            let x = c.pen.to_f64() + c.advance.to_f64() * k as f64 / n;
                            let p = xf * kurbo::Point::new(x, y.to_f64());
                            if turn != 0.0 {
                                placement.rotations.insert(*node, turn);
                            }
                            (mp_of(p.x), mp_of(p.y))
                        }
                        None => {
                            let dx = i64::from(c.width.raw()) * k as i64 / n as i64;
                            let x = i64::from(c.x.raw()) + dx;
                            let x = x.clamp(i64::from(i32::MIN + 1), i64::from(i32::MAX));
                            (Mp::new(x as i32), y)
                        }
                    };
                    placement.chars.insert(*node, (x, y));
                }
            }
        }
        placement.substitutions = layout
            .substitutions
            .iter()
            .map(|s| (Arc::clone(&s.requested), Arc::clone(&s.used)))
            .collect();
        // The faces drawn with, the characters each draws, and the face of
        // each character item.
        let mut used: BTreeMap<FaceId, BTreeSet<char>> = BTreeMap::new();
        let mut item_face: HashMap<NodeId, FaceId> = HashMap::new();
        for line in &layout.lines {
            for run in &line.runs {
                let set = used.entry(run.face).or_default();
                for g in &run.glyphs {
                    let Some(c) = line.cluster_at(g.cluster) else {
                        continue;
                    };
                    let Some(t) = st.text.get(c.range.clone()) else {
                        continue;
                    };
                    set.extend(t.chars().filter(|c| !c.is_control()));
                    for (off, _) in t.char_indices() {
                        if let Some(node) = item_at.get(&(c.range.start + off)) {
                            item_face.entry(*node).or_insert(run.face);
                        }
                    }
                }
            }
        }
        let mut index: HashMap<FaceId, usize> = HashMap::new();
        for (face, chars) in used {
            if chars.is_empty() {
                continue;
            }
            if let Some(f) = self.placed_face(&fonts, face) {
                index.insert(face, placement.faces.len());
                placement.faces.push((f, chars.into_iter().collect()));
            }
        }
        placement.char_faces = item_face
            .into_iter()
            .filter_map(|(node, face)| Some((node, *index.get(&face)?)))
            .collect();
        // The requested families whose own face refuses embedding.
        let db = fonts.db();
        for r in &input.runs {
            if placement.denied.contains(&r.font.family) {
                continue;
            }
            if let Some(m) = db.query_with_panose(&r.font, r.panose)
                && db.embedding_denied(m.face)
            {
                placement.denied.push(Arc::clone(&r.font.family));
            }
        }
        Some(placement)
    }

    fn outlines(&self, doc: &Document, story: NodeId) -> Option<Vec<xarast_doc::OutlineRun>> {
        let fonts = fonts::for_document(&self.fonts, doc);
        crate::text::story_outlines(&fonts, doc, story).map(|(runs, _)| runs)
    }

    fn font_file(&self, face: &PlacedFace, chars: &[char]) -> Option<FontFile> {
        let (fonts, id) = self
            .faces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&face.key)
            .cloned()?;
        Some(match fonts.web_font(id, chars) {
            Ok(w) => FontFile::Woff2(w.woff2),
            Err(e @ EmbedError::Denied(_)) => FontFile::Denied(Arc::from(e.to_string())),
            Err(e) => FontFile::Unavailable(Arc::from(e.to_string())),
        })
    }
}
