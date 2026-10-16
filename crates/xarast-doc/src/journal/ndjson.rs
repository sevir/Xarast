//! The journal file: one header line, then one line per entry
//! (`research/06 §10.3`).
//!
//! ```text
//! {"journal":"xarast-journal/1","stream":"…","base":12,"sum":"1a2b3c4d"}
//! {"seq":13,"ts":"2026-10-16T09:41:07.312Z","op":"tx","payload":{…},"sum":"…"}
//! {"seq":14,"ts":"2026-10-16T09:41:07.530Z","op":"undo","sum":"…"}
//! ```
//!
//! `base` is the last entry the journal's base holds; entries follow it with
//! consecutive `seq` numbers. `sum` is the first eight hex digits of the
//! SHA-256 of the line up to (not including) `,"sum"`, so that a line
//! damaged in a way that still parses is caught too.
//!
//! [`read`] never panics, whatever the bytes. It stops — cleanly, keeping
//! everything before — at the first line that is torn (no newline: the
//! write a crash interrupted), corrupt (bad sum, bad JSON, unknown entry)
//! or out of sequence, and says which. Payload blobs are resolved through
//! the store [`super::blob::with_blobs`] installed.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::Op;

/// The header's `journal` value.
pub const MAGIC: &str = "xarast-journal/1";

/// A line longer than this is not read (and is not written: the writer
/// treats the entry as one the journal cannot represent).
pub const MAX_LINE: usize = 64 << 20;

/// `,"sum":"01234567"}`: the fixed-length tail of every line.
const TAIL: usize = 18;

/// The first line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// The journal stream: the autosave entry it belongs to.
    pub stream: String,
    /// The last entry the base holds.
    pub base: u64,
}

#[derive(Serialize, Deserialize)]
struct HeaderRepr {
    journal: String,
    stream: String,
    base: u64,
}

#[derive(Deserialize)]
struct EntryRepr {
    seq: u64,
    ts: String,
    #[serde(flatten)]
    op: Op,
}

fn sum(prefix: &[u8]) -> String {
    super::blob::hex(&Sha256::digest(prefix)[..4])
}

fn seal(prefix: &str) -> String {
    let s = sum(prefix.as_bytes());
    format!("{prefix},\"sum\":\"{s}\"}}\n")
}

/// The header line, newline included.
#[must_use]
pub fn header_line(h: &Header) -> String {
    let json = serde_json::to_string(&HeaderRepr {
        journal: MAGIC.to_owned(),
        stream: h.stream.clone(),
        base: h.base,
    })
    .unwrap_or_default();
    // `{…}` → `{…` and seal it.
    seal(json.strip_suffix('}').unwrap_or(&json))
}

/// An entry's line, newline included. `ts` is written as given (see
/// [`rfc3339`]).
///
/// # Errors
///
/// When the entry cannot be serialised: a non-finite number (JSON has
/// none), a blob the installed store refused. The caller treats the entry
/// as one the journal cannot represent.
pub fn entry_line(seq: u64, ts: &str, op: &Op) -> Result<String, serde_json::Error> {
    let body = serde_json::to_string(op)?;
    let inner = body
        .strip_prefix('{')
        .and_then(|b| b.strip_suffix('}'))
        .unwrap_or("");
    let ts = serde_json::to_string(ts)?;
    Ok(seal(&format!("{{\"seq\":{seq},\"ts\":{ts},{inner}")))
}

/// One entry read back.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Its sequence number.
    pub seq: u64,
    /// When it was written.
    pub ts: String,
    /// What it is.
    pub op: Op,
}

/// Why [`read`] stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// At the end: every line was read.
    End,
    /// The first line is missing or is not a journal header.
    NoHeader,
    /// The last line has no newline: a write cut short.
    Torn {
        /// Its line number (the header is line 0).
        line: usize,
    },
    /// A line that is damaged or not understood.
    Corrupt {
        /// Its line number.
        line: usize,
        /// What is wrong with it.
        why: String,
    },
    /// A `seq` that does not follow the one before.
    Gap {
        /// Its line number.
        line: usize,
        /// The `seq` that should have come.
        expected: u64,
        /// The one that did.
        found: u64,
    },
}

