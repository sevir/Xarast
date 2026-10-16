//! Large byte payloads in the journal: bitmap pixels, encoded originals,
//! embedded fonts.
//!
//! Writing a 40 MB photograph into an NDJSON line, as an array of numbers,
//! every time the bitmap table changes, would make the journal useless. A
//! payload of [`INLINE_MAX`] bytes or more is therefore written once, as a
//! content-addressed file the caller stores beside the journal, and the line
//! carries only its SHA-256. Smaller payloads stay inline, as hex.
//!
//! The store is installed for the duration of one serialisation or
//! deserialisation with [`with_blobs`]; serde has no other way to reach
//! state. Without one, every payload is written inline and a reference
//! cannot be read.

use std::cell::RefCell;
use std::sync::Arc;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

/// Payloads this long or longer go to the store.
pub const INLINE_MAX: usize = 4096;

/// Where large payloads go and come from.
pub trait BlobStore {
    /// Keeps `bytes` under `key` (their SHA-256, in lowercase hex).
    ///
    /// # Errors
    ///
    /// Why they could not be kept; the serialisation fails with it.
    fn put(&mut self, key: &str, bytes: &Arc<[u8]>) -> Result<(), String>;

    /// The bytes kept under `key`. A store must check them against the key;
    /// [`with_blobs`] does it again.
    fn get(&mut self, key: &str) -> Option<Arc<[u8]>>;

    /// A key already computed for these very bytes (the same allocation),
    /// so that a table serialised again does not hash its pixels again.
    fn known(&self, _bytes: &Arc<[u8]>) -> Option<String> {
        None
    }
}

thread_local! {
    static STORE: RefCell<Option<Box<dyn BlobStore>>> = const { RefCell::new(None) };
}

/// Runs `f` with `store` installed for this thread, and hands the store
/// back with the result.
pub fn with_blobs<R>(store: Box<dyn BlobStore>, f: impl FnOnce() -> R) -> (R, Box<dyn BlobStore>) {
    let previous = STORE.with(|s| s.borrow_mut().replace(store));
    let r = f();
    let store = STORE.with(|s| {
        let mut slot = s.borrow_mut();
        let ours = slot.take();
        *slot = previous;
        ours
    });
    // `f` cannot take the store out (the slot is private), so it is there.
    (r, store.unwrap_or_else(|| Box::new(NoStore)))
}

/// A store that keeps nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoStore;

impl BlobStore for NoStore {
    fn put(&mut self, _: &str, _: &Arc<[u8]>) -> Result<(), String> {
        Err("no blob store".into())
    }
    fn get(&mut self, _: &str) -> Option<Arc<[u8]>> {
        None
    }
}

/// The SHA-256 of `bytes`, lowercase hex.
#[must_use]
pub fn key_of(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[hi, lo]| Some(digit(*hi)? << 4 | digit(*lo)?))
        .collect()
}

/// A key is 64 lowercase hex digits and nothing else, so that it can name a
/// file without escaping anything.
#[must_use]
pub fn is_key(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Repr {
    Hex(String),
    Blob { sha256: String, len: u64 },
}

/// `#[serde(with = "…")]` for an `Arc<[u8]>` field.
pub(crate) fn serialize<S: Serializer>(bytes: &Arc<[u8]>, s: S) -> Result<S::Ok, S::Error> {
    use serde::ser::Error as _;
    if bytes.len() < INLINE_MAX {
        return Repr::Hex(hex(bytes)).serialize(s);
    }
    let stored = STORE.with(|st| {
        let mut st = st.borrow_mut();
        let Some(store) = st.as_mut() else {
            return Ok(None);
        };
        let key = store.known(bytes).unwrap_or_else(|| key_of(bytes));
        store.put(&key, bytes).map(|()| Some(key))
    });
    match stored.map_err(S::Error::custom)? {
        Some(sha256) => Repr::Blob {
            sha256,
            len: bytes.len() as u64,
        }
        .serialize(s),
        None => Repr::Hex(hex(bytes)).serialize(s),
    }
}

/// `#[serde(with = "…")]` for an `Arc<[u8]>` field.
pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<[u8]>, D::Error> {
    match Repr::deserialize(d)? {
        Repr::Hex(h) => unhex(&h)
            .map(Arc::from)
            .ok_or_else(|| D::Error::custom("bad hex payload")),
        Repr::Blob { sha256, len } => {
            if !is_key(&sha256) {
                return Err(D::Error::custom("bad blob key"));
            }
            let bytes = STORE
                .with(|st| st.borrow_mut().as_mut().and_then(|s| s.get(&sha256)))
                .ok_or_else(|| D::Error::custom("missing blob"))?;
            if bytes.len() as u64 != len || key_of(&bytes) != sha256 {
                return Err(D::Error::custom("blob does not match its key"));
            }
            Ok(bytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct Mem(HashMap<String, Arc<[u8]>>);

    impl BlobStore for Mem {
        fn put(&mut self, key: &str, bytes: &Arc<[u8]>) -> Result<(), String> {
            self.0.insert(key.to_owned(), Arc::clone(bytes));
            Ok(())
        }
        fn get(&mut self, key: &str) -> Option<Arc<[u8]>> {
            self.0.get(key).cloned()
        }
    }

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Holder {
        #[serde(with = "super")]
        b: Arc<[u8]>,
    }

    #[test]
    fn small_inline_large_by_reference_and_checked() {
        let small = Holder {
            b: Arc::from(&b"abc"[..]),
        };
        let json = serde_json::to_string(&small).unwrap();
        assert_eq!(json, r#"{"b":{"hex":"616263"}}"#);
        assert_eq!(serde_json::from_str::<Holder>(&json).unwrap(), small);

        let big = Holder {
            b: Arc::from(vec![7u8; INLINE_MAX]),
        };
        let (json, store) = with_blobs(Box::new(Mem::default()), || {
            serde_json::to_string(&big).unwrap()
        });
        assert!(json.contains("sha256"), "{json}");
        let (back, _) = with_blobs(store, || serde_json::from_str::<Holder>(&json));
        assert_eq!(back.unwrap(), big);
        // Without the store the reference cannot be read.
        assert!(serde_json::from_str::<Holder>(&json).is_err());
        // A store that hands back other bytes is caught.
        let mut liar = Mem::default();
        let key = key_of(&big.b);
        liar.0.insert(key, Arc::from(vec![8u8; INLINE_MAX]));
        let (back, _) = with_blobs(Box::new(liar), || serde_json::from_str::<Holder>(&json));
        assert!(back.is_err());
        for bad in [
            r#"{"b":{"hex":"6"}}"#,
            r#"{"b":{"hex":"zz"}}"#,
            r#"{"b":{"blob":{"sha256":"../etc","len":1}}}"#,
        ] {
            assert!(serde_json::from_str::<Holder>(bad).is_err(), "{bad}");
        }
    }
}
