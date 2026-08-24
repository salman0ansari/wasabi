//! Visual tokens for the desktop shell, derived from the design reference:
//! WhatsApp-style light theme on a warm off-white canvas.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::Rgba;

// Accent family

/// gpui's `rgb()` is not const at the pinned rev; colors are declared as
/// consts, so this replicates it as one.
pub const fn rgb_const(hex: u32) -> Rgba {
    rgba_const(hex << 8 | 0xFF)
}

/// Same shape as gpui's non-const `rgba()`.
pub const fn rgba_const(hex: u32) -> Rgba {
    let [r, g, b, a] = hex.to_be_bytes();
    Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: a as f32 / 255.0,
    }
}

const ACCENT: Rgba = rgb_const(0x1FA855);
const ACCENT_TEXT: Rgba = rgb_const(0x087A50);

// Surfaces
const CANVAS: Rgba = rgb_const(0xF3F5F6);
const SURFACE: Rgba = rgb_const(0xFFFFFF);
const NAV_RAIL: Rgba = rgb_const(0xF7F8F9);
const BORDER: Rgba = rgb_const(0xE4E7EA);

// Text
const TEXT_PRIMARY: Rgba = rgb_const(0x111B21);
const TEXT_SECONDARY: Rgba = rgb_const(0x667781);
const TEXT_ON_ACCENT: Rgba = rgb_const(0xFFFFFF);

// Bubbles
const BUBBLE_IN: Rgba = rgb_const(0xFFFFFF);
const BUBBLE_OUT: Rgba = rgb_const(0xD9FDD3);

// States
const ROW_SELECTED: Rgba = rgb_const(0xEEF0F2);
const ROW_HOVER: Rgba = rgba_const(0x111B210A);
const CHIP_IDLE: Rgba = rgb_const(0xF0F2F3);
const DANGER: Rgba = rgb_const(0xCC4B43);
const WARN: Rgba = rgb_const(0xD98E28);
const SKELETON: Rgba = rgb_const(0xE7EAE7);
const SCRIM: Rgba = rgba_const(0x111B2140);

static DARK_MODE: AtomicBool = AtomicBool::new(false);

pub fn set_dark_mode(dark: bool) {
    DARK_MODE.store(dark, Ordering::Release);
}

fn dark_mode() -> bool {
    DARK_MODE.load(Ordering::Acquire)
}

pub fn accent() -> Rgba {
    if dark_mode() { rgb_const(0x21C063) } else { ACCENT }
}

pub fn accent_text() -> Rgba {
    if dark_mode() { rgb_const(0x53D995) } else { ACCENT_TEXT }
}

pub fn canvas() -> Rgba {
    if dark_mode() { rgb_const(0x0B141A) } else { CANVAS }
}

pub fn surface() -> Rgba {
    if dark_mode() { rgb_const(0x111B21) } else { SURFACE }
}

pub fn nav_rail() -> Rgba {
    if dark_mode() { rgb_const(0x0F191F) } else { NAV_RAIL }
}

pub fn border() -> Rgba {
    if dark_mode() { rgb_const(0x27343B) } else { BORDER }
}

pub fn text_primary() -> Rgba {
    if dark_mode() { rgb_const(0xE9EDEF) } else { TEXT_PRIMARY }
}

pub fn text_secondary() -> Rgba {
    if dark_mode() { rgb_const(0xAEBAC1) } else { TEXT_SECONDARY }
}

pub fn text_on_accent() -> Rgba {
    TEXT_ON_ACCENT
}

pub fn bubble_in() -> Rgba {
    if dark_mode() { rgb_const(0x202C33) } else { BUBBLE_IN }
}

pub fn bubble_out() -> Rgba {
    if dark_mode() { rgb_const(0x005C4B) } else { BUBBLE_OUT }
}

pub fn row_selected() -> Rgba {
    if dark_mode() { rgb_const(0x2A3942) } else { ROW_SELECTED }
}

pub fn row_hover() -> Rgba {
    if dark_mode() { rgba_const(0xFFFFFF0B) } else { ROW_HOVER }
}

pub fn chip_idle() -> Rgba {
    if dark_mode() { rgb_const(0x202C33) } else { CHIP_IDLE }
}

pub fn danger() -> Rgba {
    if dark_mode() { rgb_const(0xFF6B6B) } else { DANGER }
}

pub fn warn() -> Rgba {
    if dark_mode() { rgb_const(0xC9861A) } else { WARN }
}

pub fn skeleton() -> Rgba {
    if dark_mode() { rgb_const(0x2A3942) } else { SKELETON }
}

pub fn scrim() -> Rgba {
    if dark_mode() { rgba_const(0x00000070) } else { SCRIM }
}

/// Sender-name palette (purple/blue variants per the design reference).
const SENDER_PALETTE: [Rgba; 5] = [
    rgb_const(0x7C5CBF),
    rgb_const(0x3F7FBF),
    rgb_const(0xBF5F82),
    rgb_const(0x2F8F6F),
    rgb_const(0xB08A2E),
];

pub fn sender_color(seed: &str) -> Rgba {
    let mut hasher = DefaultHasher::default();
    seed.hash(&mut hasher);
    SENDER_PALETTE[(hasher.finish() % SENDER_PALETTE.len() as u64) as usize]
}

// Layout metrics
pub const NAV_W: f32 = 56.0;
pub const CHAT_LIST_W: f32 = 382.0;
pub const RIGHT_PANEL_W: f32 = 380.0;
pub const CHAT_ROW_H: f32 = 74.0;
pub const BUBBLE_MAX_W: f32 = 560.0;

// Corner radii
pub const RADIUS_SM: f32 = 6.0;
pub const RADIUS_MD: f32 = 8.0;
pub const RADIUS_LG: f32 = 8.0;

// Type scale (px)
pub const TEXT_SIZE: f32 = 14.0;
pub const TEXT_SIZE_SM: f32 = 12.0;
pub const TEXT_NAME: f32 = 15.0;
pub const TEXT_TITLE: f32 = 24.0;

pub fn scaled_text(base: f32, scale: u16) -> f32 {
    base * f32::from(scale.clamp(100, 150)) / 100.0
}
