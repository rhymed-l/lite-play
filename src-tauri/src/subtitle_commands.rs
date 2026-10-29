//! Tauri commands for the subtitle feature. Thin adapters: every rule they rely on lives in the
//! `subtitle::*` modules, which have their own tests.

use crate::engine::{Cmd, EngineHandle};
use crate::subtitle::install::{self, Hooks, Latest, UpdateState};
use crate::subtitle::net;
use crate::subtitle::recognizer;
use crate::subtitle::release::Asset;
use crate::subtitle::settings::{DesktopPos, SubtitleSettings};
use crate::subtitle_window;
use crate::subtitle::view::TrackSubtitles;
use crate::subtitle::worker::{self, Level, Msg, Progress, RealTranscriber};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

const SUPPORTED: bool = cfg!(windows);
const UNSUPPORTED: &str = "字幕功能目前只支持 Windows";

pub struct SubtitleState {
    pub root: PathBuf,
    pub settings_path: PathBuf,
    pub settings: Mutex<SubtitleSettings>,
    pub update: Mutex<UpdateState>,
    pub latest: Mutex<Option<Latest>>,
    pub progress: Mutex<Option<Progress>>,
    /// An install, update or uninstall is running.
    pub busy: AtomicBool,
    pub cancel: AtomicBool,
    pub worker: Option<Sender<Msg>>,
}

#[derive(Serialize, Clone)]
pub struct SubtitleStatus {
    pub supported: bool,
    pub installed: bool,
    pub installing: bool,
    pub update: UpdateState,
    pub settings: SubtitleSettings,
    pub progress: Option<Progress>,
}

#[derive(Serialize, Clone)]
struct InstallEvent {
    /// "runtime" | "model" | "vad" | "selftest" while working, then "done" or "error".
    stage: String,
    done: u64,
    total: u64,
    message: Option<String>,
}

impl SubtitleState {
    /// Called by the desktop window after the user has dragged it somewhere.
    pub fn remember_desktop_position(&self, pos: DesktopPos) {
        save_settings(self, |s| s.desktop_pos = Some(pos));
    }
}

fn installed(state: &SubtitleState) -> bool {
    install::active_dir(&state.root).is_some()
}

fn status(state: &SubtitleState) -> SubtitleStatus {
    SubtitleStatus {
        supported: SUPPORTED,
        installed: installed(state),
        installing: state.busy.load(Ordering::Relaxed),
        update: state.update.lock().map(|u| u.clone()).unwrap_or(UpdateState::NotInstalled),
        settings: state.settings.lock().map(|s| s.clone()).unwrap_or_default(),
        progress: state.progress.lock().ok().and_then(|p| p.clone()),
    }
}

fn save_settings(state: &SubtitleState, change: impl FnOnce(&mut SubtitleSettings)) -> SubtitleSettings {
    let mut guard = state.settings.lock().expect("settings lock");
    change(&mut guard);
    *guard = guard.clone().normalized();
    let _ = guard.save(&state.settings_path);
    guard.clone()
}

/// Tells the engine whether subtitles are on. The engine then (re)configures the worker.
fn apply_enabled(engine: &EngineHandle, enabled: bool) {
    engine.send(Cmd::SubtitleEnabled(enabled));
}

/// Tells the UI the settings changed and makes the desktop window match them.
fn announce_settings(app: &AppHandle, settings: &SubtitleSettings) {
    let _ = app.emit("subtitle-settings", settings.clone());
    let window_app = app.clone();
    let settings = settings.clone();
    // Windows must be created on the main thread's event loop, not on a command's worker thread.
    let _ = app.run_on_main_thread(move || subtitle_window::apply(&window_app, &settings));
}

// ---------- setup ----------

