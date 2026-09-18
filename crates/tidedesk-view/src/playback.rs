//! Audio: Opus datagrams → jitter buffer → default output device.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use tidedesk_core::audio::{self as fmt, StereoResampler};
use tidedesk_core::streaming::audio_buffer_ms;

fn samples_for_ms(ms: usize) -> usize {
    fmt::SAMPLE_RATE as usize * fmt::CHANNELS * ms / 1000
}

#[derive(Default)]
struct Jitter {
    samples: VecDeque<f32>,
    playing: bool,
    game_boost: Arc<AtomicBool>,
    applied_boost: bool,
}

impl Jitter {
    /// Apply profile changes inside the buffer lock, including already queued PCM.
    fn tune(&mut self) -> usize {
        let boost = self.game_boost.load(Ordering::Relaxed);
        let (target_ms, max_ms) = audio_buffer_ms(boost);
        let target = samples_for_ms(target_ms);
        let trim = self.samples.len() > samples_for_ms(max_ms)
            || (boost && !self.applied_boost && self.samples.len() > target);
        self.applied_boost = boost;
        if trim {
            let excess = self.samples.len().saturating_sub(target) & !1;
            self.samples.drain(..excess);
        }
        target
    }
}

/// Decodes packets on the network side and hands PCM to the device callback.
pub struct AudioSink {
    jitter: Arc<Mutex<Jitter>>,
    decoder: opus::Decoder,
    next_seq: Option<u32>,
    pcm: Vec<f32>,
}

impl AudioSink {
    fn new(jitter: Arc<Mutex<Jitter>>) -> Result<Self> {
        Ok(Self {
            jitter,
            decoder: opus::Decoder::new(fmt::SAMPLE_RATE, opus::Channels::Stereo)?,
            next_seq: None,
            pcm: Vec::new(),
        })
    }

    pub fn push_packet(&mut self, seq: u32, packet: &[u8]) {
        let mut expected = self.next_seq.unwrap_or(seq);
        let gap = seq.wrapping_sub(expected);
        if gap > u32::MAX / 2 {
            return; // older than what we already played
        }
        // Conceal up to 50 ms of lost packets; beyond that just resync.
        if gap > 0 && gap <= 5 {
            while expected != seq {
                self.decode(&[]);
                expected = expected.wrapping_add(1);
            }
        }
        self.decode(packet);
        self.next_seq = Some(seq.wrapping_add(1));
    }

    fn decode(&mut self, packet: &[u8]) {
        // A real packet may carry up to 60 ms, so give it room. For a lost
        // packet (empty input) Opus synthesises exactly as much audio as the
        // buffer holds, so the buffer must be one 10 ms frame or timing drifts.
        let len = if packet.is_empty() { 1 } else { 6 } * fmt::FRAME_SAMPLES * fmt::CHANNELS;
        self.pcm.resize(len, 0.0);
        let Ok(frames) = self.decoder.decode_float(packet, &mut self.pcm, false) else {
            return;
        };
        let mut j = self.jitter.lock().unwrap();
        j.samples.extend(&self.pcm[..frames * fmt::CHANNELS]);
        j.tune();
    }
}

