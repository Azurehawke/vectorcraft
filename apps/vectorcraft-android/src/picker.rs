//! File dialogs on Android: a file browser drawn in the app.
//!
//! Android's own picker answers with `content://` URIs and needs a Java activity to receive them;
//! VectorCraft opens and saves paths. So the app asks for "All files access" (granted once in
//! Settings, `file_access`) and browses shared storage itself. VectorCraft asks for a path through
//! [`Services::start_pick`](vectorcraft_ui_egui::Services): the browser opens, and its answer goes
//! back on the channel, where the app picks it up and carries on.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};

use vectorcraft_engine::cmd::fileio;
use vectorcraft_ui_egui::picks::PickRequest;

/// Shared storage ("Internal storage" in Android's Files app).
const SHARED: &str = "/storage/emulated/0";
/// Entries listed at most: a folder is still untrusted input.
const MAX_ENTRIES: usize = 5000;
const ROW_HEIGHT: f32 = 34.0;
/// Rows on a phone: room for a fingertip.
const ROW_HEIGHT_COMPACT: f32 = 44.0;
/// Narrower than this (points), the browser takes the whole screen (a phone).
const COMPACT_WIDTH: f32 = 700.0;

/// The browser showing now, if any, shared by the app's services and the window.
#[derive(Clone, Default)]
pub struct Shared(Rc<RefCell<State>>);

#[derive(Default)]
struct State {
    open: Option<Browser>,
    /// The folder the last browser ended in, where the next one starts.
    last_dir: Option<PathBuf>,
}

impl Shared {
    /// Show a browser for `request`; its answer (no paths: cancelled) arrives on the receiver.
    pub fn start(&self, request: PickRequest) -> Option<Receiver<Vec<String>>> {
        let (tx, rx) = channel();
        let mut state = self.0.borrow_mut();
        // A second request while one shows: the first is cancelled.
        if let Some(old) = state.open.take() {
            let _ = old.answer.send(Vec::new());
        }
        let start = state.last_dir.clone().filter(|d| d.is_dir());
        state.open = Some(Browser::new(request, start, tx));
        Some(rx)
    }

    /// Draw the browser, if one is showing.
    pub fn show(&self, ctx: &egui::Context) {
        let mut state = self.0.borrow_mut();
        let Some(browser) = state.open.as_mut() else { return };
        if let Some(paths) = browser.show(ctx) {
            if !paths.is_empty() {
                state.last_dir = state.open.as_ref().map(|b| b.dir.clone());
            }
            if let Some(b) = state.open.take() {
                let _ = b.answer.send(paths);
            }
            ctx.request_repaint();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    File,
    Files,
    Folder,
    Save,
}

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    path: PathBuf,
    dir: bool,
}

struct Browser {
    mode: Mode,
    title: &'static str,
    /// (label, extensions without the dot, lowercase).
    filters: Vec<(String, Vec<String>)>,
    /// Index into `filters`; `filters.len()` is every file.
    filter: usize,
    dir: PathBuf,
    entries: Vec<Entry>,
    error: Option<String>,
    selected: Vec<PathBuf>,
    name: String,
    confirm_replace: Option<PathBuf>,
    new_folder: Option<String>,
    access: bool,
    access_note: Option<String>,
    answer: Sender<Vec<String>>,
}

fn filters_of<N, X, E>(list: impl IntoIterator<Item = (N, X)>) -> Vec<(String, Vec<String>)>
where
    N: AsRef<str>,
    X: IntoIterator<Item = E>,
    E: AsRef<str>,
{
    list.into_iter()
        .map(|(name, exts)| (name.as_ref().to_string(), exts.into_iter().map(|e| e.as_ref().trim_start_matches('.').to_ascii_lowercase()).collect()))
        .collect()
}

impl Browser {
    fn new(request: PickRequest, last: Option<PathBuf>, answer: Sender<Vec<String>>) -> Self {
        let (mode, title, filters, folder, name) = match &request {
            PickRequest::Open(p) => {
                (Mode::File, "Open", filters_of(p.filters.iter().map(|(n, e)| (n.as_str(), e.iter()))), p.folder.clone(), String::new())
            }
            PickRequest::OpenMany => (Mode::Files, "Place", filters_of(fileio::place_filters()), None, String::new()),
            PickRequest::Save(p) => {
                (Mode::Save, "Save As", filters_of(p.filters.iter().map(|(n, e)| (n.as_str(), e.iter()))), p.folder.clone(), p.name.clone())
            }
            PickRequest::Folder => (Mode::Folder, "Choose a Folder", Vec::new(), None, String::new()),
        };
        // A save dialog's folder (Templates, presets) may not exist yet.
        if mode == Mode::Save
            && let Some(f) = &folder
        {
            let _ = std::fs::create_dir_all(f);
        }
        let start = folder.map(PathBuf::from).filter(|d| d.is_dir()).or(last).unwrap_or_else(default_dir);
        let mut b = Self {
            mode,
            title,
            filters,
            filter: 0,
            dir: start.clone(),
            entries: Vec::new(),
            error: None,
            selected: Vec::new(),
            name,
            confirm_replace: None,
            new_folder: None,
            access: crate::file_access::granted(),
            access_note: None,
            answer,
        };
        b.go(start);
        b
    }

