//! Commands for the raster ink layer (`NodeKind::Ink`): pen strokes written by the Pencil overlay.
//! Each stroke is one undo step; the layer keeps its strokes as data, so erasing and undo never
//! re-trace the rest of the drawing.

use std::sync::Arc;

use serde_json::{Value, json};
use vectorcraft_color::Paint;
use vectorcraft_doc::ink::{INK_DATA_VERSION, InkData, InkPoint, InkStore, InkStroke};
use vectorcraft_doc::{Appearance, NodeId, NodeKind};

use super::create::add_node;
use super::*;
use crate::{EngineError, Result, Session};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "ink.stroke",
            "Ink Stroke",
            [],
            None,
            "{points: [[x,y]…] or [[x,y,pressure]…], color?: [r,g,b,a] 0..255 (black), width?: pt (2)} add one pen stroke to the selected ink layer, or to a new ink layer → {id}",
            has_doc,
            stroke
        ),
        cmd!(
            "ink.erase",
            "Erase Ink",
            [],
            None,
            "{at: [x,y], radius?: pt (4)} remove the strokes of the selected ink layer that touch the circle → {removed}",
            has_ink_selection,
            erase
        ),
    ]
}

/// Ink strokes can only be erased from a selected ink layer.
fn has_ink_selection(s: &Session) -> std::result::Result<(), String> {
    ink_target(s).map(|_| ()).ok_or_else(|| "select one ink layer".into())
}

/// The single selected object, when it is an ink layer.
fn ink_target(s: &Session) -> Option<NodeId> {
    let st = s.active()?;
    match st.selection.objects.as_slice() {
        [id] if matches!(st.doc.node(*id).map(|n| &n.kind), Some(NodeKind::Ink { .. })) => Some(*id),
        _ => None,
    }
}

fn bad_ink(cmd: &str, e: impl std::fmt::Display) -> EngineError {
    bad(cmd, e.to_string())
}

/// Reads `points`, each `[x, y]` or `[x, y, pressure]`. A point without pressure presses fully.
fn parse_points(p: &Value, cmd: &str) -> Result<Vec<InkPoint>> {
    let a = p.get("points").and_then(Value::as_array).ok_or_else(|| bad(cmd, "missing array `points`"))?;
    a.iter()
        .map(|v| {
            let c = v.as_array().ok_or_else(|| bad(cmd, "each point is [x,y] or [x,y,pressure]"))?;
            let num = |i: usize| c.get(i).and_then(Value::as_f64).ok_or_else(|| bad(cmd, "point values must be numbers"));
            let pressure = match c.get(2) {
                Some(v) => v.as_f64().ok_or_else(|| bad(cmd, "pressure must be a number"))?,
                None => 1.0,
            };
            Ok(InkPoint { x: num(0)?, y: num(1)?, pressure: pressure as f32 })
        })
        .collect()
}

/// Reads `color` as `[r, g, b, a]` in 0..255; black and opaque when absent.
fn color_param(p: &Value, cmd: &str) -> Result<[u8; 4]> {
    let Some(v) = p.get("color").filter(|v| !v.is_null()) else { return Ok([0, 0, 0, 255]) };
    let a = v.as_array().filter(|a| a.len() == 4).ok_or_else(|| bad(cmd, "color is [r,g,b,a]"))?;
    let mut out = [0u8; 4];
    for (o, c) in out.iter_mut().zip(a) {
        let n = c.as_f64().filter(|n| (0.0..=255.0).contains(n)).ok_or_else(|| bad(cmd, "color channels are 0..255"))?;
        *o = n.round() as u8;
    }
    Ok(out)
}

fn stroke(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "ink.stroke";
    let stroke = InkStroke { color: color_param(p, C)?, width: f64_or(p, "width", 2.0), points: parse_points(p, C)? };
    // Validate with the same limits the file reader uses; the checked copy is what gets stored.
    let mut check = InkStore::new();
    check.insert(stroke).map_err(|e| bad_ink(C, e))?;
    let stroke = check.to_data().strokes.pop().ok_or_else(|| bad(C, "empty stroke"))?;

    match ink_target(s) {
        Some(id) => s.edit("Ink Stroke", |d, _| {
            if !d.is_editable(id) {
                return Err(EngineError::Other("the target layer is locked or hidden".into()));
            }
            let node = d.node_mut(id).ok_or_else(|| bad(C, "ink layer is gone"))?;
            let NodeKind::Ink { data } = &mut node.kind else { return Err(bad(C, "not an ink layer")) };
            let mut next = (**data).clone();
            next.strokes.push(stroke);
            // Re-check the whole layer: the stroke-count and point-count limits apply to the layer.
            let checked = InkStore::from_data(next).map_err(|e| bad_ink(C, e))?.to_data();
            *data = Arc::new(checked);
            Ok(json!({ "id": id.0 }))
        }),
        None => {
            let data = InkData { version: INK_DATA_VERSION, strokes: vec![stroke] };
            let ap = Appearance::basic(Paint::None, Paint::None, 1.0);
            add_node(s, "Ink Layer", NodeKind::Ink { data: Arc::new(data) }, ap, None)
        }
    }
}

