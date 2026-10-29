use std::cmp::Ordering;
use std::io;
use std::path::{Path, PathBuf};

const SUPPORTED_EXTENSIONS: [&str; 5] = ["mp3", "m4a", "flac", "ogg", "wav"];

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            SUPPORTED_EXTENSIONS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

enum Chunk {
    Digits(String),
    Text(String),
}

fn chunks(s: &str) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    for ch in s.chars() {
        let is_digit = ch.is_ascii_digit();
        match out.last_mut() {
            Some(Chunk::Digits(d)) if is_digit => d.push(ch),
            Some(Chunk::Text(t)) if !is_digit => t.extend(ch.to_lowercase()),
            _ if is_digit => out.push(Chunk::Digits(ch.to_string())),
            _ => out.push(Chunk::Text(ch.to_lowercase().collect())),
        }
    }
    out
}

fn cmp_digit_runs(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(cb.iter()) {
        let ord = match (x, y) {
            (Chunk::Digits(p), Chunk::Digits(q)) => cmp_digit_runs(p, q),
            (Chunk::Text(p), Chunk::Text(q)) => p.cmp(q),
            (Chunk::Digits(_), Chunk::Text(_)) => Ordering::Less,
            (Chunk::Text(_), Chunk::Digits(_)) => Ordering::Greater,
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    ca.len().cmp(&cb.len())
}

pub fn scan_folder(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() && is_supported(&entry.path()) {
            files.push(entry.path());
        }
    }
    files.sort_by(|a, b| {
        let name = |p: &PathBuf| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        natural_cmp(&name(a), &name(b))
    });
    Ok(files)
}

/// The first command-line argument (after the program name) that names an existing,
/// supported audio file. Relative paths are resolved against `cwd`.
pub fn first_audio_arg(args: &[String], cwd: &Path) -> Option<PathBuf> {
    args.iter().skip(1).find_map(|arg| {
        if arg.starts_with('-') {
            return None;
        }
        let path = PathBuf::from(arg);
        let path = if path.is_absolute() { path } else { cwd.join(path) };
        (path.is_file() && is_supported(&path)).then_some(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;
    use std::fs;
    use std::path::Path;

    fn names(paths: &[std::path::PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn natural_cmp_orders_numbers_by_value() {
        assert_eq!(natural_cmp("2.mp3", "10.mp3"), Ordering::Less);
        assert_eq!(natural_cmp("10.mp3", "2.mp3"), Ordering::Greater);
    }

    #[test]
    fn natural_cmp_ignores_case() {
        assert_eq!(natural_cmp("a.mp3", "B.mp3"), Ordering::Less);
    }

    #[test]
    fn natural_cmp_handles_huge_digit_runs_without_overflow() {
        let big = "99999999999999999999999999.mp3";
        let bigger = "100000000000000000000000000.mp3";
        assert_eq!(natural_cmp(big, bigger), Ordering::Less);
    }

    #[test]
    fn is_supported_checks_extension_case_insensitively() {
        assert!(is_supported(Path::new("A.M4A")));
        assert!(is_supported(Path::new("x.flac")));
        assert!(!is_supported(Path::new("a.txt")));
        assert!(!is_supported(Path::new("noext")));
    }

    #[test]
    fn scan_folder_is_single_level_filtered_and_naturally_sorted() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["10.mp3", "2.mp3", "SONG.MP3", "notes.txt", "中文 歌.m4a"] {
            fs::write(dir.path().join(f), b"x").unwrap();
        }
        fs::create_dir(dir.path().join("子 目录")).unwrap();
        fs::write(dir.path().join("子 目录").join("x.mp3"), b"x").unwrap();

        let found = names(&scan_folder(dir.path()).unwrap());

        let mut as_set = found.clone();
        as_set.sort();
        let mut expected = vec!["10.mp3", "2.mp3", "SONG.MP3", "中文 歌.m4a"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(as_set, expected);

        let pos = |n: &str| found.iter().position(|x| x == n).unwrap();
        assert!(pos("2.mp3") < pos("10.mp3"));
    }

    #[test]
    fn scan_folder_errors_on_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scan_folder(&dir.path().join("nope")).is_err());
    }

    #[test]
    fn first_audio_arg_skips_program_name_flags_and_unplayable_entries() {
        let dir = tempfile::tempdir().unwrap();
        let song = dir.path().join("歌 1.MP3");
        fs::write(&song, b"x").unwrap();
        fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        fs::create_dir(dir.path().join("folder.mp3")).unwrap();
        let args = vec![
            "lite-play.exe".to_string(),
            "--flag".to_string(),
            dir.path().join("notes.txt").to_string_lossy().into_owned(),
            dir.path().join("folder.mp3").to_string_lossy().into_owned(),
            dir.path().join("missing.mp3").to_string_lossy().into_owned(),
            song.to_string_lossy().into_owned(),
        ];
        assert_eq!(first_audio_arg(&args, Path::new(".")), Some(song));
    }

    #[test]
    fn first_audio_arg_resolves_relative_paths_against_cwd() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.mp3"), b"x").unwrap();
        let args = vec!["lite-play.exe".to_string(), "a.mp3".to_string()];
        assert_eq!(
            first_audio_arg(&args, dir.path()),
            Some(dir.path().join("a.mp3"))
        );
    }

    #[test]
    fn first_audio_arg_is_none_without_a_playable_file() {
        let args = vec!["lite-play.exe".to_string()];
        assert_eq!(first_audio_arg(&args, Path::new(".")), None);
    }
}
