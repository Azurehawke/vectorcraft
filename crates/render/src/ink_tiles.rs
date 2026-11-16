//! Raster tiles for the ink layer: the strokes of an [`InkStore`] drawn into square tiles and cached
//! until they change.
//!
//! Tiles live only in memory. The document keeps the strokes, so a tile is rebuilt from them whenever
//! it is missing. A stroke changes only the tiles its area touches, so [`InkTiles::invalidate`] drops
//! just those and leaves the rest of the cache alone.

use std::collections::HashMap;

use vectorcraft_doc::ink::InkStore;
use vello_cpu::kurbo::{Affine, BezPath, Cap, Join, Stroke};
use vello_cpu::peniko::{self};
use vello_cpu::{Pixmap, RasterizerSettings, RenderContext, RenderSettings, Resources};

/// Side of one tile in document units.
pub const INK_TILE: f64 = 512.0;
/// Smallest and largest device pixels per document unit. Keeps a tile within `u16` pixels.
const MIN_SCALE: f64 = 1.0 / 16.0;
const MAX_SCALE: f64 = 64.0;

/// Position of a tile: its index in units of [`INK_TILE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub x: i64,
    pub y: i64,
}

impl TileKey {
    /// The tile containing document point `p`.
    pub fn containing(p: [f64; 2]) -> Self {
        Self { x: (p[0] / INK_TILE).floor() as i64, y: (p[1] / INK_TILE).floor() as i64 }
    }

    /// The tile's area in document units: `[min_x, min_y, max_x, max_y]`.
    pub fn bounds(self) -> [f64; 4] {
        let (x, y) = (self.x as f64 * INK_TILE, self.y as f64 * INK_TILE);
        [x, y, x + INK_TILE, y + INK_TILE]
    }
}

/// The rasterized tiles of one ink layer at one zoom level.
pub struct InkTiles {
    scale: f64,
    pixels: u16,
    tiles: HashMap<TileKey, Pixmap>,
    resources: Resources,
}

impl InkTiles {
    /// A cache at `scale` device pixels per document unit. A bad scale falls back to 1.
    pub fn new(scale: f64) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 { scale.clamp(MIN_SCALE, MAX_SCALE) } else { 1.0 };
        let pixels = (INK_TILE * scale).round() as u16;
        Self { scale, pixels, tiles: HashMap::new(), resources: Resources::new() }
    }

    /// Side of a tile in device pixels.
    pub fn tile_pixels(&self) -> u16 {
        self.pixels
    }

    /// Number of tiles currently rasterized.
    pub fn cached(&self) -> usize {
        self.tiles.len()
    }

    /// The tile at `key`, rasterized from `store` if it is not cached.
    pub fn tile(&mut self, store: &InkStore, key: TileKey) -> &Pixmap {
        if !self.tiles.contains_key(&key) {
            let pixmap = self.rasterize(store, key);
            self.tiles.insert(key, pixmap);
        }
        // Inserted just above when missing, so this lookup cannot fail.
        self.tiles.entry(key).or_insert_with(|| Pixmap::new(self.pixels, self.pixels))
    }

    /// Drops every cached tile that `area` (document units) touches. Other tiles stay cached.
    pub fn invalidate(&mut self, area: [f64; 4]) {
        self.tiles.retain(|key, _| !overlaps(key.bounds(), area));
    }

    /// Drops every cached tile.
    pub fn clear(&mut self) {
        self.tiles.clear();
    }

    fn rasterize(&mut self, store: &InkStore, key: TileKey) -> Pixmap {
        let px = self.pixels;
        let mut ctx = RenderContext::new_with(px, px, RenderSettings { num_threads: 0, ..Default::default() });
        let origin = key.bounds();
        ctx.set_transform(Affine::scale(self.scale) * Affine::translate((-origin[0], -origin[1])));
        for id in store.query(origin) {
            let Some(stroke) = store.get(id) else { continue };
            let Some((first, rest)) = stroke.points.split_first() else { continue };
            let mut path = BezPath::new();
            path.move_to((first.x, first.y));
            for p in rest {
                path.line_to((p.x, p.y));
            }
            // A single point still gets a dot: a zero-length segment draws nothing, so it gets
            // a half-unit one, which round caps turn into a dot.
            if rest.is_empty() {
                path.line_to((first.x + 0.5, first.y));
            }
            let [r, g, b, a] = stroke.color;
            ctx.set_paint(peniko::Color::from_rgba8(r, g, b, a));
            let mut style = Stroke::new(stroke.width);
            style.start_cap = Cap::Round;
            style.end_cap = Cap::Round;
            style.join = Join::Round;
            ctx.set_stroke(style);
            ctx.stroke_path(&path);
        }
        ctx.flush();
        let mut pixmap = Pixmap::new(px, px);
        ctx.render_with(&mut pixmap, &mut self.resources, RasterizerSettings::default());
        pixmap
    }
}

