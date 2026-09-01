//! One-at-a-time PCM playback on a dedicated output thread.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rodio::buffer::SamplesBuffer;
use rodio::{Decoder, OutputStream, Sink, Source};

use super::encode;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PlaybackKey {
    pub chat: String,
    pub media: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayError {
    DeviceMissing,
    Decode,
}

impl PlayError {
    pub fn user_message(self) -> &'static str {
        match self {
            Self::DeviceMissing => "No playback device is available.",
            Self::Decode => "Could not play this voice message.",
        }
    }
}

enum Command {
    Play {
        key: PlaybackKey,
        samples: Vec<f32>,
        sample_rate: u32,
    },
    Pause,
    Resume,
    Stop,
    Shutdown,
}

struct Shared {
    key: Option<PlaybackKey>,
    playing: bool,
    finished: bool,
}

pub struct Player {
    commands: Mutex<Option<Sender<Command>>>,
    shared: Arc<Mutex<Shared>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Player {
    pub fn new() -> Self {
        Self {
            commands: Mutex::new(None),
            shared: Arc::new(Mutex::new(Shared {
                key: None,
                playing: false,
                finished: false,
            })),
            worker: Mutex::new(None),
        }
    }

    pub fn play_file(&self, key: PlaybackKey, path: &Path) -> Result<Duration, PlayError> {
        let (samples, sample_rate) = load_samples(path)?;
        if samples.is_empty() {
            return Err(PlayError::Decode);
        }
        let duration = Duration::from_secs_f32(samples.len() as f32 / sample_rate.max(1) as f32);
        self.ensure_thread()?;
        self.send(Command::Play {
            key,
            samples,
            sample_rate,
        });
        Ok(duration)
    }

    pub fn pause(&self) {
        self.send(Command::Pause);
    }

    pub fn resume(&self) {
        self.send(Command::Resume);
    }

    pub fn stop(&self) {
        self.send(Command::Stop);
        if let Ok(mut shared) = self.shared.lock() {
            shared.key = None;
            shared.playing = false;
            shared.finished = false;
        }
    }

    pub fn is_current(&self, chat: &str, media: &str) -> bool {
        self.shared.lock().ok().is_some_and(|shared| {
            shared.key.as_ref().is_some_and(|key| key.chat == chat && key.media == media)
        })
    }

    pub fn is_playing(&self, chat: &str, media: &str) -> bool {
        self.shared.lock().ok().is_some_and(|shared| {
            shared.playing
                && shared
                    .key
                    .as_ref()
                    .is_some_and(|key| key.chat == chat && key.media == media)
        })
    }

    pub fn take_finished(&self) -> Option<PlaybackKey> {
        let Ok(mut shared) = self.shared.lock() else {
            return None;
        };
        if shared.finished {
            shared.finished = false;
            shared.playing = false;
            shared.key.clone()
        } else {
            None
        }
    }

    fn ensure_thread(&self) -> Result<(), PlayError> {
        let mut worker = self.worker.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if worker.is_some() {
            return Ok(());
        }
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let shared = Arc::clone(&self.shared);
        let handle = thread::Builder::new()
            .name("wasabi-voice-play".into())
            .spawn(move || playback_loop(rx, shared, ready_tx))
            .map_err(|_| PlayError::DeviceMissing)?;
        let ready = ready_rx.recv().unwrap_or(false);
        if !ready {
            let _ = tx.send(Command::Shutdown);
            let _ = handle.join();
            return Err(PlayError::DeviceMissing);
        }
        *self
            .commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tx);
        *worker = Some(handle);
        Ok(())
    }

    fn send(&self, command: Command) {
        let commands = self.commands.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(tx) = commands.as_ref() {
            let _ = tx.send(command);
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.send(Command::Shutdown);
        if let Ok(mut worker) = self.worker.lock()
            && let Some(handle) = worker.take()
        {
            let _ = handle.join();
        }
    }
}

fn playback_loop(
    rx: mpsc::Receiver<Command>,
    shared: Arc<Mutex<Shared>>,
    ready: mpsc::Sender<bool>,
) {
    let Ok((_stream, handle)) = OutputStream::try_default() else {
        let _ = ready.send(false);
        return;
    };
    let _ = ready.send(true);
    let mut sink: Option<Sink> = None;
    loop {
        let command = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(command) => Some(command),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if let Some(command) = command {
            match command {
                Command::Play {
                    key,
                    samples,
                    sample_rate,
                } => {
                    if let Some(current) = sink.take() {
                        current.stop();
                    }
                    match Sink::try_new(&handle) {
                        Ok(next) => {
                            next.append(SamplesBuffer::new(1, sample_rate, samples));
                            next.play();
                            if let Ok(mut state) = shared.lock() {
                                state.key = Some(key);
                                state.playing = true;
                                state.finished = false;
                            }
                            sink = Some(next);
                        }
                        Err(_) => {
                            if let Ok(mut state) = shared.lock() {
                                state.key = None;
                                state.playing = false;
                                state.finished = false;
                            }
                        }
                    }
                }
                Command::Pause => {
                    if let Some(current) = sink.as_ref() {
                        current.pause();
                    }
                    if let Ok(mut state) = shared.lock() {
                        state.playing = false;
                    }
                }
                Command::Resume => {
                    if let Some(current) = sink.as_ref() {
                        current.play();
                    }
                    if let Ok(mut state) = shared.lock() {
                        state.playing = true;
                        state.finished = false;
                    }
                }
                Command::Stop => {
                    if let Some(current) = sink.take() {
                        current.stop();
                    }
                    if let Ok(mut state) = shared.lock() {
                        state.key = None;
                        state.playing = false;
                        state.finished = false;
                    }
                }
                Command::Shutdown => break,
            }
        }
        if let Some(current) = sink.as_ref()
            && current.empty()
        {
            if let Ok(mut state) = shared.lock() {
                state.playing = false;
                state.finished = state.key.is_some();
            }
            sink = None;
        }
    }
}

fn load_samples(path: &Path) -> Result<(Vec<f32>, u32), PlayError> {
    let bytes = std::fs::read(path).map_err(|_| PlayError::Decode)?;
    if encode::is_ogg_opus(&bytes) {
        let decoded = encode::decode_ogg_opus(&bytes).map_err(|_| PlayError::Decode)?;
        let samples = decoded
            .samples
            .iter()
            .map(|sample| *sample as f32 / 32768.0)
            .collect();
        return Ok((samples, decoded.sample_rate.max(1)));
    }
    let file = File::open(path).map_err(|_| PlayError::Decode)?;
    let decoder = Decoder::new(BufReader::new(file)).map_err(|_| PlayError::Decode)?;
    let sample_rate = decoder.sample_rate();
    let channels = decoder.channels().max(1) as usize;
    let interleaved: Vec<f32> = decoder.convert_samples::<f32>().collect();
    if channels == 1 {
        return Ok((interleaved, sample_rate.max(1)));
    }
    let mut mono = Vec::with_capacity(interleaved.len() / channels);
    for frame in interleaved.chunks(channels) {
        let sum: f32 = frame.iter().sum();
        mono.push(sum / channels as f32);
    }
    Ok((mono, sample_rate.max(1)))
}
