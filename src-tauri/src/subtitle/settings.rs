use crate::memory::sibling_with_suffix;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

const MIN_FONT: u32 = 14;
const MAX_FONT: u32 = 96;
const DEFAULT_FG: &str = "#ffffff";
const DEFAULT_BG: &str = "#000000";

/// Physical pixels; x and y can be negative when a monitor sits left of or above the primary.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct DesktopPos {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct SubtitleSettings {
    pub enabled: bool,
    pub desktop_enabled: bool,
    pub desktop_locked: bool,
    pub font_size: u32,
    pub scheme: String,
    pub custom_fg: String,
    pub custom_bg: String,
    /// How hard transcription may work: "light", "balanced" or "full".
    pub power: String,
    pub desktop_pos: Option<DesktopPos>,
}

impl Default for SubtitleSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            desktop_enabled: false,
            desktop_locked: false,
            font_size: 28,
            scheme: "dark".to_string(),
            custom_fg: DEFAULT_FG.to_string(),
            custom_bg: DEFAULT_BG.to_string(),
            power: "light".to_string(),
            desktop_pos: None,
        }
    }
}

fn is_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

impl SubtitleSettings {
    pub fn normalized(mut self) -> Self {
        self.font_size = self.font_size.clamp(MIN_FONT, MAX_FONT);
        if !is_hex_color(&self.custom_fg) {
            self.custom_fg = DEFAULT_FG.to_string();
        }
        if !is_hex_color(&self.custom_bg) {
            self.custom_bg = DEFAULT_BG.to_string();
        }
        if !matches!(self.power.as_str(), "light" | "balanced" | "full") {
            self.power = "light".to_string();
        }
        self
    }

    pub fn load(path: &Path) -> SubtitleSettings {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(_) => {
                let _ = fs::copy(path, sibling_with_suffix(path, ".bak"));
                return Self::default();
            }
        };
        let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
        match serde_json::from_slice::<SubtitleSettings>(body) {
            Ok(s) => s.normalized(),
            Err(_) => {
                let _ = fs::write(sibling_with_suffix(path, ".bak"), &bytes);
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = sibling_with_suffix(path, ".tmp");
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        fs::write(&tmp, json)?;
        fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn defaults_are_all_off() {
        let s = SubtitleSettings::default();
        assert!(!s.enabled);
        assert!(!s.desktop_enabled);
        assert!(!s.desktop_locked);
        assert_eq!(s.font_size, 28);
        assert_eq!(s.scheme, "dark");
        assert_eq!(s.desktop_pos, None);
    }

    #[test]
    fn roundtrip_keeps_desktop_position() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subtitle-settings.json");
        let s = SubtitleSettings {
            enabled: true,
            desktop_pos: Some(DesktopPos { x: -1920, y: 1040, width: 900, height: 90 }),
            ..SubtitleSettings::default()
        };
        s.save(&path).unwrap();
        assert_eq!(SubtitleSettings::load(&path), s);
    }

    #[test]
    fn font_size_is_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        fs::write(&path, r#"{"font_size":5}"#).unwrap();
        assert_eq!(SubtitleSettings::load(&path).font_size, 14);
        fs::write(&path, r#"{"font_size":500}"#).unwrap();
        assert_eq!(SubtitleSettings::load(&path).font_size, 96);
    }

    #[test]
    fn bad_color_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        fs::write(&path, r##"{"custom_fg":"red","custom_bg":"#12345"}"##).unwrap();
        let s = SubtitleSettings::load(&path);
        assert_eq!(s.custom_fg, "#ffffff");
        assert_eq!(s.custom_bg, "#000000");
    }

    #[test]
    fn the_power_level_defaults_to_the_gentle_one() {
        assert_eq!(SubtitleSettings::default().power, "light");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        fs::write(&path, r#"{"font_size":30}"#).unwrap();
        assert_eq!(SubtitleSettings::load(&path).power, "light", "files from older versions have no level");
    }

    #[test]
    fn the_power_level_roundtrips_and_unknown_values_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        for good in ["light", "balanced", "full"] {
            fs::write(&path, format!(r#"{{"power":"{good}"}}"#)).unwrap();
            assert_eq!(SubtitleSettings::load(&path).power, good);
        }
        fs::write(&path, r#"{"power":"turbo"}"#).unwrap();
        assert_eq!(SubtitleSettings::load(&path).power, "light");
        fs::write(&path, r#"{"power":""}"#).unwrap();
        assert_eq!(SubtitleSettings::load(&path).power, "light");
    }

    #[test]
    fn good_color_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        fs::write(&path, r##"{"custom_fg":"#A1b2C3"}"##).unwrap();
        assert_eq!(SubtitleSettings::load(&path).custom_fg, "#A1b2C3");
    }

    #[test]
    fn corrupt_file_is_backed_up_and_defaults_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        fs::write(&path, "{not json").unwrap();
        assert_eq!(SubtitleSettings::load(&path), SubtitleSettings::default());
        assert_eq!(
            fs::read_to_string(dir.path().join("s.json.bak")).unwrap(),
            "{not json"
        );
    }

    #[test]
    fn missing_file_gives_defaults_without_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        assert_eq!(SubtitleSettings::load(&path), SubtitleSettings::default());
        assert!(!dir.path().join("s.json.bak").exists());
    }

    #[test]
    fn partial_json_fills_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        fs::write(&path, r#"{"font_size":40}"#).unwrap();
        let s = SubtitleSettings::load(&path);
        assert_eq!(s.font_size, 40);
        assert!(!s.enabled);
        assert_eq!(s.scheme, "dark");
    }

    #[test]
    fn utf8_bom_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(br#"{"font_size":33}"#);
        fs::write(&path, bytes).unwrap();
        assert_eq!(SubtitleSettings::load(&path).font_size, 33);
        assert!(!dir.path().join("s.json.bak").exists());
    }

    #[test]
    fn save_leaves_no_temp_files_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a").join("b");
        let path = sub.join("s.json");
        SubtitleSettings::default().save(&path).unwrap();
        let names: Vec<_> = fs::read_dir(&sub)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["s.json".to_string()]);
    }
}
