//! Raster ink layers in the native file: they save at version 5 and round-trip; older versions
//! refuse them instead of dropping the ink.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use vectorcraft_doc::ink::{INK_DATA_VERSION, InkData, InkPoint, InkStroke};
use vectorcraft_doc::{Document, Node, NodeKind};
use vectorcraft_format::{FormatError, INK_SINCE, SaveOptions, VERSION, load, save_with};
use vectorcraft_testkit::fixtures;

/// The rich fixture with one ink layer added to its first layer.
fn doc_with_ink() -> Document {
    let mut d = (*fixtures::rich_session().doc().unwrap().doc).clone();
    let stroke = InkStroke { color: [0, 0, 0, 255], width: 2.0, points: vec![InkPoint { x: 1.0, y: 2.0, pressure: 1.0 }, InkPoint { x: 9.0, y: 4.0, pressure: 0.5 }] };
    let ink = Node::new(d.alloc_id(), NodeKind::Ink { data: Arc::new(InkData { version: INK_DATA_VERSION, strokes: vec![stroke] }) });
    let layer = Arc::make_mut(d.layers.first_mut().unwrap());
    if let NodeKind::Layer { children, .. } = &mut layer.kind {
        children.push(Arc::new(ink));
    } else {
        panic!("first root is not a layer");
    }
    d
}

fn opts(version: u32) -> SaveOptions {
    SaveOptions { version, ..SaveOptions::default() }
}

#[test]
fn ink_layers_round_trip_at_the_current_version() {
    assert_eq!(VERSION, 5);
    let d = doc_with_ink();
    assert!(d.has_ink());
    let bytes = save_with(&d, &opts(VERSION)).unwrap();
    let back = load(&bytes).unwrap();
    assert!(back.has_ink());
}

#[test]
fn older_versions_refuse_ink_instead_of_dropping_it() {
    let d = doc_with_ink();
    for version in 1..INK_SINCE {
        assert!(matches!(save_with(&d, &opts(version)), Err(FormatError::InkTooOld(v)) if v == version), "version {version}");
    }
}

#[test]
fn documents_without_ink_still_save_at_every_version() {
    let d = (*fixtures::rich_session().doc().unwrap().doc).clone();
    assert!(!d.has_ink());
    for version in 1..=VERSION {
        save_with(&d, &opts(version)).unwrap_or_else(|e| panic!("version {version}: {e}"));
    }
}
