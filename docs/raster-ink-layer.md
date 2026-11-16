# Raster ink layer (design)

Status: milestones 1, 2 and 3a landed on branch `raster-ink-layer`: the stroke store and grid index in
`crates/doc/src/ink.rs` (milestone 1), the in-memory tile cache in `crates/render/src/ink_tiles.rs`
(milestone 2), and `NodeKind::Ink` carried through the document, bounds and transforms (milestone 3a).
Milestone 3b adds the `ink.stroke` and `ink.erase` commands (`crates/engine/src/cmd/ink.rs`), each
committed stroke or erase one undo step. The Pencil overlay and the file version bump are not in yet, so
the layer is not yet drawn by the app's tile cache or saved in the file. Not a parity area; it is a proposal for hand-writing
performance on pen hardware.

## Problem

Each Pencil or Paintbrush stroke becomes a permanent vector path. While a stroke is written, the preview
re-runs the freehand fit on the whole stroke at every pointer move, and Smart Guide targets are gathered
again after each stroke. Long handwriting therefore slows the pen down.

## Proposal

Add a layer-level object, `NodeKind::InkLayer`, that holds:

- **Strokes as data.** Each stroke is a sampled polyline with colour, width and per-point pressure. This is
  the source of truth for editing, deleting, undo and export.
- **A raster tile cache.** Tiles (512 document units, as in InkCraft) are rasterized from the strokes and
  kept only in memory. They are never written to the file.

Behaviour:

- Drawing rasterizes only the new stroke into the tiles it touches. The cost per stroke stays flat.
- Delete and the eraser remove whole strokes and redraw the tiles they touched.
- Select, hit testing and undo work per stroke, as in InkCraft.
- Opening a file draws only the visible tiles, from the stored strokes.

## Integration points

- `crates/doc`: the new node kind, its serde form, and a stroke index. The spatial index must be updated
  incrementally, not rebuilt after every edit.
- `crates/render`: a tile cache keyed by layer and tile, filled with `vello_cpu` on worker threads and
  composited by the GPU as today. Blend modes and opacity apply to the layer as a whole.
- `crates/engine`: `ink.*` commands (begin, extend, commit, erase, delete) so that tools, the control channel,
  MCP and the CLI reach them. Each commit is one undo step.
- `crates/format`: a version bump with a migration (older files load with no ink layers). Fuzz the new
  reader under `engine/tests/import_fuzz.rs` and `format/tests/prop_format.rs`.
- `crates/ui-egui`: the Pencil and Paintbrush tools emit `ink.*` commands during a stroke, drawing the live
  stroke as an overlay until pen-up, as InkCraft does.

## Export

Strokes are exported as vector paths, so PDF, SVG and EPS output keep ink as vectors. Only the on-screen
tile cache is raster. Blend modes that cannot be expressed on paths are rasterized at export, as today for
live effects.

## Trade-offs

- Per-stroke editing and deletion are kept. Pure paint-style bitmap editing is not part of this design.
- Memory grows with the visible area, not with the stroke count. Tiles outside the view can be evicted.
- Zooming in redraws tiles at the new scale, so the first frame after a large zoom may stutter.
- Opening a dense file rasterizes the visible tiles first, which costs time once.

## Measurement before and after

- `vectorcraft-cli perf` before and after, on an idle machine, plus a new budget for writing 2,000 Pencil
  strokes (per-move cost).
- A test notebook of handwritten sentences, timed as a fixed sequence of pointer events through the control
  channel.

## Milestones

1. Stroke store and incremental index in `crates/doc`, with fuzz tests. **Done** (`ink.rs`: stable ids, a grid
   index updated per stroke, limits, versioned data, property tests). The format migration moves to milestone 2.
2. Tile cache in `crates/render`, with a headless test that a stroke is drawn and a deleted stroke is gone.
   **Done** (`ink_tiles.rs`: 512-unit tiles drawn with `vello_cpu`, invalidation by area, tests for drawn,
   deleted and untouched tiles, and a single-point dot).
3. Add the ink layer to the document and the file: a new node kind (about 210 match sites to update),
   format version bump and migration, and fuzz tests on the new reader. Then `ink.*` commands with undo, and
   the Pencil overlay in `crates/ui-egui`.
4. Perf budgets, parity table and gap entry updates (`docs/target-app-parity.md`, `docs/gaps.md`).

## Open questions

- Should a pressure curve be stored with each stroke (as today) or resampled on open?
- Tile size and eviction policy at high zoom.
- Whether the ink layer should appear as one layer or as one object per stroke in the Layers panel.

## Clean-room note

This design uses only the project's own code and the public documentation for PDF, SVG and EPS. No Adobe
software was used or consulted.