    fn go(&mut self, dir: PathBuf) {
        self.selected.clear();
        self.confirm_replace = None;
        match list(&dir) {
            Ok(e) => {
                self.entries = e;
                self.error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(e);
            }
        }
        self.dir = dir;
    }

    fn shows(&self, e: &Entry) -> bool {
        if e.dir {
            return true;
        }
        if self.mode == Mode::Folder {
            return false;
        }
        let Some((_, exts)) = self.filters.get(self.filter) else { return true };
        if exts.is_empty() {
            return true;
        }
        let ext = Path::new(&e.name).extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        exts.iter().any(|x| x == "*" || *x == ext)
    }

    fn save_path(&self) -> Option<PathBuf> {
        let name = self.name.trim();
        if name.is_empty() || name.contains('/') {
            return None;
        }
        let mut name = name.to_string();
        if Path::new(&name).extension().is_none()
            && let Some(ext) = self.filters.get(self.filter).and_then(|(_, e)| e.first()).filter(|e| *e != "*")
        {
            name = format!("{name}.{ext}");
        }
        Some(self.dir.join(name))
    }

    fn confirm_save(&mut self, answer: &mut Option<Vec<String>>) {
        let Some(path) = self.save_path() else { return };
        if path.exists() && self.confirm_replace.as_ref() != Some(&path) {
            self.confirm_replace = Some(path);
            return;
        }
        *answer = Some(vec![path.to_string_lossy().to_string()]);
    }

