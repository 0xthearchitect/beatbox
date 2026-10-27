//! `beatbox`, the window.
//!
//! `web/dist` in the system's webview (WebKitGTK, WebView2), one window,
//! maximized, its door wired to [`Shell::answer`]. Every decision about a
//! request is in the library; this file is only Tauri.
//!
//! **The transport** is two things: the page `invoke`s `call` with an
//! action and its payload and gets the envelope back, and the window hands
//! the page what it was not asked about by calling
//! `window.__beatboxPushed(name, payload)` (`bridge/client.ts`).
//!
//! **The Content-Security-Policy** is the page's own, in its `<meta>`, and
//! `app.security.csp` is left empty so Tauri adds nothing to it. See
//! `web/vite.config.ts` for what it admits and why.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(feature = "proof")]
mod proof;

use std::process::ExitCode;
use std::sync::Arc;
use std::thread;

use beatbox_contract::{Envelope, wire};
use beatbox_shell::closing::{ANSWER_WITHIN, Close};
use beatbox_shell::language::Language;
use beatbox_shell::link::Link;
use beatbox_shell::preferences::Preferences;
use beatbox_shell::{Shell, Window, beatbox_home, one};
use serde_json::{Value, json};
use tauri::{Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};

/// Where the window opens: the hero is the way in.
const START_ROUTE: &str = "hero";

/// The page, as the shell is allowed to know it.
struct Page(WebviewWindow);

impl Window for Page {
    fn push(&self, name: &str, payload: &Value) {
        // JSON is a JavaScript expression, so both go in as they are.
        let script = format!(
            "window.__beatboxPushed && window.__beatboxPushed({}, {payload})",
            json!(name)
        );
        let _ = self.0.eval(script);
    }

    fn go(&self) {
        for (label, window) in self.0.app_handle().webview_windows() {
            if label != self.0.label() {
                let _ = window.destroy();
            }
        }
        let _ = self.0.destroy();
    }

    fn display(&self, id: &str, name: &str, url: &str, password: &str) -> Result<(), String> {
        let label = format!("machine-{id}");
        let app = self.0.app_handle();
        if let Some(window) = app.get_webview_window(&label) {
            let _ = window.unminimize();
            window.set_focus().map_err(|error| error.to_string())?;
            return Ok(());
        }
        let configuration = json!({
            "id": id,
            "name": name,
            "url": url,
            "password": password,
        });
        WebviewWindowBuilder::new(app, &label, WebviewUrl::App("index.html".into()))
            .title(format!("BeatBox ({name})"))
            .initialization_script(format!("window.__beatboxDisplay = {configuration};"))
            .inner_size(1100.0, 780.0)
            .min_inner_size(640.0, 480.0)
            .build()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn undisplay(&self, id: &str) {
        if let Some(window) = self
            .0
            .app_handle()
            .get_webview_window(&format!("machine-{id}"))
        {
            let _ = window.destroy();
        }
    }
}

/// The page's one door.
#[tauri::command]
async fn call(
    shell: tauri::State<'_, Arc<Shell>>,
    action: String,
    payload: Value,
) -> Result<Envelope, String> {
    let shell = Arc::clone(&shell);
    tauri::async_runtime::spawn_blocking(move || shell.answer(&action, &payload))
        .await
        .map_err(|error| error.to_string())
}

/// The title bar's buttons and drag (`web/src/components/TitleBar.tsx`):
/// the main window has no decorations, so the page draws them and asks here.
/// Answers whether the window is maximized now.
#[tauri::command]
fn frame(window: WebviewWindow, what: String) -> Result<bool, String> {
    let done = match what.as_str() {
        "state" => Ok(()),
        "drag" => window.start_dragging(),
        "minimize" => window.minimize(),
        "maximize" if window.is_maximized().unwrap_or(false) => window.unmaximize(),
        "maximize" => window.maximize(),
        // A request, not a destroy: CloseRequested still asks about
        // whatever is running.
        "close" => window.close(),
        other => return Err(format!("no such frame action: {other}")),
    };
    done.map_err(|error| error.to_string())?;
    window.is_maximized().map_err(|error| error.to_string())
}

/// Something said before there is a page to say it in.
fn tell(language: &Language, title: &str, body: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Info)
        .set_title(language.text(title))
        .set_description(language.text(body))
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let proving: Option<Vec<String>> = arguments
        .iter()
        .position(|argument| argument == "--proof")
        .map(|at| arguments[at + 1..].to_vec());
    if proving.is_some() && !cfg!(feature = "proof") {
        eprintln!("beatbox: this build carries no proofs (build it with --features proof)");
        return ExitCode::from(2);
    }
    let home = beatbox_home();
    let language = Language::at(home.join("shell.json"));
    let preferences = Preferences::at(home.join("shell.json"));
    let link = Link::new(wire::socket_path(), true);
    #[cfg(feature = "proof")]
    if let Some(code) = proving
        .as_ref()
        .and_then(|what| proof::without_a_window(what))
    {
        return code;
    }

