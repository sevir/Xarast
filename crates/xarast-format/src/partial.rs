//! Partial recovery of a malformed `document.svg` (phase 6 F6.9).
//!
//! The strict SVG reader refuses malformed XML. A document that was cut off
//! by a crash, a full disk or a bad sync is still mostly there, so the
//! recovering open ([`crate::OpenOptions::recover`]) keeps the longest
//! well-formed prefix and closes the elements still open at that point: the
//! result is parsed by the same reader as any other document, and what comes
//! after the damage is lost.
//!
//! [`close_partial`] is a small tag scanner, not a second XML parser: it only
//! has to find where the tags stop making sense. Whatever it keeps still goes
//! through the real parser, which has the final word (namespaces, entities,
//! limits).

#![deny(clippy::arithmetic_side_effects)]

/// A well-formed prefix of a damaged document, closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partial {
    /// The repaired document: the prefix plus the end tags it was missing.
    pub bytes: Vec<u8>,
    /// How many bytes of the input were kept.
    pub kept: usize,
    /// How many end tags were appended.
    pub closed: usize,
}

/// The index of `pat` in `s` at or after `from`.
fn find(s: &str, from: usize, pat: &str) -> Option<usize> {
    s.get(from..)?.find(pat).and_then(|p| p.checked_add(from))
}

/// The end (exclusive) of the tag that starts at `at`, honouring quotes.
fn tag_end(s: &str, at: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (i, b) in s.as_bytes().get(at..)?.iter().enumerate() {
        match (quote, *b) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(*b),
            (None, b'>') => return i.checked_add(at)?.checked_add(1),
            _ => {}
        }
    }
    None
}

fn name_of(tag_body: &str) -> &str {
    let end = tag_body
        .find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
        .unwrap_or(tag_body.len());
    tag_body.get(..end).unwrap_or_default()
}

/// Keeps the longest prefix of `svg` whose tags nest properly, and closes it.
///
/// Returns `None` when there is no root element to keep, when the input has a
/// DOCTYPE (never accepted, `research/06 §5.3`), or when it is nested deeper
/// than `max_depth`.
#[must_use]
pub fn close_partial(svg: &[u8], max_depth: usize) -> Option<Partial> {
    let text = match std::str::from_utf8(svg) {
        Ok(t) => t,
        Err(e) => std::str::from_utf8(svg.get(..e.valid_up_to())?).ok()?,
    };
    let mut stack: Vec<&str> = Vec::new();
    let mut good = 0usize;
    let mut root_seen = false;
    let mut pos = 0usize;
    while let Some(lt) = find(text, pos, "<") {
        let rest = text.get(lt..)?;
        let end = if rest.starts_with("<?") {
            find(text, lt, "?>").and_then(|e| e.checked_add(2))
        } else if rest.starts_with("<!--") {
            find(text, lt, "-->").and_then(|e| e.checked_add(3))
        } else if rest.starts_with("<![CDATA[") {
            find(text, lt, "]]>").and_then(|e| e.checked_add(3))
        } else if rest.starts_with("<!") {
            return None;
        } else if rest.starts_with("</") {
            let Some(e) = tag_end(text, lt) else { break };
            let body = text.get(lt.checked_add(2)?..e.checked_sub(1)?)?;
            if stack.last().copied() != Some(body.trim_end()) {
                break;
            }
            stack.pop();
            Some(e)
        } else {
            let Some(e) = tag_end(text, lt) else { break };
            let body = text.get(lt.checked_add(1)?..e.checked_sub(1)?)?;
            let name = name_of(body);
            if name.is_empty() || (stack.is_empty() && root_seen) {
                break;
            }
            if !body.ends_with('/') {
                if stack.len() >= max_depth {
                    break;
                }
                stack.push(name);
            }
            root_seen = true;
            Some(e)
        };
        let Some(e) = end else { break };
        good = e;
        pos = e;
        if root_seen && stack.is_empty() {
            break; // the root closed: anything after is trailing
        }
    }
    if !root_seen {
        return None;
    }
    let mut bytes = text.get(..good)?.as_bytes().to_vec();
    let closed = stack.len();
    for name in stack.iter().rev() {
        bytes.extend_from_slice(b"</");
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(b'>');
    }
    Some(Partial {
        bytes,
        kept: good,
        closed,
    })
}
