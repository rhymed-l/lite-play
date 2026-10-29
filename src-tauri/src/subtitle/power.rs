//! How many recognizer processes to run at once, from the user's chosen level and the machine.
//!
//! Measured on the development machine (32 logical cores, 48 GB): one recognizer needs about
//! 393 MB whatever its thread count; one process cannot use more than ~7 cores (8 threads is
//! already the plateau), but many processes together scale to about 16 before gaining nothing.

/// Memory budgeted per recognizer process, in MB. Measured 393 MB; the rest is headroom.
const MB_PER_JOB: u64 = 450;
/// Memory always left for the system and the player itself, in MB.
const RESERVE_MB: u64 = 3072;
/// Past this many processes the measured throughput stopped improving.
const MAX_JOBS: u64 = 16;
/// Threads per process when several run together (measured best: many small processes).
const THREADS_WHEN_PARALLEL: u32 = 2;
/// A lone process gets a few more threads (measured: 4 was the sweet spot for one).
const THREADS_WHEN_ALONE: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Level {
    /// About a quarter of the cores: stays out of the way of everything else.
    #[default]
    Light,
    /// About half of the cores.
    Balanced,
    /// All cores (up to the point where more stops helping).
    Full,
}

impl Level {
    pub fn from_name(name: &str) -> Level {
        match name {
            "balanced" => Level::Balanced,
            "full" => Level::Full,
            _ => Level::Light,
        }
    }

    /// Share of the cores this level may use, as (numerator, denominator).
    fn share(self) -> (u64, u64) {
        match self {
            Level::Light => (1, 4),
            Level::Balanced => (1, 2),
            Level::Full => (1, 1),
        }
    }
}

/// How many recognizer processes may run at once: the level's share of the cores (each process
/// takes two), but never more than the free memory allows, never fewer than one, never more
/// than the measured useful maximum.
pub fn jobs_for(level: Level, logical_cores: usize, free_mb: u64) -> usize {
    let cores = logical_cores.max(1) as u64;
    let (num, den) = level.share();
    let by_cores = (cores * num / den / THREADS_WHEN_PARALLEL as u64).max(1);
    let by_memory = free_mb.saturating_sub(RESERVE_MB) / MB_PER_JOB;
    by_cores.min(by_memory).clamp(1, MAX_JOBS) as usize
}

pub fn threads_per_job(jobs: usize) -> u32 {
    if jobs <= 1 {
        THREADS_WHEN_ALONE
    } else {
        THREADS_WHEN_PARALLEL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024;

    #[test]
    fn the_machine_this_was_measured_on_gets_4_8_and_16_jobs() {
        // 32 logical cores, 23 GB free (a typical moment on the development machine)
        assert_eq!(jobs_for(Level::Light, 32, 23 * GB), 4);
        assert_eq!(jobs_for(Level::Balanced, 32, 23 * GB), 8);
        assert_eq!(jobs_for(Level::Full, 32, 23 * GB), 16);
    }

    #[test]
    fn full_never_goes_past_sixteen_even_on_a_bigger_machine() {
        assert_eq!(jobs_for(Level::Full, 128, 256 * GB), 16);
        assert_eq!(jobs_for(Level::Balanced, 128, 256 * GB), 16);
    }

    #[test]
    fn a_small_machine_still_gets_at_least_one_job() {
        assert_eq!(jobs_for(Level::Light, 2, 16 * GB), 1);
        assert_eq!(jobs_for(Level::Balanced, 1, 16 * GB), 1);
        assert_eq!(jobs_for(Level::Full, 0, 16 * GB), 1, "zero cores is nonsense, treated as one");
    }

    #[test]
    fn low_free_memory_caps_the_job_count_whatever_the_level() {
        // 4 GB free: (4096 - 3072) / 450 = 2 jobs
        assert_eq!(jobs_for(Level::Full, 32, 4 * GB), 2);
        assert_eq!(jobs_for(Level::Balanced, 32, 4 * GB), 2);
        // Light wants 4 and memory allows 2
        assert_eq!(jobs_for(Level::Light, 32, 4 * GB), 2);
    }

    #[test]
    fn memory_cap_does_not_bite_when_there_is_plenty() {
        // exactly enough for 8 jobs: 3072 + 8 * 450
        assert_eq!(jobs_for(Level::Balanced, 32, 3072 + 8 * 450), 8);
        // one megabyte short of the eighth job
        assert_eq!(jobs_for(Level::Balanced, 32, 3072 + 8 * 450 - 1), 7);
    }

    #[test]
    fn almost_no_free_memory_still_allows_one_job_rather_than_none() {
        assert_eq!(jobs_for(Level::Full, 32, 0), 1);
        assert_eq!(jobs_for(Level::Full, 32, 1000), 1, "below the reserve: one job, slowly");
    }

    #[test]
    fn each_job_uses_two_threads_except_when_it_is_the_only_one() {
        assert_eq!(threads_per_job(8), 2);
        assert_eq!(threads_per_job(2), 2);
        assert_eq!(threads_per_job(1), 4, "a lone job may use a few more threads (measured: 4 is the sweet spot)");
    }

    #[test]
    fn levels_parse_from_their_settings_names_and_default_to_light() {
        assert_eq!(Level::from_name("light"), Level::Light);
        assert_eq!(Level::from_name("balanced"), Level::Balanced);
        assert_eq!(Level::from_name("full"), Level::Full);
        assert_eq!(Level::from_name(""), Level::Light);
        assert_eq!(Level::from_name("turbo"), Level::Light);
    }

    #[test]
    fn the_default_level_is_the_gentle_one() {
        // the app must not grab half the machine the first time the switch is turned on
        assert_eq!(Level::default(), Level::Light);
    }
}
