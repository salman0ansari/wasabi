//! Microphone capture via cpal. Encoding happens after stop, never in the callback.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, StreamConfig};

use super::encode::{
    self, EncodeError, EncodedVoice, SAMPLE_RATE, duration_seconds, f32_to_i16, resample_linear,
};

const MAX_SECONDS: u32 = 16 * 60;
const MIN_SAMPLES: usize = SAMPLE_RATE as usize / 5; // 200 ms

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordError {
    DeviceMissing,
    Permission,
    Encode,
    TooShort,
    Cancelled,
}

impl RecordError {
    pub fn user_message(self) -> &'static str {
        match self {
            Self::DeviceMissing => {
                "No microphone is available. Connect a microphone and allow wasabi to use it."
            }
            Self::Permission => {
                "Wasabi cannot access the microphone. Check input permissions and try again."
            }
            Self::Encode => "Could not encode the voice message. Try recording again.",
            Self::TooShort => "Recording was too short.",
            Self::Cancelled => "Recording discarded.",
        }
    }
}

impl From<EncodeError> for RecordError {
    fn from(_: EncodeError) -> Self {
        Self::Encode
    }
}

enum StopKind {
    Finish,
    Cancel,
}

struct CaptureBuffer {
    samples: Vec<f32>,
    error: Option<RecordError>,
}

pub struct Recorder {
    stop: Sender<StopKind>,
    done: Receiver<Result<RawCapture, RecordError>>,
    started_at: Instant,
    worker: Option<JoinHandle<()>>,
}

struct RawCapture {
    samples: Vec<f32>,
    sample_rate: u32,
}

impl Recorder {
    pub fn start() -> Result<Self, RecordError> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or(RecordError::DeviceMissing)?;
        let supported = device
            .default_input_config()
            .map_err(|_| RecordError::DeviceMissing)?;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let channels = config.channels.max(1) as usize;
        let sample_rate = config.sample_rate.0;
        if sample_rate == 0 {
            return Err(RecordError::DeviceMissing);
        }
        let max_samples = (MAX_SECONDS as usize).saturating_mul(sample_rate as usize);
        let buffer = Arc::new(Mutex::new(CaptureBuffer {
            samples: Vec::with_capacity(sample_rate as usize * 4),
            error: None,
        }));
        let (stop_tx, stop_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker_buffer = Arc::clone(&buffer);

        let worker = thread::Builder::new()
            .name("wasabi-voice-record".into())
            .spawn(move || {
                let built = match build_stream(
                    &device,
                    &config,
                    sample_format,
                    channels,
                    max_samples,
                    Arc::clone(&worker_buffer),
                ) {
                    Ok(stream) => stream,
                    Err(error) => {
                        let _ = done_tx.send(Err(error));
                        return;
                    }
                };
                if built.play().is_err() {
                    let _ = done_tx.send(Err(RecordError::Permission));
                    return;
                }
                let kind = stop_rx.recv().unwrap_or(StopKind::Cancel);
                drop(built);
                let (samples, error) = {
                    let mut captured = worker_buffer
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    (std::mem::take(&mut captured.samples), captured.error)
                };
                if let Some(error) = error {
                    let _ = done_tx.send(Err(error));
                    return;
                }
                match kind {
                    StopKind::Cancel => {
                        let _ = done_tx.send(Err(RecordError::Cancelled));
                    }
                    StopKind::Finish => {
                        let _ = done_tx.send(Ok(RawCapture {
                            samples,
                            sample_rate,
                        }));
                    }
                }
            })
            .map_err(|_| RecordError::DeviceMissing)?;

        Ok(Self {
            stop: stop_tx,
            done: done_rx,
            started_at: Instant::now(),
            worker: Some(worker),
        })
    }

    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    pub fn elapsed(&self) -> std::time::Duration {
        self.started_at.elapsed()
    }

    pub fn timed_out(&self) -> bool {
        self.elapsed().as_secs() >= u64::from(MAX_SECONDS)
    }

    pub fn finish(mut self) -> Result<EncodedVoice, RecordError> {
        let _ = self.stop.send(StopKind::Finish);
        let raw = self.recv_done()?;
        if raw.samples.len() < MIN_SAMPLES {
            return Err(RecordError::TooShort);
        }
        let resampled = resample_linear(&raw.samples, raw.sample_rate, SAMPLE_RATE);
        let pcm = f32_to_i16(&resampled);
        if pcm.len() < MIN_SAMPLES {
            return Err(RecordError::TooShort);
        }
        let bytes = encode::encode_pcm_to_ogg_opus(&pcm, SAMPLE_RATE)?;
        Ok(EncodedVoice {
            bytes,
            duration_seconds: duration_seconds(pcm.len(), SAMPLE_RATE).max(1),
        })
    }

