//! Ink strokes for the raster ink layer: the stroke store, a grid index that updates one stroke at a
//! time, and validated loading.
//!
//! This module is data only. Drawing the strokes into tiles belongs to `vectorcraft_render`. Strokes keep
//! stable ids that never shift, so inserting or removing one touches only the grid cells it covers. The
//! file holds the strokes and not the tiles; the index is rebuilt when a file is loaded.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use serde::{Deserialize, Serialize};

/// Version of [`InkData`]. Loading any other version is an error until a migration exists for it.
pub const INK_DATA_VERSION: u32 = 1;
/// Largest number of strokes one ink store holds.
pub const MAX_STROKES: usize = 1_000_000;
/// Largest number of points in one stroke.
pub const MAX_POINTS_PER_STROKE: usize = 100_000;
/// Largest number of points in one store.
pub const MAX_TOTAL_POINTS: usize = 20_000_000;
/// Largest absolute coordinate, in document units. Keeps grid cell counts bounded.
pub const MAX_COORD: f64 = 1_000_000.0;
/// Widest stroke, in document units.
pub const MAX_WIDTH: f64 = 500.0;
/// Side of one index grid cell, in document units.
const CELL: f64 = 256.0;
/// A stroke spanning more cells than this per axis is kept on a list checked by every query, instead of
/// being registered in every cell it covers.
const MAX_CELLS_PER_AXIS: i64 = 64;

/// One sampled point of a stroke.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct InkPoint {
    pub x: f64,
    pub y: f64,
    /// Pen pressure, 0 to 1.
    pub pressure: f32,
}

/// One pen stroke: a polyline with its colour and nominal width.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InkStroke {
    /// RGBA, 0 to 255 per channel.
    pub color: [u8; 4],
    pub width: f64,
    pub points: Vec<InkPoint>,
}

/// The stored form of an ink layer's strokes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InkData {
    pub version: u32,
    pub strokes: Vec<InkStroke>,
}

/// Stable identifier of a stroke within one store. Ids are never reused by the same store.
pub type InkId = u64;

/// Why ink data was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InkError {
    /// The data's version is not one this build reads.
    Version(u32),
    /// A count is over one of the `MAX_*` limits. The string names which.
    TooMany(&'static str),
    /// A stroke has no points.
    EmptyStroke,
    /// A coordinate, pressure or width is not a finite number in range.
    BadNumber,
}

impl fmt::Display for InkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InkError::Version(v) => write!(f, "ink data version {v} is not supported"),
            InkError::TooMany(what) => write!(f, "too many {what} in the ink layer"),
            InkError::EmptyStroke => write!(f, "an ink stroke has no points"),
            InkError::BadNumber => write!(f, "an ink stroke has a bad coordinate, pressure or width"),
        }
    }
}

impl std::error::Error for InkError {}

/// Checks one stroke against the limits. Pressure is clamped to 0 to 1 rather than refused.
fn validate(mut stroke: InkStroke) -> Result<InkStroke, InkError> {
    if stroke.points.is_empty() {
        return Err(InkError::EmptyStroke);
    }
    if stroke.points.len() > MAX_POINTS_PER_STROKE {
        return Err(InkError::TooMany("points in one stroke"));
    }
    if !(stroke.width.is_finite() && stroke.width > 0.0 && stroke.width <= MAX_WIDTH) {
        return Err(InkError::BadNumber);
    }
    for p in &mut stroke.points {
        let in_range = p.x.is_finite() && p.y.is_finite() && p.x.abs() <= MAX_COORD && p.y.abs() <= MAX_COORD;
        if !in_range || !p.pressure.is_finite() {
            return Err(InkError::BadNumber);
        }
        p.pressure = p.pressure.clamp(0.0, 1.0);
    }
    Ok(stroke)
}

/// Bounds of a stroke, widened by half its width: `[min_x, min_y, max_x, max_y]`.
fn stroke_bounds(stroke: &InkStroke) -> [f64; 4] {
    let half = stroke.width / 2.0;
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for p in &stroke.points {
        b[0] = b[0].min(p.x);
        b[1] = b[1].min(p.y);
        b[2] = b[2].max(p.x);
        b[3] = b[3].max(p.y);
    }
    [b[0] - half, b[1] - half, b[2] + half, b[3] + half]
}

