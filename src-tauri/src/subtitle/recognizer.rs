use crate::subtitle::srt::Cue;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const EXE_REL: &str = "runtime/sherpa-onnx-vad-with-offline-asr.exe";

const FILLER_CHARS: &str = "嗯啊呃哦唉哎呀哈";

/// `8.870 -- 9.964: 第一集` -> a cue. Log lines and anything else are not results.
fn parse_line(line: &str) -> Option<Cue> {
    let (range, text) = line.split_once(": ")?;
    let (a, b) = range.split_once(" -- ")?;
    let start: f64 = a.trim().parse().ok()?;
    let end: f64 = b.trim().parse().ok()?;
    if !start.is_finite() || !end.is_finite() {
        return None;
    }
    Some(Cue { start, end, text: text.trim().to_string() })
}

pub fn parse_output(stdout: &str) -> Vec<Cue> {
    stdout.lines().filter_map(|l| parse_line(l.trim_end())).collect()
}

/// True when nothing is left after dropping whitespace, punctuation and filler syllables.
pub fn is_filler(text: &str) -> bool {
    text.chars()
        .filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation() && !is_cjk_punctuation(*c))
        .all(|c| FILLER_CHARS.contains(c))
}

fn is_cjk_punctuation(c: char) -> bool {
    matches!(c,
        '\u{3000}'..='\u{303f}' | '\u{ff00}'..='\u{ff0f}' | '\u{ff1a}'..='\u{ff20}'
        | '\u{ff3b}'..='\u{ff40}' | '\u{ff5b}'..='\u{ff65}' | '\u{2010}'..='\u{2027}')
}