    pub fn cancel(mut self) {
        let _ = self.stop.send(StopKind::Cancel);
        let _ = self.recv_done();
    }

    fn recv_done(&mut self) -> Result<RawCapture, RecordError> {
        let result = self.done.recv().unwrap_or(Err(RecordError::Cancelled));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        result
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.stop.send(StopKind::Cancel);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    channels: usize,
    max_samples: usize,
    buffer: Arc<Mutex<CaptureBuffer>>,
) -> Result<cpal::Stream, RecordError> {
    let err_buffer = Arc::clone(&buffer);
    let on_error = move |error: cpal::StreamError| {
        let mapped = map_stream_error(&error);
        if let Ok(mut guard) = err_buffer.lock() {
            guard.error = Some(mapped);
        }
    };
    match sample_format {
        SampleFormat::F32 => {
            open_stream::<f32>(device, config, channels, max_samples, buffer, on_error)
        }
        SampleFormat::I16 => {
            open_stream::<i16>(device, config, channels, max_samples, buffer, on_error)
        }
        SampleFormat::U16 => {
            open_stream::<u16>(device, config, channels, max_samples, buffer, on_error)
        }
        SampleFormat::I32 => {
            open_stream::<i32>(device, config, channels, max_samples, buffer, on_error)
        }
        SampleFormat::U32 => {
            open_stream::<u32>(device, config, channels, max_samples, buffer, on_error)
        }
        SampleFormat::F64 => {
            open_stream::<f64>(device, config, channels, max_samples, buffer, on_error)
        }
        SampleFormat::I8 => {
            open_stream::<i8>(device, config, channels, max_samples, buffer, on_error)
        }
        SampleFormat::U8 => {
            open_stream::<u8>(device, config, channels, max_samples, buffer, on_error)
        }
        SampleFormat::I64 => {
            open_stream::<i64>(device, config, channels, max_samples, buffer, on_error)
        }
        SampleFormat::U64 => {
            open_stream::<u64>(device, config, channels, max_samples, buffer, on_error)
        }
        _ => Err(RecordError::DeviceMissing),
    }
}

fn open_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: usize,
    max_samples: usize,
    buffer: Arc<Mutex<CaptureBuffer>>,
    on_error: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, RecordError>
where
    T: Sample + cpal::SizedSample + Send + 'static,
    f32: cpal::FromSample<T>,
{
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let Ok(mut guard) = buffer.lock() else {
                    return;
                };
                if guard.error.is_some() || guard.samples.len() >= max_samples {
                    return;
                }
                for frame in data.chunks(channels) {
                    if guard.samples.len() >= max_samples {
                        break;
                    }
                    let mono = if frame.len() == 1 {
                        frame[0].to_sample::<f32>()
                    } else if frame.is_empty() {
                        0.0
                    } else {
                        frame.iter().map(|sample| sample.to_sample::<f32>()).sum::<f32>()
                            / frame.len() as f32
                    };
                    guard.samples.push(mono);
                }
            },
            on_error,
            None,
        )
        .map_err(map_build_error)
}

fn map_build_error(error: cpal::BuildStreamError) -> RecordError {
    match error {
        cpal::BuildStreamError::DeviceNotAvailable => RecordError::DeviceMissing,
        cpal::BuildStreamError::StreamConfigNotSupported => RecordError::DeviceMissing,
        cpal::BuildStreamError::BackendSpecific { err } => {
            let message = err.to_string().to_ascii_lowercase();
            if message.contains("permission") || message.contains("denied") || message.contains("busy")
            {
                RecordError::Permission
            } else {
                RecordError::DeviceMissing
            }
        }
        _ => RecordError::DeviceMissing,
    }
}

fn map_stream_error(error: &cpal::StreamError) -> RecordError {
    match error {
        cpal::StreamError::DeviceNotAvailable => RecordError::DeviceMissing,
        cpal::StreamError::BackendSpecific { err } => {
            let message = err.to_string().to_ascii_lowercase();
            if message.contains("permission") || message.contains("denied") {
                RecordError::Permission
            } else {
                RecordError::DeviceMissing
            }
        }
    }
}
