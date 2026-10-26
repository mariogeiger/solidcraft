//! What a point cloud holds besides its points: segments and triangles between them, and named
//! numbers on its points, segments and triangles (a PLY file's properties), to colour the cloud
//! by and to filter it with. A point's numbers vary along the segments and across the triangles
//! that meet there; a segment's or a triangle's are constant on it. A segment with `width` and
//! `height` numbers is drawn as a bead of that cross-section; without them, as a line.

use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::canvas::{base64_decode, base64_encode};
use crate::point_cloud::MAX_POINTS;
use crate::{DocError, Result};

/// Most segments or triangles in one cloud.
pub const MAX_CELLS: usize = 20_000_000;
/// Most named numbers per point, segment and triangle together.
pub const MAX_COLUMNS: usize = 64;

/// The elements a column of numbers is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum On {
    Point,
    Segment,
    Triangle,
}

impl On {
    pub fn name(self) -> &'static str {
        match self {
            On::Point => "point",
            On::Segment => "segment",
            On::Triangle => "triangle",
        }
    }
}

/// One named number on every element of a kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub on: On,
    /// Whole numbers (labels, counts): coloured by a palette rather than a ramp.
    #[serde(default)]
    pub integer: bool,
    pub values: Floats,
}

impl Column {
    /// The smallest and largest finite value.
    pub fn span(&self) -> Option<[f64; 2]> {
        let mut it = self.values.0.iter().filter(|v| v.is_finite()).map(|v| f64::from(*v));
        let first = it.next()?;
        Some(it.fold([first, first], |[lo, hi], v| [lo.min(v), hi.max(v)]))
    }
}

/// Segments and triangles over a cloud's points, and the numbers on all three.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cells {
    /// Point indices, two per segment.
    #[serde(default, skip_serializing_if = "Indices::is_empty")]
    pub segments: Indices,
    /// Point indices, three per triangle, counter-clockwise seen from its front.
    #[serde(default, skip_serializing_if = "Indices::is_empty")]
    pub triangles: Indices,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<Column>,
}

impl Cells {
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty() && self.triangles.is_empty() && self.columns.is_empty()
    }
    pub fn segment_count(&self) -> usize {
        self.segments.0.len() / 2
    }
    pub fn triangle_count(&self) -> usize {
        self.triangles.0.len() / 3
    }
    /// The column named `name` on elements of kind `on`: a name may be on several kinds.
    pub fn column(&self, name: &str, on: On) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name && c.on == on)
    }
    /// The smallest and largest value named `name`, on whichever elements carry it.
    pub fn span(&self, name: &str) -> Option<[f64; 2]> {
        self.columns.iter().filter(|c| c.name == name).filter_map(Column::span).reduce(|[a, b], [c, d]| [a.min(c), b.max(d)])
    }
    /// Whether the values named `name` are whole numbers wherever they are.
    pub fn integer(&self, name: &str) -> bool {
        let mut named = self.columns.iter().filter(|c| c.name == name).peekable();
        named.peek().is_some() && named.all(|c| c.integer)
    }
    /// How many elements of a kind there are, the cloud having `points` points.
    pub fn count(&self, on: On, points: usize) -> usize {
        match on {
            On::Point => points,
            On::Segment => self.segment_count(),
            On::Triangle => self.triangle_count(),
        }
    }
    /// Every index names a point, and every column has one number per element.
    pub fn check(&self, points: usize) -> Result<()> {
        let bad = |what: String| Err(DocError::Invalid(format!("point cloud cells: {what}")));
        if !self.segments.0.len().is_multiple_of(2) || !self.triangles.0.len().is_multiple_of(3) {
            return bad("segments are not pairs or triangles not triples".into());
        }
        if self.segment_count() > MAX_CELLS || self.triangle_count() > MAX_CELLS || self.columns.len() > MAX_COLUMNS {
            return bad("too many segments, triangles or properties".into());
        }
        if self.segments.0.iter().chain(self.triangles.0.iter()).any(|i| *i as usize >= points) {
            return bad("an index names a missing point".into());
        }
        for c in &self.columns {
            if c.values.0.len() != self.count(c.on, points) {
                return bad(format!("property `{}` does not have one number per {}", c.name, c.on.name()));
            }
        }
        Ok(())
    }
}

/// Which elements are shown: those whose property lies in a range (both ends included).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    pub property: String,
    pub range: [f64; 2],
}

/// Indices into a cloud's points, shared between copies of the design.
#[derive(Clone, Debug, Default)]
pub struct Indices(pub Arc<[u32]>);

impl Indices {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Numbers, shared between copies of the design.
#[derive(Clone, Debug, Default)]
pub struct Floats(pub Arc<[f32]>);

impl PartialEq for Indices {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

impl PartialEq for Floats {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.iter().map(|v| v.to_bits()).eq(other.0.iter().map(|v| v.to_bits()))
    }
}

/// The largest base64 text of `n` four-byte words.
fn base64_len(n: usize) -> usize {
    (n * 4).div_ceil(3) * 4 + 16
}

fn words<'de, D: Deserializer<'de>>(d: D, max: usize) -> std::result::Result<Vec<[u8; 4]>, D::Error> {
    let s = String::deserialize(d)?;
    if s.len() > base64_len(max) {
        return Err(serde::de::Error::custom("too many numbers"));
    }
    let b = base64_decode(&s).ok_or_else(|| serde::de::Error::custom("numbers are not base64"))?;
    if !b.len().is_multiple_of(4) {
        return Err(serde::de::Error::custom("numbers are not whole four-byte words"));
    }
    Ok(b.as_chunks::<4>().0.to_vec())
}

impl Serialize for Indices {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&base64_encode(&self.0.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()))
    }
}

impl<'de> Deserialize<'de> for Indices {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(Indices(words(d, 3 * MAX_CELLS)?.into_iter().map(u32::from_le_bytes).collect()))
    }
}

impl Serialize for Floats {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&base64_encode(&self.0.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()))
    }
}

impl<'de> Deserialize<'de> for Floats {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(Floats(words(d, MAX_POINTS.max(MAX_CELLS))?.into_iter().map(f32::from_le_bytes).collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_round_trip_and_are_checked_against_the_points() {
        let cells = Cells {
            segments: Indices(vec![0, 1, 1, 2].into()),
            triangles: Indices(vec![0, 1, 2].into()),
            columns: vec![
                Column { name: "t".into(), on: On::Point, integer: false, values: Floats(vec![0.0, 0.5, 1.0].into()) },
                Column { name: "role".into(), on: On::Segment, integer: true, values: Floats(vec![0.0, 4.0].into()) },
            ],
        };
        let back: Cells = serde_json::from_str(&serde_json::to_string(&cells).unwrap()).unwrap();
        assert_eq!(back, cells);
        cells.check(3).unwrap();
        assert!(cells.check(2).is_err(), "index 2 names a missing point");
        assert_eq!(cells.span("role"), Some([0.0, 4.0]));
        assert!(cells.integer("role") && !cells.integer("t") && !cells.integer("nope"));
        assert!(cells.column("role", On::Segment).is_some() && cells.column("role", On::Point).is_none());
        let mut short = cells.clone();
        short.columns[1].values = Floats(vec![0.0].into());
        assert!(short.check(3).is_err(), "one role for two segments");
    }
}
