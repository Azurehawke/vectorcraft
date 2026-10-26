//! What a tablet needs around the desktop UI: room for the system bars, pen-first input, a mouse
//! that behaves as one, and no on-screen keyboard while a hardware one is attached.
//!
//! - **System bars.** NativeActivity draws under the status bar and the taskbar. The app is laid
//!   out inside the part Android leaves free (its content rect), and egui is told the same insets.
//! - **Input.** vendor/winit delivers a mouse as cursor, button and wheel events (they pass
//!   through untouched) and tags each touch with its tool. For touches the pointer is rebuilt here
//!   instead of by egui-winit: the S Pen always takes it (a palm resting first can't), a palm
//!   never counts, fingers don't while the pen is down. One finger on the canvas scrolls it
//!   rather than drawing (two fingers keep VectorCraft's own pinch and undo/redo taps); elsewhere
//!   fingers tap buttons and panels.

use std::collections::{HashMap, HashSet};

use egui::{Event, Modifiers, Pos2, TouchPhase};
use vectorcraft_engine::file_access;
use vectorcraft_ui_egui::VectorcraftApp;
use winit::platform::android::activity::AndroidApp;

pub struct TabletApp {
    pub app: VectorcraftApp,
    android: AndroidApp,
    picker: crate::picker::Shared,
    touches: TouchRouter,
    insets: egui::epaint::MarginF32,
    focused: bool,
    /// The previous run's crash report, announced once.
    previous: Option<crate::crash_log::Previous>,
    /// New builds from the GitHub repository the APK came from (`updater`).
    updater: crate::updater::Updater,
}

impl TabletApp {
    pub fn new(app: VectorcraftApp, android: AndroidApp, picker: crate::picker::Shared, previous: Option<crate::crash_log::Previous>) -> Self {
        log::info!("{}", crate::crash_log::FOREGROUND);
        crate::crash_log::set_front(true);
        Self {
            app,
            android,
            picker,
            touches: TouchRouter::default(),
            insets: egui::epaint::MarginF32::default(),
            focused: true,
            previous,
            updater: crate::updater::Updater::default(),
        }
    }

    /// Say once that the last run ended badly and where its report is.
    fn crash_notice(&mut self, ctx: &egui::Context) {
        let Some(previous) = &self.previous else { return };
        let mut open = true;
        let mut ok = false;
        egui::Window::new("VectorCraft closed unexpectedly")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("The last time VectorCraft ran, it stopped unexpectedly. A crash report was saved to:");
                ui.strong(&previous.saved_to);
                ui.label("Sending it along with what you were doing helps get it fixed.");
                ok = ui.button("OK").clicked();
            });
        if !open || ok {
            self.previous = None;
        }
    }

    fn system_bar_insets(&self, ctx: &egui::Context, raw: &egui::RawInput) -> Option<egui::epaint::MarginF32> {
        let ppp = ctx.pixels_per_point();
        let screen = raw.screen_rect?;
        if !(ppp.is_finite() && ppp > 0.0) {
            return None;
        }
        let r = self.android.content_rect();
        if r.right <= r.left || r.bottom <= r.top {
            return None;
        }
        let (w, h) = (screen.width() * ppp, screen.height() * ppp);
        let inset = |px: f32| (px / ppp).clamp(0.0, 400.0);
        Some(egui::epaint::MarginF32 {
            left: inset(r.left as f32),
            top: inset(r.top as f32),
            right: inset(w - r.right as f32),
            bottom: inset(h - r.bottom as f32),
        })
    }

    /// With a keyboard cover or Bluetooth keyboard attached, text fields don't ask for the
    /// on-screen keyboard (Samsung shows its keyboard toolbar for that, taking room from the app).
    fn keep_soft_keyboard_away(&self, ctx: &egui::Context) {
        if matches!(self.android.config().keys_hidden(), ndk::configuration::KeysHidden::No) {
            ctx.output_mut(|o| o.ime = None);
        }
    }
}

