use crate::memory::sibling_with_suffix;
use crate::subtitle::install::active_dir;
pub use crate::subtitle::power::Level;
use crate::subtitle::power::{jobs_for, threads_per_job};
use crate::subtitle::queue::next_to_transcribe;
use crate::subtitle::recognizer::work_file_name;
use crate::subtitle::srt::{sidecar_path, write_srt_atomic, Cue};
use serde::Serialize;
use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const POLL: Duration = Duration::from_millis(50);
const GENERATED_FILE: &str = "generated.json";
const CANCELLED: &str = "cancelled";

/// Everything the worker needs from the outside world, so tests can run it without audio files
/// or the real recognizer. `Send + Sync` because a job runs on its own thread.
pub trait Transcriber: Send + Sync {
    /// Decodes `src` into the 16 kHz mono wav at `dst`. Returns its length in seconds.
    fn decode(&self, src: &Path, dst: &Path, cancel: &AtomicBool) -> Result<f64, String>;
    /// Recognizes `addon_dir/work/<wav_name>` with `threads` threads.
    fn recognize(
        &self,
        addon_dir: &Path,
        wav_name: &str,
        threads: u32,
        cancel: &AtomicBool,
    ) -> Result<Vec<Cue>, String>;
}

pub struct RealTranscriber;

impl Transcriber for RealTranscriber {
    fn decode(&self, src: &Path, dst: &Path, cancel: &AtomicBool) -> Result<f64, String> {
        crate::subtitle::audio::decode_to_wav(src, dst, cancel)
    }

    fn recognize(
        &self,
        addon_dir: &Path,
        wav_name: &str,
        threads: u32,
        cancel: &AtomicBool,
    ) -> Result<Vec<Cue>, String> {
        crate::subtitle::recognizer::recognize(addon_dir, wav_name, threads, cancel)
    }
}

pub enum Msg {
    /// The full picture every time: the current folder's tracks and the one being listened to.
    Configure { enabled: bool, tracks: Vec<PathBuf>, current: Option<usize> },
    /// Redo a subtitle, but only one this program wrote.
    Regenerate(PathBuf),
    /// How hard to work. `cores` is the machine's logical core count. `free_mb` pins the free
    /// memory instead of reading it from the system before every new job; it is for tests.
    SetPower { level: Level, cores: usize, free_mb: Option<u64> },
    Shutdown,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Progress {
    /// Tracks of the current folder that have a `.srt` next to them.
    pub done: usize,
    pub total: usize,
    /// File names of the tracks being worked on right now.
    pub current: Vec<String>,
    pub failed: Vec<String>,
    pub running: bool,
}

// ---------- the registry of subtitles this program wrote ----------

fn read_registry(root: &Path) -> Vec<String> {
    fs::read(root.join(GENERATED_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// True when `srt` was written by this program (and so may be replaced by "regenerate").
/// Anything else next to an audio file belongs to the user and is never overwritten.
pub fn is_generated(root: &Path, srt: &Path) -> bool {
    let key = srt.to_string_lossy();
    read_registry(root).iter().any(|p| p.as_str() == key)
}

/// Several jobs finish at the same time and each adds to the same file: without this lock the
/// read-modify-write below would let one overwrite another's entry.
static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

pub fn mark_generated(root: &Path, srt: &Path) -> io::Result<()> {
    let _guard = REGISTRY_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut list = read_registry(root);
    let key = srt.to_string_lossy().into_owned();
    if list.contains(&key) {
        return Ok(());
    }
    list.push(key);
    fs::create_dir_all(root)?;
    let path = root.join(GENERATED_FILE);
    let tmp = sibling_with_suffix(&path, ".tmp");
    fs::write(&tmp, serde_json::to_vec(&list).map_err(io::Error::other)?)?;
    fs::rename(&tmp, &path)
}

// ---------- one track ----------

fn check_cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err(CANCELLED.to_string())
    } else {
        Ok(())
    }
}

/// Removes the temporary wav on every way out of a job: success, failure and cancel.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Transcribes one audio file into the `.srt` next to it and returns the number of sentences.
/// The subtitle is only written once everything has succeeded, so a failed or cancelled run
/// leaves any existing subtitle untouched.
pub fn transcribe_one(
    root: &Path,
    transcriber: &dyn Transcriber,
    audio: &Path,
    slot: usize,
    threads: u32,
    cancel: &AtomicBool,
) -> Result<usize, String> {
    let inst = active_dir(root).ok_or("字幕组件未安装")?;
    let srt = sidecar_path(audio);
    if srt.exists() && !is_generated(root, &srt) {
        return Err("已有字幕文件，不覆盖".to_string());
    }
    check_cancelled(cancel)?;

    // Each job running at the same time has its own audio file, named after its slot.
    let work = inst.join("work");
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let wav_name = work_file_name(slot);
    let wav = work.join(&wav_name);
    let _cleanup = RemoveOnDrop(wav.clone());

    transcriber.decode(audio, &wav, cancel)?;
    check_cancelled(cancel)?;
    let cues = transcriber.recognize(&inst, &wav_name, threads, cancel)?;
    check_cancelled(cancel)?;
    if cues.is_empty() {
        return Err("没有识别到语音".to_string());
    }
    write_srt_atomic(&srt, &cues).map_err(|e| format!("无法写入字幕：{e}"))?;
    // Without the registry entry the file is simply treated as the user's: never overwritten.
    let _ = mark_generated(root, &srt);
    Ok(cues.len())
}

// ---------- the background loop ----------

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

struct Job {
    audio: PathBuf,
    /// Which temporary wav this job owns (`job-<slot>.wav`).
    slot: usize,
    cancel: Arc<AtomicBool>,
    result: Receiver<Result<usize, String>>,
    thread: JoinHandle<()>,
}

/// How hard to work, and what the machine looks like.
struct Power {
    level: Level,
    cores: usize,
    /// Pins the free memory (tests). `None` reads it from the system before every new job.
    free_mb: Option<u64>,
}

struct Worker {
    root: PathBuf,
    transcriber: Arc<dyn Transcriber>,
    on_progress: Box<dyn Fn(Progress) + Send>,
    enabled: bool,
    power: Power,
    tracks: Vec<PathBuf>,
    current: Option<usize>,
    /// Tracks that failed in this run; they are not retried until the user asks.
    failed: Vec<PathBuf>,
    regenerate: VecDeque<PathBuf>,
    jobs: Vec<Job>,
    /// Something may be left to do. Cleared when a scan finds nothing, so an idle worker does
    /// not stat every track of a large folder on each tick.
    need_scan: bool,
    configured: bool,
    last_emitted: Option<Progress>,
}

/// Free physical memory in MB, or `None` when the system will not say.
pub fn free_memory_mb() -> Option<u64> {
    system_free_mb()
}

#[cfg(windows)]
fn system_free_mb() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `status` is a properly sized, initialised MEMORYSTATUSEX that lives for the call.
    unsafe { GlobalMemoryStatusEx(&mut status).ok()? };
    Some(status.ullAvailPhys / (1024 * 1024))
}

#[cfg(not(windows))]
fn system_free_mb() -> Option<u64> {
    None
}

impl Worker {
    fn snapshot(&self) -> Progress {
        let mut current: Vec<String> = self.jobs.iter().map(|j| name_of(&j.audio)).collect();
        current.sort();
        Progress {
            done: self.tracks.iter().filter(|t| sidecar_path(t).exists()).count(),
            total: self.tracks.len(),
            current,
            failed: self
                .tracks
                .iter()
                .filter(|t| self.failed.contains(t))
                .map(|t| name_of(t))
                .collect(),
            running: !self.jobs.is_empty(),
        }
    }

