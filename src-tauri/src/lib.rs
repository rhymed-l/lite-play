mod commands;
mod controller;
mod engine;
mod gesture;
mod hotkeys;
mod memory;
mod navigator;
mod player;
mod playlist;
mod subtitle;
mod subtitle_commands;
mod subtitle_window;
mod tray;
mod watcher;

use engine::{Cmd, EngineHandle};
use std::path::PathBuf;
use tauri::{AppHandle, Manager, WindowEvent};

/// Hands the first playable file among `args` to the engine, if there is one.
fn open_from_args(engine: &EngineHandle, args: &[String], cwd: &std::path::Path) -> bool {
    match playlist::first_audio_arg(args, cwd) {
        Some(file) => {
            engine.send(Cmd::OpenFile(file));
            true
        }
        None => false,
    }
}

fn on_second_instance(app: &AppHandle, args: Vec<String>, cwd: String) {
    if let Some(engine) = app.try_state::<EngineHandle>() {
        open_from_args(&engine, &args, &PathBuf::from(cwd));
    }
    tray::show_main_window(app);
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
            on_second_instance(app, args, cwd);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let engine = engine::spawn(app.handle().clone(), dir.join("memory.json"));
            app.manage(engine.clone());
            app.manage(subtitle_commands::setup(app.handle(), engine.clone(), &dir));
            tray::setup(app.handle(), engine.clone())?;
            hotkeys::setup(app.handle(), engine.clone());

            let args: Vec<String> = std::env::args().collect();
            let cwd = std::env::current_dir().unwrap_or_default();
            open_from_args(&engine, &args, &cwd);

            // Look for a newer add-on once per start, a little after the window is up.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(5));
                if let Some(state) = handle.try_state::<subtitle_commands::SubtitleState>() {
                    subtitle_commands::check_for_updates(&handle, &state);
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::play_pause,
            commands::next_track,
            commands::prev_track,
            commands::play_index,
            commands::seek_to,
            commands::set_mode,
            commands::set_direction,
            commands::set_volume,
            commands::set_memory_enabled,
            commands::add_folder,
            commands::select_folder,
            commands::open_file,
            commands::get_snapshot,
            subtitle_commands::get_subtitle_status,
            subtitle_commands::get_subtitle,
            subtitle_commands::set_subtitle_enabled,
            subtitle_commands::install_subtitle_addon,
            subtitle_commands::cancel_subtitle_install,
            subtitle_commands::check_subtitle_updates,
            subtitle_commands::uninstall_subtitle_addon,
            subtitle_commands::regenerate_subtitle,
            subtitle_commands::set_subtitle_options,
            subtitle_commands::set_desktop_subtitle,
            subtitle_commands::set_subtitle_power,
            subtitle_commands::get_power_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running lite-play");
}
