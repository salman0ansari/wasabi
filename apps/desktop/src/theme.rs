//! Visual tokens for the desktop shell, derived from the design reference:
//! WhatsApp-style light theme on a warm off-white canvas.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use gpui::{Rgba, rgb, rgba};

// Accent family
pub const ACCENT: Rgba = rgb(0x25D366);
pub const ACCENT_TEXT: Rgba = rgb(0x128C7E);

// Surfaces
pub const CANVAS: Rgba = rgb(0xF0F2F5);
pub const SURFACE: Rgba = rgb(0xFFFFFF);
pub const NAV_RAIL: Rgba = rgb(0xF7F8F6);
pub const BORDER: Rgba = rgb(0xE2E7E1);

// Text
pub const TEXT_PRIMARY: Rgba = rgb(0x111B21);
pub const TEXT_SECONDARY: Rgba = rgb(0x667781);
pub const TEXT_ON_ACCENT: Rgba = rgb(0xFFFFFF);

// Bubbles
pub const BUBBLE_IN: Rgba = rgb(0xFFFFFF);
pub const BUBBLE_OUT: Rgba = rgb(0xD9FDD3);

// States
pub const ROW_SELECTED: Rgba = rgb(0xE4F4E0);
pub const ROW_HOVER: Rgba = rgba(0x111B210A);
pub const CHIP_IDLE: Rgba = rgb(0xEAEDEA);
pub const DANGER: Rgba = rgb(0xCC4B43);
pub const WARN: Rgba = rgb(0xD98E28);
pub const SKELETON: Rgba = rgb(0xE7EAE7);

/// Sender-name palette (purple/blue variants per the design reference).
const SENDER_PALETTE: [Rgba; 5] = [
    rgb(0x7C5CBF),
    rgb(0x3F7FBF),
    rgb(0xBF5F82),
    rgb(0x2F8F6F),
    rgb(0xB08A2E),
];

pub fn sender_color(seed: &str) -> Rgba {
    let mut hasher = DefaultHasher::default();
    seed.hash(&mut hasher);
    SENDER_PALETTE[(hasher.finish() % SENDER_PALETTE.len() as u64) as usize]
}

// Layout metrics
pub const TITLEBAR_H: f32 = 56.0;
pub const NAV_W: f32 = 68.0;
pub const CHAT_LIST_W: f32 = 340.0;
pub const RIGHT_PANEL_W: f32 = 300.0;
pub const CHAT_ROW_H: f32 = 72.0;
pub const DATE_CHIP_H: f32 = 34.0;
pub const BUBBLE_MAX_W: f32 = 560.0;

// Corner radii
pub const RADIUS_SM: f32 = 10.0;
pub const RADIUS_MD: f32 = 12.0;
pub const RADIUS_LG: f32 = 14.0;

// Type scale (px)
pub const TEXT_SIZE: f32 = 14.0;
pub const TEXT_SIZE_SM: f32 = 12.0;
pub const TEXT_NAME: f32 = 15.0;
