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
    pub memory_enabled: bool,
    pub positions: BTreeMap<String, f64>,
    pub folders: Vec<String>,
    pub current_folder: Option<String>,
    pub last_track: Option<String>,
    pub mode: Mode,
    pub direction: Direction,
    pub volume: f32,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            version: 1,
            memory_enabled: true,
            positions: BTreeMap::new(),
            folders: Vec::new(),
            current_folder: None,
            last_track: None,
            mode: Mode::Sequential,
            direction: Direction::Forward,
            volume: 0.8,
        }
    }
}

pub(crate) fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
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

    pub fn position_for(&self, track: &str) -> Option<f64> {
        if !self.memory_enabled {
            return None;
        }
        self.positions.get(track).copied()
    }

    pub fn record_position(&mut self, track: &str, secs: f64) {
        if self.memory_enabled {
            self.positions.insert(track.to_string(), secs);
        }
    }

    pub fn clear_position(&mut self, track: &str) {
        if self.memory_enabled {
            self.positions.remove(track);
        }
    }

    pub fn add_folder(&mut self, folder: &str) {
        if !self.folders.iter().any(|f| f == folder) {
            self.folders.push(folder.to_string());
        }
        self.current_folder = Some(folder.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
    fn disabled_memory_neither_reads_nor_writes_but_keeps_history() {
        let mut m = Memory::default();
        m.record_position("a", 100.0);
        m.memory_enabled = false;

        assert_eq!(m.position_for("a"), None);
        m.record_position("a", 5.0);
        m.clear_position("a");
        assert_eq!(m.positions["a"], 100.0);

        m.memory_enabled = true;
        assert_eq!(m.position_for("a"), Some(100.0));
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
        assert!(m.memory_enabled);
        assert_eq!(m.mode, Mode::Sequential);
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
        assert_eq!(m.folders, vec!["/music".to_string(), "/podcasts".to_string()]);
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
