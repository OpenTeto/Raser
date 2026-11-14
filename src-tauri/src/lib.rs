mod discord;
mod local_files;
mod storage;
mod taskbar;
mod updater;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Shortcut, ShortcutState};

fn set_webview_visibility(app: &tauri::AppHandle, visible: bool) {
    let _ = app.emit("window-visibility", visible);
    if let Some(w) = app.get_webview_window("main") {
        let script = format!("window.__raserSetVisibility && window.__raserSetVisibility({});", visible);
        let _ = w.eval(&script);
    }
}

#[tauri::command]
fn minimize_window(window: tauri::Window) {
    set_webview_visibility(window.app_handle(), false);
    let _ = window.minimize();
    trim_working_set();
}

#[tauri::command]
fn toggle_maximize_window(window: tauri::Window) {
    if let Ok(is_maximized) = window.is_maximized() {
        if is_maximized {
            let _ = window.unmaximize();
        } else {
            let _ = window.maximize();
        }
    }
}

#[tauri::command]
fn close_window(window: tauri::Window) {
    // Hide to system tray instead of closing process so playback continues
    set_webview_visibility(window.app_handle(), false);
    let _ = window.hide();
    trim_working_set();
}

#[tauri::command]
fn exit_app() {
    std::process::exit(0);
}

#[tauri::command]
fn open_external(url: String) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", &url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    }
}

#[tauri::command]
fn discord_rpc_update(track: discord::DiscordTrackInfo) {
    discord::update_presence(track);
}

#[tauri::command]
fn discord_rpc_clear() {
    discord::clear_presence();
}

#[tauri::command]
async fn check_update() -> Result<updater::UpdateInfo, String> {
    updater::check_for_update().await
}

#[tauri::command]
async fn install_update(app: tauri::AppHandle, download_url: String) -> Result<(), String> {
    updater::download_and_install_update(app, download_url).await
}

#[tauri::command]
fn pick_local_files() -> Vec<local_files::LocalTrack> {
    local_files::pick_files()
}

#[tauri::command]
fn pick_local_folder() -> Vec<local_files::LocalTrack> {
    local_files::pick_folder()
}

#[tauri::command]
fn taskbar_playback_state(playing: bool) {
    let is_tr = is_tr_locale();
    taskbar::win::update_playback_state(playing, is_tr);
}

#[tauri::command]
fn get_disk_storage() -> std::collections::HashMap<String, serde_json::Value> {
    storage::read_storage_map()
}

#[tauri::command]
fn save_disk_storage(data: std::collections::HashMap<String, serde_json::Value>) {
    storage::save_storage_map(data);
}

#[tauri::command]
fn trim_working_set() {
    #[cfg(target_os = "windows")]
    unsafe {
        #[repr(C)]
        struct PROCESSENTRY32W {
            dw_size: u32,
            cnt_usage: u32,
            th32_process_id: u32,
            th32_default_heap_id: usize,
            th32_module_id: u32,
            cnt_threads: u32,
            th32_parent_process_id: u32,
            pc_pri_class_base: i32,
            dw_flags: u32,
            sz_exe_file: [u16; 260],
        }

        unsafe extern "system" {
            fn GetCurrentProcess() -> isize;
            fn GetCurrentProcessId() -> u32;
            fn SetProcessWorkingSetSize(h_process: isize, min_size: usize, max_size: usize) -> i32;
            fn CreateToolhelp32Snapshot(dw_flags: u32, th32_process_id: u32) -> isize;
            fn Process32FirstW(h_snapshot: isize, lppe: *mut PROCESSENTRY32W) -> i32;
            fn Process32NextW(h_snapshot: isize, lppe: *mut PROCESSENTRY32W) -> i32;
            fn CloseHandle(h_object: isize) -> i32;
            fn OpenProcess(dw_desired_access: u32, b_inherit_handle: i32, dw_process_id: u32) -> isize;
        }

        // 1. Trim current Rust process (app.exe)
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);

        // 2. Discover and trim all descendant processes (msedgewebview2.exe child & grandchild processes)
        let my_pid = GetCurrentProcessId();
        let snapshot = CreateToolhelp32Snapshot(0x00000002, 0); // TH32CS_SNAPPROCESS
        if snapshot != -1 && snapshot != 0 {
            let mut entry = PROCESSENTRY32W {
                dw_size: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                cnt_usage: 0,
                th32_process_id: 0,
                th32_default_heap_id: 0,
                th32_module_id: 0,
                cnt_threads: 0,
                th32_parent_process_id: 0,
                pc_pri_class_base: 0,
                dw_flags: 0,
                sz_exe_file: [0u16; 260],
            };

            let mut all_processes = Vec::new();
            if Process32FirstW(snapshot, &mut entry) != 0 {
                loop {
                    all_processes.push((entry.th32_process_id, entry.th32_parent_process_id));
                    if Process32NextW(snapshot, &mut entry) == 0 {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snapshot);

            let mut parent_pids = std::collections::HashSet::new();
            parent_pids.insert(my_pid);
            let mut pids_to_trim = Vec::new();

            let mut added = true;
            while added {
                added = false;
                for &(pid, ppid) in &all_processes {
                    if parent_pids.contains(&ppid) && !parent_pids.contains(&pid) {
                        parent_pids.insert(pid);
                        pids_to_trim.push(pid);
                        added = true;
                    }
                }
            }

            const PROCESS_SET_QUOTA: u32 = 0x0100;
            const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
            for pid in pids_to_trim {
                let h_proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_QUERY_INFORMATION, 0, pid);
                if h_proc != 0 {
                    let _ = SetProcessWorkingSetSize(h_proc, usize::MAX, usize::MAX);
                    let _ = CloseHandle(h_proc);
                }
            }
        }
    }
}

