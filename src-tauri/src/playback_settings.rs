use serde::{Deserialize, Serialize};

pub const SPEEDS: [f32; 7] = [0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderPlaybackSettings {
    pub speed: f32,
    pub skip_intro: f64,
    pub skip_outro: f64,
}

impl Default for FolderPlaybackSettings {
    fn default() -> Self {
        Self {
            speed: 1.0,
            skip_intro: 0.0,
            skip_outro: 0.0,
        }
    }
}

impl FolderPlaybackSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !SPEEDS.contains(&self.speed) {
            return Err("不支持的播放倍速".into());
        }
        if [self.skip_intro, self.skip_outro]
            .into_iter()
            .any(|v| !v.is_finite() || v < 0.0 || v.fract() != 0.0 || v > u32::MAX as f64)
        {
            return Err("片头片尾秒数必须为非负整数（不超过 4294967295）".into());
        }
        Ok(())
    }

    pub fn region(&self, duration: Option<f64>) -> PlaybackRegion {
        let duration = duration.filter(|d| d.is_finite() && *d >= 0.0);
        if let Some(d) = duration {
            if (self.skip_intro > 0.0 || self.skip_outro > 0.0)
                && d - self.skip_intro - self.skip_outro < 1.0
            {
                return PlaybackRegion {
                    start: 0.0,
                    end: Some(d),
                    valid: false,
                };
            }
        }
        PlaybackRegion {
            start: self.skip_intro,
            end: duration.map(|d| d - self.skip_outro),
            valid: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PlaybackRegion {
    pub start: f64,
    pub end: Option<f64>,
    pub valid: bool,
}

impl PlaybackRegion {
    pub fn resume(&self, position: Option<f64>) -> f64 {
        position
            .filter(|p| p.is_finite() && *p >= self.start && self.end.is_none_or(|end| *p < end))
            .unwrap_or(self.start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_use_one_speed_and_no_skips_if_default() {
        // Arrange
        let settings = FolderPlaybackSettings::default();
        // Act
        let region = settings.region(Some(100.0));
        // Assert
        assert_eq!(settings.speed, 1.0);
        assert_eq!(region.start, 0.0);
        assert_eq!(region.end, Some(100.0));
    }

    #[test]
    fn should_restore_only_body_positions_if_skips_enabled() {
        // Arrange
        let settings = FolderPlaybackSettings {
            speed: 1.5,
            skip_intro: 10.0,
            skip_outro: 20.0,
        };
        let region = settings.region(Some(100.0));
        // Act
        let actual: Vec<f64> = [
            None,
            Some(5.0),
            Some(10.0),
            Some(50.0),
            Some(80.0),
            Some(f64::NAN),
        ]
        .map(|pos| region.resume(pos))
        .to_vec();
        // Assert
        assert_eq!(actual, vec![10.0, 10.0, 10.0, 50.0, 10.0, 10.0]);
    }

    #[test]
    fn should_disable_skips_if_body_is_too_short() {
        // Arrange
        let settings = FolderPlaybackSettings {
            speed: 1.0,
            skip_intro: 60.0,
            skip_outro: 40.0,
        };
        // Act
        let region = settings.region(Some(100.0));
        // Assert
        assert!(!region.valid);
        assert_eq!(region.start, 0.0);
        assert_eq!(region.end, Some(100.0));
    }

    #[test]
    fn should_keep_intro_if_duration_is_unknown() {
        // Arrange
        let settings = FolderPlaybackSettings {
            speed: 1.0,
            skip_intro: 15.0,
            skip_outro: 30.0,
        };
        // Act
        let region = settings.region(None);
        // Assert
        assert!(region.valid);
        assert_eq!(region.start, 15.0);
        assert_eq!(region.end, None);
    }

    #[test]
    fn should_reject_values_if_invalid_or_not_supported() {
        // Arrange
        let cases = [
            (0.0, 0.0, 0.0),
            (1.1, 0.0, 0.0),
            (f32::NAN, 0.0, 0.0),
            (1.0, -1.0, 0.0),
            (1.0, 0.5, 0.0),
            (1.0, 0.0, f64::INFINITY),
        ];
        // Act
        let invalid = cases.map(|(speed, skip_intro, skip_outro)| {
            FolderPlaybackSettings {
                speed,
                skip_intro,
                skip_outro,
            }
            .validate()
            .is_err()
        });
        // Assert
        assert!(invalid.into_iter().all(|v| v));
    }
}
