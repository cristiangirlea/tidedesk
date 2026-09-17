//! Audio: Opus datagrams → jitter buffer → default output device.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use tidedesk_core::audio::{self as fmt, StereoResampler};

/// Samples buffered before playback (re)starts. Absorbs network jitter.
const TARGET_MS: usize = 40;
/// Beyond this the buffer is trimmed back, bounding latency after a stall or
/// when the host's sound card clock runs slightly faster than ours.
const MAX_MS: usize = 150;

fn samples_for_ms(ms: usize) -> usize {
    fmt::SAMPLE_RATE as usize * fmt::CHANNELS * ms / 1000
}

#[derive(Default)]
struct Jitter {
    samples: VecDeque<f32>,
    playing: bool,
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
        let max = samples_for_ms(MAX_MS);
        if j.samples.len() > max {
            let excess = j.samples.len() - samples_for_ms(TARGET_MS);
            j.samples.drain(..excess & !1);
        }
    }
}

/// Opens the output device. The returned stream must be kept alive (and, as
/// cpal streams are not `Send`, on the thread that created it).
pub fn start() -> Result<(cpal::Stream, AudioSink)> {
    let jitter = Arc::new(Mutex::new(Jitter::default()));
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
                if !j.playing && j.samples.len() >= samples_for_ms(TARGET_MS) {
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
        assert!(buffered(&j) <= samples_for_ms(MAX_MS));
    }
}
