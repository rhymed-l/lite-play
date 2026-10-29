use crate::engine::Cmd;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

const QUIET_PERIOD: Duration = Duration::from_millis(500);

/// Watches one folder level and sends a single `Cmd::RefreshFolder` once changes have
/// stopped arriving for `QUIET_PERIOD`. Dropping the returned watcher stops the debounce thread.
pub fn watch(folder: &Path, engine: Sender<Cmd>) -> notify::Result<RecommendedWatcher> {
    let (ping, pings) = mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            if !matches!(event.kind, EventKind::Access(_)) {
                let _ = ping.send(());
            }
        }
    })?;
    watcher.watch(folder, RecursiveMode::NonRecursive)?;

    std::thread::Builder::new()
        .name("lite-play-watcher".into())
        .spawn(move || {
            while pings.recv().is_ok() {
                loop {
                    match pings.recv_timeout(QUIET_PERIOD) {
                        Ok(()) => continue,
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                if engine.send(Cmd::RefreshFolder).is_err() {
                    return;
                }
            }
        })
        .map_err(notify::Error::io)?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_burst_of_new_files_triggers_one_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let _watcher = watch(dir.path(), tx).unwrap();

        for i in 0..5 {
            fs::write(dir.path().join(format!("{i}.mp3")), b"x").unwrap();
        }

        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(3)),
            Ok(Cmd::RefreshFolder)
        ));
        assert!(
            rx.recv_timeout(Duration::from_millis(900)).is_err(),
            "burst must be debounced into a single refresh"
        );
    }

    #[test]
    fn watching_a_missing_folder_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel();
        assert!(watch(&dir.path().join("nope"), tx).is_err());
    }
}