    /// Draw it. `Some` once the user is done: the chosen paths, or none when cancelled.
    fn show(&mut self, ctx: &egui::Context) -> Option<Vec<String>> {
        if !self.access {
            if crate::file_access::granted() {
                self.access = true;
                let dir = self.dir.clone();
                self.go(dir);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs(1));
            }
        }
        let mut answer: Option<Vec<String>> = None;
        let mut go_to: Option<PathBuf> = None;
        let screen = ctx.content_rect();
        // A phone: the browser fills the screen, the shortcuts scroll sideways in one row and the
        // list takes the height that's left, so the buttons below it stay in sight (also with the
        // on-screen keyboard up, which shrinks the content rect).
        let compact = screen.width() < COMPACT_WIDTH;
        let row_height = if compact { ROW_HEIGHT_COMPACT } else { ROW_HEIGHT };
        // Everything but the list: title, shortcuts, path, filter, name, buttons, margins.
        let mut reserved = if compact { 300.0 } else { 330.0 };
        if !self.access {
            reserved += 190.0;
        }
        if self.mode == Mode::Save {
            reserved += 50.0;
        }
        if self.confirm_replace.is_some() {
            reserved += 26.0;
        }
        let list_height =
            if compact { (screen.height() - reserved).max(120.0) } else { (screen.height() * 0.5).max(200.0).min(screen.height() - reserved) };
        let modal = egui::Modal::new(egui::Id::new("android-file-browser")).show(ctx, |ui| {
            if compact {
                ui.set_width((screen.width() - 40.0).max(240.0));
            } else {
                ui.set_width((screen.width() * 0.7).clamp(320.0, 900.0));
            }
            ui.spacing_mut().interact_size.y = row_height;
            ui.heading(self.title);
            ui.add_space(6.0);
            if !self.access {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new("VectorCraft can't see your files yet").strong());
                    ui.label("To open and save files anywhere on this device, turn on All files access for VectorCraft.");
                    if ui.add(egui::Button::new(egui::RichText::new("Allow access…").strong()).min_size(egui::vec2(200.0, 36.0))).clicked() {
                        self.access_note = Some(match crate::file_access::request() {
                            Ok(()) => "Opening Settings… If nothing appears, use the steps below.".to_string(),
                            Err(e) => format!("Settings didn't open: {e}"),
                        });
                    }
                    if let Some(note) = &self.access_note {
                        ui.label(egui::RichText::new(note).italics());
                    }
                    ui.label("Or yourself: Settings ▸ Apps ▸ ⋮ (top right) ▸ Special access ▸ All files access ▸ VectorCraft ▸ turn it on.");
                });
                ui.add_space(6.0);
            }
            let places = places();
            let place_buttons = |ui: &mut egui::Ui, go_to: &mut Option<PathBuf>| {
                for (label, path) in &places {
                    if ui.button(label).clicked() {
                        *go_to = Some(path.clone());
                    }
                }
            };
            if compact {
                egui::ScrollArea::horizontal().id_salt("browser-places").show(ui, |ui| ui.horizontal(|ui| place_buttons(ui, &mut go_to)));
            } else {
                ui.horizontal_wrapped(|ui| place_buttons(ui, &mut go_to));
            }
            ui.horizontal(|ui| {
                let up = self.dir.parent().map(Path::to_path_buf);
                let up_button = egui::Button::new(egui::RichText::new("⬆").size(18.0)).min_size(egui::vec2(row_height, row_height));
                if ui.add_enabled(up.is_some(), up_button).on_hover_text("Up one folder").clicked() {
                    go_to = up;
                }
                // Where we are, each folder a button that goes there.
                egui::ScrollArea::horizontal().id_salt("browser-path").stick_to_right(true).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let parts = crumbs(&self.dir, &places);
                        let last = parts.len().saturating_sub(1);
                        for (i, (label, path)) in parts.into_iter().enumerate() {
                            if i > 0 {
                                ui.weak("›");
                            }
                            let text = if i == last { egui::RichText::new(label).strong() } else { egui::RichText::new(label) };
                            if ui.add(egui::Button::new(text).frame(i != last)).clicked() && i != last {
                                go_to = Some(path);
                            }
                        }
                    });
                });
            });
            ui.separator();
            egui::ScrollArea::vertical().max_height(list_height).auto_shrink([false, false]).show(ui, |ui| {
                if let Some(e) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                let shown: Vec<Entry> = self.entries.iter().filter(|e| self.shows(e)).cloned().collect();
                if shown.is_empty() && self.error.is_none() {
                    ui.weak("This folder is empty");
                }
                for e in shown {
                    let icon = if e.dir { "📁" } else { "📄" };
                    let picked = self.selected.contains(&e.path);
                    // A long name ends in "…" rather than wrapping.
                    let button = egui::Button::selectable(picked, format!("{icon}  {}", e.name)).wrap_mode(egui::TextWrapMode::Truncate);
                    let row = ui.add_sized([ui.available_width(), row_height], button);
                    if row.double_clicked() && !e.dir && matches!(self.mode, Mode::File | Mode::Files) {
                        answer = Some(vec![e.path.to_string_lossy().to_string()]);
                    } else if row.clicked() {
                        if e.dir {
                            go_to = Some(e.path.clone());
                        } else {
                            match self.mode {
                                Mode::File => self.selected = vec![e.path.clone()],
                                Mode::Files => {
                                    if picked {
                                        self.selected.retain(|p| p != &e.path);
                                    } else {
                                        self.selected.push(e.path.clone());
                                    }
                                }
                                Mode::Save => self.name.clone_from(&e.name),
                                Mode::Folder => {}
                            }
                        }
                    }
                }
            });
            ui.separator();
            if !self.filters.is_empty() && self.mode != Mode::Folder {
                ui.horizontal(|ui| {
                    ui.label("Show:");
                    let all = self.filters.len();
                    let label = |i: usize| self.filters.get(i).map_or_else(|| "All files".to_string(), |(n, _)| n.clone());
                    let current = label(self.filter);
                    let mut filter = self.filter;
                    egui::ComboBox::from_id_salt("android-browser-filter").selected_text(current).show_ui(ui, |ui| {
                        for i in 0..=all {
                            ui.selectable_value(&mut filter, i, label(i));
                        }
                    });
                    if filter != self.filter {
                        self.filter = filter;
                        self.selected.clear();
                    }
                });
            }
            if matches!(self.mode, Mode::Save | Mode::Folder) {
                ui.horizontal(|ui| match &mut self.new_folder {
                    None => {
                        if ui.button("New folder").clicked() {
                            self.new_folder = Some(String::new());
                        }
                    }
                    Some(name) => {
                        let field = ui.text_edit_singleline(name);
                        if ui.button("Create").clicked() || (field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                            let name = name.trim().to_string();
                            if !name.is_empty() && !name.contains('/') {
                                let path = self.dir.join(&name);
                                match std::fs::create_dir(&path) {
                                    Ok(()) => go_to = Some(path),
                                    Err(e) => self.error = Some(e.to_string()),
                                }
                            }
                            self.new_folder = None;
                        }
                    }
                });
            }
            if self.mode == Mode::Save {
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    let field = ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(ui.available_width()));
                    if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.confirm_save(&mut answer);
                    }
                });
            }
            if let Some(path) = self.confirm_replace.clone() {
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                ui.colored_label(ui.visuals().warn_fg_color, format!("{name} already exists. Replace it?"));
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    answer = Some(Vec::new());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match self.mode {
                    Mode::File | Mode::Files => {
                        let n = self.selected.len();
                        let label = if self.mode == Mode::Files && n > 1 {
                            format!("Place ({n})")
                        } else if self.mode == Mode::Files {
                            "Place".into()
                        } else {
                            "Open".into()
                        };
                        if ui.add_enabled(n > 0, egui::Button::new(label)).clicked() {
                            answer = Some(self.selected.iter().map(|p| p.to_string_lossy().to_string()).collect());
                        }
                    }
                    Mode::Folder => {
                        if ui.add_enabled(self.error.is_none(), egui::Button::new("Use this folder")).clicked() {
                            answer = Some(vec![self.dir.to_string_lossy().to_string()]);
                        }
                    }
                    Mode::Save => {
                        let label = if self.confirm_replace.is_some() { "Replace" } else { "Save" };
                        if ui.add_enabled(self.save_path().is_some(), egui::Button::new(label)).clicked() {
                            self.confirm_save(&mut answer);
                        }
                    }
                });
            });
        });
        if answer.is_none() && (modal.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape))) {
            answer = Some(Vec::new());
        }
        if let Some(dir) = go_to
            && answer.is_none()
        {
            self.go(dir);
        }
        answer
    }
}