/// Builds the subtitle state, starts the worker thread and wires it into the engine.
/// On platforms other than Windows nothing is started and nothing touches the network.
pub fn setup(app: &AppHandle, engine: EngineHandle, data_dir: &Path) -> SubtitleState {
    let root = data_dir.join("subtitle-addon");
    let settings_path = data_dir.join("subtitle-settings.json");
    let mut settings = SubtitleSettings::load(&settings_path);

    let mut worker_tx = None;
    if SUPPORTED {
        install::cleanup_stale(&root);
        // The switch may be on in the file while the add-on is gone (uninstalled by hand).
        if settings.enabled && install::active_dir(&root).is_none() {
            settings.enabled = false;
            let _ = settings.save(&settings_path);
        }
        let handle = app.clone();
        let engine_for_progress = engine.clone();
        let tx = worker::spawn(
            root.clone(),
            Box::new(RealTranscriber),
            Box::new(move |progress: Progress| {
                let _ = handle.emit("subtitle-progress", progress.clone());
                if let Some(state) = handle.try_state::<SubtitleState>() {
                    if let Ok(mut slot) = state.progress.lock() {
                        *slot = Some(progress);
                    }
                }
                engine_for_progress.send(Cmd::ReloadSubtitles);
            }),
        );
        send_power(&tx, &settings.power);
        engine.send(Cmd::AttachSubtitleWorker(tx.clone()));
        apply_enabled(&engine, settings.enabled);
        worker_tx = Some(tx);
    }

    if SUPPORTED {
        // Bring the desktop window back the way the user left it.
        announce_settings(app, &settings);
    }

    SubtitleState {
        root,
        settings_path,
        settings: Mutex::new(settings),
        update: Mutex::new(UpdateState::NotInstalled),
        latest: Mutex::new(None),
        progress: Mutex::new(None),
        busy: AtomicBool::new(false),
        cancel: AtomicBool::new(false),
        worker: worker_tx,
    }
}

/// Stops the worker so no recognizer process outlives the app.
pub fn shutdown(app: &AppHandle) {
    if let Some(state) = app.try_state::<SubtitleState>() {
        state.cancel.store(true, Ordering::Relaxed);
        if let Some(tx) = &state.worker {
            let _ = tx.send(Msg::Shutdown);
        }
    }
}

// ---------- install ----------

/// The three hooks `install::install` needs, wired to the real network, `tar` and recognizer.
pub(crate) fn real_install(
    app: &AppHandle,
    root: &Path,
    latest: &Latest,
    state: &UpdateState,
    cancel: &AtomicBool,
) -> Result<install::Manifest, String> {
    let download = |asset: &Asset, dest: &Path, progress: &dyn Fn(u64, u64)| {
        net::download(&asset.url, dest, progress, cancel)
    };
    // `tar` gets a working directory plus plain relative names: both stay ASCII even when the
    // app data path contains Chinese characters.
    let extract = |archive: &Path, out_dir: &Path| {
        let cwd = archive.parent().ok_or("下载文件路径无效")?;
        let rel = out_dir.strip_prefix(cwd).map_err(|_| "解压目录必须位于下载目录内")?;
        let archive_name = archive.file_name().ok_or("下载文件路径无效")?.to_string_lossy();
        install::tar_extract(cwd, &archive_name, &rel.to_string_lossy())
    };
    // Runs the recognizer on the model package's own sample sentence; any text counts.
    let selftest = |dir: &Path| {
        let work = dir.join("work");
        fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        fs::copy(dir.join("model").join("selftest.wav"), work.join("job-0.wav"))
            .map_err(|e| e.to_string())?;
        let result = recognizer::recognize(dir, "job-0.wav", 4, cancel);
        let _ = fs::remove_file(work.join("job-0.wav"));
        match result {
            Ok(cues) if !cues.is_empty() => Ok(()),
            Ok(_) => Err("自检没有识别出任何文字".to_string()),
            Err(e) => Err(format!("自检失败：{e}")),
        }
    };
    let hooks = Hooks { download: &download, extract: &extract, selftest: &selftest };
    let report = |stage: &str, done: u64, total: u64| {
        let _ = app.emit(
            "subtitle-install",
            InstallEvent { stage: stage.to_string(), done, total, message: None },
        );
    };
    install::install(root, latest, state, &hooks, &report, cancel)
}

fn finish_event(app: &AppHandle, result: &Result<install::Manifest, String>) {
    let event = match result {
        Ok(_) => InstallEvent { stage: "done".into(), done: 0, total: 0, message: None },
        Err(e) if e == "cancelled" => {
            InstallEvent { stage: "error".into(), done: 0, total: 0, message: Some("已取消".into()) }
        }
        Err(e) => InstallEvent { stage: "error".into(), done: 0, total: 0, message: Some(e.clone()) },
    };
    let _ = app.emit("subtitle-install", event);
}