    fn emit(&mut self) {
        if !self.configured {
            return;
        }
        let now = self.snapshot();
        if self.last_emitted.as_ref() != Some(&now) {
            (self.on_progress)(now.clone());
            self.last_emitted = Some(now);
        }
    }

    fn configure(&mut self, enabled: bool, tracks: Vec<PathBuf>, current: Option<usize>) {
        self.enabled = enabled;
        self.tracks = tracks;
        self.current = current;
        self.configured = true;
        self.need_scan = true;
        // A job survives a change that still includes its track; anything else makes it moot.
        for job in &self.jobs {
            if !enabled || !self.tracks.contains(&job.audio) {
                job.cancel.store(true, Ordering::Relaxed);
            }
        }
        self.emit();
    }

    fn set_power(&mut self, level: Level, cores: usize, free_mb: Option<u64>) {
        self.power = Power { level, cores, free_mb };
        self.need_scan = true; // a higher level may allow more jobs right now
    }

    fn request_regenerate(&mut self, audio: PathBuf) {
        if !is_generated(&self.root, &sidecar_path(&audio)) || self.regenerate.contains(&audio) {
            return;
        }
        self.failed.retain(|p| *p != audio);
        self.regenerate.push_back(audio);
        self.need_scan = true;
        self.emit();
    }

    /// Collects every job that has finished.
    fn poll_jobs(&mut self) {
        let mut finished = Vec::new();
        for (i, job) in self.jobs.iter().enumerate() {
            match job.result.try_recv() {
                Ok(result) => finished.push((i, result)),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    finished.push((i, Err("字幕任务异常结束".to_string())))
                }
            }
        }
        if finished.is_empty() {
            return;
        }
        // remove from the back so earlier indices stay valid
        for (i, result) in finished.into_iter().rev() {
            let job = self.jobs.remove(i);
            let _ = job.thread.join();
            match result {
                Ok(_) => self.failed.retain(|p| *p != job.audio),
                Err(e) if e == CANCELLED => {}
                Err(_) => {
                    if !self.failed.contains(&job.audio) {
                        self.failed.push(job.audio);
                    }
                }
            }
        }
        self.need_scan = true;
        self.emit();
    }

    fn lowest_free_slot(&self) -> usize {
        (0..).find(|s| !self.jobs.iter().any(|j| j.slot == *s)).unwrap_or(0)
    }

    /// Starts as many new jobs as the level, the cores and the free memory allow right now.
    /// Free memory is read again each time, so a machine that has got busy gets fewer new jobs;
    /// jobs already running are never stopped for that reason.
    fn start_more(&mut self) {
        if !self.enabled || !self.need_scan {
            return;
        }
        if active_dir(&self.root).is_none() {
            return; // keep looking: the add-on may be installed at any moment
        }
        let free_mb = self.power.free_mb.or_else(system_free_mb).unwrap_or(u64::MAX / 2);
        let allowed = jobs_for(self.power.level, self.power.cores, free_mb);

        while self.jobs.len() < allowed {
            let busy: Vec<PathBuf> = self.jobs.iter().map(|j| j.audio.clone()).collect();
            let next = self.regenerate.pop_front().or_else(|| {
                let failed = &self.failed;
                next_to_transcribe(&self.tracks, self.current, &|p: &Path| {
                    sidecar_path(p).exists()
                        || failed.iter().any(|f| f == p)
                        || busy.iter().any(|b| b == p)
                })
                .map(|i| self.tracks[i].clone())
            });
            let Some(audio) = next else {
                self.need_scan = false; // nothing left to start; wait for a change
                break;
            };
            if !self.spawn_job(audio, allowed) {
                break;
            }
        }
        // Full up with work still waiting: poll_jobs() raises need_scan again when one finishes.
        if self.jobs.len() >= allowed {
            self.need_scan = false;
        }
        self.emit();
    }

    fn spawn_job(&mut self, audio: PathBuf, planned_jobs: usize) -> bool {
        let slot = self.lowest_free_slot();
        let threads = threads_per_job(planned_jobs);
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (root, transcriber, flag, path) =
            (self.root.clone(), self.transcriber.clone(), cancel.clone(), audio.clone());
        let spawned = std::thread::Builder::new()
            .name(format!("lite-play-subtitle-job-{slot}"))
            .spawn(move || {
                let _ = tx.send(transcribe_one(&root, &*transcriber, &path, slot, threads, &flag));
            });
        match spawned {
            Ok(thread) => {
                self.jobs.push(Job { audio, slot, cancel, result: rx, thread });
                true
            }
            Err(_) => {
                self.failed.push(audio);
                false
            }
        }
    }