/// Opens the output device. The returned stream must be kept alive (and, as
/// cpal streams are not `Send`, on the thread that created it).
pub fn start(game_boost: Arc<AtomicBool>) -> Result<(cpal::Stream, AudioSink)> {
    let jitter = Arc::new(Mutex::new(Jitter {
        game_boost,
        ..Jitter::default()
    }));
    let device = cpal::default_host()
        .default_output_device()
        .context("no audio output device")?;
    let supported = device.default_output_config()?;
    let config: cpal::StreamConfig = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, &config, jitter.clone())?,
        SampleFormat::I16 => build::<i16>(&device, &config, jitter.clone())?,
        SampleFormat::I32 => build::<i32>(&device, &config, jitter.clone())?,
        SampleFormat::U16 => build::<u16>(&device, &config, jitter.clone())?,
        other => bail!("unsupported output sample format {other:?}"),
    };
    stream.play()?;
    Ok((stream, AudioSink::new(jitter)?))
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    jitter: Arc<Mutex<Jitter>>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let mut resampler = StereoResampler::new(fmt::SAMPLE_RATE, config.sample_rate);
    let mut stereo_in = Vec::new();
    let mut stereo_out: VecDeque<f32> = VecDeque::new();
    let mut scratch = Vec::new();
    let mut device_pcm = Vec::new();

    let stream = device.build_output_stream::<T, _, _>(
        *config,
        move |out: &mut [T], _| {
            let frames = out.len() / channels;
            {
                let mut j = jitter.lock().unwrap();
                let target = j.tune();
                if !j.playing && j.samples.len() >= target {
                    j.playing = true;
                }
                while j.playing && stereo_out.len() < frames * 2 {
                    let chunk = fmt::FRAME_SAMPLES * fmt::CHANNELS;
                    if j.samples.len() < chunk {
                        // Underrun: go quiet and rebuffer rather than crackle.
                        j.playing = false;
                        break;
                    }
                    stereo_in.clear();
                    stereo_in.extend(j.samples.drain(..chunk));
                    scratch.clear();
                    resampler.process(&stereo_in, &mut scratch);
                    stereo_out.extend(&scratch);
                }
            }
            let available = (stereo_out.len() / 2).min(frames);
            scratch.clear();
            scratch.extend(stereo_out.drain(..available * 2));
            device_pcm.clear();
            fmt::from_stereo(&scratch, channels, &mut device_pcm);
            for (o, s) in out.iter_mut().zip(&device_pcm) {
                *o = T::from_sample(*s);
            }
            // Anything not written stays silent: cpal pre-fills the buffer.
        },
        |e| tracing::warn!("audio playback error: {e}"),
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink() -> (AudioSink, Arc<Mutex<Jitter>>) {
        let jitter = Arc::new(Mutex::new(Jitter::default()));
        (AudioSink::new(jitter.clone()).unwrap(), jitter)
    }

    fn packets(n: usize) -> Vec<Vec<u8>> {
        let mut enc = opus::Encoder::new(
            fmt::SAMPLE_RATE,
            opus::Channels::Stereo,
            opus::Application::LowDelay,
        )
        .unwrap();
        let frame: Vec<f32> = (0..fmt::FRAME_SAMPLES * 2)
            .map(|i| ((i / 2) as f32 * 0.05).sin() * 0.3)
            .collect();
        (0..n)
            .map(|_| enc.encode_vec_float(&frame, 1500).unwrap())
            .collect()
    }

    fn buffered(j: &Arc<Mutex<Jitter>>) -> usize {
        j.lock().unwrap().samples.len()
    }

    #[test]
    fn small_gaps_are_concealed_to_keep_timing() {
        let (mut s, j) = sink();
        let p = packets(10);
        s.push_packet(0, &p[0]);
        s.push_packet(1, &p[1]);
        s.push_packet(4, &p[4]); // 2 and 3 lost
        assert_eq!(buffered(&j), 5 * fmt::FRAME_SAMPLES * fmt::CHANNELS);
    }

    #[test]
    fn late_packets_are_dropped_and_big_gaps_resync() {
        let (mut s, j) = sink();
        let p = packets(3);
        s.push_packet(100, &p[0]);
        s.push_packet(99, &p[1]); // late
        assert_eq!(buffered(&j), fmt::FRAME_SAMPLES * fmt::CHANNELS);
        s.push_packet(500, &p[2]); // host restarted numbering far ahead
        assert_eq!(buffered(&j), 2 * fmt::FRAME_SAMPLES * fmt::CHANNELS);
    }

    #[test]
    fn buffer_is_trimmed_after_a_stall() {
        let (mut s, j) = sink();
        for (seq, p) in packets(40).iter().enumerate() {
            s.push_packet(seq as u32, p); // 400 ms arriving at once
        }
        assert!(buffered(&j) <= samples_for_ms(audio_buffer_ms(false).1));
    }

    #[test]
    fn boost_trims_existing_audio_and_desktop_restores_jitter_budget() {
        let (mut s, j) = sink();
        let packets = packets(50);
        for (seq, p) in packets.iter().take(10).enumerate() {
            s.push_packet(seq as u32, p);
        }
        assert_eq!(buffered(&j), samples_for_ms(100));
        {
            let mut buffer = j.lock().unwrap();
            buffer.game_boost.store(true, Ordering::Relaxed);
            assert_eq!(buffer.tune(), samples_for_ms(20));
            assert_eq!(buffer.samples.len(), samples_for_ms(20));
        }
        for (seq, p) in packets.iter().enumerate().take(30).skip(10) {
            s.push_packet(seq as u32, p);
            assert!(buffered(&j) <= samples_for_ms(60));
        }
        j.lock().unwrap().game_boost.store(false, Ordering::Relaxed);
        for (seq, p) in packets.iter().enumerate().take(40).skip(30) {
            s.push_packet(seq as u32, p);
        }
        assert!(buffered(&j) > samples_for_ms(60));
        assert!(buffered(&j) <= samples_for_ms(150));
    }
}