    // One BeatBox at a time: two cannot drive the same machine, and the
    // second would read a running machine as lost.
    let Some(_one_window) = one::claim(&home.join("state"), || link.status().is_some()) else {
        tell(
            &language,
            "beatbox.already_open.title",
            "beatbox.already_open.body",
        );
        return ExitCode::from(1);
    };

    let shell = Shell::new(link, language, preferences);
    let setup = Arc::clone(&shell);
    let builder = tauri::Builder::default().manage(Arc::clone(&shell));
    #[cfg(feature = "proof")]
    let (builder, answers) = {
        let (reports, answers) = proof::reports();
        (
            builder
                .manage(reports)
                .invoke_handler(tauri::generate_handler![call, frame, proof::report]),
            answers,
        )
    };
    #[cfg(not(feature = "proof"))]
    let builder = builder.invoke_handler(tauri::generate_handler![call, frame]);
    let walking = proving
        .as_ref()
        .is_some_and(|what| what.first().map(String::as_str) == Some("walk"));
    let ran = builder
        .setup(move |app| {
            let window =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()));
            #[cfg(feature = "proof")]
            let window = if proving.is_some() {
                window.initialization_script(proof::WATCH)
            } else {
                window
            };
            let window = window
                .title(setup.language().text("beatbox.window.title"))
                .initialization_script(format!(
                    "if (!location.hash) location.hash = '/{START_ROUTE}';"
                ))
                // The rail's Build group (Engine, Foundations) is for whoever
                // builds BeatBox: a choice in Settings, on by default in a
                // debug build or with BEATBOX_BUILDER=1 (preferences.rs).
                .initialization_script(format!(
                    "window.__beatboxBuilder = {};",
                    setup.preferences().builder_tools()
                ))
                // Drawn by the page instead (TitleBar.tsx), the same on every
                // desktop.
                .decorations(false)
                .inner_size(1360.0, 900.0)
                // A workspace, not a panel.
                .maximized(!walking)
                .build()?;
            setup.attach(Arc::new(Page(window.clone())));
            #[cfg(feature = "proof")]
            if let Some(what) = proving {
                proof::run(
                    tauri::Manager::app_handle(app).clone(),
                    window.clone(),
                    Arc::clone(&setup),
                    what,
                    answers,
                );
            }
            let shell = Arc::clone(&setup);
            let closing = window.clone();
            window.on_window_event(move |event| {
                let WindowEvent::CloseRequested { api, .. } = event else {
                    return;
                };
                // Never closed over something still running without asking:
                // the answer comes back as host.closing, or not at all.
                api.prevent_close();
                let shell = Arc::clone(&shell);
                let window = closing.clone();
                thread::spawn(move || match shell.close_requested() {
                    Close::Now => {
                        shell.detach();
                        let _ = window.destroy();
                    }
                    Close::Ask(_) => {
                        thread::sleep(ANSWER_WITHIN);
                        shell.unanswered();
                    }
                    Close::Wait => {}
                });
            });
            Ok(())
        })
        .run(tauri::generate_context!());
    match ran {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("beatbox: {error}");
            ExitCode::from(1)
        }
    }
}
