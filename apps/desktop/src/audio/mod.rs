//! Local voice-note capture, Opus encoding, and playback.

mod capture;
mod encode;
mod playback;

pub use capture::{RecordError, Recorder};
pub use playback::{PlaybackKey, Player};

pub fn format_clock(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
