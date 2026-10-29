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
}

/// Owns the output device, so it must live on the thread that created it.
pub struct RodioBackend {
    device: MixerDeviceSink,
    player: Option<Player>,
    volume: f32,
}

impl RodioBackend {
    pub fn new() -> Result<Self, String> {
        let device = DeviceSinkBuilder::open_default_sink().map_err(|e| e.to_string())?;
        Ok(Self {
            device,
            player: None,
            volume: 0.8,
        })
    }
}

impl AudioBackend for RodioBackend {
    fn load(&mut self, path: &Path) -> Result<Option<f64>, String> {
        self.stop();
        let file = File::open(path).map_err(|e| e.to_string())?;
        let decoder = Decoder::try_from(file).map_err(|e| e.to_string())?;
        let duration = decoder.total_duration().map(|d| d.as_secs_f64());

        let player = Player::connect_new(self.device.mixer());
        player.pause();
        player.set_volume(self.volume);
        player.append(decoder);
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
        self.player
            .as_ref()
            .map_or(0.0, |p| p.get_pos().as_secs_f64())
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
}
