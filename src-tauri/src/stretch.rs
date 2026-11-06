//! Streaming, pitch-preserving tempo conversion.

use rodio::{source::SeekError, ChannelCount, SampleRate, Source};
use signalsmith_stretch::Stretch;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const BLOCK_FRAMES: usize = 512;

/// Controls are shared with the audio thread. Position measures consumed output
/// frames mapped to the ORIGINAL timeline, never the decoder's read-ahead.
#[derive(Clone)]
pub(crate) struct TempoControl {
    speed: Arc<AtomicU32>,
    position: Arc<AtomicU64>,
}

impl TempoControl {
    pub(crate) fn new(speed: f32) -> Self {
        let control = Self {
            speed: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
            position: Arc::new(AtomicU64::new(0.0_f64.to_bits())),
        };
        control.set_speed(speed);
        control
    }

    pub(crate) fn set_speed(&self, speed: f32) {
        let speed = if speed.is_finite() {
            speed.clamp(0.5, 2.0)
        } else {
            1.0
        };
        self.speed.store(speed.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn position(&self) -> f64 {
        f64::from_bits(self.position.load(Ordering::Relaxed))
    }

    fn speed(&self) -> f64 {
        f32::from_bits(self.speed.load(Ordering::Relaxed)) as f64
    }

    pub(crate) fn publish(&self, seconds: f64) {
        self.position.store(seconds.to_bits(), Ordering::Relaxed);
    }
}

struct TimelineBlock {
    remaining: usize,
    input_per_output: f64,
}

/// Fixed-format streaming source. Storage is bounded by one processing block
/// plus the DSP's fixed latency, independently of track length.
pub(crate) struct TempoSource<S: Source> {
    source: S,
    control: TempoControl,
    dsp: Stretch,
    channels: ChannelCount,
    sample_rate: SampleRate,
    duration: Option<Duration>,
    input: Vec<f32>,
    output: Vec<f32>,
    output_index: usize,
    timeline: VecDeque<TimelineBlock>,
    started: bool,
    eof: bool,
    flushed: bool,
    skip_frames: usize,
    read_frames: usize,
    center_frames: usize,
    output_fraction: f64,
    /// Original-timeline frames credited to the consumed output.
    position_frames: f64,
    origin: f64,
    channel_index: usize,
}

impl<S: Source> TempoSource<S> {
    pub(crate) fn new(source: S, control: TempoControl) -> Self {
        let channels = source.channels();
        let sample_rate = source.sample_rate();
        let duration = source.total_duration();
        let mut dsp = Stretch::preset_default(channels.get() as u32, sample_rate.get());
        // Tempo changes must NEVER change the frequency multiplier.
        dsp.set_transpose_factor(1.0, None);
        let capacity = (dsp
            .input_latency()
            .max(dsp.output_latency())
            .max(BLOCK_FRAMES * 2)
            + BLOCK_FRAMES * 2)
            * channels.get() as usize;
        Self {
            source,
            control,
            dsp,
            channels,
            sample_rate,
            duration,
            input: Vec::with_capacity(capacity),
            output: Vec::with_capacity(capacity),
            output_index: 0,
            timeline: VecDeque::new(),
            started: false,
            eof: false,
            flushed: false,
            skip_frames: 0,
            read_frames: 0,
            center_frames: 0,
            output_fraction: 0.0,
            position_frames: 0.0,
            origin: 0.0,
            channel_index: 0,
        }
    }

    fn read_input(&mut self, frames: usize) {
        let channels = self.channels.get() as usize;
        self.input.clear();
        for _ in 0..frames {
            let mut actual_samples = 0;
            for _ in 0..channels {
                if !self.eof {
                    if let Some(sample) = self.source.next() {
                        self.input.push(sample);
                        actual_samples += 1;
                        continue;
                    }
                    self.eof = true;
                }
                self.input.push(0.0);
            }
            if !self.eof {
                self.read_frames += 1;
            } else if actual_samples > 0 {
                // A malformed incomplete final frame is zero-padded.
                self.read_frames += 1;
            }
        }
    }

    fn start(&mut self, speed: f64) {
        self.dsp.reset();
        self.read_input(self.dsp.input_latency());
        self.dsp.seek(&self.input, speed);
        self.skip_frames = self.dsp.output_latency();
        self.started = true;
    }

    fn refill(&mut self) -> bool {
        let channels = self.channels.get() as usize;
        self.output_index = 0;
        self.output.clear();
        loop {
            if self.flushed {
                return false;
            }
            // Atomic changes take effect only at a processing boundary.
            let speed = self.control.speed();
            if !self.started {
                self.start(speed);
            }
            if self.eof && self.center_frames == self.read_frames {
                self.output
                    .resize(self.dsp.output_latency() * channels, 0.0);
                self.dsp.flush(&mut self.output);
                self.flushed = true;
                // Credit the sub-frame remainder a rounding step dropped, so the final
                // position lands exactly on the original duration.
                self.consumed_frame(self.output_fraction * speed);
            } else {
                self.read_input(BLOCK_FRAMES);
                let frames = if self.eof {
                    BLOCK_FRAMES.min(self.read_frames - self.center_frames)
                } else {
                    BLOCK_FRAMES
                };
                if frames == 0 {
                    continue;
                }
                self.input.truncate(frames * channels);
                let exact_output = frames as f64 / speed + self.output_fraction;
                let output_frames = exact_output.floor() as usize;
                self.output_fraction = exact_output - output_frames as f64;
                self.center_frames += frames;
                if output_frames == 0 {
                    // Only the final sub-frame remainder of a stream lands here: at most
                    // one input frame of audio (inaudible) is dropped, and its time is
                    // credited when the stream finishes so the position stays exact.
                    continue;
                }
                self.output.resize(output_frames * channels, 0.0);
                self.dsp.process(&self.input, &mut self.output);
                self.timeline.push_back(TimelineBlock {
                    remaining: output_frames,
                    input_per_output: frames as f64 / output_frames as f64,
                });
            }
            let skip = self.skip_frames.min(self.output.len() / channels);
            self.skip_frames -= skip;
            self.output_index = skip * channels;
            if self.output_index < self.output.len() {
                return true;
            }
            self.output.clear();
        }
    }

    fn consumed_frame(&mut self, input_frames: f64) {
        self.position_frames += input_frames;
        let seconds = self.origin + self.position_frames / self.sample_rate.get() as f64;
        self.control.publish(
            self.duration
                .map_or(seconds, |d| seconds.min(d.as_secs_f64())),
        );
    }
}

impl<S: Source> Iterator for TempoSource<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if !self.started && self.control.speed() == 1.0 {
            let sample = self.source.next()?;
            self.channel_index += 1;
            if self.channel_index == self.channels.get() as usize {
                self.channel_index = 0;
                self.consumed_frame(1.0);
            }
            return Some(sample);
        }
        // Finish a partially emitted bypass frame before switching paths.
        if !self.started && self.channel_index != 0 {
            let sample = self.source.next()?;
            self.channel_index += 1;
            if self.channel_index == self.channels.get() as usize {
                self.channel_index = 0;
                self.consumed_frame(1.0);
            }
            return Some(sample);
        }
        if self.output_index == self.output.len() && !self.refill() {
            return None;
        }
        // A flushed DSP can include padding beyond our exact duration budget.
        if self.timeline.is_empty() {
            return None;
        }
        let sample = self.output[self.output_index];
        self.output_index += 1;
        self.channel_index += 1;
        if self.channel_index == self.channels.get() as usize {
            self.channel_index = 0;
            let block = self.timeline.front_mut().unwrap();
            let increment = block.input_per_output;
            block.remaining -= 1;
            if block.remaining == 0 {
                self.timeline.pop_front();
            }
            self.consumed_frame(increment);
        }
        Some(sample)
    }
}

