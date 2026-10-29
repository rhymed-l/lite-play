use crate::controller::Snapshot;
use crate::subtitle::view::{active_index, load_for, Status, TrackSubtitles};
pub use crate::subtitle::worker::Msg;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

/// What changed since the previous snapshot. `None` means "nothing to tell the UI".
#[derive(Debug, Default)]
pub struct Update {
    /// The current track's subtitles, when they were (re)loaded and are different.
    pub track: Option<TrackSubtitles>,
    /// The line to show now, when it differs from before: `Some(None)` is "show nothing".
    pub active: Option<Option<usize>>,
}

/// Keeps the subtitle side of the playback engine: which track's subtitles are loaded, which
/// line is current, and what the background worker has been told. Pure bookkeeping; the engine
/// feeds it playback snapshots and forwards the answers as UI events.
pub struct SubtitleSync {
    worker: Option<Sender<Msg>>,
    enabled: bool,
    /// What the worker was last configured with: (enabled, tracks, current).
    told: Option<(bool, Vec<String>, Option<usize>)>,
    loaded_for: Option<Option<String>>,
    view: TrackSubtitles,
    active: Option<usize>,
    reload_requested: bool,
    /// The view the UI last received, to avoid sending the same thing twice.
    emitted: Option<TrackSubtitles>,
}

fn empty_view() -> TrackSubtitles {
    TrackSubtitles { path: None, status: Status::None, cues: Vec::new() }
}

impl SubtitleSync {
    pub fn new() -> Self {
        SubtitleSync {
            worker: None,
            enabled: false,
            told: None,
            loaded_for: None,
            view: empty_view(),
            active: None,
            reload_requested: false,
            emitted: None,
        }
    }