    fn stop(&mut self) {
        for job in &self.jobs {
            job.cancel.store(true, Ordering::Relaxed);
        }
        for job in self.jobs.drain(..) {
            let _ = job.thread.join();
        }
    }
}

fn run(rx: Receiver<Msg>, mut worker: Worker) {
    loop {
        match rx.recv_timeout(POLL) {
            Ok(Msg::Configure { enabled, tracks, current }) => worker.configure(enabled, tracks, current),
            Ok(Msg::Regenerate(audio)) => worker.request_regenerate(audio),
            Ok(Msg::SetPower { level, cores, free_mb }) => worker.set_power(level, cores, free_mb),
            Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                worker.stop();
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        worker.poll_jobs();
        worker.start_more();
    }
}

/// Starts the subtitle thread. `root` is the add-on directory (`subtitle-addon/`).
pub fn start(
    root: PathBuf,
    transcriber: Box<dyn Transcriber>,
    on_progress: Box<dyn Fn(Progress) + Send>,
) -> (Sender<Msg>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let worker = Worker {
        root,
        transcriber: Arc::from(transcriber),
        on_progress,
        enabled: false,
        power: Power {
            level: Level::default(),
            cores: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
            free_mb: None,
        },
        tracks: Vec::new(),
        current: None,
        failed: Vec::new(),
        regenerate: VecDeque::new(),
        jobs: Vec::new(),
        need_scan: false,
        configured: false,
        last_emitted: None,
    };
    let handle = std::thread::Builder::new()
        .name("lite-play-subtitle".into())
        .spawn(move || run(rx, worker))
        .expect("spawn subtitle worker thread");
    (tx, handle)
}

pub fn spawn(
    root: PathBuf,
    transcriber: Box<dyn Transcriber>,
    on_progress: Box<dyn Fn(Progress) + Send>,
) -> Sender<Msg> {
    start(root, transcriber, on_progress).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitle::install::Manifest;
    use crate::subtitle::srt::{sidecar_path, Cue};
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Sender;
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    #[derive(Clone)]
    enum Outcome {
        Text(&'static str),
        Empty,
        Fail(&'static str),
        /// Waits for the cancel flag, like the real recognizer does.
        Block,
        /// Takes this long and ignores the cancel flag.
        Slow(u64),
    }

    struct Fake {
        outcomes: Mutex<HashMap<String, Outcome>>,
        decoded: Mutex<Vec<String>>,
        /// Which track each work wav belongs to, keyed by the wav's file name.
        by_wav: Mutex<HashMap<String, String>>,
        entered: Mutex<Vec<String>>,
        cancelled: Mutex<Vec<String>>,
        version: Mutex<u32>,
        /// Jobs inside `recognize` right now, and the most there ever were at once.
        running: Mutex<usize>,
        peak: Mutex<usize>,
        /// (work wav name, thread count) of every recognize call.
        calls: Mutex<Vec<(String, u32)>>,
    }

    impl Fake {
        fn new() -> Arc<Fake> {
            Arc::new(Fake {
                outcomes: Mutex::new(HashMap::new()),
                decoded: Mutex::new(Vec::new()),
                by_wav: Mutex::new(HashMap::new()),
                entered: Mutex::new(Vec::new()),
                cancelled: Mutex::new(Vec::new()),
                version: Mutex::new(1),
                running: Mutex::new(0),
                peak: Mutex::new(0),
                calls: Mutex::new(Vec::new()),
            })
        }
        fn peak(&self) -> usize {
            *self.peak.lock().unwrap()
        }
        fn calls(&self) -> Vec<(String, u32)> {
            self.calls.lock().unwrap().clone()
        }
        fn set(&self, name: &str, outcome: Outcome) {
            self.outcomes.lock().unwrap().insert(name.to_string(), outcome);
        }
        fn decoded(&self) -> Vec<String> {
            self.decoded.lock().unwrap().clone()
        }
        fn count(&self, name: &str) -> usize {
            self.decoded().iter().filter(|n| n.as_str() == name).count()
        }
        fn entered(&self, name: &str) -> bool {
            self.entered.lock().unwrap().iter().any(|n| n == name)
        }
        fn was_cancelled(&self, name: &str) -> bool {
            self.cancelled.lock().unwrap().iter().any(|n| n == name)
        }
    }

    /// Decrements the fake's running count when dropped, however `recognize` returns.
    struct Leave<'a>(&'a Fake);

    impl Drop for Leave<'_> {
        fn drop(&mut self) {
            *self.0.running.lock().unwrap() -= 1;
        }
    }

    fn scopeguard_like(fake: &Fake) -> Leave<'_> {
        Leave(fake)
    }

    struct Shared(Arc<Fake>);

    impl Transcriber for Shared {
        fn decode(&self, src: &Path, dst: &Path, _cancel: &AtomicBool) -> Result<f64, String> {
            let name = src.file_name().unwrap().to_string_lossy().into_owned();
            let wav = dst.file_name().unwrap().to_string_lossy().into_owned();
            self.0.decoded.lock().unwrap().push(name.clone());
            self.0.by_wav.lock().unwrap().insert(wav, name);
            fs::create_dir_all(dst.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::write(dst, b"wav").map_err(|e| e.to_string())?;
            Ok(1.0)
        }

        fn recognize(
            &self,
            addon: &Path,
            wav_name: &str,
            threads: u32,
            cancel: &AtomicBool,
        ) -> Result<Vec<Cue>, String> {
            // like the real recognizer, it needs its own audio to exist
            if !addon.join("work").join(wav_name).is_file() {
                return Err(format!("missing work file {wav_name}"));
            }
            let name = self.0.by_wav.lock().unwrap().get(wav_name).cloned().unwrap_or_default();
            self.0.calls.lock().unwrap().push((wav_name.to_string(), threads));
            self.0.entered.lock().unwrap().push(name.clone());
            {
                let mut running = self.0.running.lock().unwrap();
                *running += 1;
                let mut peak = self.0.peak.lock().unwrap();
                *peak = (*peak).max(*running);
            }
            let _leave = scopeguard_like(&self.0);
            let outcome = self
                .0
                .outcomes
                .lock()
                .unwrap()
                .get(&name)
                .cloned()
                .unwrap_or(Outcome::Text("文字"));
            let text = |t: &str| {
                let v = *self.0.version.lock().unwrap();
                vec![Cue { start: 0.0, end: 1.0, text: format!("{t}-v{v}") }]
            };
            match outcome {
                Outcome::Text(t) => Ok(text(t)),
                Outcome::Empty => Ok(Vec::new()),
                Outcome::Fail(m) => Err(m.to_string()),
                Outcome::Slow(ms) => {
                    std::thread::sleep(Duration::from_millis(ms));
                    Ok(text("慢"))
                }
                Outcome::Block => loop {
                    if cancel.load(Ordering::Relaxed) {
                        self.0.cancelled.lock().unwrap().push(name);
                        return Err("cancelled".to_string());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                },
            }
        }
    }

    struct Harness {
        root: tempfile::TempDir,
        dir: tempfile::TempDir,
        fake: Arc<Fake>,
        progress: Arc<Mutex<Vec<Progress>>>,
        tx: Sender<Msg>,
        handle: Option<JoinHandle<()>>,
    }

    impl Harness {
        fn without_component() -> Harness {
            let root = tempfile::tempdir().unwrap();
            let dir = tempfile::tempdir().unwrap();
            let fake = Fake::new();
            let progress = Arc::new(Mutex::new(Vec::new()));
            let sink = progress.clone();
            let (tx, handle) = start(
                root.path().to_path_buf(),
                Box::new(Shared(fake.clone())),
                Box::new(move |p| sink.lock().unwrap().push(p)),
            );
            Harness { root, dir, fake, progress, tx, handle: Some(handle) }
        }

        fn new() -> Harness {
            let h = Harness::without_component();
            h.install_component();
            h
        }

        fn install_component(&self) {
            let inst = self.root.path().join("inst-1");
            fs::create_dir_all(inst.join("work")).unwrap();
            let manifest = Manifest {
                runtime_version: "2.0.1".into(),
                runtime_digest: "sha256:a".into(),
                model_date: "2025-09-09".into(),
                model_digest: "sha256:b".into(),
                vad_digest: "sha256:c".into(),
            };
            fs::write(inst.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
            fs::write(self.root.path().join("active.json"), br#"{"dir":"inst-1"}"#).unwrap();
        }

        fn audio(&self, name: &str) -> PathBuf {
            let p = self.dir.path().join(name);
            fs::write(&p, b"audio").unwrap();
            p
        }

        fn srt(&self, name: &str) -> PathBuf {
            sidecar_path(&self.dir.path().join(name))
        }

        /// True when no temporary wav of any job is left in the add-on's work directory.
        fn work_dir_is_empty(&self) -> bool {
            fs::read_dir(self.root.path().join("inst-1").join("work"))
                .map(|d| d.count() == 0)
                .unwrap_or(true)
        }

        fn configure(&self, enabled: bool, tracks: &[PathBuf], current: Option<usize>) {
            self.tx
                .send(Msg::Configure { enabled, tracks: tracks.to_vec(), current })
                .unwrap();
        }

        fn last_progress(&self) -> Option<Progress> {
            self.progress.lock().unwrap().last().cloned()
        }

        fn settled(&self, done: usize) -> bool {
            self.last_progress().is_some_and(|p| p.done == done && !p.running)
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = self.tx.send(Msg::Shutdown);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let started = Instant::now();
        while !cond() {
            assert!(started.elapsed() < Duration::from_secs(5), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn pause(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn names(h: &Harness, list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(|n| h.audio(n)).collect()
    }

    // ---------- the registry of subtitles this program wrote ----------

    #[test]
    fn registry_roundtrips_chinese_paths_and_ignores_duplicates() {
        let root = tempfile::tempdir().unwrap();
        let srt = PathBuf::from(r"D:\书 库\【无人区别墅】女主人.srt");
        assert!(!is_generated(root.path(), &srt));
        mark_generated(root.path(), &srt).unwrap();
        mark_generated(root.path(), &srt).unwrap();
        assert!(is_generated(root.path(), &srt));
        assert!(!is_generated(root.path(), Path::new(r"D:\书 库\别的.srt")));
        let list: Vec<String> =
            serde_json::from_slice(&fs::read(root.path().join("generated.json")).unwrap()).unwrap();
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn corrupt_registry_means_nothing_counts_as_generated() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("generated.json"), "{not json").unwrap();
        assert!(!is_generated(root.path(), Path::new("a.srt")));
        mark_generated(root.path(), Path::new("a.srt")).unwrap();
        assert!(is_generated(root.path(), Path::new("a.srt")));
    }

    #[test]
    fn many_threads_marking_at_once_lose_no_entry() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().to_path_buf();
        let threads: Vec<_> = (0..16)
            .map(|t| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for i in 0..12 {
                        mark_generated(&path, Path::new(&format!("book/{t:02}-{i:02}.srt"))).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let list: Vec<String> =
            serde_json::from_slice(&fs::read(root.path().join("generated.json")).unwrap()).unwrap();
        assert_eq!(list.len(), 16 * 12, "entries were overwritten by a concurrent writer");
    }

    // ---------- one track ----------

    #[test]
    fn transcribe_one_without_a_component_is_an_error() {
        let h = Harness::without_component();
        let t = h.audio("a.m4a");
        let err = transcribe_one(h.root.path(), &Shared(h.fake.clone()), &t, 0, 2, &AtomicBool::new(false))
            .unwrap_err();
        assert!(err.contains("未安装"), "{err}");
        assert!(h.fake.decoded().is_empty());
    }

    #[test]
    fn transcribe_one_refuses_to_overwrite_a_subtitle_it_did_not_write() {
        let h = Harness::new();
        let t = h.audio("a.m4a");
        fs::write(h.srt("a.m4a"), "用户的字幕").unwrap();
        let err = transcribe_one(h.root.path(), &Shared(h.fake.clone()), &t, 0, 2, &AtomicBool::new(false))
            .unwrap_err();
        assert!(err.contains("不覆盖"), "{err}");
        assert_eq!(fs::read_to_string(h.srt("a.m4a")).unwrap(), "用户的字幕");
        assert!(h.fake.decoded().is_empty(), "refused before any work was done");
    }

    #[test]
    fn transcribe_one_discards_the_result_when_cancelled_in_the_meantime() {
        let h = Harness::new();
        h.fake.set("a.m4a", Outcome::Slow(50));
        let t = h.audio("a.m4a");
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            flag.store(true, Ordering::Relaxed);
        });
        let err = transcribe_one(h.root.path(), &Shared(h.fake.clone()), &t, 0, 2, &cancel).unwrap_err();
        canceller.join().unwrap();
        assert_eq!(err, "cancelled");
        assert!(!h.srt("a.m4a").exists());
        assert!(h.work_dir_is_empty(), "a temporary wav was left behind");
    }

    // ---------- the background loop ----------

    #[test]
    fn transcribes_from_current_track_then_wraps() {
        let h = Harness::new();
        let t = names(&h, &["t0.m4a", "t1.m4a", "t2.m4a"]);
        // one job at a time, so the order in which they finish is the order they were picked
        h.tx.send(Msg::SetPower { level: Level::Light, cores: 2, free_mb: Some(32 * 1024) }).unwrap();
        h.configure(true, &t, Some(1));
        wait_until("all three subtitles", || h.settled(3));
        assert_eq!(h.fake.decoded(), vec!["t1.m4a", "t2.m4a", "t0.m4a"]);
        for n in ["t0.m4a", "t1.m4a", "t2.m4a"] {
            assert!(h.srt(n).exists(), "{n}");
        }
        assert!(h.work_dir_is_empty(), "a temporary wav was left behind");
    }

    #[test]
    fn existing_srt_is_never_touched_or_redone() {
        let h = Harness::new();
        let t = names(&h, &["t0.m4a", "t1.m4a", "t2.m4a"]);
        let user = "1\n00:00:01,000 --> 00:00:02,000\n用户自己的字幕\n";
        fs::write(h.srt("t1.m4a"), user).unwrap();
        h.configure(true, &t, Some(0));
        wait_until("the other two", || h.settled(3));
        // Which of the two runs first depends on how many jobs run at once; what matters is
        // that exactly those two were done and the user's own subtitle was left out.
        let mut done = h.fake.decoded();
        done.sort();
        assert_eq!(done, vec!["t0.m4a", "t2.m4a"]);
        assert_eq!(fs::read_to_string(h.srt("t1.m4a")).unwrap(), user);
    }

    #[test]
    fn zero_sentences_writes_nothing_and_is_not_retried() {
        let h = Harness::new();
        h.fake.set("t0.m4a", Outcome::Empty);
        let t = names(&h, &["t0.m4a"]);
        h.configure(true, &t, Some(0));
        wait_until("t0 marked failed", || {
            h.last_progress().is_some_and(|p| p.failed == vec!["t0.m4a".to_string()] && !p.running)
        });
        assert!(!h.srt("t0.m4a").exists());
        pause(400);
        assert_eq!(h.fake.count("t0.m4a"), 1);
        assert!(h.work_dir_is_empty(), "a temporary wav was left behind");
    }

    #[test]
    fn failure_is_skipped_and_next_continues() {
        let h = Harness::new();
        h.fake.set("t0.m4a", Outcome::Fail("boom"));
        let t = names(&h, &["t0.m4a", "t1.m4a"]);
        h.configure(true, &t, Some(0));
        wait_until("t1 done", || h.settled(1));
        let p = h.last_progress().unwrap();
        assert_eq!(p.failed, vec!["t0.m4a".to_string()]);
        assert!(!h.srt("t0.m4a").exists());
        assert!(h.srt("t1.m4a").exists());
        assert!(h.work_dir_is_empty(), "a temporary wav was left behind");
    }

    #[test]
    fn disabled_does_nothing() {
        let h = Harness::new();
        let t = names(&h, &["t0.m4a"]);
        h.configure(false, &t, Some(0));
        pause(300);
        assert!(h.fake.decoded().is_empty());
        let p = h.last_progress().unwrap();
        assert!(!p.running);
        assert_eq!((p.done, p.total), (0, 1));
    }

    #[test]
    fn missing_component_does_nothing_until_it_appears() {
        let h = Harness::without_component();
        let t = names(&h, &["t0.m4a"]);
        h.configure(true, &t, Some(0));
        pause(300);
        assert!(h.fake.decoded().is_empty());
        h.install_component();
        wait_until("it notices the component", || h.srt("t0.m4a").exists());
    }

    #[test]
    fn reconfigure_to_another_folder_cancels_the_running_job() {
        let h = Harness::new();
        h.fake.set("slow.m4a", Outcome::Block);
        let slow = h.audio("slow.m4a");
        let other = h.audio("other.m4a");
        h.configure(true, &[slow.clone()], Some(0));
        wait_until("slow job running", || h.fake.entered("slow.m4a"));

        h.configure(true, &[other], Some(0));
        wait_until("other done", || h.srt("other.m4a").exists());
        wait_until("slow cancelled", || h.fake.was_cancelled("slow.m4a"));
        assert!(!h.srt("slow.m4a").exists());
        wait_until("idle", || h.last_progress().is_some_and(|p| !p.running));
        assert!(h.work_dir_is_empty(), "a temporary wav was left behind");

        // a cancelled track is not remembered as a failure
        h.fake.set("slow.m4a", Outcome::Text("好"));
        h.configure(true, &[slow], Some(0));
        wait_until("slow now done", || h.srt("slow.m4a").exists());
    }

    #[test]
    fn switching_the_switch_off_cancels_the_running_job() {
        let h = Harness::new();
        h.fake.set("t0.m4a", Outcome::Block);
        let t = names(&h, &["t0.m4a"]);
        h.configure(true, &t, Some(0));
        wait_until("running", || h.fake.entered("t0.m4a"));
        h.configure(false, &t, Some(0));
        wait_until("cancelled", || h.fake.was_cancelled("t0.m4a"));
        assert!(!h.srt("t0.m4a").exists());
    }

    #[test]
    fn same_folder_reconfigure_lets_the_running_job_finish() {
        let h = Harness::new();
        h.fake.set("t0.m4a", Outcome::Slow(300));
        let t = names(&h, &["t0.m4a", "t1.m4a"]);
        h.configure(true, &t, Some(0));
        wait_until("t0 running", || h.fake.entered("t0.m4a"));
        h.configure(true, &t, Some(1));
        wait_until("both done", || h.settled(2));
        assert!(h.srt("t0.m4a").exists(), "the running job was not cancelled");
    }

    #[test]
    fn regenerate_only_overwrites_files_this_program_wrote() {
        let h = Harness::new();
        let t = names(&h, &["a.m4a", "b.m4a"]);
        let user = "1\n00:00:01,000 --> 00:00:02,000\n用户的\n";
        fs::write(h.srt("b.m4a"), user).unwrap();
        h.configure(true, &t, Some(0));
        wait_until("a done", || h.settled(2));
        assert!(fs::read_to_string(h.srt("a.m4a")).unwrap().contains("-v1"));

        *h.fake.version.lock().unwrap() = 2;
        h.tx.send(Msg::Regenerate(t[1].clone())).unwrap();
        h.tx.send(Msg::Regenerate(t[0].clone())).unwrap();
        wait_until("a redone", || {
            fs::read_to_string(h.srt("a.m4a")).is_ok_and(|s| s.contains("-v2"))
        });
        assert_eq!(fs::read_to_string(h.srt("b.m4a")).unwrap(), user);
        assert_eq!(h.fake.count("b.m4a"), 0);
    }

    #[test]
    fn a_failed_regeneration_keeps_the_old_subtitle() {
        let h = Harness::new();
        let t = names(&h, &["a.m4a"]);
        h.configure(true, &t, Some(0));
        wait_until("a done", || h.settled(1));
        let before = fs::read_to_string(h.srt("a.m4a")).unwrap();

        h.fake.set("a.m4a", Outcome::Fail("boom"));
        h.tx.send(Msg::Regenerate(t[0].clone())).unwrap();
        wait_until("failure recorded", || {
            h.last_progress().is_some_and(|p| p.failed == vec!["a.m4a".to_string()])
        });
        assert_eq!(fs::read_to_string(h.srt("a.m4a")).unwrap(), before);
    }

    #[test]
    fn progress_counts_existing_srt_files() {
        let h = Harness::new();
        let t = names(&h, &["t0.m4a", "t1.m4a", "t2.m4a"]);
        fs::write(h.srt("t1.m4a"), "x").unwrap();
        h.configure(false, &t, Some(0));
        wait_until("progress", || h.last_progress().is_some());
        let p = h.last_progress().unwrap();
        assert_eq!(p, Progress { done: 1, total: 3, current: vec![], failed: vec![], running: false });
    }

    #[test]
    fn progress_is_only_emitted_when_it_changes() {
        let h = Harness::new();
        let t = names(&h, &["t0.m4a"]);
        h.configure(false, &t, Some(0));
        h.configure(false, &t, Some(0));
        pause(300);
        assert_eq!(h.progress.lock().unwrap().len(), 1);
    }

    #[test]
    fn song_names_with_chinese_and_brackets() {
        let h = Harness::new();
        let t = names(&h, &["【无人区别墅】女主人.m4a", "0002 001 血门.m4a"]);
        h.configure(true, &t, Some(0));
        wait_until("both", || h.settled(2));
        assert!(h.dir.path().join("【无人区别墅】女主人.srt").exists());
        assert!(h.dir.path().join("0002 001 血门.srt").exists());
    }

    // ---------- performance levels: several jobs at once ----------

    fn many(h: &Harness, n: usize) -> Vec<PathBuf> {
        (0..n).map(|i| h.audio(&format!("t{i:02}.m4a"))).collect()
    }

    fn set_limits(h: &Harness, level: Level, cores: usize, free_mb: u64) {
        h.tx.send(Msg::SetPower { level, cores, free_mb: Some(free_mb) }).unwrap();
    }

    #[test]
    fn the_light_level_runs_a_quarter_of_the_cores_at_once() {
        let h = Harness::new();
        for i in 0..12 {
            h.fake.set(&format!("t{i:02}.m4a"), Outcome::Slow(250));
        }
        let t = many(&h, 12);
        set_limits(&h, Level::Light, 16, 32 * 1024); // 16 cores, light -> 2 jobs
        h.configure(true, &t, Some(0));
        wait_until("all twelve", || h.settled(12));
        assert_eq!(h.fake.peak(), 2, "never more than the level allows, and it did use them");
    }

    #[test]
    fn the_full_level_uses_every_job_slot_it_is_given() {
        let h = Harness::new();
        for i in 0..24 {
            h.fake.set(&format!("t{i:02}.m4a"), Outcome::Slow(250));
        }
        let t = many(&h, 24);
        set_limits(&h, Level::Full, 16, 32 * 1024); // 16 cores, full -> 8 jobs
        h.configure(true, &t, Some(0));
        wait_until("all of them", || h.settled(24));
        assert_eq!(h.fake.peak(), 8);
    }

    #[test]
    fn jobs_running_together_each_have_their_own_work_wav() {
        let h = Harness::new();
        for i in 0..6 {
            h.fake.set(&format!("t{i:02}.m4a"), Outcome::Slow(200));
        }
        let t = many(&h, 6);
        set_limits(&h, Level::Full, 12, 32 * 1024); // 6 jobs
        h.configure(true, &t, Some(0));
        wait_until("all six", || h.settled(6));
        let calls = h.fake.calls();
        let wavs: std::collections::HashSet<_> = calls.iter().map(|c| c.0.clone()).collect();
        assert!(wavs.len() >= 2, "concurrent jobs must not share one wav: {calls:?}");
        for i in 0..6 {
            assert!(h.srt(&format!("t{i:02}.m4a")).exists(), "t{i:02} must get its own subtitle");
        }
        assert!(h.work_dir_is_empty());
    }

    #[test]
    fn with_many_jobs_the_first_ones_picked_are_the_current_track_and_those_after_it() {
        let h = Harness::new();
        for i in 0..8 {
            h.fake.set(&format!("t{i:02}.m4a"), Outcome::Slow(400));
        }
        let t = many(&h, 8);
        set_limits(&h, Level::Full, 8, 32 * 1024); // 8 cores, full -> 4 jobs
        h.configure(true, &t, Some(5));
        wait_until("four started", || h.fake.entered.lock().unwrap().len() >= 4);
        let mut first_four: Vec<String> = h.fake.entered.lock().unwrap().iter().take(4).cloned().collect();
        first_four.sort();
        // the track being listened to (t05), the two after it, and the list wraps to t00
        let mut expected: Vec<String> =
            ["t05.m4a", "t06.m4a", "t07.m4a", "t00.m4a"].iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(first_four, expected);
    }

    #[test]
    fn each_track_is_transcribed_exactly_once_even_with_many_jobs() {
        let h = Harness::new();
        for i in 0..20 {
            h.fake.set(&format!("t{i:02}.m4a"), Outcome::Slow(60));
        }
        let t = many(&h, 20);
        set_limits(&h, Level::Full, 16, 32 * 1024);
        h.configure(true, &t, Some(0));
        wait_until("all twenty", || h.settled(20));
        for i in 0..20 {
            assert_eq!(h.fake.count(&format!("t{i:02}.m4a")), 1, "t{i:02}");
        }
    }

    #[test]
    fn the_subtitle_text_belongs_to_the_right_track() {
        let h = Harness::new();
        let t = many(&h, 8);
        for i in 0..8 {
            h.fake.set(&format!("t{i:02}.m4a"), Outcome::Slow(100));
        }
        set_limits(&h, Level::Full, 16, 32 * 1024);
        h.configure(true, &t, Some(0));
        wait_until("all eight", || h.settled(8));
        // every srt must contain its own marker, not a neighbour's
        for i in 0..8 {
            let text = fs::read_to_string(h.srt(&format!("t{i:02}.m4a"))).unwrap();
            assert!(text.contains("慢-v1"), "{text}");
        }
    }

    #[test]
    fn a_lone_job_gets_four_threads_and_parallel_jobs_get_two() {
        let h = Harness::new();
        let t = many(&h, 1);
        set_limits(&h, Level::Light, 8, 32 * 1024); // 8 cores, light -> 1 job
        h.configure(true, &t, Some(0));
        wait_until("one done", || h.settled(1));
        assert!(h.fake.calls().iter().all(|c| c.1 == 4), "{:?}", h.fake.calls());

        let h2 = Harness::new();
        for i in 0..6 {
            h2.fake.set(&format!("t{i:02}.m4a"), Outcome::Slow(150));
        }
        let t2 = many(&h2, 6);
        set_limits(&h2, Level::Full, 12, 32 * 1024);
        h2.configure(true, &t2, Some(0));
        wait_until("six done", || h2.settled(6));
        assert!(h2.fake.calls().iter().all(|c| c.1 == 2), "{:?}", h2.fake.calls());
    }

    #[test]
    fn tight_memory_limits_new_jobs_but_never_stops_running_ones() {
        let h = Harness::new();
        for i in 0..10 {
            h.fake.set(&format!("t{i:02}.m4a"), Outcome::Slow(300));
        }
        let t = many(&h, 10);
        // 4 GB free: the memory rule allows 2 jobs although the level would allow more
        set_limits(&h, Level::Full, 32, 4 * 1024);
        h.configure(true, &t, Some(0));
        wait_until("all ten", || h.settled(10));
        assert_eq!(h.fake.peak(), 2);
    }

    #[test]
    fn changing_the_level_while_running_takes_effect_for_the_next_jobs() {
        let h = Harness::new();
        for i in 0..16 {
            h.fake.set(&format!("t{i:02}.m4a"), Outcome::Slow(300));
        }
        let t = many(&h, 16);
        set_limits(&h, Level::Light, 16, 32 * 1024); // 2 jobs
        h.configure(true, &t, Some(0));
        wait_until("two running", || h.fake.peak() >= 2);
        set_limits(&h, Level::Full, 16, 32 * 1024); // 8 jobs
        wait_until("more than two at once", || h.fake.peak() > 2);
        wait_until("all sixteen", || h.settled(16));
        assert!(h.fake.peak() <= 8);
    }

    #[test]
    fn switching_off_cancels_every_running_job() {
        let h = Harness::new();
        for i in 0..4 {
            h.fake.set(&format!("t{i:02}.m4a"), Outcome::Block);
        }
        let t = many(&h, 4);
        set_limits(&h, Level::Full, 8, 32 * 1024); // 4 jobs
        h.configure(true, &t, Some(0));
        wait_until("four running", || h.fake.peak() == 4);
        h.configure(false, &t, Some(0));
        wait_until("all four cancelled", || (0..4).all(|i| h.fake.was_cancelled(&format!("t{i:02}.m4a"))));
        assert!((0..4).all(|i| !h.srt(&format!("t{i:02}.m4a")).exists()));
        wait_until("work dir clean", || h.work_dir_is_empty());
    }

    #[test]
    fn a_failing_track_does_not_stop_the_others() {
        let h = Harness::new();
        h.fake.set("t01.m4a", Outcome::Fail("boom"));
        let t = many(&h, 6);
        set_limits(&h, Level::Full, 12, 32 * 1024);
        h.configure(true, &t, Some(0));
        wait_until("five succeed", || h.settled(5));
        assert_eq!(h.last_progress().unwrap().failed, vec!["t01.m4a".to_string()]);
        assert!(h.work_dir_is_empty());
    }

    #[test]
    fn progress_lists_every_track_being_worked_on() {
        let h = Harness::new();
        for i in 0..4 {
            h.fake.set(&format!("t{i:02}.m4a"), Outcome::Block);
        }
        let t = many(&h, 4);
        set_limits(&h, Level::Full, 8, 32 * 1024);
        h.configure(true, &t, Some(0));
        wait_until("four running", || {
            h.last_progress().is_some_and(|p| p.current.len() == 4)
        });
        let mut shown = h.last_progress().unwrap().current;
        shown.sort();
        assert_eq!(shown, vec!["t00.m4a", "t01.m4a", "t02.m4a", "t03.m4a"]);
    }

    #[test]
    fn shutdown_stops_the_thread_even_mid_job() {
        let mut h = Harness::new();
        h.fake.set("t0.m4a", Outcome::Block);
        let t = names(&h, &["t0.m4a"]);
        h.configure(true, &t, Some(0));
        wait_until("running", || h.fake.entered("t0.m4a"));

        h.tx.send(Msg::Shutdown).unwrap();
        let handle = h.handle.take().unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = handle.join();
            let _ = done_tx.send(());
        });
        assert!(done_rx.recv_timeout(Duration::from_secs(2)).is_ok(), "worker did not stop");
        assert!(h.work_dir_is_empty(), "a temporary wav was left behind");
    }
}

#[cfg(test)]
mod live {
    use super::*;
    use crate::subtitle::srt::read_srt;

    /// End to end with the real decoder and the real recognizer. Needs an installed add-on
    /// (`LITE_PLAY_ADDON_ROOT`, a `subtitle-addon` directory) and a real episode
    /// (`LITE_PLAY_EPISODE`); the episode is copied, never modified.
    #[test]
    #[ignore = "needs an installed add-on and a real episode"]
    fn real_episode_becomes_a_srt_through_the_worker() {
        let root = PathBuf::from(std::env::var("LITE_PLAY_ADDON_ROOT").expect("LITE_PLAY_ADDON_ROOT"));
        let episode = PathBuf::from(std::env::var("LITE_PLAY_EPISODE").expect("LITE_PLAY_EPISODE"));
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join(episode.file_name().unwrap());
        fs::copy(&episode, &copy).unwrap();

        let started = std::time::Instant::now();
        let cancel = AtomicBool::new(false);
        let count = transcribe_one(&root, &RealTranscriber, &copy, 0, 4, &cancel).expect("transcribe_one");
        eprintln!("{count} sentences in {:.1}s", started.elapsed().as_secs_f64());

        let srt = sidecar_path(&copy);
        let cues = read_srt(&srt).expect("the written srt parses back");
        assert_eq!(cues.len(), count);
        assert!(cues.windows(2).all(|w| w[0].start <= w[1].start), "sorted by start");
        assert!(cues.iter().all(|c| c.end > c.start && !c.text.trim().is_empty()));
        assert!(is_generated(&root, &srt));
        assert!(!root.join("inst-1").join("work").join("job.wav").exists());
        for c in cues.iter().take(8) {
            eprintln!("  {:7.2} -> {:7.2}  {}", c.start, c.end, c.text);
        }
        // a second call refuses to redo it only through the queue; transcribe_one itself may
        // overwrite what this program wrote, which is what "regenerate" relies on
        assert_eq!(transcribe_one(&root, &RealTranscriber, &copy, 0, 4, &cancel).unwrap(), count);
    }
}

#[cfg(test)]
mod live_levels {
    use super::*;
    use std::time::Instant;

    /// Runs `episodes` copies of one real episode through the real worker at `level` and returns
    /// the seconds it took, the most jobs seen at once, and how many subtitles were written.
    fn run(root: &Path, episode: &Path, episodes: usize, level: Level, cores: usize) -> (f64, usize, usize) {
        let dir = tempfile::tempdir().unwrap();
        let tracks: Vec<PathBuf> = (0..episodes)
            .map(|i| {
                let p = dir.path().join(format!("ep{i:02}.m4a"));
                fs::copy(episode, &p).unwrap();
                p
            })
            .collect();
        let peak = Arc::new(Mutex::new(0usize));
        let seen = peak.clone();
        let (tx, handle) = start(
            root.to_path_buf(),
            Box::new(RealTranscriber),
            Box::new(move |p: Progress| {
                let mut peak = seen.lock().unwrap();
                *peak = (*peak).max(p.current.len());
            }),
        );
        let started = Instant::now();
        tx.send(Msg::SetPower { level, cores, free_mb: None }).unwrap();
        tx.send(Msg::Configure { enabled: true, tracks: tracks.clone(), current: Some(0) }).unwrap();
        while tracks.iter().any(|t| !sidecar_path(t).exists()) {
            assert!(started.elapsed().as_secs() < 600, "timed out");
            std::thread::sleep(Duration::from_millis(100));
        }
        let secs = started.elapsed().as_secs_f64();
        tx.send(Msg::Shutdown).unwrap();
        handle.join().unwrap();
        let written = tracks.iter().filter(|t| sidecar_path(t).exists()).count();
        let most = *peak.lock().unwrap();
        (secs, most, written)
    }

    #[test]
    #[ignore = "needs an installed add-on and a real episode; takes about a minute"]
    fn higher_levels_finish_the_same_work_faster() {
        let root = PathBuf::from(std::env::var("LITE_PLAY_ADDON_ROOT").expect("LITE_PLAY_ADDON_ROOT"));
        let episode = PathBuf::from(std::env::var("LITE_PLAY_EPISODE").expect("LITE_PLAY_EPISODE"));
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        let episodes = 8;
        let (light_s, light_peak, light_n) = run(&root, &episode, episodes, Level::Light, cores);
        let (full_s, full_peak, full_n) = run(&root, &episode, episodes, Level::Full, cores);
        eprintln!("{episodes} episodes, {cores} cores");
        eprintln!("  light: {light_s:5.1} s  at most {light_peak} at once  ({light_n} written)");
        eprintln!("  full : {full_s:5.1} s  at most {full_peak} at once  ({full_n} written)");
        assert_eq!(light_n, episodes);
        assert_eq!(full_n, episodes);
        assert!(full_peak > light_peak, "full must run more at once than light");
        assert!(full_s < light_s, "full should finish the same work sooner");
    }
}
