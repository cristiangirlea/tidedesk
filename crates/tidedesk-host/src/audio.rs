//! System audio capture (loopback of the default output device) → Opus.
//!
//! On Windows, cpal opens a WASAPI loopback stream when an *input* stream is
//! built on an *output* device. WASAPI delivers nothing while no application
//! plays sound, so a silent host spends no CPU here.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use tidedesk_core::audio::{self as fmt, StereoResampler};

/// Starts loopback capture; encoded Opus packets are offered to `packets`
/// without blocking and dropped if the network task is behind.
pub fn start(packets: tokio::sync::mpsc::Sender<Vec<u8>>, stop: Arc<AtomicBool>) -> Result<()> {
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            // cpal streams are not `Send`, so the stream lives on this thread.
            let stream = match open_stream(packets) {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
            }
            drop(stream);
        })?;
    ready_rx
        .recv()
        .context("audio thread exited during start-up")?
}

fn open_stream(packets: tokio::sync::mpsc::Sender<Vec<u8>>) -> Result<cpal::Stream> {
    let device = cpal::default_host()
        .default_output_device()
        .context("no audio output device to capture")?;
    let supported = device.default_output_config()?;
    let config: cpal::StreamConfig = supported.config();
    tracing::info!(
        "capturing system audio: {} Hz, {} ch, {:?}",
        config.sample_rate,
        config.channels,
        supported.sample_format()
    );
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, &config, packets)?,
        SampleFormat::I16 => build::<i16>(&device, &config, packets)?,
        SampleFormat::I32 => build::<i32>(&device, &config, packets)?,
        SampleFormat::U16 => build::<u16>(&device, &config, packets)?,
        other => bail!("unsupported loopback sample format {other:?}"),
    };
    stream.play()?;
    Ok(stream)
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    packets: tokio::sync::mpsc::Sender<Vec<u8>>,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    let mut encoder = opus::Encoder::new(
        fmt::SAMPLE_RATE,
        opus::Channels::Stereo,
        opus::Application::LowDelay,
    )?;
    encoder.set_bitrate(opus::Bitrate::Bits(fmt::DEFAULT_BITRATE))?;
    encoder.set_inband_fec(true)?;
    encoder.set_packet_loss_perc(5)?;

    let mut resampler = StereoResampler::new(config.sample_rate, fmt::SAMPLE_RATE);
    let mut float = Vec::new();
    let mut stereo = Vec::new();
    let mut pcm = Vec::with_capacity(fmt::FRAME_SAMPLES * fmt::CHANNELS * 4);
    let mut out = [0u8; 1500];

    let stream = device.build_input_stream::<T, _, _>(
        *config,
        move |data: &[T], _| {
            float.clear();
            float.extend(data.iter().map(|s| s.to_sample::<f32>()));
            stereo.clear();
            fmt::to_stereo(&float, channels, &mut stereo);
            resampler.process(&stereo, &mut pcm);

            let frame = fmt::FRAME_SAMPLES * fmt::CHANNELS;
            let mut consumed = 0;
            while pcm.len() - consumed >= frame {
                if let Ok(n) = encoder.encode_float(&pcm[consumed..consumed + frame], &mut out) {
                    // Full channel = network behind; dropping beats queueing.
                    let _ = packets.try_send(out[..n].to_vec());
                }
                consumed += frame;
            }
            pcm.drain(..consumed);
        },
        |e| tracing::warn!("audio capture error: {e}"),
        None,
    )?;
    Ok(stream)
}
