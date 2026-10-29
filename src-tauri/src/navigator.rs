use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use serde::{Deserialize, Serialize};

const SHUFFLE_AVOID_RECENT: usize = 20;
const HISTORY_CAP: usize = 1000;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Mode {
    Sequential,
    Single,
    Shuffle,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Direction {
    Forward,
    Reverse,
}

pub struct Navigator {
    len: usize,
    mode: Mode,
    direction: Direction,
    current: Option<usize>,
    history: Vec<usize>,
    rng: StdRng,
}

impl Navigator {
    pub fn new(seed: u64) -> Self {
        Self {
            len: 0,
            mode: Mode::Sequential,
            direction: Direction::Forward,
            current: None,
            history: Vec::new(),
            rng: StdRng::seed_from_u64(seed),
        }
    }

    pub fn set_len(&mut self, len: usize) {
        self.len = len;
        self.history.clear();
        if self.current.is_some_and(|c| c >= len) {
            self.current = None;
        }
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    pub fn set_direction(&mut self, direction: Direction) {
        self.direction = direction;
    }

    pub fn current(&self) -> Option<usize> {
        self.current
    }

    pub fn jump_to(&mut self, idx: usize) {
        if idx < self.len {
            self.move_to(idx);
        }
    }

    pub fn first(&self) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        match self.direction {
            Direction::Forward => Some(0),
            Direction::Reverse => Some(self.len - 1),
        }
    }

    pub fn next_manual(&mut self) -> Option<usize> {
        let next = match self.mode {
            Mode::Shuffle => self.pick_random(),
            Mode::Sequential | Mode::Single => self.step(true),
        };
        if let Some(idx) = next {
            self.move_to(idx);
        }
        next
    }

    pub fn prev_manual(&mut self) -> Option<usize> {
        match self.mode {
            Mode::Shuffle => {
                if self.history.len() <= 1 {
                    return None;
                }
                self.history.pop();
                let prev = *self.history.last()?;
                self.current = Some(prev);
                Some(prev)
            }
            Mode::Sequential | Mode::Single => {
                let prev = self.step(false)?;
                self.move_to(prev);
                Some(prev)
            }
        }
    }

    pub fn next_auto(&mut self) -> Option<usize> {
        match self.mode {
            Mode::Single => self.current,
            _ => self.next_manual(),
        }
    }

    fn move_to(&mut self, idx: usize) {
        self.current = Some(idx);
        self.history.push(idx);
        if self.history.len() > HISTORY_CAP {
            self.history.remove(0);
        }
    }

    /// One step along the list in the playing direction (`forward`) or against it.
    fn step(&self, forward: bool) -> Option<usize> {
        let Some(cur) = self.current else {
            return if forward { self.first() } else { None };
        };
        let ascending = (self.direction == Direction::Forward) == forward;
        if ascending {
            (cur + 1 < self.len).then_some(cur + 1)
        } else {
            cur.checked_sub(1)
        }
    }

    fn pick_random(&mut self) -> Option<usize> {
        match self.len {
            0 => return None,
            1 => return Some(0),
            _ => {}
        }
        let window = SHUFFLE_AVOID_RECENT.min(self.len - 1);
        let recent = &self.history[self.history.len().saturating_sub(window)..];
        let mut candidates: Vec<usize> = (0..self.len)
            .filter(|i| !recent.contains(i) && Some(*i) != self.current)
            .collect();
        if candidates.is_empty() {
            candidates = (0..self.len).filter(|i| Some(*i) != self.current).collect();
        }
        let pick = self.rng.random_range(0..candidates.len());
        Some(candidates[pick])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nav(len: usize, mode: Mode, dir: Direction) -> Navigator {
        let mut n = Navigator::new(42);
        n.set_len(len);
        n.set_mode(mode);
        n.set_direction(dir);
        n
    }

    #[test]
    fn sequential_forward_stops_at_the_end() {
        let mut n = nav(3, Mode::Sequential, Direction::Forward);
        n.jump_to(0);
        assert_eq!(n.next_manual(), Some(1));
        assert_eq!(n.next_manual(), Some(2));
        assert_eq!(n.next_manual(), None);
        assert_eq!(n.current(), Some(2));
    }

    #[test]
    fn sequential_reverse_walks_backwards_and_stops() {
        let mut n = nav(3, Mode::Sequential, Direction::Reverse);
        n.jump_to(2);
        assert_eq!(n.next_manual(), Some(1));
        assert_eq!(n.next_manual(), Some(0));
        assert_eq!(n.next_manual(), None);
    }

    #[test]
    fn sequential_auto_returns_none_after_last_track() {
        let mut n = nav(3, Mode::Sequential, Direction::Forward);
        n.jump_to(2);
        assert_eq!(n.next_auto(), None);
        assert_eq!(n.current(), Some(2));
    }

    #[test]
    fn sequential_prev_at_start_does_nothing() {
        let mut n = nav(3, Mode::Sequential, Direction::Forward);
        n.jump_to(0);
        assert_eq!(n.prev_manual(), None);
        assert_eq!(n.current(), Some(0));

        let mut r = nav(3, Mode::Sequential, Direction::Reverse);
        r.jump_to(2);
        assert_eq!(r.prev_manual(), None);
    }

    #[test]
    fn first_follows_direction() {
        assert_eq!(nav(3, Mode::Sequential, Direction::Forward).first(), Some(0));
        assert_eq!(nav(3, Mode::Sequential, Direction::Reverse).first(), Some(2));
        assert_eq!(nav(0, Mode::Sequential, Direction::Forward).first(), None);
    }

    #[test]
    fn next_manual_without_current_starts_at_first() {
        let mut f = nav(3, Mode::Sequential, Direction::Forward);
        assert_eq!(f.next_manual(), Some(0));
        let mut r = nav(3, Mode::Sequential, Direction::Reverse);
        assert_eq!(r.next_manual(), Some(2));
    }

    #[test]
    fn single_mode_repeats_on_auto_but_manual_steps_through_list() {
        let mut n = nav(3, Mode::Single, Direction::Forward);
        n.jump_to(1);
        assert_eq!(n.next_auto(), Some(1));
        assert_eq!(n.next_manual(), Some(2));
    }

    #[test]
    fn shuffle_with_one_track_always_plays_it() {
        let mut n = nav(1, Mode::Shuffle, Direction::Forward);
        n.jump_to(0);
        for _ in 0..10 {
            assert_eq!(n.next_auto(), Some(0));
        }
    }

    #[test]
    fn shuffle_with_two_tracks_alternates() {
        let mut n = nav(2, Mode::Shuffle, Direction::Forward);
        n.jump_to(0);
        let got: Vec<_> = (0..10).map(|_| n.next_auto().unwrap()).collect();
        assert_eq!(got, vec![1, 0, 1, 0, 1, 0, 1, 0, 1, 0]);
    }

    #[test]
    fn shuffle_manual_next_also_avoids_current() {
        let mut n = nav(2, Mode::Shuffle, Direction::Forward);
        n.jump_to(1);
        assert_eq!(n.next_manual(), Some(0));
        assert_eq!(n.next_manual(), Some(1));
    }

    #[test]
    fn shuffle_with_three_tracks_is_a_fixed_rotation() {
        let mut n = nav(3, Mode::Shuffle, Direction::Forward);
        n.jump_to(0);
        let mut seq = vec![0usize];
        for _ in 0..30 {
            seq.push(n.next_auto().unwrap());
        }
        for w in seq.windows(3) {
            assert!(w[0] != w[1] && w[1] != w[2] && w[0] != w[2], "{:?}", w);
        }
    }

    #[test]
    fn shuffle_with_many_tracks_avoids_current_and_last_twenty() {
        let mut n = nav(100, Mode::Shuffle, Direction::Forward);
        n.jump_to(0);
        let mut seq = vec![0usize];
        for _ in 0..500 {
            let r = n.next_auto().unwrap();
            assert!(r < 100);
            assert_ne!(r, *seq.last().unwrap());
            let recent = &seq[seq.len().saturating_sub(20)..];
            assert!(!recent.contains(&r), "{} repeated within last 20", r);
            seq.push(r);
        }
    }

    #[test]
    fn shuffle_prev_walks_back_through_history() {
        let mut n = nav(10, Mode::Shuffle, Direction::Forward);
        n.jump_to(0);
        let a = n.next_auto().unwrap();
        let _b = n.next_auto().unwrap();
        assert_eq!(n.prev_manual(), Some(a));
        assert_eq!(n.prev_manual(), Some(0));
        assert_eq!(n.prev_manual(), None);
    }

    #[test]
    fn set_len_clears_shuffle_history() {
        let mut n = nav(10, Mode::Shuffle, Direction::Forward);
        n.jump_to(0);
        n.next_auto();
        n.next_auto();
        n.set_len(5);
        assert_eq!(n.prev_manual(), None);
    }

    #[test]
    fn set_len_drops_current_when_out_of_range() {
        let mut n = nav(10, Mode::Sequential, Direction::Forward);
        n.jump_to(7);
        n.set_len(5);
        assert_eq!(n.current(), None);

        n.jump_to(3);
        n.set_len(5);
        assert_eq!(n.current(), Some(3));
    }
}
