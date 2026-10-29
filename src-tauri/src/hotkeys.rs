use crate::engine::{Cmd, EngineHandle};
use crate::gesture::{GestureAction, GestureDetector, Key};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

const HOLD_THRESHOLD_MS: u64 = 400;
const SEEK_STEP_MS: u64 = 200;
const SEEK_SECS: f64 = 5.0;
const TICK: Duration = Duration::from_millis(50);

/// The three fixed global shortcuts, in a stable order.
pub fn bindings() -> Vec<(Shortcut, Key)> {
    let mods = Some(Modifiers::CONTROL | Modifiers::ALT);
    vec![
        (Shortcut::new(mods, Code::Space), Key::PlayPause),
        (Shortcut::new(mods, Code::ArrowRight), Key::Next),
        (Shortcut::new(mods, Code::ArrowLeft), Key::Prev),
    ]
}

pub fn label(key: Key) -> &'static str {
    match key {
        Key::PlayPause => "Ctrl+Alt+空格",
        Key::Next => "Ctrl+Alt+→",
        Key::Prev => "Ctrl+Alt+←",
    }
}

pub fn action_to_cmd(action: GestureAction) -> Cmd {
    match action {
        GestureAction::Tap(Key::PlayPause) => Cmd::PlayPause,
        GestureAction::Tap(Key::Next) => Cmd::Next,
        GestureAction::Tap(Key::Prev) => Cmd::Prev,
        GestureAction::Seek(Key::Next) => Cmd::SeekBy(SEEK_SECS),
        GestureAction::Seek(Key::Prev) => Cmd::SeekBy(-SEEK_SECS),
        GestureAction::Seek(Key::PlayPause) => {
            unreachable!("the detector never emits Seek for play/pause")
        }
    }
}

fn dispatch(engine: &EngineHandle, actions: Vec<GestureAction>) {
    for action in actions {
        engine.send(action_to_cmd(action));
    }
}

/// Registers the global shortcuts. Combinations another program already owns are reported
/// to the UI through the engine instead of aborting startup.
pub fn setup(app: &AppHandle, engine: EngineHandle) {
    // Windows reports a real key-up (`Released`) for hotkeys and never repeats `Pressed`
    // while held, so no stale-key inference is needed (the third argument is `None`).
    let detector = Arc::new(Mutex::new(GestureDetector::new(
        HOLD_THRESHOLD_MS,
        SEEK_STEP_MS,
        None,
    )));
    let started = Instant::now();
    let mut failed: Vec<&'static str> = Vec::new();

    for (shortcut, key) in bindings() {
        let detector = Arc::clone(&detector);
        let engine = engine.clone();
        let result = app
            .global_shortcut()
            .on_shortcut(shortcut, move |_app, _shortcut, event| {
                let now = started.elapsed().as_millis() as u64;
                let actions = {
                    let Ok(mut d) = detector.lock() else {
                        return;
                    };
                    match event.state() {
                        ShortcutState::Pressed => d.press(key, now),
                        ShortcutState::Released => d.release(key, now),
                    }
                };
                dispatch(&engine, actions);
            });
        if result.is_err() {
            failed.push(label(key));
        }
    }

    if !failed.is_empty() {
        engine.send(Cmd::ReportError(format!(
            "快捷键 {} 已被其他程序占用",
            failed.join("、")
        )));
    }

    let ticker_engine = engine;
    let _ = std::thread::Builder::new()
        .name("lite-play-hotkeys".into())
        .spawn(move || loop {
            std::thread::sleep(TICK);
            let now = started.elapsed().as_millis() as u64;
            let actions = match detector.lock() {
                Ok(mut d) => d.tick(now),
                Err(_) => return,
            };
            dispatch(&ticker_engine, actions);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gesture::GestureAction::{Seek, Tap};

    #[test]
    fn taps_map_to_play_pause_next_and_prev() {
        assert!(matches!(action_to_cmd(Tap(Key::PlayPause)), Cmd::PlayPause));
        assert!(matches!(action_to_cmd(Tap(Key::Next)), Cmd::Next));
        assert!(matches!(action_to_cmd(Tap(Key::Prev)), Cmd::Prev));
    }

    #[test]
    fn holds_seek_five_seconds_forward_or_back() {
        assert!(matches!(action_to_cmd(Seek(Key::Next)), Cmd::SeekBy(d) if d == 5.0));
        assert!(matches!(action_to_cmd(Seek(Key::Prev)), Cmd::SeekBy(d) if d == -5.0));
    }

    #[test]
    #[should_panic]
    fn seeking_with_play_pause_is_a_bug() {
        let _ = action_to_cmd(Seek(Key::PlayPause));
    }

    #[test]
    fn the_three_fixed_bindings_are_distinct_and_resolvable() {
        let bindings = bindings();
        assert_eq!(bindings.len(), 3);
        let mut ids: Vec<u32> = bindings.iter().map(|(s, _)| s.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 3);
        let keys: Vec<Key> = bindings.iter().map(|(_, k)| *k).collect();
        assert_eq!(keys, vec![Key::PlayPause, Key::Next, Key::Prev]);
    }

    #[test]
    fn labels_name_the_ctrl_alt_combinations() {
        let labels: Vec<_> = bindings().iter().map(|(_, k)| label(*k)).collect();
        assert_eq!(
            labels,
            vec!["Ctrl+Alt+空格", "Ctrl+Alt+→", "Ctrl+Alt+←"]
        );
    }
}
