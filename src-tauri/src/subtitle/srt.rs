use crate::memory::sibling_with_suffix;
use serde::Serialize;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One subtitle line; times are seconds from the start of the audio.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// `book/01.m4a` -> `book/01.srt`.
pub fn sidecar_path(audio: &Path) -> PathBuf {
    audio.with_extension("srt")
}

fn fmt_time(secs: f64) -> String {
    let total_ms = (secs.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        total_ms / 3_600_000,
        total_ms / 60_000 % 60,
        total_ms / 1000 % 60,
        total_ms % 1000
    )
}

pub fn format_srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, c) in cues.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n",
            i + 1,
            fmt_time(c.start),
            fmt_time(c.end),
            c.text
        ));
    }
    out
}

fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim();
    let (hms, ms) = s.split_once([',', '.'])?;
    let mut parts = hms.split(':');
    let h: u64 = parts.next()?.trim().parse().ok()?;
    let m: u64 = parts.next()?.trim().parse().ok()?;
    let sec: u64 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() || m > 59 || sec > 59 {
        return None;
    }
    let ms_digits = ms.trim();
    if ms_digits.is_empty() || ms_digits.len() > 3 || !ms_digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let ms_val: u64 = format!("{:0<3}", ms_digits).parse().ok()?;
    Some((h * 3600 + m * 60 + sec) as f64 + ms_val as f64 / 1000.0)
}

fn parse_time_line(line: &str) -> Option<(f64, f64)> {
    let (a, b) = line.split_once("-->")?;
    let start = parse_time(a)?;
    let end = parse_time(b.split_whitespace().next()?)?;
    Some((start, end))
}

pub fn parse_srt(text: &str) -> Result<Vec<Cue>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut cues = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    let flush = |block: &mut Vec<&str>, cues: &mut Vec<Cue>| {
        if let Some(pos) = block.iter().position(|l| l.contains("-->")) {
            if let Some((start, end)) = parse_time_line(block[pos]) {
                let body = block[pos + 1..].join("\n");
                if !body.trim().is_empty() {
                    cues.push(Cue { start, end, text: body });
                }
            }
        }
        block.clear();
    };
    for line in normalized.split('\n') {
        if line.trim().is_empty() {
            flush(&mut block, &mut cues);
        } else {
            block.push(line);
        }
    }
    flush(&mut block, &mut cues);
    if cues.is_empty() {
        return Err("no valid subtitle blocks".to_string());
    }
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    Ok(cues)
}

pub fn read_srt(path: &Path) -> Result<Vec<Cue>, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let text = String::from_utf8(bytes).map_err(|_| "not valid UTF-8".to_string())?;
    parse_srt(&text)
}

pub fn write_srt_atomic(path: &Path, cues: &[Cue]) -> io::Result<()> {
    let tmp = sibling_with_suffix(path, ".tmp");
    fs::write(&tmp, format_srt(cues))?;
    fs::rename(&tmp, path)
}

