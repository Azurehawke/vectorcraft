//! Ink Pencil: writes handwriting into the raster ink layer (`NodeKind::Ink`) instead of making a
//! vector path per stroke, so a long line of writing does not slow the pen down. The stroke is
//! previewed as a plain polyline while the pen is down and committed as one `ink.stroke` command
//! (one undo step) on release. Pen pressure is kept per point.

use serde_json::{Value, json};
use vectorcraft_geom::Point;

use super::{FEEDBACK, polyline};
use crate::{Action, Cursor, Mods, Overlay, PointerEvent, PointerKind, Tool, ToolContext};

/// Samples closer than this (points) to the last one are dropped; they add nothing to the ink.
const MIN_STEP: f64 = 0.25;

#[derive(Default)]
pub struct InkTool {
    points: Vec<(Point, f32)>,
    active: bool,
}

impl InkTool {
    /// The stroke as `ink.stroke` points, each `[x, y, pressure]`.
    fn stroke_json(&self) -> Value {
        Value::Array(self.points.iter().map(|(p, f)| json!([p.x, p.y, (f64::from(*f) * 1000.0).round() / 1000.0])).collect())
    }

    fn push(&mut self, p: Point, pressure: f32) {
        if self.points.last().is_some_and(|(l, _)| l.distance(p) < MIN_STEP) {
            return;
        }
        self.points.push((p, pressure));
    }
}

impl Tool for InkTool {
    fn id(&self) -> &'static str {
        "inkPencil"
    }

    fn pointer(&mut self, _cx: &ToolContext, ev: &PointerEvent) -> Vec<Action> {
        match ev.kind {
            PointerKind::Down => {
                self.active = true;
                self.points.clear();
                self.points.push((ev.pos, ev.pressure));
                vec![]
            }
            PointerKind::Drag if self.active => {
                self.push(ev.pos, ev.pressure);
                vec![]
            }
            PointerKind::Up if self.active => {
                self.active = false;
                self.push(ev.pos, ev.pressure);
                let out = if self.points.is_empty() { vec![] } else { vec![Action::Exec("ink.stroke".into(), json!({"points": self.stroke_json()}))] };
                self.points.clear();
                out
            }
            _ => vec![],
        }
    }

    fn overlays(&self, _cx: &ToolContext) -> Vec<Overlay> {
        if !self.active || self.points.len() < 2 {
            return vec![];
        }
        let pts: Vec<Point> = self.points.iter().map(|(p, _)| *p).collect();
        vec![Overlay::Path { path: polyline(&pts), color: FEEDBACK, width: 1.0, dashed: false }]
    }

    fn cursor(&self, _cx: &ToolContext, _p: Point, _m: Mods) -> Cursor {
        Cursor::Crosshair
    }

    fn busy(&self) -> bool {
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use vectorcraft_doc::Selection;

    #[test]
    fn a_stroke_is_one_ink_command_with_pressure() {
        let (d, _) = doc_with_rect();
        let s = Selection::default();
        let pd = paint();
        let cx = cx(&d, &s, &pd);
        let mut t = InkTool::default();
        let pe = |k, x: f64, y: f64, p: f32| PointerEvent { kind: k, pos: Point::new(x, y), mods: Mods::default(), pressure: p };
        assert!(t.pointer(&cx, &pe(PointerKind::Down, 0.0, 0.0, 0.5)).is_empty());
        assert!(t.pointer(&cx, &pe(PointerKind::Drag, 0.0, 0.0, 0.5)).is_empty(), "a repeat sample is dropped");
        assert!(t.pointer(&cx, &pe(PointerKind::Drag, 10.0, 5.0, 1.0)).is_empty());
        let out = t.pointer(&cx, &pe(PointerKind::Up, 20.0, 5.0, 1.0));
        let [Action::Exec(cmd, v)] = out.as_slice() else { panic!("{out:?}") };
        assert_eq!(cmd, "ink.stroke");
        let pts = v["points"].as_array().unwrap();
        assert_eq!(pts.len(), 3);
        assert_eq!(pts[0][2], 0.5);
        assert!(!t.busy());
    }

    #[test]
    fn a_click_without_movement_still_writes_a_dot() {
        let (d, _) = doc_with_rect();
        let s = Selection::default();
        let pd = paint();
        let cx = cx(&d, &s, &pd);
        let mut t = InkTool::default();
        let pe = |k, x: f64| PointerEvent { kind: k, pos: Point::new(x, 3.0), mods: Mods::default(), pressure: 1.0 };
        t.pointer(&cx, &pe(PointerKind::Down, 7.0));
        let out = t.pointer(&cx, &pe(PointerKind::Up, 7.0));
        assert!(matches!(out.as_slice(), [Action::Exec(c, v)] if c == "ink.stroke" && v["points"].as_array().unwrap().len() == 1));
    }
}
