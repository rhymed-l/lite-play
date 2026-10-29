use crate::controller::{Controller, Snapshot};
use crate::navigator::{Direction, Mode};
use crate::player::{AudioBackend, RodioBackend};
use crate::subtitle::sync::{SubtitleSync, Update};
use crate::subtitle::view::TrackSubtitles;
use crate::subtitle::worker;
use crate::watcher;
use notify::RecommendedWatcher;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

const TICK: Duration = Duration::from_millis(200);
const PLAYING_PUSH_INTERVAL_MS: u64 = 500;
const NO_DEVICE: &str = "没有可用的音频输出设备";

pub enum Cmd {
    PlayPause,
    Next,
    Prev,
    PlayIndex(usize),
    SeekTo(f64),
    SeekBy(f64),
    SetMode(Mode),
    SetDirection(Direction),
    SetVolume(f32),
    SetMemoryEnabled(bool),
    AddFolder(PathBuf),
    SelectFolder(PathBuf),
    OpenFile(PathBuf),
    RefreshFolder,
    ReportError(String),
    GetSnapshot(Sender<Snapshot>),
    Shutdown(Sender<()>),
    /// Hands the engine the subtitle worker's inbox; until then subtitles are inert.
    AttachSubtitleWorker(Sender<worker::Msg>),
    SubtitleEnabled(bool),
    /// The `.srt` of the current track may have changed on disk: read it again.
    ReloadSubtitles,
    GetSubtitle(Sender<(TrackSubtitles, Option<usize>)>),
}

/// Payload of the `subtitle-active` event: the line to highlight, or none.
#[derive(Serialize, Clone)]
struct ActiveLine {
    index: Option<usize>,
}

#[derive(Clone)]
pub struct EngineHandle {
    tx: Sender<Cmd>,
}

impl EngineHandle {
    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn snapshot(&self) -> Option<Snapshot> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::GetSnapshot(tx));
        rx.recv_timeout(Duration::from_secs(2)).ok()
    }

    pub fn subtitle(&self) -> Option<(TrackSubtitles, Option<usize>)> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::GetSubtitle(tx));
        rx.recv_timeout(Duration::from_secs(2)).ok()
    }

    /// Asks the engine to save its state and stop; waits briefly for the reply.
    pub fn shutdown(&self) {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Shutdown(tx));
        let _ = rx.recv_timeout(Duration::from_secs(2));
    }
}

/// Stands in when no output device can be opened, so the app still starts and shows why.
struct SilentBackend;

impl AudioBackend for SilentBackend {
    fn load(&mut self, _path: &Path) -> Result<Option<f64>, String> {
        Err(NO_DEVICE.to_string())
    }
    fn play(&mut self) {}
    fn pause(&mut self) {}
    fn stop(&mut self) {}
    fn seek(&mut self, _secs: f64) -> Result<(), String> {
        Err(NO_DEVICE.to_string())
    }
    fn position(&self) -> f64 {
        0.0
    }
    fn is_finished(&self) -> bool {
        false
    }
    fn set_volume(&mut self, _v: f32) {}
}

enum Backend {
    Rodio(RodioBackend),
    Silent(SilentBackend),
}

impl Backend {
    fn inner(&self) -> &dyn AudioBackend {
        match self {
            Backend::Rodio(b) => b,
            Backend::Silent(b) => b,
        }
    }

    fn inner_mut(&mut self) -> &mut dyn AudioBackend {
        match self {
            Backend::Rodio(b) => b,
            Backend::Silent(b) => b,
        }
    }
}

impl AudioBackend for Backend {
    fn load(&mut self, path: &Path) -> Result<Option<f64>, String> {
        self.inner_mut().load(path)
    }
    fn play(&mut self) {
        self.inner_mut().play()
    }
    fn pause(&mut self) {
        self.inner_mut().pause()
    }
    fn stop(&mut self) {
        self.inner_mut().stop()
    }
    fn seek(&mut self, secs: f64) -> Result<(), String> {
        self.inner_mut().seek(secs)
    }
    fn position(&self) -> f64 {
        self.inner().position()
    }
    fn is_finished(&self) -> bool {
        self.inner().is_finished()
    }
    fn set_volume(&mut self, v: f32) {
        self.inner_mut().set_volume(v)
    }
}