/// A journal file, read up to the first line that is not sound.
#[derive(Debug, Clone)]
pub struct Journal {
    /// The header, if the first line is one.
    pub header: Option<Header>,
    /// The sound entries, in order, with consecutive `seq` from
    /// `header.base + 1`.
    pub entries: Vec<Entry>,
    /// Why reading stopped.
    pub stop: Stop,
    /// How many bytes the header and the sound entries take: a writer
    /// continuing the journal truncates to this.
    pub sound_len: usize,
}

/// Checks a line's tail and sum and returns its JSON.
fn unseal(line: &[u8]) -> Result<String, String> {
    if line.len() > MAX_LINE {
        return Err("line too long".into());
    }
    if line.len() < TAIL + 2 {
        return Err("line too short".into());
    }
    let (prefix, tail) = line.split_at(line.len() - TAIL);
    let ok_shape = tail.starts_with(b",\"sum\":\"") && tail.ends_with(b"\"}");
    if !ok_shape {
        return Err("no sum".into());
    }
    let given = &tail[8..16];
    if given != sum(prefix).as_bytes() {
        return Err("sum mismatch".into());
    }
    let mut json = std::str::from_utf8(prefix)
        .map_err(|_| "not UTF-8".to_owned())?
        .to_owned();
    json.push('}');
    Ok(json)
}

/// Reads a journal file. Never panics.
#[must_use]
pub fn read(bytes: &[u8]) -> Journal {
    let mut out = Journal {
        header: None,
        entries: Vec::new(),
        stop: Stop::End,
        sound_len: 0,
    };
    let mut pos = 0usize;
    let mut line_no = 0usize;
    let mut next_seq = 0u64;
    while pos < bytes.len() {
        let rest = &bytes[pos..];
        let Some(nl) = rest.iter().position(|b| *b == b'\n') else {
            out.stop = if line_no == 0 {
                Stop::NoHeader
            } else {
                Stop::Torn { line: line_no }
            };
            return out;
        };
        let line = &rest[..nl];
        let json = match unseal(line) {
            Ok(j) => j,
            Err(why) => {
                out.stop = if line_no == 0 {
                    Stop::NoHeader
                } else {
                    Stop::Corrupt { line: line_no, why }
                };
                return out;
            }
        };
        if line_no == 0 {
            match serde_json::from_str::<HeaderRepr>(&json) {
                Ok(h) if h.journal == MAGIC => {
                    next_seq = h.base.saturating_add(1);
                    out.header = Some(Header {
                        stream: h.stream,
                        base: h.base,
                    });
                }
                _ => {
                    out.stop = Stop::NoHeader;
                    return out;
                }
            }
        } else {
            match serde_json::from_str::<EntryRepr>(&json) {
                Ok(e) if e.seq == next_seq => {
                    next_seq = next_seq.saturating_add(1);
                    out.entries.push(Entry {
                        seq: e.seq,
                        ts: e.ts,
                        op: e.op,
                    });
                }
                Ok(e) => {
                    out.stop = Stop::Gap {
                        line: line_no,
                        expected: next_seq,
                        found: e.seq,
                    };
                    return out;
                }
                Err(e) => {
                    out.stop = Stop::Corrupt {
                        line: line_no,
                        why: e.to_string(),
                    };
                    return out;
                }
            }
        }
        pos += nl + 1;
        out.sound_len = pos;
        line_no += 1;
    }
    if line_no == 0 {
        out.stop = Stop::NoHeader;
    }
    out
}

