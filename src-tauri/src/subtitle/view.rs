use crate::subtitle::srt::{cue_at, read_srt, sidecar_path, Cue};
use serde::Serialize;
use std::path::Path;

/// How long a line stays on screen after its end time, so it does not flash by.
pub const LINGER_SECS: f64 = 2.0;

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// No subtitle file next to the audio (yet).
    None,
    Ready,
    /// A subtitle file exists but cannot be read (wrong encoding, not an SRT).
    Unreadable,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct TrackSubtitles {
    pub path: Option<String>,
    pub status: Status,
    pub cues: Vec<Cue>,
}

/// The subtitles of one track, read from the `.srt` next to it. Never writes anything.
pub fn load_for(audio: Option<&Path>) -> TrackSubtitles {
    let Some(audio) = audio else {
        return TrackSubtitles { path: None, status: Status::None, cues: Vec::new() };
    };
    let path = Some(audio.to_string_lossy().into_owned());
    let srt = sidecar_path(audio);
    if !srt.is_file() {
        return TrackSubtitles { path, status: Status::None, cues: Vec::new() };
    }
    match read_srt(&srt) {
        Ok(cues) => TrackSubtitles { path, status: Status::Ready, cues },
        Err(_) => TrackSubtitles { path, status: Status::Unreadable, cues: Vec::new() },
    }
}

/// The line to show at `pos`: the one being spoken, else the previous one for [`LINGER_SECS`].
pub fn active_index(cues: &[Cue], pos: f64) -> Option<usize> {
    if let Some(i) = cue_at(cues, pos) {
        return Some(i);
    }
    let after = cues.partition_point(|c| c.start <= pos);
    let prev = after.checked_sub(1)?;
    (pos < cues[prev].end + LINGER_SECS).then_some(prev)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitle::srt::Cue;
    use std::fs;

    fn cue(start: f64, end: f64) -> Cue {
        Cue { start, end, text: "x".to_string() }
    }

    #[test]
    fn no_audio_or_no_sidecar_is_none() {
        let none = load_for(None);
        assert_eq!(none, TrackSubtitles { path: None, status: Status::None, cues: vec![] });

        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("0002 001 血门.m4a");
        fs::write(&audio, b"audio").unwrap();
        let got = load_for(Some(&audio));
        assert_eq!(got.status, Status::None);
        assert!(got.cues.is_empty());
        assert_eq!(got.path.as_deref(), Some(audio.to_string_lossy().as_ref()));
    }

    #[test]
    fn readable_sidecar_is_ready_with_cues() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("0002 001 血门.m4a");
        fs::write(&audio, b"audio").unwrap();
        fs::write(dir.path().join("0002 001 血门.srt"), "1\n00:00:01,000 --> 00:00:02,000\n血门\n").unwrap();
        let got = load_for(Some(&audio));
        assert_eq!(got.status, Status::Ready);
        assert_eq!(got.cues.len(), 1);
        assert_eq!(got.cues[0].text, "血门");
    }

    #[test]
    fn gbk_sidecar_is_unreadable_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("a.m4a");
        fs::write(&audio, b"audio").unwrap();
        let srt = dir.path().join("a.srt");
        let mut gbk = b"1\n00:00:01,000 --> 00:00:02,000\n".to_vec();
        gbk.extend_from_slice(&[0xD6, 0xD0, 0xCE, 0xC4, b'\n']);
        fs::write(&srt, &gbk).unwrap();
        let got = load_for(Some(&audio));
        assert_eq!(got.status, Status::Unreadable);
        assert!(got.cues.is_empty());
        assert_eq!(fs::read(&srt).unwrap(), gbk, "the user's file is never rewritten");
    }

    #[test]
    fn status_serializes_as_a_lowercase_word() {
        assert_eq!(serde_json::to_string(&Status::None).unwrap(), "\"none\"");
        assert_eq!(serde_json::to_string(&Status::Ready).unwrap(), "\"ready\"");
        assert_eq!(serde_json::to_string(&Status::Unreadable).unwrap(), "\"unreadable\"");
    }

    #[test]
    fn active_index_inside_a_cue() {
        let cues = [cue(1.0, 2.0), cue(5.0, 6.0)];
        assert_eq!(active_index(&cues, 1.5), Some(0));
        assert_eq!(active_index(&cues, 5.0), Some(1));
    }

    #[test]
    fn lingers_two_seconds_after_end() {
        let cues = [cue(1.0, 2.0)];
        assert_eq!(active_index(&cues, 3.9), Some(0));
        assert_eq!(active_index(&cues, 4.1), None);
    }

    #[test]
    fn before_first_cue_is_none() {
        assert_eq!(active_index(&[cue(1.0, 2.0)], 0.5), None);
    }

    #[test]
    fn next_cue_wins_over_lingering_previous() {
        let cues = [cue(1.0, 2.0), cue(3.0, 4.0)];
        assert_eq!(active_index(&cues, 2.5), Some(0), "still lingering");
        assert_eq!(active_index(&cues, 3.0), Some(1));
    }

    #[test]
    fn empty_cues_is_none() {
        assert_eq!(active_index(&[], 1.0), None);
    }
}