fn overlaps(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

/// Grid cells covered by `b`, as inclusive `(min_cell, max_cell)` per axis.
fn cell_span(b: [f64; 4]) -> ((i64, i64), (i64, i64)) {
    let cell = |v: f64| (v / CELL).floor() as i64;
    ((cell(b[0]), cell(b[1])), (cell(b[2]), cell(b[3])))
}

/// Distance from `p` to the stroke's polyline. A single point is its own polyline.
fn distance_to_polyline(points: &[InkPoint], p: [f64; 2]) -> f64 {
    let dist = |a: [f64; 2], b: [f64; 2]| {
        let (vx, vy) = (b[0] - a[0], b[1] - a[1]);
        let len2 = vx * vx + vy * vy;
        let t = if len2 > 0.0 { (((p[0] - a[0]) * vx + (p[1] - a[1]) * vy) / len2).clamp(0.0, 1.0) } else { 0.0 };
        let (cx, cy) = (a[0] + vx * t, a[1] + vy * t);
        ((p[0] - cx).powi(2) + (p[1] - cy).powi(2)).sqrt()
    };
    match points {
        [] => f64::MAX,
        [only] => dist([only.x, only.y], [only.x, only.y]),
        _ => points
            .windows(2)
            .map(|w| match *w {
                [a, b] => dist([a.x, a.y], [b.x, b.y]),
                _ => f64::MAX,
            })
            .fold(f64::MAX, f64::min),
    }
}

struct Entry {
    stroke: InkStroke,
    bounds: [f64; 4],
}

/// The strokes of one ink layer, with a grid index over them.
#[derive(Default)]
pub struct InkStore {
    entries: BTreeMap<InkId, Entry>,
    next: InkId,
    total_points: usize,
    cells: HashMap<(i64, i64), BTreeSet<InkId>>,
    /// Strokes too large for the grid. Every query checks them.
    large: BTreeSet<InkId>,
}

impl InkStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, id: InkId) -> Option<&InkStroke> {
        self.entries.get(&id).map(|e| &e.stroke)
    }

    /// Ids of all strokes, ascending (drawing order).
    pub fn ids(&self) -> impl Iterator<Item = InkId> + '_ {
        self.entries.keys().copied()
    }

    /// Adds a stroke as a new id, after checking it against the limits.
    pub fn insert(&mut self, stroke: InkStroke) -> Result<InkId, InkError> {
        let id = self.next;
        self.put(id, stroke)?;
        self.next = id.checked_add(1).ok_or(InkError::TooMany("strokes"))?;
        Ok(id)
    }

    /// Puts a stroke back under an id it had before, for undo. Fails if the id is taken.
    pub fn reinsert(&mut self, id: InkId, stroke: InkStroke) -> Result<(), InkError> {
        if self.entries.contains_key(&id) {
            return Err(InkError::TooMany("strokes with that id"));
        }
        self.put(id, stroke)?;
        self.next = self.next.max(id.saturating_add(1));
        Ok(())
    }

    fn put(&mut self, id: InkId, stroke: InkStroke) -> Result<(), InkError> {
        if self.entries.len() >= MAX_STROKES {
            return Err(InkError::TooMany("strokes"));
        }
        let stroke = validate(stroke)?;
        if self.total_points + stroke.points.len() > MAX_TOTAL_POINTS {
            return Err(InkError::TooMany("points"));
        }
        let bounds = stroke_bounds(&stroke);
        self.total_points += stroke.points.len();
        self.register(id, bounds);
        self.entries.insert(id, Entry { stroke, bounds });
        Ok(())
    }

    /// Removes a stroke, returning it. Only the cells it covered are updated.
    pub fn remove(&mut self, id: InkId) -> Option<InkStroke> {
        let entry = self.entries.remove(&id)?;
        self.unregister(id, entry.bounds);
        self.total_points -= entry.stroke.points.len();
        Some(entry.stroke)
    }

    fn register(&mut self, id: InkId, bounds: [f64; 4]) {
        let ((x0, y0), (x1, y1)) = cell_span(bounds);
        if x1 - x0 >= MAX_CELLS_PER_AXIS || y1 - y0 >= MAX_CELLS_PER_AXIS {
            self.large.insert(id);
            return;
        }
        for x in x0..=x1 {
            for y in y0..=y1 {
                self.cells.entry((x, y)).or_default().insert(id);
            }
        }
    }

    fn unregister(&mut self, id: InkId, bounds: [f64; 4]) {
        let ((x0, y0), (x1, y1)) = cell_span(bounds);
        if x1 - x0 >= MAX_CELLS_PER_AXIS || y1 - y0 >= MAX_CELLS_PER_AXIS {
            self.large.remove(&id);
            return;
        }
        for x in x0..=x1 {
            for y in y0..=y1 {
                if let Some(set) = self.cells.get_mut(&(x, y)) {
                    set.remove(&id);
                    if set.is_empty() {
                        self.cells.remove(&(x, y));
                    }
                }
            }
        }
    }

    /// Ids of strokes whose bounds overlap `area` (`[min_x, min_y, max_x, max_y]`), ascending.
    pub fn query(&self, area: [f64; 4]) -> Vec<InkId> {
        let ((x0, y0), (x1, y1)) = cell_span(area);
        let mut candidates: BTreeSet<InkId> = self.large.clone();
        for x in x0..=x1 {
            for y in y0..=y1 {
                if let Some(set) = self.cells.get(&(x, y)) {
                    candidates.extend(set.iter().copied());
                }
            }
        }
        candidates
            .into_iter()
            .filter(|id| self.entries.get(id).is_some_and(|e| overlaps(e.bounds, area)))
            .collect()
    }

    /// Ids of strokes that pass within `radius` of `p`, taking each stroke's width into account. Ascending.
    pub fn hit(&self, p: [f64; 2], radius: f64) -> Vec<InkId> {
        let margin = radius + MAX_WIDTH / 2.0;
        let area = [p[0] - margin, p[1] - margin, p[0] + margin, p[1] + margin];
        self.query(area)
            .into_iter()
            .filter(|id| {
                self.entries.get(id).is_some_and(|e| {
                    distance_to_polyline(&e.stroke.points, p) <= radius + e.stroke.width / 2.0
                })
            })
            .collect()
    }

    /// The stored form, strokes in id order.
    pub fn to_data(&self) -> InkData {
        InkData {
            version: INK_DATA_VERSION,
            strokes: self.entries.values().map(|e| e.stroke.clone()).collect(),
        }
    }

    /// Builds a store from stored data, checking the version and every limit. Ids are assigned in order.
    pub fn from_data(data: InkData) -> Result<Self, InkError> {
        if data.version != INK_DATA_VERSION {
            return Err(InkError::Version(data.version));
        }
        if data.strokes.len() > MAX_STROKES {
            return Err(InkError::TooMany("strokes"));
        }
        let mut store = Self::new();
        for stroke in data.strokes {
            store.insert(stroke)?;
        }
        Ok(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn stroke(points: &[(f64, f64)], width: f64) -> InkStroke {
        InkStroke {
            color: [0, 0, 0, 255],
            width,
            points: points.iter().map(|&(x, y)| InkPoint { x, y, pressure: 1.0 }).collect(),
        }
    }

    #[test]
    fn insert_query_and_remove_update_the_index() {
        let mut store = InkStore::new();
        let a = store.insert(stroke(&[(0.0, 0.0), (10.0, 0.0)], 2.0)).unwrap();
        let b = store.insert(stroke(&[(900.0, 900.0), (910.0, 900.0)], 2.0)).unwrap();
        assert_eq!(store.query([-5.0, -5.0, 20.0, 5.0]), vec![a]);
        assert_eq!(store.query([0.0, 0.0, 1000.0, 1000.0]), vec![a, b]);
        assert!(store.remove(a).is_some());
        assert_eq!(store.query([-5.0, -5.0, 20.0, 5.0]), Vec::<InkId>::new());
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn hit_uses_the_stroke_width() {
        let mut store = InkStore::new();
        let id = store.insert(stroke(&[(0.0, 0.0), (100.0, 0.0)], 10.0)).unwrap();
        assert_eq!(store.hit([50.0, 5.5], 1.0), vec![id]);
        assert!(store.hit([50.0, 20.0], 1.0).is_empty());
    }

    #[test]
    fn reinsert_restores_the_same_id_for_undo() {
        let mut store = InkStore::new();
        let id = store.insert(stroke(&[(1.0, 1.0)], 2.0)).unwrap();
        let taken = store.remove(id).unwrap();
        store.reinsert(id, taken).unwrap();
        assert!(store.get(id).is_some());
        assert!(store.reinsert(id, stroke(&[(0.0, 0.0)], 2.0)).is_err());
    }

    #[test]
    fn large_strokes_stay_queryable() {
        let mut store = InkStore::new();
        let big = store.insert(stroke(&[(-50_000.0, -50_000.0), (50_000.0, 50_000.0)], 2.0)).unwrap();
        assert_eq!(store.query([0.0, 0.0, 1.0, 1.0]), vec![big]);
        assert!(store.remove(big).is_some());
        assert!(store.query([0.0, 0.0, 1.0, 1.0]).is_empty());
    }

    #[test]
    fn invalid_strokes_are_refused_and_pressure_is_clamped() {
        let mut store = InkStore::new();
        assert_eq!(store.insert(stroke(&[], 2.0)), Err(InkError::EmptyStroke));
        assert_eq!(store.insert(stroke(&[(0.0, 0.0)], 0.0)), Err(InkError::BadNumber));
        assert_eq!(store.insert(stroke(&[(f64::NAN, 0.0)], 2.0)), Err(InkError::BadNumber));
        assert_eq!(store.insert(stroke(&[(2.0e6, 0.0)], 2.0)), Err(InkError::BadNumber));
        let mut loud = stroke(&[(0.0, 0.0)], 2.0);
        loud.points[0].pressure = 3.0;
        let id = store.insert(loud).unwrap();
        assert_eq!(store.get(id).unwrap().points[0].pressure, 1.0);
        assert!(!store.is_empty());
    }

    #[test]
    fn data_round_trips_and_wrong_versions_are_refused() {
        let mut store = InkStore::new();
        store.insert(stroke(&[(0.0, 0.0), (5.0, 5.0)], 3.0)).unwrap();
        let data = store.to_data();
        let back = InkStore::from_data(data.clone()).unwrap();
        assert_eq!(back.to_data(), data);
        let mut newer = data;
        newer.version = INK_DATA_VERSION + 1;
        assert!(matches!(InkStore::from_data(newer), Err(InkError::Version(_))));
    }

    proptest! {
        #[test]
        fn query_matches_a_brute_force_scan(
            strokes in prop::collection::vec(
                (prop::collection::vec((-2000.0f64..2000.0, -2000.0f64..2000.0), 1..6), 0.5f64..40.0),
                0..40,
            ),
            area in (-2000.0f64..2000.0, -2000.0f64..2000.0, 0.0f64..800.0, 0.0f64..800.0),
        ) {
            let mut store = InkStore::new();
            let mut ids = Vec::new();
            for (pts, width) in &strokes {
                ids.push(store.insert(stroke(pts, *width)).unwrap());
            }
            let area = [area.0, area.1, area.0 + area.2, area.1 + area.3];
            let expected: Vec<InkId> = ids
                .iter()
                .copied()
                .filter(|id| overlaps(stroke_bounds(store.get(*id).unwrap()), area))
                .collect();
            prop_assert_eq!(store.query(area), expected);
        }

        #[test]
        fn remove_then_query_forgets_the_stroke(
            pts in prop::collection::vec((-500.0f64..500.0, -500.0f64..500.0), 1..6),
        ) {
            let mut store = InkStore::new();
            let id = store.insert(stroke(&pts, 4.0)).unwrap();
            prop_assert!(store.remove(id).is_some());
            prop_assert!(store.query([-1000.0, -1000.0, 1000.0, 1000.0]).is_empty());
            prop_assert!(store.is_empty());
        }

        /// Untrusted bytes must never panic when read as ink data.
        #[test]
        fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
            if let Ok(data) = serde_json::from_slice::<InkData>(&bytes) {
                let _ = InkStore::from_data(data);
            }
        }
    }
}