    pub fn attach(&mut self, worker: Sender<Msg>) {
        self.worker = Some(worker);
        self.told = None;
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// The `.srt` of the current track may have changed on disk.
    pub fn reload(&mut self) {
        self.reload_requested = true;
    }

    pub fn current(&self) -> (TrackSubtitles, Option<usize>) {
        (self.view.clone(), self.active)
    }

    /// Call after every playback snapshot, whatever triggered it.
    pub fn update(&mut self, snap: &Snapshot) -> Update {
        let tracks: Vec<String> = snap.tracks.iter().map(|t| t.path.clone()).collect();
        let current_path = snap.current.and_then(|i| tracks.get(i).cloned());
        self.configure_worker(tracks, snap.current);

        if !self.enabled {
            return self.show_nothing();
        }
        let mut update = Update::default();
        let before = self.active;
        let mut reloaded = false;
        if self.reload_requested || self.loaded_for.as_ref() != Some(&current_path) {
            self.reload_requested = false;
            self.loaded_for = Some(current_path.clone());
            self.view = load_for(current_path.as_deref().map(Path::new));
            if self.emitted.as_ref() != Some(&self.view) {
                self.emitted = Some(self.view.clone());
                update.track = Some(self.view.clone());
                reloaded = true;
            }
        }
        let now = active_index(&self.view.cues, snap.position);
        // The UI already knows `before`. A freshly sent view makes it forget its highlight,
        // so a line that is current must be repeated even when its index is unchanged.
        if now != before || (reloaded && now.is_some()) {
            update.active = Some(now);
        }
        self.active = now;
        update
    }

    fn show_nothing(&mut self) -> Update {
        let had_something = self.emitted.as_ref().is_some_and(|v| *v != empty_view()) || self.active.is_some();
        self.loaded_for = None;
        self.view = empty_view();
        self.reload_requested = false;
        if !had_something {
            return Update::default();
        }
        self.active = None;
        self.emitted = Some(empty_view());
        Update { track: Some(empty_view()), active: Some(None) }
    }

    fn configure_worker(&mut self, tracks: Vec<String>, current: Option<usize>) {
        let Some(worker) = &self.worker else {
            return;
        };
        let wanted = (self.enabled, tracks, current);
        if self.told.as_ref() == Some(&wanted) {
            return;
        }
        // Never tell a worker that was never switched on to switch off.
        if self.told.is_none() && !wanted.0 {
            self.told = Some(wanted);
            return;
        }
        let (enabled, tracks, current) = wanted.clone();
        let paths: Vec<PathBuf> = tracks.iter().map(PathBuf::from).collect();
        let _ = worker.send(Msg::Configure { enabled, tracks: paths, current });
        self.told = Some(wanted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{Snapshot, TrackInfo};
    use crate::navigator::{Direction, Mode};
    use crate::subtitle::view::Status;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::mpsc::{self, Receiver};

    struct Fixture {
        dir: tempfile::TempDir,
        tracks: Vec<PathBuf>,
        rx: Receiver<Msg>,
        sync: SubtitleSync,
    }

    fn fixture_with(names: &[&str], attach: bool) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let tracks: Vec<PathBuf> = names.iter().map(|n| dir.path().join(n)).collect();
        for t in &tracks {
            fs::write(t, b"audio").unwrap();
        }
        let (tx, rx) = mpsc::channel();
        let mut sync = SubtitleSync::new();
        if attach {
            sync.attach(tx);
        }
        Fixture { dir, tracks, rx, sync }
    }

    fn fixture(names: &[&str]) -> Fixture {
        fixture_with(names, true)
    }

    fn write_srt(f: &Fixture, track: usize, body: &str) {
        fs::write(f.tracks[track].with_extension("srt"), body).unwrap();
    }

    const TWO_CUES: &str =
        "1\n00:00:01,000 --> 00:00:02,000\n甲\n\n2\n00:00:05,000 --> 00:00:06,000\n乙\n";

    fn snap(f: &Fixture, current: Option<usize>, position: f64) -> Snapshot {
        Snapshot {
            folder: Some(f.dir.path().to_string_lossy().into_owned()),
            folders: vec![],
            tracks: f
                .tracks
                .iter()
                .map(|p| TrackInfo {
                    path: p.to_string_lossy().into_owned(),
                    name: p.file_name().unwrap().to_string_lossy().into_owned(),
                    unavailable: false,
                })
                .collect(),
            current,
            playing: true,
            position,
            duration: None,
            mode: Mode::Sequential,
            direction: Direction::Forward,
            volume: 0.8,
            memory_enabled: true,
            error: None,
        }
    }

    /// (enabled, number of tracks, current) of every Configure sent since the last call.
    fn configure_msgs(f: &Fixture) -> Vec<(bool, usize, Option<usize>)> {
        f.rx
            .try_iter()
            .filter_map(|m| match m {
                Msg::Configure { enabled, tracks, current } => Some((enabled, tracks.len(), current)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_view_works_without_a_worker_attached() {
        let mut f = fixture_with(&["a.m4a"], false);
        write_srt(&f, 0, TWO_CUES);
        f.sync.set_enabled(true);
        let update = f.sync.update(&snap(&f, Some(0), 1.5));
        assert_eq!(update.track.unwrap().status, Status::Ready);
        assert_eq!(update.active, Some(Some(0)));
        assert!(f.rx.try_recv().is_err());
    }

    #[test]
    fn disabled_reads_nothing_and_sends_nothing() {
        let mut f = fixture(&["a.m4a"]);
        write_srt(&f, 0, TWO_CUES);
        let update = f.sync.update(&snap(&f, Some(0), 1.5));
        assert!(update.track.is_none());
        assert_eq!(update.active, None);
        let (view, active) = f.sync.current();
        assert_eq!(view.status, Status::None);
        assert!(view.cues.is_empty());
        assert_eq!(active, None);
        assert!(configure_msgs(&f).is_empty(), "no worker traffic while the feature was never on");
    }

    #[test]
    fn enabling_sends_one_configure_and_loads_the_current_track() {
        let mut f = fixture(&["a.m4a", "b.m4a"]);
        write_srt(&f, 1, TWO_CUES);
        f.sync.set_enabled(true);
        let update = f.sync.update(&snap(&f, Some(1), 0.0));
        assert_eq!(configure_msgs(&f), vec![(true, 2, Some(1))]);
        let track = update.track.expect("the track's subtitles are emitted on first load");
        assert_eq!(track.status, Status::Ready);
        assert_eq!(track.cues.len(), 2);
        assert_eq!(update.active, None, "position 0.0 is before the first cue");
    }

    #[test]
    fn an_unchanged_snapshot_sends_nothing_more() {
        let mut f = fixture(&["a.m4a"]);
        f.sync.set_enabled(true);
        f.sync.update(&snap(&f, Some(0), 0.0));
        let _ = configure_msgs(&f);
        let again = f.sync.update(&snap(&f, Some(0), 0.2));
        assert!(configure_msgs(&f).is_empty());
        assert!(again.track.is_none());
        assert_eq!(again.active, None);
    }

    #[test]
    fn changing_track_reconfigures_the_worker_and_reloads() {
        let mut f = fixture(&["a.m4a", "b.m4a"]);
        write_srt(&f, 0, TWO_CUES);
        f.sync.set_enabled(true);
        f.sync.update(&snap(&f, Some(0), 0.0));
        let _ = configure_msgs(&f);
        let update = f.sync.update(&snap(&f, Some(1), 0.0));
        assert_eq!(configure_msgs(&f), vec![(true, 2, Some(1))]);
        assert_eq!(update.track.unwrap().status, Status::None);
    }

    #[test]
    fn a_changed_track_list_reconfigures_the_worker() {
        let mut f = fixture(&["a.m4a", "b.m4a"]);
        f.sync.set_enabled(true);
        f.sync.update(&snap(&f, Some(0), 0.0));
        let _ = configure_msgs(&f);
        f.tracks.pop();
        f.sync.update(&snap(&f, Some(0), 0.0));
        assert_eq!(configure_msgs(&f), vec![(true, 1, Some(0))]);
    }

    #[test]
    fn active_line_follows_the_position() {
        let mut f = fixture(&["a.m4a"]);
        write_srt(&f, 0, TWO_CUES);
        f.sync.set_enabled(true);
        f.sync.update(&snap(&f, Some(0), 0.0));
        assert_eq!(f.sync.update(&snap(&f, Some(0), 1.5)).active, Some(Some(0)));
        assert_eq!(f.sync.update(&snap(&f, Some(0), 1.8)).active, None, "same line: no event");
        assert_eq!(f.sync.update(&snap(&f, Some(0), 5.5)).active, Some(Some(1)));
        assert_eq!(f.sync.update(&snap(&f, Some(0), 20.0)).active, Some(None));
        assert_eq!(f.sync.current().1, None);
    }

    #[test]
    fn reload_picks_up_a_subtitle_that_appeared_and_reports_it_once() {
        let mut f = fixture(&["a.m4a"]);
        f.sync.set_enabled(true);
        f.sync.update(&snap(&f, Some(0), 0.0));
        assert_eq!(f.sync.current().0.status, Status::None);

        write_srt(&f, 0, TWO_CUES);
        f.sync.reload();
        let update = f.sync.update(&snap(&f, Some(0), 0.0));
        assert_eq!(update.track.unwrap().status, Status::Ready);

        f.sync.reload();
        assert!(
            f.sync.update(&snap(&f, Some(0), 0.0)).track.is_none(),
            "unchanged content is not emitted again"
        );
    }

    #[test]
    fn no_current_track_means_no_subtitles() {
        let mut f = fixture(&["a.m4a"]);
        f.sync.set_enabled(true);
        let update = f.sync.update(&snap(&f, None, 0.0));
        assert_eq!(configure_msgs(&f), vec![(true, 1, None)]);
        let track = update.track.unwrap();
        assert_eq!((track.path, track.status), (None, Status::None));
    }

    #[test]
    fn turning_it_off_cancels_the_worker_and_clears_the_view() {
        let mut f = fixture(&["a.m4a"]);
        write_srt(&f, 0, TWO_CUES);
        f.sync.set_enabled(true);
        f.sync.update(&snap(&f, Some(0), 1.5));
        let _ = configure_msgs(&f);

        f.sync.set_enabled(false);
        let update = f.sync.update(&snap(&f, Some(0), 1.5));
        assert_eq!(configure_msgs(&f), vec![(false, 1, Some(0))]);
        assert_eq!(update.track.unwrap().status, Status::None);
        assert_eq!(update.active, Some(None));
    }

    #[test]
    fn a_subtitle_file_that_vanished_is_dropped_on_reload() {
        let mut f = fixture(&["a.m4a"]);
        write_srt(&f, 0, TWO_CUES);
        f.sync.set_enabled(true);
        f.sync.update(&snap(&f, Some(0), 0.0));
        fs::remove_file(f.tracks[0].with_extension("srt")).unwrap();
        f.sync.reload();
        let update = f.sync.update(&snap(&f, Some(0), 0.0));
        assert_eq!(update.track.unwrap().status, Status::None);
    }
}