pub fn clean(cues: Vec<Cue>) -> Vec<Cue> {
    let mut out: Vec<Cue> = cues
        .into_iter()
        .filter(|c| c.end > c.start && !is_filler(&c.text))
        .collect();
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// Every path is relative to the add-on directory and plain ASCII: the program cannot
/// open files under a non-ASCII path (a Chinese Windows user name, for one).
pub fn build_args(wav_name: &str, threads: u32) -> Vec<String> {
    let mut args: Vec<String> = [
        "--silero-vad-model=silero_vad.onnx",
        "--silero-vad-min-silence-duration=0.25",
        "--silero-vad-max-speech-duration=8",
        "--sense-voice-model=model/model.int8.onnx",
        "--sense-voice-use-itn=true",
        "--sense-voice-language=zh",
        "--tokens=model/tokens.txt",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(format!("--num-threads={}", threads.max(1)));
    args.push(format!("work/{wav_name}"));
    args
}

/// Name of the temporary wav of job number `slot`. Every job running at the same time has its
/// own, so concurrent jobs never read or overwrite each other's audio. Plain ASCII on purpose.
pub fn work_file_name(slot: usize) -> String {
    format!("job-{slot}.wav")
}

fn last_nonempty_line(text: &str) -> String {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("recognizer failed")
        .to_string()
}

/// Runs the recognizer on `addon_dir/work/<wav_name>` with `threads` threads. Polls `cancel`
/// every 100 ms.
pub fn recognize(
    addon_dir: &Path,
    wav_name: &str,
    threads: u32,
    cancel: &AtomicBool,
) -> Result<Vec<Cue>, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".to_string());
    }
    let mut cmd = Command::new(addon_dir.join(EXE_REL));
    cmd.args(build_args(wav_name, threads))
        .current_dir(addon_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("无法启动识别程序：{e}"))?;

    let read_all = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    };
    let out_reader = read_all(Box::new(child.stdout.take().expect("piped stdout")));
    let err_reader = read_all(Box::new(child.stderr.take().expect("piped stderr")));

    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cancelled".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    if !status.success() {
        return Err(last_nonempty_line(&String::from_utf8_lossy(&stderr)));
    }
    Ok(clean(parse_output(&String::from_utf8_lossy(&stdout))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitle::srt::Cue;
    use std::sync::atomic::AtomicBool;

    fn cue(start: f64, end: f64, text: &str) -> Cue {
        Cue { start, end, text: text.to_string() }
    }

    #[test]
    fn parses_result_lines_and_ignores_logs() {
        let out = "Creating recognizer ...\nStarted\n8.870 -- 9.964: 第一集\n10.918 -- 12.166: 血门\nElapsed seconds: 3.2\n";
        assert_eq!(
            parse_output(out),
            vec![cue(8.87, 9.964, "第一集"), cue(10.918, 12.166, "血门")]
        );
    }

    #[test]
    fn parses_crlf() {
        let out = "Started\r\n0.742 -- 5.568: 开放时间早上九点至下午五点\r\n";
        assert_eq!(parse_output(out), vec![cue(0.742, 5.568, "开放时间早上九点至下午五点")]);
    }

    #[test]
    fn text_containing_colons_is_kept() {
        let out = "1.0 -- 2.0: 时间：三点半\n3.0 -- 4.0: note: keep this\n";
        let cues = parse_output(out);
        assert_eq!(cues[0].text, "时间：三点半");
        assert_eq!(cues[1].text, "note: keep this");
    }

    #[test]
    fn lines_that_only_look_like_results_are_ignored() {
        let out = "inf -- 2.0: x\n1.0 -- nan: y\nabc -- 1.0: z\n1.0 - 2.0: w\n";
        assert!(parse_output(out).is_empty());
    }

    #[test]
    fn fillers_are_detected() {
        for s in ["嗯", "嗯。", "啊，嗯", "。", "  ", "", "哈哈！"] {
            assert!(is_filler(s), "{s:?} should be filler");
        }
        for s in ["嗯，好", "血门", "第1集", "ok"] {
            assert!(!is_filler(s), "{s:?} should not be filler");
        }
    }

    #[test]
    fn clean_drops_fillers_and_bad_ranges_and_sorts() {
        let cues = vec![
            cue(20.0, 21.0, "后面的"),
            cue(1.0, 2.0, "嗯。"),
            cue(5.0, 5.0, "零长度"),
            cue(6.0, 4.0, "倒着的"),
            cue(3.0, 4.0, "前面的"),
        ];
        assert_eq!(
            clean(cues),
            vec![cue(3.0, 4.0, "前面的"), cue(20.0, 21.0, "后面的")]
        );
    }

    #[test]
    fn each_job_gets_its_own_wav_and_thread_count() {
        let a = build_args("job-0.wav", 2);
        let b = build_args("job-7.wav", 4);
        assert_eq!(a.last().unwrap(), "work/job-0.wav");
        assert_eq!(b.last().unwrap(), "work/job-7.wav");
        assert!(a.iter().any(|x| x == "--num-threads=2"));
        assert!(b.iter().any(|x| x == "--num-threads=4"));
        assert!(!a.iter().any(|x| x == "--num-threads=4"));
        // everything else is identical between jobs
        let strip = |v: &Vec<String>| v.iter().filter(|x| !x.starts_with("--num-threads") && !x.starts_with("work/")).cloned().collect::<Vec<_>>();
        assert_eq!(strip(&a), strip(&b));
    }

    #[test]
    fn a_zero_thread_count_is_raised_to_one() {
        assert!(build_args("job-0.wav", 0).iter().any(|x| x == "--num-threads=1"));
    }

    #[test]
    fn work_file_names_are_plain_ascii_and_distinct() {
        let names: Vec<String> = (0..16).map(work_file_name).collect();
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), 16);
        assert!(names.iter().all(|n| n.is_ascii() && n.ends_with(".wav") && !n.contains('/') && !n.contains('\\')));
        assert_eq!(work_file_name(0), "job-0.wav");
    }

    #[test]
    fn args_are_relative_ascii_and_fixed() {
        let args = build_args("job-0.wav", 4);
        for a in &args {
            assert!(a.is_ascii(), "{a:?} must be ASCII");
            if let Some((_, value)) = a.split_once('=') {
                assert!(!value.contains(':') && !value.contains('\\'), "{a:?}");
                assert!(!value.starts_with('/'), "{a:?}");
            }
        }
        let last = args.last().unwrap();
        assert_eq!(last, "work/job-0.wav");
        for needed in [
            "--sense-voice-use-itn=true",
            "--sense-voice-language=zh",
            "--num-threads=4",
            "--silero-vad-max-speech-duration=8",
            "--silero-vad-min-silence-duration=0.25",
            "--silero-vad-model=silero_vad.onnx",
            "--sense-voice-model=model/model.int8.onnx",
            "--tokens=model/tokens.txt",
        ] {
            assert!(args.iter().any(|a| a == needed), "missing {needed}");
        }
    }

    #[test]
    fn cancel_before_start_returns_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(true);
        let err = recognize(dir.path(), "job-0.wav", 2, &cancel).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
    }

    #[test]
    fn missing_program_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        assert!(recognize(dir.path(), "job-0.wav", 2, &cancel).is_err());
    }

    #[test]
    #[ignore = "needs the real recognizer; set LITE_PLAY_ADDON_DIR"]
    fn real_recognizer_under_non_ascii_dir() {
        let dir = std::env::var("LITE_PLAY_ADDON_DIR").expect("LITE_PLAY_ADDON_DIR");
        let cancel = AtomicBool::new(false);
        let cues = recognize(std::path::Path::new(&dir), "job.wav", 4, &cancel).expect("recognize");
        assert!(!cues.is_empty());
        let all: String = cues.iter().map(|c| c.text.as_str()).collect();
        assert!(all.contains("开放"), "got: {all}");
    }
}
