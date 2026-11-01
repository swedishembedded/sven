// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for speech input to agent
// interfaces for its clients. If your team needs expertise in audio capture
// across Linux, macOS and Windows then you can procure our services by
// sending an email to info@swedishembedded.com.
//! The default input device as a [`VoiceSource`]: 16 kHz mono f32 chunks.

use anyhow::{anyhow, Context};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use tokio::sync::mpsc;

use super::session::{Capture, VoiceSource};
use super::windower::SAMPLE_RATE;

/// Chunks the device callback may have queued before audio is dropped: about
/// half a minute at the chunk size below, far more than the session lags.
const QUEUE_CHUNKS: usize = 512;
/// Device samples gathered before they are resampled and passed on (100 ms).
const CHUNK_MS: u32 = 100;

/// Captures from the system's default input device.
pub(crate) struct DefaultMicrophone;

impl VoiceSource for DefaultMicrophone {
    fn start(&mut self) -> anyhow::Result<Capture> {
        let (frames_tx, frames) = mpsc::channel(QUEUE_CHUNKS);
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
        // cpal streams are not Send: the stream lives on a thread of its own
        // for as long as `stop_tx` (held by the capture) lives.
        std::thread::Builder::new()
            .name("sven-voice-capture".into())
            .spawn(move || match open(frames_tx) {
                Ok(stream) => {
                    let _ = ready_tx.send(Ok(()));
                    // Returns when the capture is dropped.
                    let _ = stop_rx.recv();
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .context("start the capture thread")?;
        ready_rx
            .recv()
            .map_err(|_| anyhow!("the capture thread ended before the microphone opened"))??;
        Ok(Capture::new(frames, Box::new(stop_tx)))
    }
}

/// Opens the default input device and starts it.
fn open(frames: mpsc::Sender<Vec<f32>>) -> anyhow::Result<cpal::Stream> {
    let device = cpal::default_host()
        .default_input_device()
        .ok_or_else(|| anyhow!("no microphone found (no default input device)"))?;
    let supported = device
        .default_input_config()
        .context("the microphone has no usable input format")?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, &config, frames),
        SampleFormat::I16 => build::<i16>(&device, &config, frames),
        SampleFormat::U16 => build::<u16>(&device, &config, frames),
        SampleFormat::I32 => build::<i32>(&device, &config, frames),
        other => Err(anyhow!("unsupported microphone sample format {other:?}")),
    }?;
    stream.play().context("start the microphone")?;
    Ok(stream)
}

fn build<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    frames: mpsc::Sender<Vec<f32>>,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels.max(1));
    let rate = config.sample_rate.0;
    let chunk = (rate * CHUNK_MS / 1000) as usize;
    let mut pending: Vec<f32> = Vec::with_capacity(chunk * 2);
    let stream = device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                for frame in data.chunks(channels) {
                    let sum: f32 = frame.iter().map(|s| s.to_sample::<f32>()).sum();
                    pending.push(sum / frame.len() as f32);
                }
                if pending.len() >= chunk {
                    let mono = sven_audio::resample_linear(&pending, rate, SAMPLE_RATE as u32);
                    pending.clear();
                    // A full queue means the session is far behind; losing
                    // audio beats blocking the device thread.
                    let _ = frames.try_send(mono);
                }
            },
            |e| tracing::warn!("microphone stream: {e}"),
            None,
        )
        .context("open the microphone stream")?;
    Ok(stream)
}