fn run_install(app: AppHandle, enable_after: bool) {
    let Some(state) = app.try_state::<SubtitleState>() else {
        return;
    };
    let engine = app.state::<EngineHandle>();
    state.cancel.store(false, Ordering::Relaxed);

    let result = (|| {
        let latest = net::fetch_latest()?;
        let current = install::active_dir(&state.root).and_then(|d| install::load_manifest(&d));
        let update = install::compare(current.as_ref(), &latest);
        if let Ok(mut slot) = state.latest.lock() {
            *slot = Some(latest.clone());
        }
        if update == UpdateState::UpToDate {
            return current.ok_or_else(|| "没有可用的组件".to_string());
        }
        real_install(&app, &state.root, &latest, &update, &state.cancel)
    })();

    if let Ok(manifest) = &result {
        if let Ok(mut slot) = state.update.lock() {
            *slot = UpdateState::UpToDate;
        }
        let _ = manifest;
        if enable_after {
            let settings = save_settings(&state, |s| s.enabled = true);
            apply_enabled(&engine, true);
            announce_settings(&app, &settings);
        }
    }
    state.busy.store(false, Ordering::Relaxed);
    finish_event(&app, &result);
}

// ---------- commands ----------

#[tauri::command]
pub fn get_subtitle_status(state: State<SubtitleState>) -> SubtitleStatus {
    status(&state)
}

#[tauri::command]
pub async fn get_subtitle(engine: State<'_, EngineHandle>) -> Result<(TrackSubtitles, Option<usize>), ()> {
    engine.subtitle().ok_or(())
}

#[tauri::command]
pub fn set_subtitle_enabled(
    app: AppHandle,
    state: State<SubtitleState>,
    engine: State<EngineHandle>,
    enabled: bool,
) -> Result<(), String> {
    if !SUPPORTED {
        return Err(UNSUPPORTED.to_string());
    }
    if enabled && !installed(&state) {
        return Err("组件未安装".to_string());
    }
    let settings = save_settings(&state, |s| s.enabled = enabled);
    apply_enabled(&engine, enabled);
    announce_settings(&app, &settings);
    Ok(())
}

/// Downloads and installs (or updates) the add-on, then switches subtitles on.
#[tauri::command]
pub fn install_subtitle_addon(app: AppHandle, state: State<SubtitleState>) -> Result<(), String> {
    if !SUPPORTED {
        return Err(UNSUPPORTED.to_string());
    }
    if state.busy.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    std::thread::Builder::new()
        .name("lite-play-subtitle-install".into())
        .spawn(move || run_install(app, true))
        .map(|_| ())
        .map_err(|e| {
            state.busy.store(false, Ordering::Relaxed);
            e.to_string()
        })
}

#[tauri::command]
pub fn cancel_subtitle_install(state: State<SubtitleState>) {
    state.cancel.store(true, Ordering::Relaxed);
}