/// `2026-10-16T09:41:07.312Z` for a time since the Unix epoch.
#[must_use]
pub fn rfc3339(since_epoch: std::time::Duration) -> String {
    let secs = since_epoch.as_secs();
    let millis = since_epoch.subsec_millis();
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{millis:03}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal(entries: &[(u64, Op)]) -> String {
        let mut s = header_line(&Header {
            stream: "s".into(),
            base: 4,
        });
        for (seq, op) in entries {
            s.push_str(&entry_line(*seq, "2026-10-16T00:00:00.000Z", op).unwrap());
        }
        s
    }

    #[test]
    fn reads_what_it_writes_and_stops_cleanly() {
        let ok = journal(&[(5, Op::Undo), (6, Op::Redo), (7, Op::DiscardRedo)]);
        let j = read(ok.as_bytes());
        assert_eq!(j.stop, Stop::End);
        assert_eq!(j.entries.len(), 3);
        assert_eq!(j.header.as_ref().unwrap().base, 4);
        assert_eq!(j.sound_len, ok.len());
        assert!(ok.lines().nth(1).unwrap().starts_with(r#"{"seq":5,"ts":"#));
        assert!(ok.contains(r#""op":"undo""#));

        // Torn: the last line lost its end.
        let torn = &ok[..ok.len() - 5];
        let j = read(torn.as_bytes());
        assert_eq!(j.stop, Stop::Torn { line: 3 });
        assert_eq!(j.entries.len(), 2);
        assert_eq!(j.sound_len, ok.len() - ok.lines().last().unwrap().len() - 1);

        // A flipped byte inside a line that still parses.
        let flipped = ok.replacen("\"redo\"", "\"undo\"", 1);
        let j = read(flipped.as_bytes());
        assert!(
            matches!(j.stop, Stop::Corrupt { line: 2, .. }),
            "{:?}",
            j.stop
        );
        assert_eq!(j.entries.len(), 1);

        // A gap.
        let gap = journal(&[(5, Op::Undo), (7, Op::Redo)]);
        let j = read(gap.as_bytes());
        assert_eq!(
            j.stop,
            Stop::Gap {
                line: 2,
                expected: 6,
                found: 7
            }
        );
        // A first entry that does not follow the base.
        let j = read(journal(&[(9, Op::Undo)]).as_bytes());
        assert!(matches!(j.stop, Stop::Gap { expected: 5, .. }));

        for bad in ["", "\n", "{}\n", "not json\n", "{\"journal\":1}\n"] {
            let j = read(bad.as_bytes());
            assert_eq!(j.stop, Stop::NoHeader, "{bad:?}");
            assert!(j.entries.is_empty());
        }
    }

    #[test]
    fn non_finite_numbers_are_refused_not_written() {
        let op = Op::Tx {
            label: "x".into(),
            key: None,
            merged: false,
            steps: vec![super::super::Step::Act {
                bytes: 0,
                act: super::super::Act::Transform {
                    node: 1,
                    matrix: xarast_geom::Matrix::scale(f64::NAN, 1.0),
                },
            }],
        };
        let line = entry_line(1, "t", &op);
        // serde_json writes NaN as null, which then does not read back:
        // either way the entry must not survive as something else.
        if let Ok(line) = line {
            let s = header_line(&Header {
                stream: "s".into(),
                base: 0,
            }) + &line;
            assert!(read(s.as_bytes()).entries.is_empty());
        }
    }

    #[test]
    fn timestamps() {
        use std::time::Duration;
        assert_eq!(rfc3339(Duration::ZERO), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            rfc3339(Duration::from_millis(1_792_143_667_312)),
            "2026-10-16T09:41:07.312Z"
        );
        assert_eq!(
            rfc3339(Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00.000Z"
        );
    }

    proptest::proptest! {
        /// Whatever the bytes, reading returns, and what it returns is a
        /// prefix that reads the same again.
        #[test]
        fn hostile_bytes_never_panic(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..512)) {
            let j = read(&bytes);
            proptest::prop_assert!(j.sound_len <= bytes.len());
            let again = read(&bytes[..j.sound_len]);
            proptest::prop_assert_eq!(again.entries.len(), j.entries.len());
        }

        /// A sound journal with random damage: still never panics, and never
        /// yields more entries than were written.
        #[test]
        fn damaged_journals_never_panic(
            cut in 0usize..400,
            flips in proptest::collection::vec((0usize..400, proptest::num::u8::ANY), 0..4),
        ) {
            let mut bytes = journal(&[(5, Op::Undo), (6, Op::HoldRedo), (7, Op::Barrier { reason: "r".into() })]).into_bytes();
            for (at, v) in flips {
                if let Some(b) = bytes.get_mut(at) {
                    *b = v;
                }
            }
            bytes.truncate(cut.min(bytes.len()));
            let j = read(&bytes);
            proptest::prop_assert!(j.entries.len() <= 3);
        }
    }
}
