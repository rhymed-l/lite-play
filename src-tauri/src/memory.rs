use crate::navigator::{Direction, Mode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Memory {
    pub version: u32,
    pub positions: BTreeMap<String, f64>,
    pub folders: Vec<String>,
    pub current_folder: Option<String>,
    pub last_track: Option<String>,
    /// Last track played in each folder, so switching back resumes that folder's book.
    pub folder_last_track: BTreeMap<String, String>,
    pub mode: Mode,
    pub direction: Direction,
    pub volume: f32,
    pub folder_settings: BTreeMap<String, crate::playback_settings::FolderPlaybackSettings>,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            version: 1,
            positions: BTreeMap::new(),
            folders: Vec::new(),
            current_folder: None,
            last_track: None,
            folder_last_track: BTreeMap::new(),
            mode: Mode::Sequential,
            direction: Direction::Forward,
            volume: 0.8,
            folder_settings: BTreeMap::new(),
        }
    }
}

pub(crate) fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

impl Memory {
    pub fn load(path: &Path) -> Memory {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Memory::default(),
            Err(_) => {
                let _ = fs::copy(path, sibling_with_suffix(path, ".bak"));
                return Memory::default();
            }
        };
        let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
        match serde_json::from_slice(body) {
            Ok(m) => m,
            Err(_) => {
                let _ = fs::write(sibling_with_suffix(path, ".bak"), &bytes);
                Memory::default()
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

    /// Progress memory is always on: the user made it permanent and it is no longer a setting.
    pub fn position_for(&self, track: &str) -> Option<f64> {
        self.positions.get(track).copied()
    }

    pub fn record_position(&mut self, track: &str, secs: f64) {
        self.positions.insert(track.to_string(), secs);
    }

    pub fn clear_position(&mut self, track: &str) {
        self.positions.remove(track);
    }

    pub fn add_folder(&mut self, folder: &str) {
        if !self.folders.iter().any(|f| f == folder) {
            self.folders.push(folder.to_string());
        }
        self.current_folder = Some(folder.to_string());
    }

    /// Remembers the track each folder was last playing, so switching back resumes it.
    pub fn note_last_track(&mut self, folder: &str, track: &str) {
        self.folder_last_track
            .insert(folder.to_string(), track.to_string());
        self.last_track = Some(track.to_string());
    }

    pub fn last_track_for(&self, folder: &str) -> Option<&str> {
        self.folder_last_track.get(folder).map(String::as_str)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn should_keep_folder_settings_if_old_json_has_a_memory_switch() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.json");
        let mut memory = Memory::default();
        let settings = crate::playback_settings::FolderPlaybackSettings {
            speed: 1.5,
            skip_intro: 20.0,
            skip_outro: 30.0,
        };
        memory.folder_settings.insert("book A".into(), settings);
        memory
            .folder_settings
            .insert("book B".into(), Default::default());
        // Act
        memory.save(&path).unwrap();
        let loaded = Memory::load(&path);
        // Assert
        assert_eq!(loaded.folder_settings["book A"], settings);
        assert_eq!(loaded.folder_settings["book B"].speed, 1.0);
    }

    #[test]
    fn should_remember_last_track_if_folder_is_switched_and_restored() {
        // Arrange
        let mut m = Memory::default();
        m.add_folder(r"D:\书A");
        m.add_folder(r"D:\书B");
        // Act
        m.note_last_track(r"D:\书B", r"D:\书B\05.mp3");
        // Assert
        assert_eq!(
            m.last_track_for(r"D:\书B").map(str::to_string),
            Some(r"D:\书B\05.mp3".into())
        );
        assert_eq!(m.last_track_for(r"D:\书A"), None);
    }

    #[test]
    fn should_default_folder_settings_if_old_json_is_loaded() {
        // Arrange
        let old = r#"{"volume":0.5,"positions":{"a.mp3":42.0}}"#;
        // Act
        let loaded: Memory = serde_json::from_str(old).unwrap();
        // Assert
        assert!(loaded.folder_settings.is_empty());
        assert_eq!(loaded.positions["a.mp3"], 42.0);
    }

    #[test]
    fn roundtrip_keeps_chinese_and_spaced_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.json");
        let mut m = Memory::default();
        m.record_position(r"D:\音乐 库\歌 1.mp3", 211.5);
        m.add_folder(r"D:\音乐 库");
        m.mode = Mode::Shuffle;
        m.direction = Direction::Reverse;
        m.save(&path).unwrap();

        let loaded = Memory::load(&path);
        assert_eq!(loaded, m);
        assert_eq!(loaded.position_for(r"D:\音乐 库\歌 1.mp3"), Some(211.5));
    }

    #[test]
    fn positions_are_always_recorded_and_cleared() {
        let mut m = Memory::default();
        m.record_position("a", 100.0);
        assert_eq!(m.position_for("a"), Some(100.0));
        m.clear_position("a");
        assert_eq!(m.position_for("a"), None);
    }

    #[test]
    fn corrupt_file_is_backed_up_and_defaults_returned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.json");
        fs::write(&path, "{not json").unwrap();

        let m = Memory::load(&path);
        assert_eq!(m, Memory::default());
        assert_eq!(
            fs::read_to_string(dir.path().join("memory.json.bak")).unwrap(),
            "{not json"
        );
    }

    #[test]
    fn missing_file_gives_defaults_without_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.json");
        assert_eq!(Memory::load(&path), Memory::default());
        assert!(!dir.path().join("memory.json.bak").exists());
    }

    #[test]
    fn partial_json_fills_remaining_fields_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.json");
        fs::write(&path, r#"{"volume":0.5}"#).unwrap();

        let m = Memory::load(&path);
        assert_eq!(m.volume, 0.5);
        assert_eq!(m.mode, Mode::Sequential);
        assert!(m.folder_last_track.is_empty());
    }

    #[test]
    fn save_leaves_no_temp_files_and_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("nested").join("deeper");
        let path = sub.join("memory.json");
        Memory::default().save(&path).unwrap();

        let names: Vec<_> = fs::read_dir(&sub)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["memory.json".to_string()]);
    }

    #[test]
    fn add_folder_is_idempotent_and_becomes_current() {
        let mut m = Memory::default();
        m.add_folder("/music");
        m.add_folder("/podcasts");
        m.add_folder("/music");
        assert_eq!(
            m.folders,
            vec!["/music".to_string(), "/podcasts".to_string()]
        );
        assert_eq!(m.current_folder.as_deref(), Some("/music"));
    }

    #[test]
    fn utf8_bom_prefix_from_windows_editors_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.json");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(br#"{"volume":0.25}"#);
        fs::write(&path, bytes).unwrap();

        let m = Memory::load(&path);

        assert_eq!(m.volume, 0.25);
        assert!(!dir.path().join("memory.json.bak").exists());
    }

    #[test]
    fn file_that_is_not_valid_utf8_is_backed_up_before_defaults_are_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.json");
        let bytes: Vec<u8> = vec![0xFF, 0xFE, 0x00, 0x80, b'{'];
        fs::write(&path, &bytes).unwrap();

        let m = Memory::load(&path);

        assert_eq!(m, Memory::default());
        assert_eq!(fs::read(dir.path().join("memory.json.bak")).unwrap(), bytes);
    }
}