fn apply(c: &mut Controller<Backend>, cmd: Cmd) {
    match cmd {
        Cmd::PlayPause => c.play_pause(),
        Cmd::Next => c.next(),
        Cmd::Prev => c.prev(),
        Cmd::PlayIndex(i) => c.play_index(i),
        Cmd::SeekTo(s) => c.seek_to(s),
        Cmd::SeekBy(d) => c.seek_by(d),
        Cmd::SetMode(m) => c.set_mode(m),
        Cmd::SetDirection(d) => c.set_direction(d),
        Cmd::SetVolume(v) => c.set_volume(v),
        Cmd::SetMemoryEnabled(e) => c.set_memory_enabled(e),
        Cmd::AddFolder(p) => c.add_folder(&p),
        Cmd::SelectFolder(p) => c.select_folder(&p),
        Cmd::OpenFile(p) => c.open_file(&p),
        Cmd::RefreshFolder => c.refresh_folder(),
        Cmd::ReportError(message) => c.report_notice(message),
        Cmd::GetSnapshot(reply) => {
            let _ = reply.send(c.snapshot());
        }
        // Handled by the engine loop itself, which owns the subtitle state.
        Cmd::Shutdown(_)
        | Cmd::AttachSubtitleWorker(_)
        | Cmd::SubtitleEnabled(_)
        | Cmd::ReloadSubtitles
        | Cmd::GetSubtitle(_) => {}
    }
}

/// Starts the playback thread. The audio device and the `Controller` live only on that thread.
pub fn spawn(app: AppHandle, memory_path: PathBuf) -> EngineHandle {
    let (tx, rx) = mpsc::channel::<Cmd>();
    let own_tx = tx.clone();
    std::thread::Builder::new()
        .name("lite-play-engine".into())
        .spawn(move || run(app, memory_path, rx, own_tx))
        .expect("spawn engine thread");
    EngineHandle { tx }
}

/// Keeps exactly one folder watcher alive, on whatever folder is current.
fn sync_watcher(
    folder: Option<&str>,
    watched: &mut Option<String>,
    watcher: &mut Option<RecommendedWatcher>,
    tx: &Sender<Cmd>,
) {
    if watched.as_deref() == folder {
        return;
    }
    *watcher = None;
    *watched = folder.map(String::from);
    if let Some(f) = folder {
        *watcher = watcher::watch(Path::new(f), tx.clone()).ok();
    }
}

fn run(app: AppHandle, memory_path: PathBuf, rx: mpsc::Receiver<Cmd>, tx: Sender<Cmd>) {
    let (backend, device_error) = match RodioBackend::new() {
        Ok(b) => (Backend::Rodio(b), None),
        Err(e) => (Backend::Silent(SilentBackend), Some(e)),
    };
    let memory = crate::memory::Memory::load(&memory_path);
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let mut controller = Controller::new(backend, memory, memory_path, seed);
    controller.restore();
    if let Some(e) = device_error {
        controller.report_notice(format!("无法打开音频输出设备：{e}"));
    }

    let started = Instant::now();
    let push = |snap: Snapshot| {
        let _ = app.emit("snapshot", snap);
    };
    let mut subtitles = SubtitleSync::new();
    let push_subtitles = |update: Update| {
        if let Some(track) = update.track {
            let _ = app.emit("subtitle-track", track);
        }
        if let Some(index) = update.active {
            let _ = app.emit("subtitle-active", ActiveLine { index });
        }
    };

    let mut watcher: Option<RecommendedWatcher> = None;
    let mut watched: Option<String> = None;

    let first = controller.snapshot();
    sync_watcher(first.folder.as_deref(), &mut watched, &mut watcher, &tx);
    let mut last_shape = (first.current, first.playing);
    push_subtitles(subtitles.update(&first));
    push(first);
    let mut last_push_ms = 0u64;

    loop {
        match rx.recv_timeout(TICK) {
            Ok(Cmd::Shutdown(ack)) => {
                controller.shutdown();
                let _ = ack.send(());
                break;
            }
            Ok(Cmd::GetSubtitle(reply)) => {
                let _ = reply.send(subtitles.current());
            }
            Ok(cmd) => {
                match cmd {
                    Cmd::AttachSubtitleWorker(worker) => subtitles.attach(worker),
                    Cmd::SubtitleEnabled(enabled) => subtitles.set_enabled(enabled),
                    Cmd::ReloadSubtitles => subtitles.reload(),
                    other => apply(&mut controller, other),
                }
                let snap = controller.snapshot();
                sync_watcher(snap.folder.as_deref(), &mut watched, &mut watcher, &tx);
                last_shape = (snap.current, snap.playing);
                last_push_ms = started.elapsed().as_millis() as u64;
                push_subtitles(subtitles.update(&snap));
                push(snap);
            }
            Err(RecvTimeoutError::Timeout) => {
                let now_ms = started.elapsed().as_millis() as u64;
                controller.tick(now_ms);
                let snap = controller.snapshot();
                push_subtitles(subtitles.update(&snap));
                let shape = (snap.current, snap.playing);
                let due = snap.playing
                    && now_ms.saturating_sub(last_push_ms) >= PLAYING_PUSH_INTERVAL_MS;
                if shape != last_shape || due {
                    last_shape = shape;
                    last_push_ms = now_ms;
                    push(snap);
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                controller.shutdown();
                break;
            }
        }
    }
}