/// Distance from `(x, y)` to the segment `a`–`b`.
fn segment_distance(x: f64, y: f64, a: &InkPoint, b: &InkPoint) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((x - a.x) * dx + (y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (a.x + dx * t - x).hypot(a.y + dy * t - y)
}

/// Whether the stroke, widened by its width, comes within `radius` of `(x, y)`. Segments are
/// tested, so fast pen movement between sparse samples still erases what it crosses.
fn touches(stroke: &InkStroke, x: f64, y: f64, radius: f64) -> bool {
    let reach = radius + stroke.width / 2.0;
    match stroke.points.as_slice() {
        [only] => (only.x - x).hypot(only.y - y) <= reach,
        pts => pts.iter().zip(pts.iter().skip(1)).any(|(a, b)| segment_distance(x, y, a, b) <= reach),
    }
}

fn erase(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "ink.erase";
    let at = p.get("at").and_then(Value::as_array).ok_or_else(|| bad(C, "at is [x,y]"))?;
    let (Some(x), Some(y)) = (at.first().and_then(Value::as_f64), at.get(1).and_then(Value::as_f64)) else {
        return Err(bad(C, "at is [x,y]"));
    };
    let radius = f64_or(p, "radius", 4.0).clamp(0.0, 10_000.0);
    let id = ink_target(s).ok_or_else(|| bad(C, "select one ink layer"))?;

    // Nothing to erase: no undo step.
    let hits = match s.doc()?.doc.node(id).map(|n| &n.kind) {
        Some(NodeKind::Ink { data }) => data.strokes.iter().filter(|st| touches(st, x, y, radius)).count(),
        _ => 0,
    };
    if hits == 0 {
        return Ok(json!({ "removed": 0 }));
    }
    let removed = s.edit("Erase Ink", |d, _| {
        if !d.is_editable(id) {
            return Err(EngineError::Other("the target layer is locked or hidden".into()));
        }
        let node = d.node_mut(id).ok_or_else(|| bad(C, "ink layer is gone"))?;
        let NodeKind::Ink { data } = &mut node.kind else { return Err(bad(C, "not an ink layer")) };
        let mut next = (**data).clone();
        let before = next.strokes.len();
        next.strokes.retain(|st| !touches(st, x, y, radius));
        let removed = before - next.strokes.len();
        *data = Arc::new(next);
        Ok(removed)
    })?;
    Ok(json!({ "removed": removed }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", &json!({"width": 400, "height": 300})).unwrap();
        s
    }

    fn layer_strokes(s: &Session, id: u64) -> usize {
        match s.active().unwrap().doc.node(NodeId(id)).map(|n| &n.kind) {
            Some(NodeKind::Ink { data }) => data.strokes.len(),
            _ => panic!("not an ink layer"),
        }
    }

    fn undo(s: &mut Session) {
        s.execute("edit.undo", &json!({})).unwrap();
    }

    #[test]
    fn first_stroke_makes_a_layer_and_undo_removes_it() {
        let mut s = session();
        let r = s.execute("ink.stroke", &json!({"points": [[10, 10], [50, 40, 0.5]]})).unwrap();
        let id = r["id"].as_u64().unwrap();
        assert_eq!(layer_strokes(&s, id), 1);
        undo(&mut s);
        assert!(s.active().unwrap().doc.node(NodeId(id)).is_none());
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(layer_strokes(&s, id), 1);
    }

    #[test]
    fn strokes_append_to_the_selected_layer_one_undo_step_each() {
        let mut s = session();
        let id = s.execute("ink.stroke", &json!({"points": [[10, 10], [20, 10]]})).unwrap()["id"].as_u64().unwrap();
        s.execute("ink.stroke", &json!({"points": [[30, 10], [40, 10]], "color": [255, 0, 0, 255], "width": 3}))
            .unwrap();
        assert_eq!(layer_strokes(&s, id), 2);
        undo(&mut s);
        assert_eq!(layer_strokes(&s, id), 1);
    }

    #[test]
    fn erase_removes_touching_strokes_and_undo_restores_them() {
        let mut s = session();
        let id = s.execute("ink.stroke", &json!({"points": [[10, 10], [100, 10]]})).unwrap()["id"].as_u64().unwrap();
        s.execute("ink.stroke", &json!({"points": [[10, 200], [100, 200]]})).unwrap();
        // The first stroke is crossed mid-segment; its endpoints are 45 pt away.
        let r = s.execute("ink.erase", &json!({"at": [55, 12], "radius": 2})).unwrap();
        assert_eq!(r["removed"], 1);
        assert_eq!(layer_strokes(&s, id), 1);
        undo(&mut s);
        assert_eq!(layer_strokes(&s, id), 2);
    }

    #[test]
    fn erase_that_hits_nothing_records_no_step() {
        let mut s = session();
        let id = s.execute("ink.stroke", &json!({"points": [[10, 10], [100, 10]]})).unwrap()["id"].as_u64().unwrap();
        let r = s.execute("ink.erase", &json!({"at": [300, 250]})).unwrap();
        assert_eq!(r["removed"], 0);
        assert_eq!(layer_strokes(&s, id), 1);
        // The stroke is the only step, so one undo removes the layer again.
        undo(&mut s);
        assert!(s.active().unwrap().doc.node(NodeId(id)).is_none());
    }

    #[test]
    fn erase_needs_an_ink_layer_selected() {
        let mut s = session();
        assert!(s.execute("ink.erase", &json!({"at": [1, 1]})).is_err());
        s.execute("shape.rectangle", &json!({"x": 0, "y": 0, "width": 10, "height": 10})).unwrap();
        assert!(s.execute("ink.erase", &json!({"at": [1, 1]})).is_err());
    }

    #[test]
    fn bad_input_is_refused_not_panicked() {
        let mut s = session();
        for bad_params in [
            json!({}),
            json!({"points": "x"}),
            json!({"points": [[1]]}),
            json!({"points": [["a", 2]]}),
            json!({"points": [[1, 2]], "color": [1, 2, 3]}),
            json!({"points": [[1, 2]], "color": [300, 0, 0, 0]}),
            json!({"points": [[1, 2]], "width": -1}),
            json!({"points": [[1e9, 2]]}),
            json!({"points": []}),
        ] {
            assert!(s.execute("ink.stroke", &bad_params).is_err(), "accepted {bad_params}");
        }
    }
}
