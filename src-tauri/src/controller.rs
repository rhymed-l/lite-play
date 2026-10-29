use crate::memory::Memory;
use crate::navigator::{Direction, Mode, Navigator};
use crate::player::AudioBackend;
use crate::playlist::scan_folder;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const NO_PLAYABLE_TRACKS: &str = "没有可播放的歌曲";
const NEAR_END_SECS: f64 = 3.0;
const SEEK_END_MARGIN_SECS: f64 = 1.0;
const SAVE_INTERVAL_MS: u64 = 5000;
/// After the last long-press seek, a track that reached its end is not advanced for this long,
/// so holding the key at the end of a song does not skip into the next one.
const HOLD_GRACE_MS: u64 = 1000;

#[derive(Serialize, Clone, Debug)]
pub struct TrackInfo {
    pub path: String,
    pub name: String,
    pub unavailable: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    pub folder: Option<String>,
    pub folders: Vec<String>,
    pub tracks: Vec<TrackInfo>,
    pub current: Option<usize>,
    pub playing: bool,
    pub position: f64,
    pub duration: Option<f64>,
    pub mode: Mode,
    pub direction: Direction,
    pub volume: f32,
    pub memory_enabled: bool,
    pub error: Option<String>,
}

pub struct Controller<B: AudioBackend> {
    backend: B,
    memory: Memory,
    memory_path: PathBuf,
    nav: Navigator,
    folder: Option<PathBuf>,
    tracks: Vec<TrackInfo>,
    playing: bool,
    /// A track is loaded in the backend (playing or paused) and still has audio left.
    loaded: bool,
    duration: Option<f64>,
    error: Option<String>,
    /// Warnings that outlive successful playback, e.g. a hotkey another program owns.
    notices: Vec<String>,
    last_save_ms: u64,
    last_tick_ms: u64,
    last_hold_seek_ms: Option<u64>,
}

