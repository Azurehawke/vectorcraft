# Vendored crates

| Crate | Version | Licence | Why |
|---|---|---|---|
| winit | 0.30.13 | Apache-2.0 | Copied from PdfCraft's `vendor/winit` (every patch is marked `PdfCraft patch:` in the source). For the Android build (`apps/vectorcraft-android`): a mouse delivers cursor, button and wheel events instead of touches, a hovering stylus moves the cursor, and a touch's tool type (finger, stylus, eraser, mouse, palm) rides in bits 32..40 of `Touch::id`. PdfCraft's Windows and Wayland fixes come with it. Remove once a winit release covers these. |
