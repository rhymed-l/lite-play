use crate::stretch::{TempoControl, TempoSource};
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};
use std::fs::File;
use std::path::Path;
use std::time::Duration;

pub trait AudioBackend {
    /// Loads a track in the paused state. Returns its duration in seconds when known.
    fn load(&mut self, path: &Path) -> Result<Option<f64>, String>;
    fn play(&mut self);
    fn pause(&mut self);
    fn stop(&mut self);
    fn seek(&mut self, secs: f64) -> Result<(), String>;
    fn position(&self) -> f64;
    fn is_finished(&self) -> bool;
    fn set_volume(&mut self, v: f32);
    /// Sets pitch-preserving playback tempo. Backends without tempo support may ignore it.
    fn set_speed(&mut self, _speed: f32) {}
}

/// Owns the output device, so it must live on the thread that created it.
pub struct RodioBackend {
    device: MixerDeviceSink,
    player: Option<Player>,
    volume: f32,
    /// Shared with the audio thread: the atomic tempo the DSP loop reads, and the
    /// original-timeline position it publishes from consumed output.
    control: TempoControl,
}

impl RodioBackend {
    pub fn new() -> Result<Self, String> {
        let device = DeviceSinkBuilder::open_default_sink().map_err(|e| e.to_string())?;
        Ok(Self {
            device,
            player: None,
            volume: 0.8,
            control: TempoControl::new(1.0),
        })
    }
}

impl AudioBackend for RodioBackend {
    fn load(&mut self, path: &Path) -> Result<Option<f64>, String> {
        self.stop();
        let file = File::open(path).map_err(|e| e.to_string())?;
        let decoder = Decoder::try_from(file).map_err(|e| e.to_string())?;
        let duration = decoder.total_duration().map(|d| d.as_secs_f64());

        // 1x bypasses the DSP, so the exact path stays untouched until a tempo is
        // chosen; TempoSource switches itself in when the control speed changes.
        let control = self.control.clone();
        control.publish(0.0);
        let source = TempoSource::new(decoder, control);
        let player = Player::connect_new(self.device.mixer());
        player.pause();
        player.set_volume(self.volume);
        player.append(source);
        self.player = Some(player);
        Ok(duration)
    }

    fn play(&mut self) {
        if let Some(p) = &self.player {
            p.play();
        }
    }

    fn pause(&mut self) {
        if let Some(p) = &self.player {
            p.pause();
        }
    }

    fn stop(&mut self) {
        if let Some(p) = self.player.take() {
            p.stop();
        }
    }

    fn seek(&mut self, secs: f64) -> Result<(), String> {
        let player = self.player.as_ref().ok_or("no track loaded")?;
        player
            .try_seek(Duration::from_secs_f64(secs.max(0.0)))
            .map_err(|e| e.to_string())
    }

    fn position(&self) -> f64 {
        // The DSP publishes the ORIGINAL-timeline position of consumed output;
        // Player::get_pos would report stretched time whenever a tempo is active.
        self.control.position()
    }

    fn is_finished(&self) -> bool {
        self.player.as_ref().is_some_and(|p| p.empty())
    }

    fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        if let Some(p) = &self.player {
            p.set_volume(self.volume);
        }
    }

    fn set_speed(&mut self, speed: f32) {
        self.control.set_speed(speed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn check_loads_and_seeks(name: &str) {
        let mut b = RodioBackend::new().expect("audio device");
        let dur = b.load(&fixture(name)).expect("load");
        if let Some(d) = dur {
            assert!((0.8..1.3).contains(&d), "{name}: duration {d}");
        }
        b.seek(0.5).expect("seek");
        let pos = b.position();
        assert!((0.4..0.6).contains(&pos), "{name}: position {pos}");
        assert!(!b.is_finished(), "{name}: paused track must not be finished");
    }

    #[test]
    #[ignore = "needs an audio output device"]
    fn loads_and_seeks_mp3() {
        check_loads_and_seeks("tone.mp3");
    }

    #[test]
    #[ignore = "needs an audio output device"]
    fn loads_and_seeks_m4a() {
        check_loads_and_seeks("tone.m4a");
    }

    #[test]
    #[ignore = "needs an audio output device"]
    fn loads_and_seeks_wav() {
        check_loads_and_seeks("tone.wav");
    }

    #[test]
    #[ignore = "needs an audio output device"]
    fn loads_and_seeks_alac_m4a() {
        check_loads_and_seeks("tone_alac.m4a");
    }

    #[test]
    #[ignore = "needs an audio output device"]
    fn corrupt_file_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.mp3");
        fs::write(&bad, [7u8, 99, 3, 200, 14, 1, 1, 0, 250, 66, 12, 5].repeat(200)).unwrap();
        let mut b = RodioBackend::new().expect("audio device");
        assert!(b.load(&bad).is_err());
    }

    #[test]
    #[ignore = "needs an audio output device"]
    fn missing_file_is_an_error() {
        let mut b = RodioBackend::new().expect("audio device");
        assert!(b.load(Path::new("Z:/definitely/not/here.mp3")).is_err());
    }

    #[test]
    #[ignore = "needs an audio output device"]
    fn nothing_loaded_is_not_finished() {
        let b = RodioBackend::new().expect("audio device");
        assert!(!b.is_finished());
    }

    #[test]
    #[ignore = "needs an audio output device"]
    fn plays_to_the_end_and_reports_finished() {
        let mut b = RodioBackend::new().expect("audio device");
        b.set_volume(0.0);
        b.load(&fixture("tone.wav")).unwrap();
        b.play();
        std::thread::sleep(std::time::Duration::from_millis(1600));
        assert!(b.is_finished());
    }

    #[test]
    #[ignore = "needs an audio output device"]
    fn tempo_changes_duration_but_position_stays_original() {
        let mut b = RodioBackend::new().expect("audio device");
        b.set_volume(0.0);
        b.load(&fixture("tone.wav")).unwrap();
        b.set_speed(2.0);
        // A 2x tempo consumes the track in about half its original time, while
        // the reported position keeps measuring the original timeline. Allow for
        // the callback buffer: the device drains lazily, so wait well past the
        // 2x-consumption point before expecting the track to be finished.
        b.play();
        std::thread::sleep(std::time::Duration::from_millis(600));
        let pos = b.position();
        assert!((0.7..1.1).contains(&pos), "position {pos} should follow the original timeline");
        // Half a 1-second tone at 2x is consumed in 0.5s, but the device's
        // callback buffer drains lazily, so "not finished yet" cannot be
        // asserted reliably at a fixed wall-clock point. The position above is
        // the original-timeline proof; only the eventual end is checked here.
        std::thread::sleep(std::time::Duration::from_millis(2500));
        assert!(b.is_finished());
    }
}