fn lossy(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

impl<B: AudioBackend> Controller<B> {
    pub fn new(mut backend: B, memory: Memory, memory_path: PathBuf, seed: u64) -> Self {
        let mut nav = Navigator::new(seed);
        nav.set_mode(memory.mode);
        nav.set_direction(memory.direction);
        backend.set_volume(memory.volume);
        Self {
            backend,
            memory,
            memory_path,
            nav,
            folder: None,
            tracks: Vec::new(),
            playing: false,
            loaded: false,
            duration: None,
            error: None,
            notices: Vec::new(),
            last_save_ms: 0,
            last_tick_ms: 0,
            last_hold_seek_ms: None,
        }
    }

    pub fn restore(&mut self) {
        let Some(folder) = self.memory.current_folder.clone() else {
            return;
        };
        let dir = PathBuf::from(&folder);
        let Ok(files) = scan_folder(&dir) else {
            self.error = Some(format!("文件夹不可用：{folder}"));
            return;
        };
        self.folder = Some(dir);
        self.set_tracks(files);
        let last = self.memory.last_track.clone();
        let idx = last
            .and_then(|p| self.tracks.iter().position(|t| t.path == p))
            .or_else(|| self.nav.first());
        if let Some(idx) = idx {
            self.nav.jump_to(idx);
            self.load_track(idx);
        }
    }

    pub fn add_folder(&mut self, dir: &Path) {
        self.switch_folder(dir);
    }

    pub fn select_folder(&mut self, dir: &Path) {
        self.switch_folder(dir);
    }

    pub fn open_file(&mut self, file: &Path) {
        let dir = match file.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        if !self.switch_folder(dir) {
            return;
        }
        let wanted = file.file_name();
        let found = self
            .tracks
            .iter()
            .position(|t| Path::new(&t.path).file_name() == wanted);
        match found {
            Some(idx) => self.play_index(idx),
            None => self.error = Some("不支持的音频格式".to_string()),
        }
    }

    pub fn refresh_folder(&mut self) {
        let Some(dir) = self.folder.clone() else {
            return;
        };
        let Ok(files) = scan_folder(&dir) else {
            self.stop_playback();
            self.tracks.clear();
            self.nav.set_len(0);
            self.folder = None;
            self.error = Some(format!("文件夹不可用：{}", lossy(&dir)));
            return;
        };
        let current = self.nav.current().map(|i| self.tracks[i].path.clone());
        let unavailable: HashSet<String> = self
            .tracks
            .iter()
            .filter(|t| t.unavailable)
            .map(|t| t.path.clone())
            .collect();
        self.set_tracks(files);
        for t in &mut self.tracks {
            t.unavailable = unavailable.contains(&t.path);
        }
        match current.and_then(|c| self.tracks.iter().position(|t| t.path == c)) {
            Some(idx) => self.nav.jump_to(idx),
            None => self.stop_playback(),
        }
    }

    fn set_tracks(&mut self, files: Vec<PathBuf>) {
        self.tracks = files
            .iter()
            .map(|p| TrackInfo {
                path: lossy(p),
                name: p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                unavailable: false,
            })
            .collect();
        self.nav.set_len(0);
        self.nav.set_len(self.tracks.len());
    }

    fn switch_folder(&mut self, dir: &Path) -> bool {
        let files = match scan_folder(dir) {
            Ok(files) => files,
            Err(e) => {
                self.error = Some(format!("文件夹不可用：{} ({e})", lossy(dir)));
                return false;
            }
        };
        self.save_position();
        self.stop_playback();
        self.folder = Some(dir.to_path_buf());
        self.memory.add_folder(&lossy(dir));
        self.set_tracks(files);
        self.error = None;
        self.persist();
        true
    }

    fn stop_playback(&mut self) {
        self.backend.stop();
        self.playing = false;
        self.loaded = false;
        self.duration = None;
    }

    /// Loads the track paused, resuming at its remembered position when memory is on.
    fn load_track(&mut self, idx: usize) -> bool {
        let path = self.tracks[idx].path.clone();
        match self.backend.load(Path::new(&path)) {
            Err(_) => {
                self.tracks[idx].unavailable = true;
                self.loaded = false;
                self.duration = None;
                false
            }
            Ok(duration) => {
                self.duration = duration;
                self.loaded = true;
                self.tracks[idx].unavailable = false;
                if let Some(pos) = self.memory.position_for(&path) {
                    let _ = self.backend.seek(pos);
                }
                self.memory.last_track = Some(path);
                true
            }
        }
    }

    pub fn report_notice(&mut self, message: String) {
        if !self.notices.contains(&message) {
            self.notices.push(message);
        }
    }

    fn display_error(&self) -> Option<String> {
        let mut parts: Vec<&str> = self.notices.iter().map(String::as_str).collect();
        if let Some(e) = &self.error {
            parts.push(e);
        }
        (!parts.is_empty()).then(|| parts.join("；"))
    }

    fn persist(&mut self) {
        if self.memory.save(&self.memory_path).is_err() {
            self.error = Some("无法保存播放记忆".to_string());
        }
    }

    /// Remembers where the current track is, or forgets it near the end or at the very start.
    fn save_position(&mut self) {
        if !self.loaded {
            return;
        }
        let Some(idx) = self.nav.current() else {
            return;
        };
        let path = self.tracks[idx].path.clone();
        let pos = self.backend.position();
        let near_end = self.duration.is_some_and(|d| pos >= d - NEAR_END_SECS);
        if near_end || pos < 1.0 {
            self.memory.clear_position(&path);
        } else {
            self.memory.record_position(&path, pos);
        }
    }

    /// Plays `first`; a track that cannot be decoded is marked and the next one is tried.
    /// Gives up after one attempt per track, so a folder of broken files cannot loop.
    fn play_from(&mut self, first: usize) -> bool {
        self.play_from_dir(first, true)
    }

    /// `forward == false` keeps skipping backwards past broken tracks (used by Prev).
    fn play_from_dir(&mut self, first: usize, forward: bool) -> bool {
        let mut idx = first;
        for _ in 0..self.tracks.len() {
            if self.nav.current() != Some(idx) {
                self.nav.jump_to(idx);
            }
            if self.load_track(idx) {
                self.backend.play();
                self.playing = true;
                self.error = None;
                return true;
            }
            let step = if forward {
                self.nav.next_manual()
            } else {
                self.nav.prev_manual()
            };
            match step {
                Some(next) => idx = next,
                None => break,
            }
        }
        self.stop_playback();
        self.error = Some(NO_PLAYABLE_TRACKS.to_string());
        false
    }

    fn on_finished(&mut self) {
        if let Some(idx) = self.nav.current() {
            let path = self.tracks[idx].path.clone();
            self.memory.clear_position(&path);
        }
        match self.nav.next_auto() {
            Some(next) => {
                self.play_from(next);
            }
            None => self.stop_playback(),
        }
        self.persist();
    }

    pub fn play_pause(&mut self) {
        if self.tracks.is_empty() {
            return;
        }
        if self.playing {
            self.backend.pause();
            self.playing = false;
            self.save_position();
            self.persist();
        } else if self.loaded {
            self.backend.play();
            self.playing = true;
        } else if let Some(idx) = self.nav.current().or_else(|| self.nav.first()) {
            self.play_from(idx);
        }
    }

    pub fn next(&mut self) {
        self.save_position();
        if let Some(idx) = self.nav.next_manual() {
            self.play_from(idx);
            self.persist();
        }
    }

    pub fn prev(&mut self) {
        self.save_position();
        if let Some(idx) = self.nav.prev_manual() {
            self.play_from_dir(idx, false);
            self.persist();
        }
    }

    pub fn play_index(&mut self, idx: usize) {
        if idx >= self.tracks.len() {
            return;
        }
        self.save_position();
        self.play_from(idx);
        self.persist();
    }

    fn clamp_seek_target(&self, secs: f64) -> f64 {
        let target = secs.max(0.0);
        match self.duration {
            Some(d) => target.min((d - SEEK_END_MARGIN_SECS).max(0.0)),
            None => target,
        }
    }

    /// Seek requested from the progress slider; a refused seek is shown to the user.
    pub fn seek_to(&mut self, secs: f64) {
        if !self.loaded {
            return;
        }
        let target = self.clamp_seek_target(secs);
        if self.backend.seek(target).is_err() {
            self.error = Some("该文件不支持跳转".to_string());
        }
    }

    /// Seek step from a held hotkey. Repeats every ~200 ms, so a refused seek stays silent
    /// and the end of the track is not treated as "finished" while the key is still held.
    pub fn seek_by(&mut self, delta: f64) {
        if !self.loaded {
            return;
        }
        self.last_hold_seek_ms = Some(self.last_tick_ms);
        let target = self.clamp_seek_target(self.backend.position() + delta);
        let _ = self.backend.seek(target);
    }

    fn hold_seek_active(&self, now_ms: u64) -> bool {
        self.last_hold_seek_ms
            .is_some_and(|t| now_ms.saturating_sub(t) < HOLD_GRACE_MS)
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.nav.set_mode(mode);
        self.memory.mode = mode;
        self.persist();
    }

    pub fn set_direction(&mut self, direction: Direction) {
        self.nav.set_direction(direction);
        self.memory.direction = direction;
        self.persist();
    }

    pub fn set_volume(&mut self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        self.memory.volume = volume;
        self.backend.set_volume(volume);
    }

    pub fn set_memory_enabled(&mut self, enabled: bool) {
        self.memory.memory_enabled = enabled;
        self.persist();
    }

    pub fn tick(&mut self, now_ms: u64) {
        self.last_tick_ms = now_ms;
        if self.playing && self.loaded && self.backend.is_finished() {
            if !self.hold_seek_active(now_ms) {
                self.on_finished();
            }
            return;
        }
        if self.playing && now_ms.saturating_sub(self.last_save_ms) >= SAVE_INTERVAL_MS {
            self.last_save_ms = now_ms;
            self.save_position();
            self.persist();
        }
    }

    pub fn shutdown(&mut self) {
        self.save_position();
        self.persist();
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            folder: self.folder.as_deref().map(lossy),
            folders: self.memory.folders.clone(),
            tracks: self.tracks.clone(),
            current: self.nav.current(),
            playing: self.playing,
            position: if self.loaded { self.backend.position() } else { 0.0 },
            duration: self.duration,
            mode: self.memory.mode,
            direction: self.memory.direction,
            volume: self.memory.volume,
            memory_enabled: self.memory.memory_enabled,
            error: self.display_error(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[derive(Default)]
    struct FakeBackend {
        loads: Vec<PathBuf>,
        seeks: Vec<f64>,
        pos: f64,
        finished: bool,
        duration: Option<f64>,
        playing: bool,
        seek_fails: bool,
    }

    impl AudioBackend for FakeBackend {
        fn load(&mut self, path: &Path) -> Result<Option<f64>, String> {
            self.loads.push(path.to_path_buf());
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.contains("bad") {
                return Err("decode error".into());
            }
            self.pos = 0.0;
            self.finished = false;
            self.playing = false;
            Ok(self.duration)
        }
        fn play(&mut self) {
            self.playing = true;
        }
        fn pause(&mut self) {
            self.playing = false;
        }
        fn stop(&mut self) {
            self.playing = false;
        }
        fn seek(&mut self, secs: f64) -> Result<(), String> {
            if self.seek_fails {
                return Err("seek refused".into());
            }
            self.seeks.push(secs);
            self.pos = secs;
            Ok(())
        }
        fn position(&self) -> f64 {
            self.pos
        }
        fn is_finished(&self) -> bool {
            self.finished
        }
        fn set_volume(&mut self, _v: f32) {}
    }

    struct Fixture {
        music: tempfile::TempDir,
        store: tempfile::TempDir,
        c: Controller<FakeBackend>,
    }

    fn fixture_with(backend: FakeBackend, names: &[&str]) -> Fixture {
        let music = tempfile::tempdir().unwrap();
        for n in names {
            fs::write(music.path().join(n), b"x").unwrap();
        }
        let store = tempfile::tempdir().unwrap();
        let mut c = Controller::new(
            backend,
            Memory::default(),
            store.path().join("memory.json"),
            7,
        );
        c.add_folder(music.path());
        Fixture { music, store, c }
    }

    fn fixture(names: &[&str]) -> Fixture {
        fixture_with(FakeBackend::default(), names)
    }

    fn path_of(f: &Fixture, name: &str) -> String {
        f.music.path().join(name).to_string_lossy().into_owned()
    }

    fn reload(f: &Fixture) -> Controller<FakeBackend> {
        let path = f.store.path().join("memory.json");
        Controller::new(FakeBackend::default(), Memory::load(&path), path, 7)
    }

    fn track_names(c: &Controller<FakeBackend>) -> Vec<String> {
        c.snapshot().tracks.iter().map(|t| t.name.clone()).collect()
    }

    #[test]
    fn remembers_position_per_song_across_switching() {
        let mut f = fixture(&["a.mp3", "b.mp3", "c.mp3"]);
        f.c.play_pause();
        assert_eq!(f.c.snapshot().current, Some(0));
        f.c.backend.pos = 211.0;
        f.c.next();
        assert_eq!(f.c.snapshot().current, Some(1));

        f.c.play_index(0);

        assert_eq!(f.c.backend.seeks.last().copied(), Some(211.0));
        assert_eq!(f.c.snapshot().position, 211.0);
    }

    #[test]
    fn disabled_memory_starts_from_zero_keeps_old_records_and_restores_on_enable() {
        let mut f = fixture(&["a.mp3", "b.mp3"]);
        let pa = path_of(&f, "a.mp3");
        f.c.play_pause();
        f.c.backend.pos = 100.0;
        f.c.next();

        f.c.set_memory_enabled(false);
        f.c.backend.seeks.clear();
        f.c.play_index(0);
        assert!(f.c.backend.seeks.is_empty(), "no seek while memory is off");
        f.c.backend.pos = 50.0;
        f.c.next();
        assert_eq!(f.c.memory.positions[&pa], 100.0);

        f.c.set_memory_enabled(true);
        f.c.backend.seeks.clear();
        f.c.play_index(0);
        assert_eq!(f.c.backend.seeks.last().copied(), Some(100.0));
    }

    #[test]
    fn restore_returns_to_last_track_and_position_paused() {
        let mut f = fixture(&["a.mp3", "b.mp3"]);
        f.c.play_pause();
        f.c.next();
        f.c.backend.pos = 42.0;
        f.c.shutdown();

        let mut c2 = reload(&f);
        c2.restore();

        let s = c2.snapshot();
        assert_eq!(s.current, Some(1));
        assert!(!s.playing);
        assert_eq!(s.position, 42.0);
    }

    #[test]
    fn restore_falls_back_to_first_track_when_last_track_was_deleted() {
        let mut f = fixture(&["a.mp3", "b.mp3"]);
        f.c.play_pause();
        f.c.next();
        f.c.shutdown();
        fs::remove_file(f.music.path().join("b.mp3")).unwrap();

        let mut c2 = reload(&f);
        c2.restore();

        let s = c2.snapshot();
        assert_eq!(s.current, Some(0));
        assert_eq!(s.tracks.len(), 1);
    }

    #[test]
    fn restore_with_missing_folder_gives_empty_state_without_panicking() {
        let mut f = fixture(&["a.mp3"]);
        f.c.play_pause();
        f.c.shutdown();
        fs::remove_dir_all(f.music.path()).unwrap();

        let mut c2 = reload(&f);
        c2.restore();

        let s = c2.snapshot();
        assert_eq!(s.folder, None);
        assert!(s.tracks.is_empty());
        assert_eq!(s.current, None);
    }

    #[test]
    fn unknown_duration_keeps_position_and_seek_by_clamps_only_at_zero() {
        let mut f = fixture(&["a.mp3", "b.mp3"]);
        let pa = path_of(&f, "a.mp3");
        f.c.play_pause();
        f.c.backend.pos = 9999.0;
        f.c.next();
        assert_eq!(f.c.memory.positions[&pa], 9999.0);

        f.c.backend.pos = 10.0;
        f.c.seek_by(5.0);
        assert_eq!(f.c.backend.seeks.last().copied(), Some(15.0));
        f.c.seek_by(-999.0);
        assert_eq!(f.c.backend.seeks.last().copied(), Some(0.0));
    }

    #[test]
    fn near_the_end_clears_the_record_and_seek_by_stops_one_second_short() {
        let backend = FakeBackend {
            duration: Some(100.0),
            ..Default::default()
        };
        let mut f = fixture_with(backend, &["a.mp3", "b.mp3"]);
        let pa = path_of(&f, "a.mp3");
        f.c.memory.record_position(&pa, 50.0);
        f.c.play_pause();
        f.c.backend.pos = 99.0;
        f.c.next();
        assert!(!f.c.memory.positions.contains_key(&pa));

        f.c.backend.pos = 99.0;
        f.c.seek_by(5.0);
        assert_eq!(f.c.backend.seeks.last().copied(), Some(99.0));
    }

    #[test]
    fn natural_finish_advances_and_clears_the_finished_songs_record() {
        let mut f = fixture(&["a.mp3", "b.mp3", "c.mp3"]);
        let pa = path_of(&f, "a.mp3");
        f.c.memory.record_position(&pa, 20.0);
        f.c.play_pause();
        f.c.backend.finished = true;

        f.c.tick(0);

        assert_eq!(f.c.snapshot().current, Some(1));
        assert!(f.c.snapshot().playing);
        assert!(!f.c.memory.positions.contains_key(&pa));
    }

    #[test]
    fn last_track_finishing_stops_at_zero_and_play_restarts_it() {
        let mut f = fixture(&["a.mp3", "b.mp3"]);
        f.c.play_index(1);
        f.c.backend.pos = 77.0;
        f.c.backend.finished = true;

        f.c.tick(0);

        let s = f.c.snapshot();
        assert!(!s.playing);
        assert_eq!(s.current, Some(1));
        assert_eq!(s.position, 0.0);

        f.c.play_pause();
        assert!(f.c.snapshot().playing);
        assert!(f.c.backend.playing);
    }

    #[test]
    fn all_tracks_undecodable_stops_with_error_and_bounded_loads() {
        let mut f = fixture(&["bad1.mp3", "bad2.mp3"]);
        f.c.play_pause();

        let s = f.c.snapshot();
        assert_eq!(s.error.as_deref(), Some("没有可播放的歌曲"));
        assert!(!s.playing);
        assert!(f.c.backend.loads.len() <= 2, "loads: {}", f.c.backend.loads.len());
    }

    #[test]
    fn all_undecodable_in_single_mode_does_not_loop_forever() {
        let mut f = fixture(&["bad1.mp3", "bad2.mp3"]);
        f.c.set_mode(Mode::Single);
        f.c.play_pause();

        assert_eq!(f.c.snapshot().error.as_deref(), Some("没有可播放的歌曲"));
        assert!(f.c.backend.loads.len() <= 2);
    }

    #[test]
    fn all_undecodable_in_shuffle_mode_is_bounded_by_track_count() {
        let mut f = fixture(&["bad1.mp3", "bad2.mp3", "bad3.mp3"]);
        f.c.set_mode(Mode::Shuffle);
        f.c.play_pause();

        assert_eq!(f.c.snapshot().error.as_deref(), Some("没有可播放的歌曲"));
        assert!(f.c.backend.loads.len() <= 3);
    }

    #[test]
    fn bad_track_is_skipped_and_marked_unavailable() {
        let mut f = fixture(&["a.mp3", "bad.mp3", "c.mp3"]);
        f.c.play_pause();
        f.c.backend.finished = true;

        f.c.tick(0);

        let s = f.c.snapshot();
        assert_eq!(s.current, Some(2));
        assert!(s.playing);
        assert!(s.tracks[1].unavailable);
    }

    #[test]
    fn open_file_adopts_its_folder_and_starts_that_song() {
        let music = tempfile::tempdir().unwrap();
        for n in ["a.mp3", "b.mp3", "c.mp3"] {
            fs::write(music.path().join(n), b"x").unwrap();
        }
        let store = tempfile::tempdir().unwrap();
        let mut c = Controller::new(
            FakeBackend::default(),
            Memory::default(),
            store.path().join("memory.json"),
            7,
        );

        c.open_file(&music.path().join("b.mp3"));
        c.open_file(&music.path().join("c.mp3"));

        let s = c.snapshot();
        let dir = music.path().to_string_lossy().into_owned();
        assert_eq!(s.folder.as_deref(), Some(dir.as_str()));
        assert_eq!(s.folders, vec![dir]);
        assert_eq!(s.current, Some(2));
        assert!(s.playing);
    }

    #[test]
    fn only_the_top_level_of_a_folder_is_listed() {
        let mut f = fixture(&["a.mp3", "b.mp3"]);
        fs::create_dir(f.music.path().join("sub")).unwrap();
        fs::write(f.music.path().join("sub").join("x.mp3"), b"x").unwrap();

        f.c.refresh_folder();

        assert_eq!(track_names(&f.c), vec!["a.mp3", "b.mp3"]);
    }

    #[test]
    fn refresh_keeps_the_current_track_when_the_list_shifts() {
        let mut f = fixture(&["a.mp3", "b.mp3"]);
        f.c.play_index(1);
        fs::write(f.music.path().join("0.mp3"), b"x").unwrap();

        f.c.refresh_folder();

        let s = f.c.snapshot();
        assert_eq!(s.tracks[s.current.unwrap()].name, "b.mp3");
        assert!(s.playing);
    }

    #[test]
    fn refresh_stops_when_the_current_track_disappears() {
        let mut f = fixture(&["a.mp3", "b.mp3"]);
        f.c.play_index(1);
        fs::remove_file(f.music.path().join("b.mp3")).unwrap();

        f.c.refresh_folder();

        let s = f.c.snapshot();
        assert_eq!(s.current, None);
        assert!(!s.playing);
    }

    #[test]
    fn reverse_direction_starts_from_the_last_file() {
        let mut f = fixture(&["a.mp3", "b.mp3", "c.mp3"]);
        f.c.set_direction(Direction::Reverse);

        f.c.play_pause();

        assert_eq!(f.c.snapshot().current, Some(2));
        let last = f.c.backend.loads.last().unwrap();
        assert!(last.ends_with("c.mp3"));
    }

    #[test]
    fn shutdown_persists_settings_folders_and_last_track() {
        let mut f = fixture(&["a.mp3", "b.mp3"]);
        f.c.set_mode(Mode::Shuffle);
        f.c.play_pause();
        f.c.shutdown();

        let saved = Memory::load(&f.store.path().join("memory.json"));
        assert_eq!(saved.mode, Mode::Shuffle);
        assert_eq!(saved.folders, vec![f.music.path().to_string_lossy().into_owned()]);
        assert_eq!(saved.last_track, Some(path_of(&f, "a.mp3")));
    }

    #[test]
    fn tick_saves_the_position_every_five_seconds_while_playing() {
        let mut f = fixture(&["a.mp3", "b.mp3"]);
        let pa = path_of(&f, "a.mp3");
        let file = f.store.path().join("memory.json");
        f.c.play_pause();
        f.c.backend.pos = 12.0;

        f.c.tick(4999);
        assert!(!Memory::load(&file).positions.contains_key(&pa));

        f.c.tick(5000);
        assert_eq!(Memory::load(&file).positions[&pa], 12.0);
    }

    #[test]
    fn pausing_saves_the_position() {
        let mut f = fixture(&["a.mp3"]);
        let pa = path_of(&f, "a.mp3");
        f.c.play_pause();
        f.c.backend.pos = 30.0;

        f.c.play_pause();

        assert!(!f.c.snapshot().playing);
        assert!(!f.c.backend.playing);
        assert_eq!(f.c.memory.positions[&pa], 30.0);
    }

    #[test]
    fn selecting_a_missing_folder_reports_an_error_and_changes_nothing() {
        let mut f = fixture(&["a.mp3"]);
        f.c.play_pause();
        let before = f.c.snapshot();

        f.c.select_folder(Path::new("Z:/definitely/not/here"));

        let after = f.c.snapshot();
        assert!(after.error.is_some());
        assert_eq!(after.folder, before.folder);
        assert_eq!(after.current, before.current);
        assert!(after.playing);
    }

    #[test]
    fn prev_skips_an_undecodable_track_backwards() {
        let mut f = fixture(&["a.mp3", "bad.mp3", "c.mp3"]);
        f.c.play_index(2);

        f.c.prev();

        let s = f.c.snapshot();
        assert_eq!(s.current, Some(0));
        assert!(s.tracks[1].unavailable);
        assert!(s.playing);
    }

    #[test]
    fn holding_seek_at_the_end_does_not_advance_until_the_hold_is_over() {
        let backend = FakeBackend {
            duration: Some(100.0),
            ..Default::default()
        };
        let mut f = fixture_with(backend, &["a.mp3", "b.mp3"]);
        f.c.play_pause();
        f.c.tick(0);
        f.c.backend.pos = 98.0;
        f.c.seek_by(5.0);
        assert_eq!(f.c.backend.seeks.last().copied(), Some(99.0));
        f.c.backend.finished = true;

        f.c.tick(200);
        assert_eq!(f.c.snapshot().current, Some(0), "advanced during the hold");

        f.c.seek_by(5.0);
        f.c.tick(400);
        assert_eq!(f.c.snapshot().current, Some(0), "advanced during the hold");

        f.c.tick(1500);
        assert_eq!(f.c.snapshot().current, Some(1));
    }

    #[test]
    fn a_refused_hold_seek_does_not_raise_an_error_banner() {
        let backend = FakeBackend {
            seek_fails: true,
            ..Default::default()
        };
        let mut f = fixture_with(backend, &["a.mp3"]);
        f.c.play_pause();

        f.c.seek_by(5.0);

        assert_eq!(f.c.snapshot().error, None);
    }

    #[test]
    fn a_notice_survives_successful_playback() {
        let mut f = fixture(&["a.mp3"]);
        f.c.report_notice("快捷键 Ctrl+Alt+→ 已被其他程序占用".to_string());

        f.c.play_pause();

        assert!(f.c.snapshot().playing);
        assert_eq!(
            f.c.snapshot().error.as_deref(),
            Some("快捷键 Ctrl+Alt+→ 已被其他程序占用")
        );
    }

    #[test]
    fn notice_and_playback_error_are_both_shown() {
        let mut f = fixture(&["bad.mp3"]);
        f.c.report_notice("无法打开音频输出设备".to_string());

        f.c.play_pause();

        let shown = f.c.snapshot().error.unwrap();
        assert!(shown.contains("无法打开音频输出设备"), "{shown}");
        assert!(shown.contains("没有可播放的歌曲"), "{shown}");
    }

    #[test]
    fn a_refused_slider_seek_still_reports_an_error() {
        let backend = FakeBackend {
            seek_fails: true,
            ..Default::default()
        };
        let mut f = fixture_with(backend, &["a.mp3"]);
        f.c.play_pause();

        f.c.seek_to(30.0);

        assert!(f.c.snapshot().error.is_some());
    }
}