impl eframe::App for TabletApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some(insets) = self.system_bar_insets(ctx, raw) {
            if insets != self.insets {
                log::info!("system bar insets (points): {insets:?}");
            }
            self.insets = insets;
            raw.safe_area_insets = Some(egui::SafeAreaInsets(insets));
        }
        // Going to the background: Android may stop the app without notice, so the preferences
        // are saved now.
        if self.focused && !raw.focused {
            crate::save_prefs(&self.app);
            log::info!("{}", crate::crash_log::BACKGROUND);
            crate::crash_log::flush();
        } else if !self.focused && raw.focused {
            log::info!("{}", crate::crash_log::FOREGROUND);
            // Back from Settings with All files access on: a waiting crash report can go to Download.
            if crate::crash_log::deliver_pending() {
                self.app.status("The crash report was saved to Download › VectorCraft crash report.txt");
            }
        }
        if self.focused != raw.focused {
            crate::crash_log::set_front(raw.focused);
        }
        self.focused = raw.focused;
        let canvas = vectorcraft_ui_egui::canvas::canvas_place(ctx);
        self.touches.route(&mut raw.events, |pos| canvas.is_some_and(|(rect, layer)| rect.contains(pos) && ctx.layer_id_at(pos) == Some(layer)));
        if self.touches.pending() {
            ctx.request_repaint();
        }
        file_access::unconfined(|| self.app.raw_input_hook(raw));
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // At start-up, then at most hourly (`updater`).
        self.updater.maybe_check(ctx.input(|i| i.time));
        file_access::unconfined(|| self.app.logic(ctx));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let full = ui.max_rect();
        let m = self.insets;
        let inner = egui::Rect::from_min_max(full.min + egui::vec2(m.left, m.top), full.max - egui::vec2(m.right, m.bottom));
        if inner.width() < 100.0 || inner.height() < 100.0 {
            file_access::unconfined(|| self.app.ui(ui));
        } else {
            // Behind the status bar: the window background, so the bar reads as part of the app.
            ui.painter().rect_filled(full, 0.0, ui.visuals().window_fill);
            ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
                ui.set_clip_rect(inner);
                file_access::unconfined(|| self.app.ui(ui));
            });
        }
        // The shell's own windows use the app's fonts, which take effect on the second frame;
        // a bug in them costs a frame, not the app.
        if self.app.fonts_ready() {
            let ctx = ui.ctx().clone();
            if let Err(e) = vectorcraft_engine::guard::catch_panic(|| {
                self.picker.show(&ctx);
                self.crash_notice(&ctx);
                self.updater.show(&ctx, inner);
            }) {
                self.previous = None;
                self.app.status(format!("Internal error: {e} (please report this bug)"));
            }
        }
        self.keep_soft_keyboard_away(ui.ctx());
        crate::crash_log::beat_with(ui.ctx());
    }

    fn on_exit(&mut self) {
        crate::save_prefs(&self.app);
        log::info!("{}", crate::crash_log::EXIT);
        crate::crash_log::flush();
    }
}

/// Which tool a touch is, from the bits vendor/winit puts above the pointer id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    Finger,
    Pen,
    Palm,
}

fn tool_of(id: u64) -> Tool {
    match (id >> 32) & 0xff {
        2 | 4 => Tool::Pen, // stylus, eraser end
        5 => Tool::Palm,
        _ => Tool::Finger, // finger, mouse in touch mode, unknown
    }
}

#[derive(Clone, Copy, Debug)]
struct Down {
    tool: Tool,
    device: egui::TouchDeviceId,
    pos: Pos2,
}

#[derive(Default)]
struct TouchRouter {
    down: HashMap<u64, Down>,
    /// Touches egui doesn't see (a palm; fingers while the pen is down).
    hidden: HashSet<u64>,
    /// The touch that drives egui's pointer.
    owner: Option<u64>,
    /// Fingers that landed on the canvas: they scroll it, and where each was last.
    scrolling: HashMap<u64, Pos2>,
    modifiers: Modifiers,
    /// Pointer events for the next frame and the one after: a tap's lift waits a frame when its
    /// press came in this one, and the pointer leaves a frame after the lift, so egui (which finds
    /// what is under the pointer once a frame) sees what was tapped.
    next: Vec<Event>,
    after: Vec<Event>,
    started_now: HashSet<u64>,
}

impl TouchRouter {
    fn pending(&self) -> bool {
        !self.next.is_empty() || !self.after.is_empty()
    }

    fn pen_down(&self) -> bool {
        self.down.values().any(|d| d.tool == Tool::Pen)
    }