/// Looks for a newer add-on. Only runs when the feature is on and installed, and never
/// reports a failure: a missing network is not worth an error dialog.
#[tauri::command]
pub fn check_subtitle_updates(app: AppHandle, state: State<SubtitleState>) {
    let enabled = state.settings.lock().map(|s| s.enabled).unwrap_or(false);
    if !SUPPORTED || !enabled || !installed(&state) || state.busy.load(Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(move || {
        if let Some(state) = app.try_state::<SubtitleState>() {
            check_for_updates(&app, &state);
        }
    });
}

/// Blocking: asks GitHub what is newest and records the answer. Call from a worker thread.
pub fn check_for_updates(app: &AppHandle, state: &SubtitleState) {
    let enabled = state.settings.lock().map(|s| s.enabled).unwrap_or(false);
    if !SUPPORTED || !enabled || !installed(state) || state.busy.load(Ordering::Relaxed) {
        return;
    }
    let Ok(latest) = net::fetch_latest() else {
        return;
    };
    let manifest = install::active_dir(&state.root).and_then(|d| install::load_manifest(&d));
    let update = install::compare(manifest.as_ref(), &latest);
    if let Ok(mut slot) = state.latest.lock() {
        *slot = Some(latest);
    }
    if let Ok(mut slot) = state.update.lock() {
        *slot = update.clone();
    }
    let _ = app.emit("subtitle-update", update);
}

#[tauri::command]
pub fn uninstall_subtitle_addon(
    app: AppHandle,
    state: State<SubtitleState>,
    engine: State<EngineHandle>,
) -> Result<(), String> {
    if state.busy.swap(true, Ordering::AcqRel) {
        return Err("正在安装或更新，请稍后再试".to_string());
    }
    let settings = save_settings(&state, |s| s.enabled = false);
    apply_enabled(&engine, false);
    announce_settings(&app, &settings);
    // The worker stops its job when it is switched off; give it a moment to let go of the files.
    let mut outcome = install::uninstall(&state.root);
    for _ in 0..20 {
        if outcome.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
        outcome = install::uninstall(&state.root);
    }
    state.busy.store(false, Ordering::Relaxed);
    if let Ok(mut slot) = state.update.lock() {
        *slot = UpdateState::NotInstalled;
    }
    outcome.map_err(|e| format!("无法删除字幕组件：{e}"))
}

#[tauri::command]
pub fn regenerate_subtitle(state: State<SubtitleState>, path: PathBuf) {
    if let Some(tx) = &state.worker {
        let _ = tx.send(Msg::Regenerate(path));
    }
}

#[tauri::command]
pub fn set_subtitle_options(
    app: AppHandle,
    state: State<SubtitleState>,
    font_size: u32,
    scheme: String,
    custom_fg: String,
    custom_bg: String,
) {
    let settings = save_settings(&state, |s| {
        s.font_size = font_size;
        s.scheme = scheme;
        s.custom_fg = custom_fg;
        s.custom_bg = custom_bg;
    });
    announce_settings(&app, &settings);
}

fn logical_cores() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// Tells the worker how hard it may work. The free memory is read by the worker itself before
/// every new job, so only the level and the core count are sent.
fn send_power(tx: &Sender<Msg>, power: &str) {
    let _ = tx.send(Msg::SetPower {
        level: Level::from_name(power),
        cores: logical_cores(),
        free_mb: None,
    });
}

/// Chooses how hard transcription works: "light", "balanced" or "full". Takes effect for the next
/// job that starts; jobs already running finish as they are.
#[tauri::command]
pub fn set_subtitle_power(app: AppHandle, state: State<SubtitleState>, power: String) {
    let settings = save_settings(&state, |s| s.power = power);
    if let Some(tx) = &state.worker {
        send_power(tx, &settings.power);
    }
    announce_settings(&app, &settings);
}

/// What each level means on this machine right now, for the settings screen.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct PowerChoice {
    pub name: &'static str,
    /// How many episodes are made at once right now (the level, limited by free memory).
    pub jobs: usize,
    /// How many the level alone would allow, ignoring memory.
    pub wanted_jobs: usize,
    pub threads_per_job: u32,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct PowerInfo {
    pub cores: usize,
    pub free_mb: Option<u64>,
    pub choices: Vec<PowerChoice>,
}

/// Pure so it can be tested: the same `jobs_for` the worker uses, for each level.
pub fn power_info(cores: usize, free_mb: Option<u64>) -> PowerInfo {
    use crate::subtitle::power::{jobs_for, threads_per_job};
    let free = free_mb.unwrap_or(u64::MAX / 2);
    let choices = [("light", Level::Light), ("balanced", Level::Balanced), ("full", Level::Full)]
        .into_iter()
        .map(|(name, level)| {
            let jobs = jobs_for(level, cores, free);
            let wanted_jobs = jobs_for(level, cores, u64::MAX / 2);
            PowerChoice { name, jobs, wanted_jobs, threads_per_job: threads_per_job(jobs) }
        })
        .collect();
    PowerInfo { cores, free_mb, choices }
}

#[tauri::command]
pub fn get_power_info() -> PowerInfo {
    power_info(logical_cores(), crate::subtitle::worker::free_memory_mb())
}

/// Saves the desktop-window switches; `announce_settings` shows, hides or locks the window.
#[tauri::command]
pub fn set_desktop_subtitle(app: AppHandle, state: State<SubtitleState>, enabled: bool, locked: bool) {
    let settings = save_settings(&state, |s| {
        s.desktop_enabled = enabled;
        s.desktop_locked = locked;
    });
    announce_settings(&app, &settings);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitle::view::active_index;

    #[test]
    fn power_info_matches_what_the_worker_will_do() {
        let info = power_info(32, Some(23 * 1024));
        let names: Vec<_> = info.choices.iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["light", "balanced", "full"]);
        let jobs: Vec<_> = info.choices.iter().map(|c| c.jobs).collect();
        assert_eq!(jobs, vec![4, 8, 16]);
        assert!(info.choices.iter().all(|c| c.threads_per_job == 2));
    }

    #[test]
    fn power_info_shows_the_memory_limit_when_memory_is_short() {
        let info = power_info(32, Some(4 * 1024));
        assert_eq!(info.choices.iter().map(|c| c.jobs).collect::<Vec<_>>(), vec![2, 2, 2]);
        // what each level would run if memory were no object, so the screen can say why
        assert_eq!(info.choices.iter().map(|c| c.wanted_jobs).collect::<Vec<_>>(), vec![4, 8, 16]);
    }

    #[test]
    fn power_info_wanted_equals_actual_when_memory_is_plentiful() {
        let info = power_info(32, Some(64 * 1024));
        assert!(info.choices.iter().all(|c| c.wanted_jobs == c.jobs));
    }

    #[test]
    fn power_info_for_a_tiny_machine_still_offers_one_job() {
        let info = power_info(2, Some(16 * 1024));
        assert_eq!(info.choices.iter().map(|c| c.jobs).collect::<Vec<_>>(), vec![1, 1, 1]);
        assert!(info.choices.iter().all(|c| c.threads_per_job == 4));
    }

    #[test]
    fn power_info_without_a_memory_reading_does_not_limit_by_memory() {
        let info = power_info(32, None);
        assert_eq!(info.choices.iter().map(|c| c.jobs).collect::<Vec<_>>(), vec![4, 8, 16]);
    }

    #[test]
    #[ignore = "downloads ~180 MB from GitHub and runs the real recognizer"]
    fn live_install_end_to_end_under_a_chinese_directory() {
        // The directory name has Chinese characters and a space on purpose: that is what a
        // user with a Chinese Windows account name has, and what tar and the recognizer
        // must cope with.
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("中文 用户").join("subtitle-addon");
        let latest = net::fetch_latest().expect("fetch_latest");
        let cancel = AtomicBool::new(false);

        let started = std::time::Instant::now();
        let manifest = install::install(
            &root,
            &latest,
            &UpdateState::NotInstalled,
            &live_hooks(&cancel),
            &|stage, done, total| eprintln!("  {stage}: {done}/{total}"),
            &cancel,
        )
        .expect("install");
        eprintln!("installed in {:.1}s: {manifest:?}", started.elapsed().as_secs_f64());

        let inst = install::active_dir(&root).expect("active install");
        assert!(inst.join("runtime").join("sherpa-onnx-vad-with-offline-asr.exe").is_file());
        assert!(inst.join("model").join("model.int8.onnx").is_file());
        assert!(inst.join("silero_vad.onnx").is_file());

        // and the installed add-on really recognises speech
        fs::copy(inst.join("model").join("selftest.wav"), inst.join("work").join("job-0.wav")).unwrap();
        let cues = recognizer::recognize(&inst, "job-0.wav", 4, &cancel).expect("recognize");
        assert!(!cues.is_empty());
        assert_eq!(active_index(&cues, cues[0].start), Some(0));
        eprintln!("recognised: {}", cues.iter().map(|c| c.text.as_str()).collect::<Vec<_>>().join(" "));

        // a second run with nothing to do touches nothing
        let again = install::install(
            &root,
            &latest,
            &UpdateState::UpToDate,
            &live_hooks(&cancel),
            &|_, _, _| {},
            &cancel,
        )
        .expect("up to date");
        assert_eq!(again, manifest);
    }

    fn live_hooks(cancel: &AtomicBool) -> Hooks<'_> {
        // Same wiring as real_install, but without an AppHandle.
        fn leak<'a, T: ?Sized>(b: Box<T>) -> &'a T {
            Box::leak(b)
        }
        let download = leak(Box::new(move |asset: &Asset, dest: &Path, progress: &dyn Fn(u64, u64)| {
            net::download(&asset.url, dest, progress, cancel)
        }) as Box<dyn Fn(&Asset, &Path, &dyn Fn(u64, u64)) -> Result<(), String> + '_>);
        let extract = leak(Box::new(|archive: &Path, out_dir: &Path| {
            let cwd = archive.parent().ok_or("bad archive path")?;
            let rel = out_dir.strip_prefix(cwd).map_err(|_| "out dir outside download dir")?;
            let name = archive.file_name().ok_or("bad archive path")?.to_string_lossy();
            install::tar_extract(cwd, &name, &rel.to_string_lossy())
        }) as Box<dyn Fn(&Path, &Path) -> Result<(), String> + '_>);
        let selftest = leak(Box::new(move |dir: &Path| {
            let work = dir.join("work");
            fs::create_dir_all(&work).map_err(|e| e.to_string())?;
            fs::copy(dir.join("model").join("selftest.wav"), work.join("job-0.wav"))
                .map_err(|e| e.to_string())?;
            let result = recognizer::recognize(dir, "job-0.wav", 4, cancel);
            let _ = fs::remove_file(work.join("job-0.wav"));
            match result {
                Ok(c) if !c.is_empty() => Ok(()),
                Ok(_) => Err("selftest: no text".to_string()),
                Err(e) => Err(format!("selftest: {e}")),
            }
        }) as Box<dyn Fn(&Path) -> Result<(), String> + '_>);
        Hooks { download, extract, selftest }
    }
}
