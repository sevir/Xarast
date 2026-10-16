//! BLAKE3-256 digests (`research/06 §3.4` rule 4, `§4.4`).
//!
//! The only algorithm v1.0 defines is `blake3-256`: 32 bytes, written as 64
//! lowercase hexadecimal characters. Hashing is streaming: [`HashingReader`]
//! digests bytes as they pass through, so a resource or an entry never has to
//! be materialised just to be hashed.

use std::fmt;
use std::io::{self, Read};

/// The manifest name of the digest algorithm.
pub const BLAKE3_256: &str = "blake3-256";

/// The input size from which [`Digest::of`] hashes in parallel.
const PARALLEL_FROM: usize = 2 << 20;

/// A BLAKE3-256 digest.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    /// Digests a byte slice in one call.
    pub fn of(bytes: &[u8]) -> Digest {
        // BLAKE3 is a tree: hashing the chunks on several threads gives
        // the same digest. Below a few MiB the fork-join costs more than
        // it saves.
        if bytes.len() >= PARALLEL_FROM {
            let mut h = blake3::Hasher::new();
            h.update_rayon(bytes);
            return Digest(*h.finalize().as_bytes());
        }
        Digest(*blake3::hash(bytes).as_bytes())
    }

    /// Digests everything `r` yields, streaming. Returns the digest and the
    /// number of bytes read.
    pub fn of_reader(r: impl Read) -> io::Result<(Digest, u64)> {
        let mut h = HashingReader::new(r);
        io::copy(&mut h, &mut io::sink())?;
        Ok(h.finish())
    }

    /// The 64-character lowercase hexadecimal form.
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    /// Parses exactly 64 lowercase hexadecimal characters, the only form the
    /// schema allows. Uppercase is refused rather than folded: the manifest
    /// is canonical and a writer that emits uppercase is not conformant.
    pub fn from_hex(s: &str) -> Option<Digest> {
        let b = s.as_bytes();
        if b.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        let (pairs, _) = b.as_chunks::<2>();
        for (o, [hi, lo]) in out.iter_mut().zip(pairs) {
            *o = (nibble(*hi)? << 4) | nibble(*lo)?;
        }
        Some(Digest(out))
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({})", self.to_hex())
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Lowercase hexadecimal of a byte slice.
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        for n in [b >> 4, b & 0x0f] {
            // `from_digit` is lowercase and a nibble is always < 16.
            s.push(char::from_digit(u32::from(n), 16).unwrap_or('0'));
        }
    }
    s
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// A reader that digests everything that passes through it.
#[derive(Debug)]
pub struct HashingReader<R> {
    inner: R,
    hasher: blake3::Hasher,
    len: u64,
}

impl<R: Read> HashingReader<R> {
    /// Wraps `inner`.
    pub fn new(inner: R) -> Self {
        HashingReader {
            inner,
            hasher: blake3::Hasher::new(),
            len: 0,
        }
    }

    /// Bytes read so far.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// `true` when nothing has been read yet.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The digest of what has been read so far, and its length.
    pub fn finish(&self) -> (Digest, u64) {
        (Digest(*self.hasher.finalize().as_bytes()), self.len)
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        let read = buf.get(..n).unwrap_or_default();
        self.hasher.update(read);
        self.len = self.len.saturating_add(n as u64);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector() {
        // BLAKE3 of the empty input, from the reference implementation.
        assert_eq!(
            Digest::of(b"").to_hex(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn hex_round_trip_and_strictness() {
        let d = Digest::of(b"xarast");
        assert_eq!(Digest::from_hex(&d.to_hex()), Some(d));
        assert_eq!(Digest::from_hex(&d.to_hex().to_uppercase()), None);
        assert_eq!(Digest::from_hex(&d.to_hex()[1..]), None);
        assert_eq!(Digest::from_hex(&format!("{}0", d.to_hex())), None);
        assert_eq!(Digest::from_hex(&"g".repeat(64)), None);
        assert_eq!(Digest::from_hex(&"é".repeat(32)), None);
    }

    #[test]
    fn streaming_equals_one_shot() {
        let data: Vec<u8> = (0..1_000_003u32).map(|i| (i * 7 % 251) as u8).collect();
        let (d, n) = Digest::of_reader(&data[..]).unwrap();
        assert_eq!(n, data.len() as u64);
        assert_eq!(d, Digest::of(&data));
    }
}
