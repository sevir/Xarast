//! The serialised form of a [`Path`] (the operation journal of
//! `xarast-doc`): the verbs as one letter each (`M`, `L`, `C`, `Z`), the
//! points as one flat `x, y, x, y, …` array of millipoints and the flags as
//! their bits (absent when every point has the default). Compact, and
//! checked on the way in so that no hostile input can build a path whose
//! arrays disagree.

use serde::de::Error as _;
use serde::{Deserialize, Serialize};

use super::{Path, PointFlags, Verb};
use crate::{Mp, Point};

#[derive(Serialize, Deserialize)]
struct PathRepr {
    v: String,
    p: Vec<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    f: Vec<u8>,
}

impl Serialize for Path {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self
            .verbs
            .iter()
            .map(|v| match v {
                Verb::MoveTo => 'M',
                Verb::LineTo => 'L',
                Verb::CubicTo => 'C',
                Verb::Close => 'Z',
            })
            .collect();
        let mut p = Vec::with_capacity(self.points.len() * 2);
        for pt in &self.points {
            p.push(pt.x.0);
            p.push(pt.y.0);
        }
        let f = self.flags.iter().map(|f| f.bits()).collect();
        PathRepr { v, p, f }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Path {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Path, D::Error> {
        let r = PathRepr::deserialize(d)?;
        let mut verbs = Vec::with_capacity(r.v.len());
        for c in r.v.chars() {
            verbs.push(match c {
                'M' => Verb::MoveTo,
                'L' => Verb::LineTo,
                'C' => Verb::CubicTo,
                'Z' => Verb::Close,
                _ => return Err(D::Error::custom("unknown path verb")),
            });
        }
        if r.p.len() % 2 != 0 {
            return Err(D::Error::custom("odd coordinate count"));
        }
        let points: Vec<Point> =
            r.p.as_chunks::<2>()
                .0
                .iter()
                .map(|[x, y]| Point::new(Mp(*x), Mp(*y)))
                .collect();
        let flags: Vec<PointFlags> = r.f.into_iter().map(PointFlags::from_bits_retain).collect();
        // The structural rules only (arity, flag length): those are what
        // indexing relies on. The extent and subpath rules are not checked:
        // a path in a live document need not satisfy them, and a replay
        // must reproduce it exactly.
        let expected: usize = verbs.iter().map(|v| v.arity()).sum();
        if expected != points.len() || (!flags.is_empty() && flags.len() != points.len()) {
            return Err(D::Error::custom("path arrays do not agree"));
        }
        Ok(Path {
            verbs,
            points,
            flags,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_refuses_disagreeing_arrays() {
        let mut b = Path::builder();
        b.move_to(Point::new(Mp(1), Mp(-2)));
        b.cubic_to(
            Point::new(Mp(3), Mp(4)),
            Point::new(Mp(5), Mp(6)),
            Point::new(Mp(7), Mp(8)),
        );
        b.close();
        let p = b.build();
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Path>(&json).unwrap(), p);
        for bad in [
            r#"{"v":"ML","p":[1,2]}"#,
            r#"{"v":"M","p":[1]}"#,
            r#"{"v":"X","p":[]}"#,
            r#"{"v":"M","p":[1,2],"f":[0,0]}"#,
        ] {
            assert!(serde_json::from_str::<Path>(bad).is_err(), "{bad}");
        }
    }
}
