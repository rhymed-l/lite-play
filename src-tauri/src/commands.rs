use crate::controller::Snapshot;
use crate::engine::{Cmd, EngineHandle};
use crate::navigator::{Direction, Mode};
use std::path::PathBuf;
use tauri::State;

#[tauri::command]
pub fn play_pause(engine: State<EngineHandle>) {
    engine.send(Cmd::PlayPause);
}

#[tauri::command]
pub fn next_track(engine: State<EngineHandle>) {
    engine.send(Cmd::Next);
}

#[tauri::command]
pub fn prev_track(engine: State<EngineHandle>) {
    engine.send(Cmd::Prev);
}

#[tauri::command]
pub fn play_index(engine: State<EngineHandle>, idx: usize) {
    engine.send(Cmd::PlayIndex(idx));
}

#[tauri::command]
pub fn seek_to(engine: State<EngineHandle>, secs: f64) {
    engine.send(Cmd::SeekTo(secs));
}

#[tauri::command]
pub fn set_mode(engine: State<EngineHandle>, mode: Mode) {
    engine.send(Cmd::SetMode(mode));
}

#[tauri::command]
pub fn set_direction(engine: State<EngineHandle>, direction: Direction) {
    engine.send(Cmd::SetDirection(direction));
}

#[tauri::command]
pub fn set_volume(engine: State<EngineHandle>, volume: f32) {
    engine.send(Cmd::SetVolume(volume));
}

#[tauri::command]
pub fn set_memory_enabled(engine: State<EngineHandle>, enabled: bool) {
    engine.send(Cmd::SetMemoryEnabled(enabled));
}

#[tauri::command]
pub fn add_folder(engine: State<EngineHandle>, path: PathBuf) {
    engine.send(Cmd::AddFolder(path));
}

#[tauri::command]
pub fn select_folder(engine: State<EngineHandle>, path: PathBuf) {
    engine.send(Cmd::SelectFolder(path));
}

#[tauri::command]
pub fn open_file(engine: State<EngineHandle>, path: PathBuf) {
    engine.send(Cmd::OpenFile(path));
}

/// Async so a slow engine reply cannot freeze the main (UI) thread.
#[tauri::command]
pub async fn get_snapshot(engine: State<'_, EngineHandle>) -> Result<Option<Snapshot>, ()> {
    Ok(engine.snapshot())
}
