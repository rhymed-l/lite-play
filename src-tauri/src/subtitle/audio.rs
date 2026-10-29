use rodio::{Decoder, Source};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};
use std::fs::{self, File};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const TARGET_RATE: usize = 16_000;
/// Input frames per resampler chunk; the FFT resampler is fastest with a fixed chunk.
const CHUNK_FRAMES: usize = 4096;

pub fn wav_header(data_len: u32) -> [u8; 44] {
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_len).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&(TARGET_RATE as u32).to_le_bytes());
    h[28..32].copy_from_slice(&(TARGET_RATE as u32 * 2).to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes()); // block align
    h[34..36].copy_from_slice(&16u16.to_le_bytes()); // bits per sample
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

pub fn f32_to_i16(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

struct Sink {
    out: BufWriter<File>,
    bytes: u64,
}

impl Sink {
    fn write_samples(&mut self, samples: &[f32]) -> Result<(), String> {
        for s in samples {
            self.out
                .write_all(&f32_to_i16(*s).to_le_bytes())
                .map_err(|e| e.to_string())?;
        }
        self.bytes += samples.len() as u64 * 2;
        Ok(())
    }
}

/// Decodes any format the player can play into 16 kHz mono 16-bit PCM at `dst`, in chunks,
/// so a long episode never has to fit in memory. Returns the audio length in seconds.
/// Never opens an audio output device. On error or cancel, `dst` is removed.
pub fn decode_to_wav(src: &Path, dst: &Path, cancel: &AtomicBool) -> Result<f64, String> {
    let result = decode_inner(src, dst, cancel);
    if result.is_err() {
        let _ = fs::remove_file(dst);
    }
    result
}

fn decode_inner(src: &Path, dst: &Path, cancel: &AtomicBool) -> Result<f64, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".to_string());
    }
    let file = File::open(src).map_err(|e| e.to_string())?;
    // A bare `File`, not a `BufReader`: only `TryFrom<File>` gives the decoder the byte
    // length and seekability that m4a needs to find its index.
    let decoder = Decoder::try_from(file).map_err(|e| e.to_string())?;
    let channels = decoder.channels().get() as usize;
    let rate = decoder.sample_rate().get() as usize;

    let mut resampler = if rate == TARGET_RATE {
        None
    } else {
        Some(
            Fft::<f32>::new(rate, TARGET_RATE, CHUNK_FRAMES, 1, FixedSync::Input)
                .map_err(|e| e.to_string())?,
        )
    };
    let delay = resampler.as_ref().map_or(0, |r| r.output_delay());

    let mut sink = Sink {
        out: BufWriter::new(File::create(dst).map_err(|e| e.to_string())?),
        bytes: 0,
    };
    sink.out.write_all(&wav_header(0)).map_err(|e| e.to_string())?;

    let mut interleaved: Vec<f32> = Vec::with_capacity(CHUNK_FRAMES * channels);
    let mut mono: Vec<f32> = Vec::new();
    let mut input_frames: u64 = 0;
    let mut out_buf: Vec<f32> = Vec::new();
    let mut skip = delay;

    let push_chunk = |mono: &[f32],
                          partial: Option<usize>,
                          resampler: &mut Option<Fft<f32>>,
                          sink: &mut Sink,
                          out_buf: &mut Vec<f32>,
                          skip: &mut usize|
     -> Result<(), String> {
        let Some(r) = resampler.as_mut() else {
            return sink.write_samples(mono);
        };
        let need = r.input_frames_next();
        let mut padded = mono.to_vec();
        padded.resize(need, 0.0);
        out_buf.resize(r.output_frames_max(), 0.0);
        let input = InterleavedSlice::new(&padded, 1, need).map_err(|e| e.to_string())?;
        let cap = out_buf.len();
        let mut output = InterleavedSlice::new_mut(out_buf, 1, cap).map_err(|e| e.to_string())?;
        let indexing = Indexing {
            input_offset: 0,
            output_offset: 0,
            partial_len: partial,
            active_channels_mask: None,
        };
        let (_read, written) = r
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .map_err(|e| e.to_string())?;
        let produced = &out_buf[..written];
        let drop = (*skip).min(produced.len());
        *skip -= drop;
        sink.write_samples(&produced[drop..])
    };

    let need_frames = |resampler: &Option<Fft<f32>>| {
        resampler.as_ref().map_or(CHUNK_FRAMES, |r| r.input_frames_next())
    };

    let mut samples = decoder;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        let want = need_frames(&resampler);
        interleaved.clear();
        for s in samples.by_ref().take(want * channels) {
            interleaved.push(s);
        }
        let got_frames = interleaved.len() / channels;
        if got_frames == 0 {
            break;
        }
        mono = downmix(&interleaved[..got_frames * channels], channels);
        input_frames += got_frames as u64;
        let partial = (got_frames < want).then_some(got_frames);
        push_chunk(&mono, partial, &mut resampler, &mut sink, &mut out_buf, &mut skip)?;
        if got_frames < want {
            break;
        }
    }
    if input_frames == 0 {
        return Err("no audio decoded".to_string());
    }

    // Flush: feed empty input until the resampler's delay has been emitted.
    let expected_out = (input_frames as f64 * TARGET_RATE as f64 / rate as f64).round() as u64;
    if resampler.is_some() {
        mono.clear();
        let mut guard = 0;
        while sink.bytes / 2 < expected_out && guard < 8 {
            push_chunk(&[], Some(0), &mut resampler, &mut sink, &mut out_buf, &mut skip)?;
            guard += 1;
        }
    }

    let total_samples = (sink.bytes / 2).min(expected_out);
    let data_len = (total_samples * 2) as u32;
    sink.out.flush().map_err(|e| e.to_string())?;
    let mut file = sink.out.into_inner().map_err(|e| e.to_string())?;
    file.set_len(44 + data_len as u64).map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    file.write_all(&wav_header(data_len)).map_err(|e| e.to_string())?;
    Ok(total_samples as f64 / TARGET_RATE as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn u16_at(b: &[u8], i: usize) -> u16 {
        u16::from_le_bytes([b[i], b[i + 1]])
    }

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
    }

    #[test]
    fn header_fields() {
        let h = wav_header(3200);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(&h[8..12], b"WAVE");
        assert_eq!(&h[12..16], b"fmt ");
        assert_eq!(u16_at(&h, 20), 1, "PCM");
        assert_eq!(u16_at(&h, 22), 1, "mono");
        assert_eq!(u32_at(&h, 24), 16000);
        assert_eq!(u32_at(&h, 28), 32000, "byte rate");
        assert_eq!(u16_at(&h, 32), 2, "block align");
        assert_eq!(u16_at(&h, 34), 16, "bits");
        assert_eq!(&h[36..40], b"data");
        assert_eq!(u32_at(&h, 40), 3200);
        assert_eq!(u32_at(&h, 4), 36 + 3200);
    }

    #[test]
    fn downmix_averages() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert_eq!(downmix(&[0.25, -0.25], 1), vec![0.25, -0.25]);
        assert_eq!(downmix(&[0.3, 0.6, 0.9], 3).len(), 1);
    }

    #[test]
    fn i16_conversion_clamps() {
        assert_eq!(f32_to_i16(2.0), 32767);
        assert_eq!(f32_to_i16(-2.0), -32767);
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(0.5), 16384);
    }

    #[test]
    fn fixtures_decode_to_16k_mono() {
        for name in ["tone.mp3", "tone.m4a", "tone.wav", "tone_alac.m4a"] {
            let dir = tempfile::tempdir().unwrap();
            let dst = dir.path().join("job.wav");
            let cancel = AtomicBool::new(false);
            let secs = decode_to_wav(&fixture(name), &dst, &cancel)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!((0.8..1.3).contains(&secs), "{name}: duration {secs}");
            let bytes = fs::read(&dst).unwrap();
            assert_eq!(&bytes[0..4], b"RIFF", "{name}");
            assert_eq!(u32_at(&bytes, 24), 16000, "{name}");
            assert_eq!(u16_at(&bytes, 22), 1, "{name}");
            assert_eq!(u16_at(&bytes, 34), 16, "{name}");
            let data_len = u32_at(&bytes, 40) as usize;
            assert_eq!(bytes.len(), 44 + data_len, "{name}");
            assert_eq!(data_len % 2, 0, "{name}");
            let from_len = data_len as f64 / 32000.0;
            assert!((from_len - secs).abs() < 0.05, "{name}: {from_len} vs {secs}");
        }
    }

    #[test]
    fn decoded_tone_is_not_silent() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("job.wav");
        decode_to_wav(&fixture("tone.wav"), &dst, &AtomicBool::new(false)).unwrap();
        let bytes = fs::read(&dst).unwrap();
        let peak = bytes[44..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
            .max()
            .unwrap();
        assert!(peak > 1000, "peak {peak}");
    }

    #[test]
    fn corrupt_input_errors_and_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.mp3");
        fs::write(&bad, [7u8, 99, 3, 200, 14, 1, 1, 0, 250, 66, 12, 5].repeat(200)).unwrap();
        let dst = dir.path().join("job.wav");
        assert!(decode_to_wav(&bad, &dst, &AtomicBool::new(false)).is_err());
        assert!(!dst.exists());
    }

    #[test]
    fn missing_input_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("job.wav");
        assert!(decode_to_wav(Path::new("Z:/nope/x.mp3"), &dst, &AtomicBool::new(false)).is_err());
        assert!(!dst.exists());
    }

    #[test]
    fn cancel_removes_partial_output() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("job.wav");
        let err = decode_to_wav(&fixture("tone.wav"), &dst, &AtomicBool::new(true)).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert!(!dst.exists());
    }

    #[test]
    #[ignore = "needs a real episode; set LITE_PLAY_EPISODE"]
    fn real_episode_length_matches_the_decoder() {
        use rodio::Source;
        let src = std::env::var("LITE_PLAY_EPISODE").expect("LITE_PLAY_EPISODE");
        // The player decodes with the same rodio decoder, so its own sample count is the
        // right reference: subtitle times then line up with playback positions.
        let reference = {
            let d = rodio::Decoder::try_from(std::fs::File::open(&src).unwrap()).unwrap();
            let (ch, rate) = (d.channels().get() as u64, d.sample_rate().get() as f64);
            (d.count() as u64 / ch) as f64 / rate
        };
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("job.wav");
        let started = std::time::Instant::now();
        let secs = decode_to_wav(Path::new(&src), &dst, &AtomicBool::new(false)).unwrap();
        eprintln!(
            "decoded {secs:.3}s (decoder says {reference:.3}s) in {:.2}s",
            started.elapsed().as_secs_f64()
        );
        assert!((secs - reference).abs() < 0.01, "got {secs}, decoder says {reference}");
        let bytes = fs::read(&dst).unwrap();
        assert_eq!(bytes.len(), 44 + (u32_at(&bytes, 40) as usize));
    }

    /// Writes a mono 16-bit wav at `rate` Hz that is silent except for one loud sample.
    fn impulse_wav(path: &Path, rate: u32, seconds: f64, impulse_at: f64) {
        let total = (rate as f64 * seconds) as usize;
        let at = (rate as f64 * impulse_at) as usize;
        let mut data = vec![0u8; total * 2];
        data[at * 2..at * 2 + 2].copy_from_slice(&30000i16.to_le_bytes());
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&data);
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn resampled_audio_stays_aligned_with_the_source_timeline() {
        // A sound at 1.5 s must still be at 1.5 s after resampling, whatever the source rate:
        // subtitle times are read off this wav and compared with the player's position.
        for rate in [8_000u32, 22_050, 44_100, 48_000] {
            let dir = tempfile::tempdir().unwrap();
            let src = dir.path().join("impulse.wav");
            let dst = dir.path().join("out.wav");
            impulse_wav(&src, rate, 3.0, 1.5);
            decode_to_wav(&src, &dst, &AtomicBool::new(false)).unwrap();
            let bytes = fs::read(&dst).unwrap();
            let (peak_at, _) = bytes[44..]
                .chunks_exact(2)
                .enumerate()
                .map(|(i, c)| (i, i16::from_le_bytes([c[0], c[1]]).unsigned_abs()))
                .max_by_key(|(_, v)| *v)
                .unwrap();
            let expected = 1.5 * 16000.0;
            assert!(
                (peak_at as f64 - expected).abs() <= 3.0,
                "{rate} Hz: the sound landed at sample {peak_at}, expected {expected}"
            );
        }
    }

    #[test]
    fn non_ascii_names_work() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("中文 目录");
        fs::create_dir_all(&sub).unwrap();
        let src = sub.join("0002 001 血门.wav");
        fs::copy(fixture("tone.wav"), &src).unwrap();
        let dst = sub.join("输出 job.wav");
        decode_to_wav(&src, &dst, &AtomicBool::new(false)).unwrap();
        assert!(dst.exists());
    }
}