fn is_tr_locale() -> bool {
    #[cfg(target_os = "windows")]
    unsafe {
        unsafe extern "system" {
            fn GetUserDefaultUILanguage() -> u16;
        }
        if GetUserDefaultUILanguage() == 0x041F {
            return true;
        }
    }
    if let Ok(lang) = std::env::var("LANG") {
        if lang.to_lowercase().starts_with("tr") {
            return true;
        }
    }
    false
}

fn send_media_command(app: &tauri::AppHandle, cmd: &str) {
    let _ = app.emit("media-command", cmd);
    if let Some(window) = app.get_webview_window("main") {
        let script = format!("window.__raserMediaCommand && window.__raserMediaCommand('{}');", cmd);
        let _ = window.eval(&script);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::default().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        match shortcut.key {
                            Code::MediaPlayPause => send_media_command(app, "toggle"),
                            Code::MediaTrackNext => send_media_command(app, "next"),
                            Code::MediaTrackPrevious => send_media_command(app, "prev"),
                            Code::MediaStop => send_media_command(app, "stop"),
                            _ => {}
                        }
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            minimize_window,
            toggle_maximize_window,
            close_window,
            exit_app,
            open_external,
            discord_rpc_update,
            discord_rpc_clear,
            check_update,
            install_update,
            pick_local_files,
            pick_local_folder,
            taskbar_playback_state,
            get_disk_storage,
            save_disk_storage,
            trim_working_set
        ])
        .setup(|app| {
            let _ = app.global_shortcut().register(Shortcut::new(None, Code::MediaPlayPause));
            let _ = app.global_shortcut().register(Shortcut::new(None, Code::MediaTrackNext));
            let _ = app.global_shortcut().register(Shortcut::new(None, Code::MediaTrackPrevious));
            let _ = app.global_shortcut().register(Shortcut::new(None, Code::MediaStop));

            // Setup System Tray
            let is_tr = is_tr_locale();
            taskbar::win::setup_taskbar_buttons(app.handle(), is_tr);
            let show_text = if is_tr { "Raser'ı Göster" } else { "Show Raser" };
            let toggle_text = if is_tr { "Oynat / Durdur" } else { "Play / Pause" };
            let next_text = if is_tr { "Sonraki Şarkı" } else { "Next Track" };
            let prev_text = if is_tr { "Önceki Şarkı" } else { "Previous Track" };
            let quit_text = if is_tr { "Çıkış" } else { "Quit" };

            let show_i = MenuItem::with_id(app, "show", show_text, true, None::<&str>)?;
            let toggle_i = MenuItem::with_id(app, "toggle_play", toggle_text, true, None::<&str>)?;
            let next_i = MenuItem::with_id(app, "next_track", next_text, true, None::<&str>)?;
            let prev_i = MenuItem::with_id(app, "prev_track", prev_text, true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", quit_text, true, None::<&str>)?;

            let tray_menu = Menu::with_items(app, &[
                &show_i,
                &toggle_i,
                &next_i,
                &prev_i,
                &quit_i,
            ])?;

            let mut tray_builder = TrayIconBuilder::new()
                .menu(&tray_menu)
                .tooltip("Raser")
                .on_menu_event(|app, event| {
                    match event.id.as_ref() {
                        "show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.unminimize();
                                let _ = w.set_focus();
                                let _ = w.eval("window.__raserSetVisibility && window.__raserSetVisibility(true);");
                            }
                        }
                        "toggle_play" => send_media_command(app, "toggle"),
                        "next_track" => send_media_command(app, "next"),
                        "prev_track" => send_media_command(app, "prev"),
                        "quit" => {
                            std::process::exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.eval("window.__raserSetVisibility && window.__raserSetVisibility(false);");
                                let _ = w.hide();
                                trim_working_set();
                            } else {
                                let _ = w.show();
                                let _ = w.unminimize();
                                let _ = w.set_focus();
                                let _ = w.eval("window.__raserSetVisibility && window.__raserSetVisibility(true);");
                            }
                        }
                    }
                });

            if let Some(icon) = app.default_window_icon() {
                tray_builder = tray_builder.icon(icon.clone());
            }

            let _ = tray_builder.build(app)?;

            let initial_storage = storage::read_storage_raw();
            if let Some(window) = app.get_webview_window("main") {
                let script = format!("window.__RASER_INITIAL_STORAGE__ = {};", initial_storage);
                let _ = window.eval(&script);
                let _ = window.show();

                let w_clone = window.clone();
                window.on_window_event(move |event| match event {
                    tauri::WindowEvent::Resized(_) => {
                        if let Ok(minimized) = w_clone.is_minimized() {
                            if minimized {
                                let _ = w_clone.eval("window.__raserSetVisibility && window.__raserSetVisibility(false);");
                                trim_working_set();
                            } else {
                                let _ = w_clone.eval("window.__raserSetVisibility && window.__raserSetVisibility(true);");
                            }
                        }
                    }
                    _ => {}
                });
            }

            // Periodic working set memory trim to keep background RAM usage minimal (< 90MB)
            std::thread::spawn(|| {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(30));
                    trim_working_set();
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while building tauri application");
}