    fn press(&self, out: &mut Vec<Event>, pos: Pos2, pressed: bool) {
        out.push(Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: self.modifiers });
    }

    fn lift(&mut self, id: u64, pos: Pos2, out: &mut Vec<Event>) {
        let release = Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: self.modifiers };
        if self.started_now.contains(&id) {
            self.next.push(release);
            self.after.push(Event::PointerGone);
        } else {
            out.push(release);
            self.next.push(Event::PointerGone);
        }
    }

    fn hide(&mut self, id: u64, out: &mut Vec<Event>) {
        self.scrolling.remove(&id);
        if !self.hidden.insert(id) {
            return;
        }
        if let Some(d) = self.down.get(&id) {
            out.push(Event::Touch { device_id: d.device, id: egui::TouchId(id), phase: TouchPhase::Cancel, pos: d.pos, force: None });
        }
    }

    /// Replace egui-winit's touch-to-pointer translation (module docs). egui-winit puts a touch's
    /// pointer events right after its `Touch` event; those go. Other pointer events (a mouse's)
    /// stay.
    fn route(&mut self, events: &mut Vec<Event>, on_canvas: impl Fn(Pos2) -> bool) {
        let mut out = std::mem::take(&mut self.next);
        self.next = std::mem::take(&mut self.after);
        self.started_now.clear();
        let mut after_touch = false;
        for event in std::mem::take(events) {
            match event {
                Event::Touch { device_id, id, phase, pos, .. } => {
                    after_touch = true;
                    self.touch(device_id, id.0, phase, pos, &on_canvas, &mut out);
                    let ended = matches!(phase, TouchPhase::End | TouchPhase::Cancel);
                    let hidden = if ended { self.hidden.remove(&id.0) } else { self.hidden.contains(&id.0) };
                    if !hidden {
                        out.push(event);
                    }
                }
                Event::PointerMoved(_) | Event::PointerButton { .. } | Event::PointerGone if after_touch => {
                    if let Event::PointerButton { modifiers, .. } = event {
                        self.modifiers = modifiers;
                    }
                }
                other => {
                    after_touch = false;
                    if let Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } = &other {
                        self.modifiers = *modifiers;
                    }
                    out.push(other);
                }
            }
        }
        *events = out;
    }

    fn touch(&mut self, device: egui::TouchDeviceId, id: u64, phase: TouchPhase, pos: Pos2, on_canvas: &impl Fn(Pos2) -> bool, out: &mut Vec<Event>) {
        let tool = tool_of(id);
        match phase {
            TouchPhase::Start => {
                let pen_was_down = self.pen_down();
                self.down.insert(id, Down { tool, device, pos });
                if tool == Tool::Palm || (tool == Tool::Finger && pen_was_down) {
                    self.hidden.insert(id);
                    return;
                }
                if tool == Tool::Pen {
                    if let Some(owner) = self.owner.take()
                        && let Some(d) = self.down.get(&owner).copied()
                    {
                        self.press(out, d.pos, false);
                        out.push(Event::PointerGone);
                    }
                    let fingers: Vec<u64> = self.down.iter().filter(|(_, d)| d.tool == Tool::Finger).map(|(id, _)| *id).collect();
                    for finger in fingers {
                        self.hide(finger, out);
                    }
                }
                // A finger on the canvas scrolls it; it doesn't press anything.
                if tool == Tool::Finger && on_canvas(pos) {
                    self.scrolling.insert(id, pos);
                    return;
                }
                if self.owner.is_none() {
                    self.owner = Some(id);
                    self.started_now.insert(id);
                    out.retain(|e| !matches!(e, Event::PointerGone));
                    self.next.retain(|e| !matches!(e, Event::PointerGone));
                    self.after.retain(|e| !matches!(e, Event::PointerGone));
                    out.push(Event::PointerMoved(pos));
                    self.press(out, pos, true);
                }
            }
            TouchPhase::Move => {
                if let Some(d) = self.down.get_mut(&id) {
                    d.pos = pos;
                }
                if self.owner == Some(id) {
                    out.push(Event::PointerMoved(pos));
                }
                // One finger scrolls; two or more are VectorCraft's pinch and pan.
                let fingers = self.down.values().filter(|d| d.tool == Tool::Finger).count();
                if let Some(last) = self.scrolling.get_mut(&id) {
                    let delta = pos - *last;
                    *last = pos;
                    if fingers == 1 && delta != egui::Vec2::ZERO {
                        out.push(Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta, phase: TouchPhase::Move, modifiers: Modifiers::NONE });
                    }
                }
            }
            TouchPhase::End | TouchPhase::Cancel => {
                self.down.remove(&id);
                self.scrolling.remove(&id);
                if self.owner == Some(id) {
                    self.owner = None;
                    self.lift(id, pos, out);
                }
            }
        }
    }
}
