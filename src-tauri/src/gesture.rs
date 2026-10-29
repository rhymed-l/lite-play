#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    PlayPause,
    Next,
    Prev,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GestureAction {
    Tap(Key),
    Seek(Key),
}

const ALL_KEYS: [Key; 3] = [Key::PlayPause, Key::Next, Key::Prev];

impl Key {
    fn index(self) -> usize {
        match self {
            Key::PlayPause => 0,
            Key::Next => 1,
            Key::Prev => 2,
        }
    }

    fn distinguishes_hold(self) -> bool {
        self != Key::PlayPause
    }
}

#[derive(Clone, Copy)]
struct Held {
    start: u64,
    last_event: u64,
    last_seek: Option<u64>,
}

pub struct GestureDetector {
    threshold_ms: u64,
    step_ms: u64,
    stale_ms: Option<u64>,
    held: [Option<Held>; 3],
}

impl GestureDetector {
    pub fn new(threshold_ms: u64, step_ms: u64, stale_ms: Option<u64>) -> Self {
        Self {
            threshold_ms,
            step_ms,
            stale_ms,
            held: [None; 3],
        }
    }

    pub fn press(&mut self, key: Key, now_ms: u64) -> Vec<GestureAction> {
        let slot = &mut self.held[key.index()];
        if let Some(h) = slot {
            h.last_event = now_ms;
            return Vec::new();
        }
        *slot = Some(Held {
            start: now_ms,
            last_event: now_ms,
            last_seek: None,
        });
        if key.distinguishes_hold() {
            Vec::new()
        } else {
            vec![GestureAction::Tap(key)]
        }
    }

    pub fn release(&mut self, key: Key, now_ms: u64) -> Vec<GestureAction> {
        match self.held[key.index()].take() {
            Some(h) => self.on_release(key, h.start, now_ms),
            None => Vec::new(),
        }
    }

    pub fn tick(&mut self, now_ms: u64) -> Vec<GestureAction> {
        let mut out = Vec::new();
        for key in ALL_KEYS {
            let Some(h) = self.held[key.index()] else {
                continue;
            };
            let is_stale = self
                .stale_ms
                .is_some_and(|limit| now_ms.saturating_sub(h.last_event) > limit);
            if is_stale {
                self.held[key.index()] = None;
                out.extend(self.on_release(key, h.start, h.last_event));
                continue;
            }
            if !key.distinguishes_hold() || now_ms.saturating_sub(h.start) < self.threshold_ms {
                continue;
            }
            let due = h
                .last_seek
                .is_none_or(|t| now_ms.saturating_sub(t) >= self.step_ms);
            if due {
                if let Some(slot) = self.held[key.index()].as_mut() {
                    slot.last_seek = Some(now_ms);
                }
                out.push(GestureAction::Seek(key));
            }
        }
        out
    }

    fn on_release(&self, key: Key, start: u64, at: u64) -> Vec<GestureAction> {
        if key.distinguishes_hold() && at.saturating_sub(start) < self.threshold_ms {
            vec![GestureAction::Tap(key)]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GestureAction::{Seek, Tap};

    fn det() -> GestureDetector {
        GestureDetector::new(400, 200, None)
    }

    fn none() -> Vec<GestureAction> {
        Vec::new()
    }

    #[test]
    fn short_press_taps_on_release() {
        let mut d = det();
        assert_eq!(d.press(Key::Next, 0), none());
        assert_eq!(d.release(Key::Next, 100), vec![Tap(Key::Next)]);
    }

    #[test]
    fn hold_seeks_at_threshold_then_every_step_and_release_is_silent() {
        let mut d = det();
        d.press(Key::Next, 0);
        assert_eq!(d.tick(399), none());
        assert_eq!(d.tick(400), vec![Seek(Key::Next)]);
        assert_eq!(d.tick(500), none());
        assert_eq!(d.tick(600), vec![Seek(Key::Next)]);
        assert_eq!(d.release(Key::Next, 650), none());
    }

    #[test]
    fn release_exactly_at_threshold_counts_as_long_press() {
        let mut d = det();
        d.press(Key::Next, 0);
        assert_eq!(d.release(Key::Next, 400), none());
    }

    #[test]
    fn repeated_press_events_do_not_reset_the_start_time() {
        let mut d = det();
        d.press(Key::Next, 0);
        assert_eq!(d.press(Key::Next, 30), none());
        assert_eq!(d.press(Key::Next, 60), none());
        assert_eq!(d.tick(400), vec![Seek(Key::Next)]);
    }

    #[test]
    fn release_without_press_is_ignored() {
        let mut d = det();
        assert_eq!(d.release(Key::Prev, 10), none());
    }

    #[test]
    fn play_pause_taps_once_per_physical_press() {
        let mut d = det();
        assert_eq!(d.press(Key::PlayPause, 0), vec![Tap(Key::PlayPause)]);
        assert_eq!(d.press(Key::PlayPause, 30), none());
        assert_eq!(d.release(Key::PlayPause, 60), none());
        assert_eq!(d.press(Key::PlayPause, 100), vec![Tap(Key::PlayPause)]);
    }

    #[test]
    fn holding_next_does_not_disturb_a_prev_tap() {
        let mut d = det();
        d.press(Key::Next, 0);
        assert_eq!(d.press(Key::Prev, 50), none());
        assert_eq!(d.release(Key::Prev, 80), vec![Tap(Key::Prev)]);
        assert_eq!(d.tick(400), vec![Seek(Key::Next)]);
    }

    #[test]
    fn a_second_hold_starts_fresh_after_release() {
        let mut d = det();
        d.press(Key::Next, 0);
        assert_eq!(d.tick(400), vec![Seek(Key::Next)]);
        assert_eq!(d.release(Key::Next, 500), none());
        d.press(Key::Next, 1000);
        assert_eq!(d.tick(1300), none());
        assert_eq!(d.tick(1400), vec![Seek(Key::Next)]);
    }

    #[test]
    fn without_stale_inference_a_key_without_release_keeps_seeking() {
        let mut d = det();
        d.press(Key::Next, 0);
        assert_eq!(d.tick(10_000), vec![Seek(Key::Next)]);
    }

    #[test]
    fn stale_key_with_no_repeats_is_inferred_released_as_a_tap() {
        let mut d = GestureDetector::new(400, 200, Some(700));
        d.press(Key::Next, 0);
        assert_eq!(d.tick(800), vec![Tap(Key::Next)]);
    }

    #[test]
    fn stale_key_after_a_long_hold_is_released_silently() {
        let mut d = GestureDetector::new(400, 200, Some(700));
        d.press(Key::Next, 0);
        d.press(Key::Next, 500);
        assert_eq!(d.tick(500), vec![Seek(Key::Next)]);
        assert_eq!(d.tick(1201), none());
        assert_eq!(d.tick(1500), none());
    }

    #[test]
    fn stale_play_pause_expires_so_it_can_fire_again() {
        let mut d = GestureDetector::new(400, 200, Some(700));
        assert_eq!(d.press(Key::PlayPause, 0), vec![Tap(Key::PlayPause)]);
        assert_eq!(d.tick(800), none());
        assert_eq!(d.press(Key::PlayPause, 900), vec![Tap(Key::PlayPause)]);
    }
}