impl<S: Source> Source for TempoSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> ChannelCount {
        self.channels
    }
    fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }
    // Dynamic tempo means output duration cannot be known. Backend retains
    // original duration separately; seeks are explicitly original-time seeks.
    fn total_duration(&self) -> Option<Duration> {
        None
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        let pos = self.duration.map_or(pos, |duration| pos.min(duration));
        self.source.try_seek(pos)?;
        self.dsp.reset();
        self.input.clear();
        self.output.clear();
        self.output_index = 0;
        self.timeline.clear();
        self.started = false;
        self.eof = false;
        self.flushed = false;
        self.read_frames = 0;
        self.center_frames = 0;
        self.skip_frames = 0;
        self.output_fraction = 0.0;
        self.position_frames = 0.0;
        self.origin = pos.as_secs_f64();
        self.channel_index = 0;
        self.control.publish(self.origin);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    fn tone(seconds: f32, channels: u16) -> SamplesBuffer {
        let samples = (0..(seconds * 48_000.0) as usize)
            .flat_map(|frame| {
                let value = (frame as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin();
                std::iter::repeat_n(value, channels as usize)
            })
            .collect::<Vec<_>>();
        SamplesBuffer::new(
            channels.try_into().unwrap(),
            48_000.try_into().unwrap(),
            samples,
        )
    }

    #[test]
    fn should_report_original_position_if_output_is_consumed() {
        // Arrange: two seconds at 2x tempo.
        let control = TempoControl::new(2.0);
        let mut source = TempoSource::new(tone(2.0, 1), control.clone());
        // Act: consume one output sample; decoder pre-roll is not playback.
        source.next().unwrap();
        // Assert: position advances by two original samples, not read-ahead.
        assert!((control.position() - 2.0 / 48_000.0).abs() < 1e-6);
        let count = source.by_ref().count() + 1;
        assert_eq!(count, 48_000);
        assert!((control.position() - 2.0).abs() < 1e-6);
    }

    #[test]
    fn should_clear_old_tail_if_seek_resets_original_timeline() {
        // Arrange: start tempo processing and leave a buffered block.
        let control = TempoControl::new(0.5);
        let mut source = TempoSource::new(tone(2.0, 2), control.clone());
        for _ in 0..100 { source.next().unwrap(); }
        // Act: seek to original 1.5 seconds.
        source.try_seek(Duration::from_secs_f64(1.5)).unwrap();
        // Assert: position origin resets and exactly 0.5 original seconds remain.
        assert_eq!(control.position(), 1.5);
        assert_eq!(source.count(), 96_000);
        assert!((control.position() - 2.0).abs() < 1e-6);
    }

    #[test]
    fn should_end_without_underflow_if_final_frame_rounds_to_zero_output() {
        // Arrange: an odd number of original frames at 2x.
        let control = TempoControl::new(2.0);
        let source = SamplesBuffer::new(1.try_into().unwrap(), 48_000.try_into().unwrap(), vec![0.0; 3_073]);
        // Act: consume the source completely.
        let output: Vec<f32> = TempoSource::new(source, control.clone()).collect();
        // Assert: fractional rounding never creates a zero-sized timeline block.
        assert_eq!(output.len(), 1_536);
        assert!((control.position() - 3_073.0 / 48_000.0).abs() < 1e-6);
    }

    #[test]
    fn should_use_new_rate_if_control_changes_during_playback() {
        // Arrange: begin at 1x using the exact bypass.
        let control = TempoControl::new(1.0);
        let mut source = TempoSource::new(tone(2.0, 1), control.clone());
        // Act: consume 0.5 seconds, then change live to 2x.
        let first = source.by_ref().take(24_000).count();
        control.set_speed(2.0);
        let remainder: Vec<f32> = source.collect();
        // Assert: remaining 1.5 original seconds take 0.75 output seconds.
        assert_eq!(first, 24_000);
        assert_eq!(remainder.len(), 36_000);
        assert!((control.position() - 2.0).abs() < 1e-6);
    }

    #[test]
    fn should_keep_memory_bounded_if_track_is_long() {
        // Arrange: an effectively infinite synthetic source, not a decoded file buffer.
        let control = TempoControl::new(0.5);
        let mut source = TempoSource::new(rodio::source::SineWave::new(440.0), control);
        let capacity = source.input.capacity() + source.output.capacity();
        // Act: consume many blocks.
        for _ in 0..200_000 { source.next().unwrap(); }
        // Assert: persistent memory depends on block/latency, not elapsed length.
        assert_eq!(source.input.capacity() + source.output.capacity(), capacity);
        assert!(source.timeline.len() < 32);
    }

    #[test]
    fn should_preserve_stereo_phase_if_speed_changes() {
        // Arrange: both channels contain the same tone.
        let source = TempoSource::new(tone(1.0, 2), TempoControl::new(2.0));
        // Act: collect the pitch-preserving tempo output.
        let output: Vec<f32> = source.collect();
        // Assert: stereo alignment and requested 2x duration. The DSP analyses channels
        // together to keep the stereo image coherent, so even identical inputs differ by
        // a float-level amount (~-82 dB on this tone); -60 dB is the transparency bound.
        assert_eq!(output.len() % 2, 0);
        let max_difference = output
            .chunks_exact(2)
            .map(|frame| (frame[0] - frame[1]).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_difference < 1e-3, "stereo max difference {max_difference}");
        assert!((output.len() as f64 / 96_000.0 - 0.5).abs() <= 0.1);
    }

    #[test]
    fn should_preserve_pitch_and_duration_if_speed_changes() {
        // Arrange: a two-second 440 Hz tone, with the tempo conversion missing.
        for speed in [0.5_f32, 1.0, 1.5, 2.0] {
            let source = TempoSource::new(tone(2.0, 1), TempoControl::new(speed));
            // Act: collect the streaming output.
            let output: Vec<f32> = source.collect();
            // Assert: duration follows tempo, not pitch.
            let seconds = output.len() as f64 / 48_000.0;
            assert!(
                (seconds - 2.0 / speed as f64).abs() <= 0.1,
                "speed {speed}: duration {seconds}"
            );
            let middle = &output[output.len() / 4..output.len() * 3 / 4];
            let crossings = middle
                .windows(2)
                .filter(|s| s[0] <= 0.0 && s[1] > 0.0)
                .count();
            let frequency = crossings as f64 * 48_000.0 / middle.len() as f64;
            assert!(
                (frequency / 440.0 - 1.0).abs() <= 0.02,
                "speed {speed}: Hz {frequency}"
            );
        }
    }
}