fn overlaps(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// Alpha of the premultiplied pixel at `(x, y)` in a tile, or `None` if it is outside the tile.
pub fn alpha_at(pixmap: &Pixmap, x: u16, y: u16) -> Option<u8> {
    let w = pixmap.width();
    if x >= w || y >= pixmap.height() {
        return None;
    }
    let index = (y as usize * w as usize + x as usize) * 4 + 3;
    pixmap.data_as_u8_slice().get(index).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vectorcraft_doc::ink::{InkPoint, InkStroke};

    fn line(y: f64, x0: f64, x1: f64) -> InkStroke {
        InkStroke {
            color: [20, 40, 200, 255],
            width: 20.0,
            points: vec![
                InkPoint { x: x0, y, pressure: 1.0 },
                InkPoint { x: x1, y, pressure: 1.0 },
            ],
        }
    }

    #[test]
    fn a_drawn_stroke_shows_in_its_tile() {
        let mut store = InkStore::new();
        store.insert(line(100.0, 10.0, 500.0)).unwrap();
        let mut tiles = InkTiles::new(1.0);
        assert_eq!(tiles.tile_pixels(), 512);
        let pixmap = tiles.tile(&store, TileKey { x: 0, y: 0 });
        assert_eq!(alpha_at(pixmap, 250, 100), Some(255));
        assert_eq!(alpha_at(pixmap, 250, 400), Some(0));
    }

    #[test]
    fn a_deleted_stroke_leaves_its_tile_after_invalidation() {
        let mut store = InkStore::new();
        let id = store.insert(line(100.0, 10.0, 500.0)).unwrap();
        let mut tiles = InkTiles::new(1.0);
        assert_eq!(alpha_at(tiles.tile(&store, TileKey { x: 0, y: 0 }), 250, 100), Some(255));

        let removed = store.remove(id).unwrap();
        let area = [10.0 - 10.0, 100.0 - 10.0, 500.0 + 10.0, 100.0 + 10.0];
        assert_eq!(removed.points.len(), 2);
        tiles.invalidate(area);
        assert_eq!(alpha_at(tiles.tile(&store, TileKey { x: 0, y: 0 }), 250, 100), Some(0));
    }

    #[test]
    fn invalidation_keeps_tiles_the_change_does_not_touch() {
        let mut store = InkStore::new();
        store.insert(line(100.0, 10.0, 500.0)).unwrap();
        let mut tiles = InkTiles::new(1.0);
        tiles.tile(&store, TileKey { x: 0, y: 0 });
        tiles.tile(&store, TileKey { x: 3, y: 3 });
        assert_eq!(tiles.cached(), 2);
        tiles.invalidate([10.0, 90.0, 20.0, 110.0]);
        assert_eq!(tiles.cached(), 1);
    }

    #[test]
    fn a_single_point_draws_a_dot_and_bad_scales_fall_back() {
        let mut store = InkStore::new();
        store.insert(InkStroke {
            color: [0, 0, 0, 255],
            width: 10.0,
            points: vec![InkPoint { x: 50.0, y: 50.0, pressure: 1.0 }],
        })
        .unwrap();
        let mut tiles = InkTiles::new(f64::NAN);
        assert_eq!(tiles.tile_pixels(), 512);
        assert!(alpha_at(tiles.tile(&store, TileKey { x: 0, y: 0 }), 50, 50).unwrap_or(0) > 0);
        assert_eq!(InkTiles::new(1.0e9).tile_pixels(), 512 * 64);
    }
}
