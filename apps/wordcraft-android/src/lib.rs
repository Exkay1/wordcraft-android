//! WordCraft on Android.
//!
//! Runs the same [`wordcraft_ui_egui::WordApp`] as the desktop app through eframe's NativeActivity
//! runner (wgpu: Vulkan, with a GLES fallback). Differences from the desktop app: no system file
//! dialog (the app's built-in file browser is used), no printing, no system clipboard hooks and no
//! control channel. A one-finger drag on the page scrolls it ([`TouchScroll`]); a long press then
//! drag selects text.
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

/// Points a finger must travel before a touch counts as a scroll instead of a tap.
const SCROLL_SLOP: f32 = 10.0;
/// Seconds a finger must rest on the page before a touch acts as a mouse press, so that dragging
/// selects text.
const LONG_PRESS_SECS: f64 = 0.45;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    /// No finger down.
    #[default]
    Idle,
    /// A finger is down outside the page, or after a long press: pointer events pass unchanged.
    Pass,
    /// A finger is down on the page and has not decided between tap, scroll and long press.
    Pending,
    /// One finger is dragging the page.
    Scrolling,
    /// Two or more fingers are down (pinch zoom): no pointer events.
    Multi,
}

/// Turns one-finger drags on the document into scrolling, as in other Android apps.
///
/// egui turns a touch into mouse events, so a drag would select text. On the page, the mouse
/// press is held back until the touch ends (a tap) or rests for [`LONG_PRESS_SECS`] (a long press,
/// after which dragging selects). A drag past [`SCROLL_SLOP`] sends wheel events instead, which the
/// canvas scrolls with, including coasting after a flick.
#[derive(Default)]
struct TouchScroll {
    mode: Mode,
    touches: u32,
    primary: Option<egui::TouchId>,
    start: egui::Pos2,
    last: egui::Pos2,
    started: f64,
    held: Option<egui::Event>,
    /// Drop the mouse release and the pointer-gone event that follow a scroll.
    swallow: bool,
}

fn wheel(delta: egui::Vec2, phase: egui::TouchPhase) -> egui::Event {
    egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta, phase, modifiers: egui::Modifiers::NONE }
}

impl TouchScroll {
    fn rewrite(&mut self, raw: &mut egui::RawInput, page: Option<egui::Rect>, ctx: &egui::Context) {
        let now = raw.time.unwrap_or(0.0);
        let events = std::mem::take(&mut raw.events);
        let mut out: Vec<egui::Event> = Vec::with_capacity(events.len() + 2);
        if self.mode == Mode::Pending && now - self.started >= LONG_PRESS_SECS {
            out.extend(self.held.take());
            self.mode = Mode::Pass;
        }
        for event in events {
            if let egui::Event::Touch { id, phase, pos, .. } = &event {
                let (id, phase, pos) = (*id, *phase, *pos);
                out.push(event);
                match phase {
                    egui::TouchPhase::Start => {
                        self.touches += 1;
                        if self.touches == 1 {
                            self.primary = Some(id);
                            self.start = pos;
                            self.last = pos;
                            self.started = now;
                            self.held = None;
                            self.swallow = false;
                            self.mode = if page.is_some_and(|r| r.contains(pos)) { Mode::Pending } else { Mode::Pass };
                        } else if matches!(self.mode, Mode::Pending | Mode::Scrolling) {
                            self.held = None;
                            self.swallow = true;
                            self.mode = Mode::Multi;
                        }
                    }
                    egui::TouchPhase::Move => {
                        if Some(id) == self.primary {
                            match self.mode {
                                Mode::Pending if (pos - self.start).length() > SCROLL_SLOP => {
                                    self.held = None;
                                    self.mode = Mode::Scrolling;
                                    out.push(wheel(pos - self.start, egui::TouchPhase::Move));
                                    self.last = pos;
                                }
                                Mode::Scrolling => {
                                    out.push(wheel(pos - self.last, egui::TouchPhase::Move));
                                    self.last = pos;
                                }
                                _ => {}
                            }
                        }
                    }
                    egui::TouchPhase::End | egui::TouchPhase::Cancel => {
                        self.touches = self.touches.saturating_sub(1);
                        if Some(id) == self.primary {
                            self.primary = None;
                            match self.mode {
                                // A tap: the press goes out now, the release follows in the stream.
                                Mode::Pending => {
                                    if phase == egui::TouchPhase::End {
                                        out.extend(self.held.take());
                                    }
                                    self.held = None;
                                }
                                Mode::Scrolling => {
                                    out.push(wheel(egui::Vec2::ZERO, egui::TouchPhase::End));
                                    self.swallow = true;
                                }
                                Mode::Multi => self.swallow = true,
                                Mode::Pass | Mode::Idle => {}
                            }
                        }
                        if self.touches == 0 {
                            self.mode = Mode::Idle;
                            self.held = None;
                        }
                    }
                }
                continue;
            }
            match &event {
                egui::Event::PointerButton { pressed: true, .. } => match self.mode {
                    Mode::Pending if self.held.is_none() => self.held = Some(event),
                    Mode::Pending | Mode::Scrolling | Mode::Multi => {}
                    Mode::Pass | Mode::Idle => out.push(event),
                },
                egui::Event::PointerButton { pressed: false, .. } => {
                    if !self.swallow {
                        out.push(event);
                    }
                }
                egui::Event::PointerGone => {
                    self.swallow = false;
                    out.push(event);
                }
                _ => out.push(event),
            }
        }
        raw.events = out;
        if self.mode == Mode::Pending {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

struct Shell {
    app: WordApp,
    touch: TouchScroll,
}

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.logic(ctx);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.touch.rewrite(raw, self.app.canvas.canvas_rect, ctx);
        self.app.raw_input_hook(raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
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
            Ok(Box::new(Shell { app, touch: TouchScroll::default() }))
        }),
    );
    if let Err(e) = result {
        log::error!("WordCraft stopped: {e}");
    }
}
