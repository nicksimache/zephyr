use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, RunEvent, State, WebviewWindow, WindowEvent,
};
use tokio::sync::watch;

use crate::{api::Server, provider, types::Failure};

struct Runtime {
    server: Mutex<Option<Server>>,
    result: watch::Receiver<Option<Result<u16, String>>>,
    exiting: AtomicBool,
    installing: AtomicBool,
}

#[tauri::command]
async fn gateway_port(runtime: State<'_, Runtime>) -> Result<u16, String> {
    let mut result = runtime.result.clone();
    let ready = result
        .wait_for(Option::is_some)
        .await
        .map_err(|error| error.to_string())?;
    ready.as_ref().unwrap().clone()
}

#[tauri::command]
fn hide_to_tray(window: WebviewWindow) -> Result<(), String> {
    window.hide().map_err(|error| error.to_string())
}

#[tauri::command]
async fn install_ollama(runtime: State<'_, Runtime>) -> Result<(), Failure> {
    if runtime.installing.swap(true, Ordering::SeqCst) {
        return Err(Failure::new(
            "install_busy",
            "Ollama installation is already in progress",
        ));
    }
    let _reset = InstallReset(&runtime.installing);
    install_package().await
}

struct InstallReset<'a>(&'a AtomicBool);
impl Drop for InstallReset<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[cfg(windows)]
async fn install_package() -> Result<(), Failure> {
    let winget = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .map(|path| path.join("Microsoft/WindowsApps/winget.exe"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| "winget.exe".into());
    let output = provider::quiet_command(winget)
        .args([
            "install",
            "--id",
            "Ollama.Ollama",
            "--exact",
            "--source",
            "winget",
            "--silent",
            "--accept-package-agreements",
            "--accept-source-agreements",
            "--disable-interactivity",
        ])
        .output()
        .await
        .map_err(|error| Failure::new("install_failed", format!("Cannot run winget: {error}")))?;
    if !output.status.success() {
        let message = format!(
            "winget exited with {}. {}",
            output.status,
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .replace('\r', "")
        );
        return Err(Failure::new("install_failed", message));
    }
    Ok(())
}

#[cfg(not(windows))]
async fn install_package() -> Result<(), Failure> {
    Err(Failure::new(
        "install_failed",
        "Install Ollama from https://ollama.com/download",
    ))
}

pub fn run() -> anyhow::Result<()> {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_window(app)
        }))
        .invoke_handler(tauri::generate_handler![
            gateway_port,
            hide_to_tray,
            install_ollama
        ])
        .setup(|app| {
            let (ready, result) = watch::channel(None);
            app.manage(Runtime {
                server: Mutex::new(None),
                result,
                exiting: AtomicBool::new(false),
                installing: AtomicBool::new(false),
            });
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move { start_gateway(handle, ready).await });

            let open = MenuItem::with_id(app, "open", "Show Zephyr", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Zephyr", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Zephyr")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        show_window(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())?;

    app.run(|app, event| {
        if let RunEvent::ExitRequested { api, code, .. } = event {
            let state = app.state::<Runtime>();
            if !state.exiting.swap(true, Ordering::SeqCst) {
                api.prevent_exit();
                let server = state.server.lock().unwrap().take();
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Some(server) = server {
                        server.stop().await;
                    }
                    app.exit(code.unwrap_or(0));
                });
            }
        }
    });
    Ok(())
}

async fn start_gateway(app: AppHandle, ready: watch::Sender<Option<Result<u16, String>>>) {
    let server = match Server::start().await {
        Ok(server) => server,
        Err(error) => {
            ready.send_replace(Some(Err(format!("{error:#}"))));
            return;
        }
    };
    let gateway = server.gateway.clone();
    let port = server.port;
    let mut startup = Some(server);
    {
        let state = app.state::<Runtime>();
        let mut slot = state.server.lock().unwrap();
        if !state.exiting.load(Ordering::SeqCst) {
            *slot = startup.take();
        }
    }
    if let Some(server) = startup {
        server.stop().await;
        return;
    }
    ready.send_replace(Some(Ok(port)));
    if provider::installed_path().is_some()
        && gateway.version().await.is_err()
        && !gateway.closing().is_cancelled()
    {
        let _ = gateway.start_provider().await;
    }
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
