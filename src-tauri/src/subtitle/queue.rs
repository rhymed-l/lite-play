use std::path::{Path, PathBuf};

/// The next track to transcribe: starting at `current` (0 when absent or out of range),
/// walking the list in order and wrapping once, the first one `skip` does not rule out.
pub fn next_to_transcribe(
    tracks: &[PathBuf],
    current: Option<usize>,
    skip: &dyn Fn(&Path) -> bool,
) -> Option<usize> {
    let len = tracks.len();
    let start = current.filter(|&c| c < len).unwrap_or(0);
    (0..len)
        .map(|offset| (start + offset) % len)
        .find(|&i| !skip(&tracks[i]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tracks(n: usize) -> Vec<PathBuf> {
        (0..n).map(|i| PathBuf::from(format!("t{i}.mp3"))).collect()
    }

    fn skip_names(names: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |p: &Path| names.iter().any(|n| p == Path::new(n))
    }

    #[test]
    fn starts_at_current() {
        assert_eq!(next_to_transcribe(&tracks(5), Some(2), &|_| false), Some(2));
    }

    #[test]
    fn wraps_around_to_the_start() {
        let skip = skip_names(&["t3.mp3", "t4.mp3"]);
        assert_eq!(next_to_transcribe(&tracks(5), Some(3), &skip), Some(0));
    }

    #[test]
    fn skips_marked_tracks() {
        let skip = skip_names(&["t1.mp3", "t2.mp3"]);
        assert_eq!(next_to_transcribe(&tracks(5), Some(1), &skip), Some(3));
    }

    #[test]
    fn all_skipped_returns_none() {
        assert_eq!(next_to_transcribe(&tracks(3), Some(1), &|_| true), None);
    }

    #[test]
    fn empty_list_returns_none() {
        assert_eq!(next_to_transcribe(&[], Some(0), &|_| false), None);
        assert_eq!(next_to_transcribe(&[], None, &|_| false), None);
    }

    #[test]
    fn none_or_out_of_range_current_starts_at_zero() {
        assert_eq!(next_to_transcribe(&tracks(3), None, &|_| false), Some(0));
        assert_eq!(next_to_transcribe(&tracks(3), Some(99), &|_| false), Some(0));
    }

    #[test]
    fn every_track_is_checked_exactly_once() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let skip = |_: &Path| {
            calls.set(calls.get() + 1);
            true
        };
        assert_eq!(next_to_transcribe(&tracks(7), Some(4), &skip), None);
        assert_eq!(calls.get(), 7);
    }
}
