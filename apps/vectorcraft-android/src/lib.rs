//! VectorCraft on Android, for tablets used with a mouse, a keyboard and a stylus.
//!
//! Android starts the app through `NativeActivity` (no Java), which loads this library and calls
//! [`android_main`]. The UI is the desktop one (`vectorcraft-ui-egui`); what differs on Android:
//! - Files: dialogs are a file browser drawn in the app (`picker`) over shared storage, which
//!   needs "All files access" granted once in Settings (`file_access`).
//! - Preferences, fonts, swatch and style libraries, logs and Data Recovery live in the app's
//!   private folder (`HOME` below), under `.config/vectorcraft` as on Linux.
//! - Input (`tablet`): a mouse is a mouse, the S Pen draws with pressure, palms are ignored, one
//!   finger on the canvas scrolls it.
//! - Crash reports (`crash_log`): the log and stderr go to files, and a run that ended badly
//!   leaves a report in Download.
//! - Updates: new builds of the `android-latest` release are offered in the app (`updater`).
//! - No system clipboard for images, no printing, no control channel.
//!
//! Elsewhere the crate is empty, so `cargo build --workspace` on a desktop still works.
#![cfg(target_os = "android")]

mod crash_log;
mod file_access;
mod picker;
mod tablet;
mod updater;

use std::path::PathBuf;

use vectorcraft_engine::Session;
use vectorcraft_engine::cmd::fileio;
use vectorcraft_ui_egui::{Services, VectorcraftApp};
use winit::platform::android::activity::AndroidApp;

/// VectorCraft's folder for preferences and the user's own libraries: as on Linux.
fn prefs_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("vectorcraft"))
}

fn read_prefs() -> Option<vectorcraft_ui_egui::UiState> {
    let bytes = std::fs::read(prefs_dir()?.join("ui.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Save the UI and engine preferences (best effort: a failed write keeps the previous file).
pub(crate) fn save_prefs(app: &VectorcraftApp) {
    let Some(dir) = prefs_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let mut ui = app.ui.clone();
    ui.engine_prefs = app.session.prefs.to_json();
    if let Ok(bytes) = serde_json::to_vec_pretty(&ui) {
        let _ = fileio::write_atomic(&dir.join("ui.json"), &bytes);
    }
}

fn write_file(path: &str, bytes: &[u8]) -> Result<(), String> {
    fileio::write_atomic(std::path::Path::new(path), bytes).map_err(|e| e.to_string())
}

fn services(picker: picker::Shared) -> Services {
    Services {
        // Every dialog goes through the in-app browser, answering on a channel.
        start_pick: Some(Box::new(move |request| picker.start(request))),
        // The app offers a kind of dialog (Save, Open, Place, a folder) only where the host has
        // its in-line form. Every dialog here is the browser above, which always takes the
        // request, so these never run; without them Save and Open did nothing.
        pick_open: Some(Box::new(|_: &vectorcraft_ui_egui::FilePick| None)),
        pick_open_multi: Some(Box::new(Vec::new)),
        pick_save: Some(Box::new(|_: &vectorcraft_ui_egui::FilePick| None)),
        pick_folder: Some(Box::new(|| None)),
        read: Some(Box::new(|p: &str| std::fs::read(p).map_err(|e| e.to_string()))),
        write: Some(Box::new(write_file)),
        write_shared: Some(std::sync::Arc::new(write_file)),
        open_url: Some(Box::new(|url: &str| {
            let _ = webbrowser::open(url);
        })),
        ..Default::default()
    }
}

/// Entry point, called by android-activity on its own thread once the activity is created.
#[allow(unsafe_code)] // Exported by name: android-activity looks this symbol up.
#[unsafe(no_mangle)]
fn android_main(android: AndroidApp) {
    let data_dir = android.internal_data_path();
    // Logging first, with the previous run's crash report if it ended badly.
    let previous = crash_log::start(data_dir.as_deref());
    crash_log::watch();
    std::panic::set_hook(Box::new(|info| {
        let trace = std::backtrace::Backtrace::force_capture();
        log::error!("{} {info}\n{trace}", crash_log::PANIC);
        crash_log::flush();
    }));
    if let Some(dir) = &data_dir {
        // Preferences and recovery derive from HOME, which Android doesn't set.
        #[allow(unsafe_code)]
        // SAFETY: called before eframe or the app start any thread that could read the environment.
        unsafe {
            std::env::set_var("HOME", dir);
        }
        log::info!("VectorCraft {} data folder {}", env!("CARGO_PKG_VERSION"), dir.display());
    }
    file_access::set_app(android.clone());
    log::info!("All files access: {}", if file_access::granted() { "granted" } else { "not granted" });
    vectorcraft_ui_egui::i18n::detect_system_lang_in_background();
    // VectorCraft's own Fonts folder, which text.addFontFiles copies fonts into.
    if let Some(dir) = prefs_dir() {
        vectorcraft_text::set_app_font_dir(dir.join("Fonts"));
    }
    let saved = read_prefs();
    let picker = picker::Shared::default();
    let shell_android = android.clone();
    let options = eframe::NativeOptions { android_app: Some(android), renderer: eframe::Renderer::Wgpu, ..Default::default() };
    let result = eframe::run_native(
        "VectorCraft",
        options,
        Box::new(move |cc| {
            let mut app = VectorcraftApp::new(Session::new(), services(picker.clone()));
            if let Some(ui) = saved {
                app.ui = ui.sanitized();
            }
            vectorcraft_ui_egui::prefs_dialog::restore(&mut app);
            let lib = |name: &str| prefs_dir().map(|d| d.join(name).to_string_lossy().to_string());
            app.session.swatch_libraries.set_user_dir(lib("Swatches"));
            app.session.style_libraries.set_user_dir(lib("Graphic Styles"));
            app.session.libraries.set_dir(lib("Libraries"));
            app.session.recovery.set_default_folder(lib("Data Recovery"));
            if let Some(rs) = &cc.wgpu_render_state {
                let info = rs.adapter.get_info();
                let summary = format!("{} ({:?}, {:?})", info.name.trim(), info.backend, info.device_type);
                log::info!("rendering with {summary}");
                app.graphics_adapter = Some(summary);
            }
            Ok(Box::new(tablet::TabletApp::new(app, shell_android, picker, previous)))
        }),
    );
    if let Err(e) = result {
        log::error!("VectorCraft stopped: {e}");
    }
    log::info!("{}", crash_log::EXIT);
    crash_log::flush();
}
