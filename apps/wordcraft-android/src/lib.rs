//! WordCraft on Android.
//!
//! Runs the same [`wordcraft_ui_egui::WordApp`] as the desktop app through eframe's NativeActivity
//! runner (wgpu: Vulkan, with a GLES fallback). Differences from the desktop app: no system file
//! dialog (the app's built-in file browser is used), no printing, no system clipboard hooks and no
//! control channel.
//!
//! Storage: settings live in the app's private folder. The file browser starts in
//! `/storage/emulated/0` when the app may read it (Settings > Apps > WordCraft > Permissions >
//! Files and media > Allow management of all files), otherwise in the app's own files folder.
#![cfg(target_os = "android")]
// `android_main` needs `#[unsafe(no_mangle)]` and the start-up environment needs `set_var`, so this
// crate does not inherit the workspace's `unsafe_code = "forbid"`.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;

use android_activity::AndroidApp;
use wordcraft_engine::Session;
use wordcraft_ui_egui::{FileDialogAnswer, Services, WordApp};

const SHARED_STORAGE: &str = "/storage/emulated/0";

struct Shell(WordApp);

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.0.logic(ctx);
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.0.raw_input_hook(raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }
}

/// Whether the folder can be listed, which is what the file browser needs.
fn readable(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok()
}

/// Points the environment variables the shared crates read at folders an Android app may use.
fn prepare_environment(app: &AndroidApp) {
    let internal = app.internal_data_path();
    let external = app.external_data_path();
    let config = internal.as_ref().map(|d| d.join("config"));
    let cache = internal.as_ref().map(|d| d.join("cache"));
    for dir in [config.as_ref(), cache.as_ref()].into_iter().flatten() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::warn!("cannot create {}: {e}", dir.display());
        }
    }
    let shared = PathBuf::from(SHARED_STORAGE);
    let home = if readable(&shared) { Some(shared) } else { external.or(internal) };
    // SAFETY: runs once at the start of `android_main`, before this crate starts any thread.
    unsafe {
        if let Some(home) = home {
            std::env::set_var("HOME", home);
        }
        if let Some(config) = config {
            std::env::set_var("XDG_CONFIG_HOME", config);
        }
        if let Some(cache) = cache {
            std::env::set_var("TMPDIR", cache);
        }
    }
}

/// Every file dialog request is answered with an error, which makes the app show its built-in
/// file browser.
fn services() -> Services {
    Services {
        file_dialog: Some(Box::new(|_request| {
            let (answer, rx) = channel::<FileDialogAnswer>();
            let _ = answer.send(Err("no system file dialog on Android".to_string()));
            rx
        })),
        ..Default::default()
    }
}

#[unsafe(no_mangle)]
pub fn android_main(app: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("WordCraft"));
    prepare_environment(&app);

    let mut options = eframe::NativeOptions { android_app: Some(app), ..Default::default() };
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup {
        create.instance_descriptor.backends = eframe::wgpu::Backends::VULKAN | eframe::wgpu::Backends::GL;
    }

    let result = eframe::run_native(
        "WordCraft",
        options,
        Box::new(|_cc| {
            let region = wordcraft_ui_egui::i18n::system_region();
            let doc = wordcraft_engine::blank_document(&region);
            let mut session = Session::new(doc);
            session.system_region = region;
            let mut app = WordApp::new(session, services());
            app.start_screen();
            Ok(Box::new(Shell(app)))
        }),
    );
    if let Err(e) = result {
        log::error!("WordCraft stopped: {e}");
    }
}
