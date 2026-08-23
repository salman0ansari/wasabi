//! Visual tokens for the desktop shell, derived from the design reference:
//! WhatsApp-style light theme on a warm off-white canvas.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

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

pub const ACCENT: Rgba = rgb_const(0x25D366);
pub const ACCENT_TEXT: Rgba = rgb_const(0x128C7E);

// Surfaces
pub const CANVAS: Rgba = rgb_const(0xF0F2F5);
pub const SURFACE: Rgba = rgb_const(0xFFFFFF);
pub const NAV_RAIL: Rgba = rgb_const(0xF7F8F6);
pub const BORDER: Rgba = rgb_const(0xE2E7E1);

// Text
pub const TEXT_PRIMARY: Rgba = rgb_const(0x111B21);
pub const TEXT_SECONDARY: Rgba = rgb_const(0x667781);
pub const TEXT_ON_ACCENT: Rgba = rgb_const(0xFFFFFF);

// Bubbles
pub const BUBBLE_IN: Rgba = rgb_const(0xFFFFFF);
pub const BUBBLE_OUT: Rgba = rgb_const(0xD9FDD3);

// States
pub const ROW_SELECTED: Rgba = rgb_const(0xE4F4E0);
pub const ROW_HOVER: Rgba = rgba_const(0x111B210A);
pub const CHIP_IDLE: Rgba = rgb_const(0xEAEDEA);
pub const DANGER: Rgba = rgb_const(0xCC4B43);
pub const WARN: Rgba = rgb_const(0xD98E28);
pub const SKELETON: Rgba = rgb_const(0xE7EAE7);

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