/// Index of the cue with `start <= pos < end`. `cues` must be sorted by start.
pub fn cue_at(cues: &[Cue], pos: f64) -> Option<usize> {
    let after = cues.partition_point(|c| c.start <= pos);
    let idx = after.checked_sub(1)?;
    (pos < cues[idx].end).then_some(idx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn cue(start: f64, end: f64, text: &str) -> Cue {
        Cue { start, end, text: text.to_string() }
    }

    #[test]
    fn sidecar_replaces_only_the_last_extension() {
        assert_eq!(
            sidecar_path(Path::new(r"D:\书\0002 001 血门.m4a")),
            Path::new(r"D:\书\0002 001 血门.srt")
        );
        assert_eq!(sidecar_path(Path::new("a.b.mp3")), Path::new("a.b.srt"));
        assert_eq!(
            sidecar_path(Path::new("【无人区别墅】女主人.m4a")),
            Path::new("【无人区别墅】女主人.srt")
        );
    }

    #[test]
    fn format_then_parse_roundtrips() {
        let cues = vec![cue(8.87, 9.964, "第一集"), cue(3661.5, 3665.25, "过了一小时")];
        let text = format_srt(&cues);
        assert!(text.starts_with("1\n00:00:08,870 --> 00:00:09,964\n第一集\n\n2\n"));
        assert!(text.contains("01:01:01,500 --> 01:01:05,250"));
        let back = parse_srt(&text).unwrap();
        assert_eq!(back.len(), 2);
        for (a, b) in cues.iter().zip(back.iter()) {
            assert!((a.start - b.start).abs() < 0.001);
            assert!((a.end - b.end).abs() < 0.001);
            assert_eq!(a.text, b.text);
        }
    }

    #[test]
    fn parse_accepts_bom_crlf_dot_millis_and_missing_index() {
        let text = "\u{feff}1\r\n00:00:01,000 --> 00:00:02,000\r\n你好\r\n\r\n00:00:03.500 --> 00:00:04.000\r\n再见\r\n";
        let cues = parse_srt(text).unwrap();
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0], cue(1.0, 2.0, "你好"));
        assert!((cues[1].start - 3.5).abs() < 0.001);
        assert_eq!(cues[1].text, "再见");
    }

    #[test]
    fn parse_joins_multi_line_text_and_sorts_by_start() {
        let text = "2\n00:00:05,000 --> 00:00:06,000\n后面\n\n1\n00:00:01,000 --> 00:00:02,000\n第一行\n第二行\n";
        let cues = parse_srt(text).unwrap();
        assert_eq!(cues[0].text, "第一行\n第二行");
        assert_eq!(cues[1].text, "后面");
    }

    #[test]
    fn parse_skips_bad_blocks_but_fails_when_none_valid() {
        let mixed = "1\nnot a time line\n坏的\n\n2\n00:00:01,000 --> 00:00:02,000\n好的\n";
        assert_eq!(parse_srt(mixed).unwrap().len(), 1);
        assert!(parse_srt("1\nnot a time line\n坏的\n").is_err());
        assert!(parse_srt("").is_err());
    }

    #[test]
    fn read_srt_rejects_non_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gbk.srt");
        let mut bytes = b"1\n00:00:01,000 --> 00:00:02,000\n".to_vec();
        bytes.extend_from_slice(&[0xD6, 0xD0, 0xCE, 0xC4]);
        bytes.extend_from_slice(b"\n");
        fs::write(&path, bytes).unwrap();
        assert!(read_srt(&path).is_err());
    }

    #[test]
    fn read_srt_reads_a_utf8_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("0002 001 血门.srt");
        fs::write(&path, "1\n00:00:01,000 --> 00:00:02,000\n血门\n").unwrap();
        assert_eq!(read_srt(&path).unwrap(), vec![cue(1.0, 2.0, "血门")]);
    }

    #[test]
    fn write_is_atomic_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.srt");
        write_srt_atomic(&path, &[cue(1.0, 2.0, "x")]).unwrap();
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.srt".to_string()]);
        assert_eq!(read_srt(&path).unwrap().len(), 1);
    }

    #[test]
    fn cue_at_boundaries() {
        let cues = vec![cue(1.0, 2.0, "a"), cue(2.0, 3.0, "b")];
        assert_eq!(cue_at(&cues, 0.5), None);
        assert_eq!(cue_at(&cues, 1.0), Some(0));
        assert_eq!(cue_at(&cues, 1.999), Some(0));
        assert_eq!(cue_at(&cues, 2.0), Some(1));
        assert_eq!(cue_at(&cues, 3.0), None);
        assert_eq!(cue_at(&[], 1.0), None);
    }

    #[test]
    fn cue_at_returns_none_in_a_gap_between_cues() {
        let cues = vec![cue(1.0, 2.0, "a"), cue(5.0, 6.0, "b")];
        assert_eq!(cue_at(&cues, 3.0), None);
        assert_eq!(cue_at(&cues, 5.5), Some(1));
    }
}