/// The folders from a shortcut's root down to `dir`, each with where it goes: the root named as
/// its shortcut is ("Internal storage"), a folder outside every shortcut from "/".
fn crumbs(dir: &Path, places: &[(String, PathBuf)]) -> Vec<(String, PathBuf)> {
    let root = places.iter().filter(|(_, p)| dir.starts_with(p)).max_by_key(|(_, p)| p.components().count());
    let (mut out, base) = match root {
        Some((label, path)) => (vec![(label.clone(), path.clone())], path.clone()),
        None => (vec![("/".to_string(), PathBuf::from("/"))], PathBuf::from("/")),
    };
    let mut at = base.clone();
    if let Ok(rest) = dir.strip_prefix(&base) {
        for part in rest.components() {
            at.push(part);
            out.push((part.as_os_str().to_string_lossy().into_owned(), at.clone()));
        }
    }
    out
}

fn default_dir() -> PathBuf {
    let shared = PathBuf::from(SHARED);
    let docs = shared.join("Documents");
    if docs.is_dir() { docs } else { shared }
}

fn places() -> Vec<(String, PathBuf)> {
    let shared = PathBuf::from(SHARED);
    let mut out = vec![("Internal storage".to_string(), shared.clone())];
    for (label, sub) in [("Documents", "Documents"), ("Downloads", "Download"), ("Pictures", "Pictures")] {
        let p = shared.join(sub);
        if p.is_dir() {
            out.push((label.to_string(), p));
        }
    }
    if let Ok(rd) = std::fs::read_dir("/storage") {
        for e in rd.flatten().take(16) {
            let name = e.file_name().to_string_lossy().to_string();
            if name != "emulated" && name != "self" && e.path().is_dir() {
                out.push((name, e.path()));
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        out.push(("App folder".to_string(), PathBuf::from(home)));
    }
    out
}

fn list(dir: &Path) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut entries: Vec<Entry> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                return None;
            }
            let path = e.path();
            Some(Entry { dir: path.is_dir(), name, path })
        })
        .take(MAX_ENTRIES)
        .collect();
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::crumbs;
    use std::path::{Path, PathBuf};

    #[test]
    fn the_path_is_a_trail_from_its_shortcut() {
        let places = vec![
            ("Internal storage".to_string(), PathBuf::from("/storage/emulated/0")),
            ("Documents".to_string(), PathBuf::from("/storage/emulated/0/Documents")),
        ];
        let c = crumbs(Path::new("/storage/emulated/0/Documents/Jobs/2026"), &places);
        let labels: Vec<&str> = c.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["Documents", "Jobs", "2026"]);
        assert_eq!(c[1].1, PathBuf::from("/storage/emulated/0/Documents/Jobs"));
        let c = crumbs(Path::new("/storage/emulated/0/Download"), &places);
        assert_eq!(c.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(), ["Internal storage", "Download"]);
        let c = crumbs(Path::new("/mnt/x"), &places);
        assert_eq!(c.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(), ["/", "mnt", "x"]);
    }
}
