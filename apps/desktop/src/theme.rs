//! Semantic visual tokens for the native desktop shell.
//!
//! The palette and geometry are measured from the current WhatsApp Web
//! experience, while the accent and product identity remain Wasabi's own.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{App, BoxShadow, Rgba, point, px};

/// WhatsApp Web currently uses Roboto Variable. GPUI resolves the installed
/// Roboto family and falls back through the platform text system when absent;
/// bundled Inter remains available to the component library as a safe fallback.
pub const UI_FONT_FAMILY: &str = "Roboto";

/// Apply the selected component theme and restore wasabi's bundled typeface.
/// gpui-component theme changes reset typography to the theme default, so the
/// font family must be reapplied whenever the appearance changes.
pub fn apply_component_theme(mode: gpui_component::theme::ThemeMode, cx: &mut App) {
    set_dark_mode(mode.is_dark());
    gpui_component::Theme::change(mode, None, cx);
    cx.global_mut::<gpui_component::Theme>().font_family = UI_FONT_FAMILY.into();
}

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

const ACCENT: Rgba = rgb_const(0x1B8755);
const ACCENT_TEXT: Rgba = rgb_const(0x126B46);

// Surfaces
const CANVAS: Rgba = rgb_const(0xF5F1EB);
const SURFACE: Rgba = rgb_const(0xFFFFFF);
const SURFACE_ELEVATED: Rgba = rgb_const(0xF7F5F3);
const SURFACE_EMPHASIZED: Rgba = rgb_const(0xF1EEEB);
const WALLPAPER: Rgba = rgb_const(0xEAE0D3);
const NAV_RAIL: Rgba = rgb_const(0xF7F5F3);
const BORDER: Rgba = rgba_const(0x0000001A);

// Text
const TEXT_PRIMARY: Rgba = rgb_const(0x0A0A0A);
const TEXT_SECONDARY: Rgba = rgba_const(0x00000099);
const TEXT_ON_ACCENT: Rgba = rgb_const(0xFFFFFF);

// Bubbles
const BUBBLE_IN: Rgba = rgb_const(0xFFFFFF);
const BUBBLE_OUT: Rgba = rgb_const(0xD9FDD3);

// States
const ROW_SELECTED: Rgba = rgb_const(0xF1EEEB);
const ROW_HOVER: Rgba = rgba_const(0xC2BDB826);
const CHIP_IDLE: Rgba = rgb_const(0xF7F5F3);
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
    if dark_mode() {
        rgb_const(0x21C063)
    } else {
        ACCENT
    }
}

pub fn accent_text() -> Rgba {
    if dark_mode() {
        rgb_const(0x53D995)
    } else {
        ACCENT_TEXT
    }
}

pub fn canvas() -> Rgba {
    if dark_mode() {
        rgb_const(0x161717)
    } else {
        CANVAS
    }
}

pub fn surface() -> Rgba {
    if dark_mode() {
        rgb_const(0x161717)
    } else {
        SURFACE
    }
}

pub fn nav_rail() -> Rgba {
    if dark_mode() {
        rgb_const(0x1D1F1F)
    } else {
        NAV_RAIL
    }
}

pub fn border() -> Rgba {
    if dark_mode() {
        rgba_const(0xFFFFFF1A)
    } else {
        BORDER
    }
}

pub fn text_primary() -> Rgba {
    if dark_mode() {
        rgb_const(0xFAFAFA)
    } else {
        TEXT_PRIMARY
    }
}

pub fn text_secondary() -> Rgba {
    if dark_mode() {
        rgba_const(0xFFFFFF99)
    } else {
        TEXT_SECONDARY
    }
}

pub fn text_on_accent() -> Rgba {
    TEXT_ON_ACCENT
}

pub fn bubble_in() -> Rgba {
    if dark_mode() {
        rgb_const(0x242626)
    } else {
        BUBBLE_IN
    }
}

pub fn bubble_out() -> Rgba {
    if dark_mode() {
        rgb_const(0x144D37)
    } else {
        BUBBLE_OUT
    }
}

pub fn row_selected() -> Rgba {
    if dark_mode() {
        rgba_const(0xFFFFFF1A)
    } else {
        ROW_SELECTED
    }
}

pub fn row_hover() -> Rgba {
    if dark_mode() {
        rgba_const(0xFFFFFF12)
    } else {
        ROW_HOVER
    }
}

pub fn chip_idle() -> Rgba {
    if dark_mode() {
        rgb_const(0x242626)
    } else {
        CHIP_IDLE
    }
}

pub fn danger() -> Rgba {
    if dark_mode() {
        rgb_const(0xFF6B6B)
    } else {
        DANGER
    }
}

pub fn warn() -> Rgba {
    if dark_mode() {
        rgb_const(0xC9861A)
    } else {
        WARN
    }
}

pub fn skeleton() -> Rgba {
    if dark_mode() {
        rgb_const(0x323434)
    } else {
        SKELETON
    }
}

pub fn scrim() -> Rgba {
    if dark_mode() {
        rgba_const(0x00000052)
    } else {
        SCRIM
    }
}

/// Elevated neutral used by trays, menus, selected filters, and nested cards.
pub fn surface_elevated() -> Rgba {
    if dark_mode() {
        rgb_const(0x1D1F1F)
    } else {
        SURFACE_ELEVATED
    }
}

/// Stronger neutral used by the composer, incoming bubbles, and pressed cards.
pub fn surface_emphasized() -> Rgba {
    if dark_mode() {
        rgb_const(0x242626)
    } else {
        SURFACE_EMPHASIZED
    }
}

pub fn wallpaper() -> Rgba {
    if dark_mode() {
        rgb_const(0x161717)
    } else {
        WALLPAPER
    }
}

pub fn composer_surface() -> Rgba {
    if dark_mode() {
        rgb_const(0x242626)
    } else {
        SURFACE
    }
}

/// Nested quote/media overlay that stays legible on both bubble directions.
pub fn bubble_overlay() -> Rgba {
    if dark_mode() {
        rgba_const(0x00000033)
    } else {
        rgba_const(0xC2BDB826)
    }
}

pub fn read_receipt() -> Rgba {
    if dark_mode() {
        rgb_const(0x76B8FE)
    } else {
        rgb_const(0x007BFC)
    }
}

pub fn action_surface() -> Rgba {
    if dark_mode() {
        rgb_const(0xFAFAFA)
    } else {
        rgb_const(0x171616)
    }
}

pub fn action_content() -> Rgba {
    if dark_mode() {
        rgb_const(0x0A0A0A)
    } else {
        rgb_const(0xFFFFFF)
    }
}

fn shadow(offset_y: f32, blur: f32, spread: f32, alpha: u8) -> BoxShadow {
    BoxShadow {
        offset: point(px(0.0), px(offset_y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
        inset: false,
        color: rgba_const(u32::from(alpha)).into(),
    }
}

pub fn header_shadow() -> Vec<BoxShadow> {
    vec![shadow(1.0, 4.0, 0.0, 0x1F)]
}

pub fn bubble_shadow() -> Vec<BoxShadow> {
    vec![shadow(1.0, 0.5, 0.0, 0x21)]
}

pub fn composer_shadow() -> Vec<BoxShadow> {
    vec![shadow(1.0, 6.0, 0.0, 0x1F)]
}

pub fn overlay_shadow() -> Vec<BoxShadow> {
    vec![shadow(12.0, 16.0, -4.0, 0x1A)]
}

pub fn modal_shadow() -> Vec<BoxShadow> {
    vec![shadow(2.0, 18.0, 0.0, 0x42), shadow(8.0, 10.0, 0.0, 0x1A)]
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
pub const NAV_W: f32 = 64.0;
pub const CHAT_LIST_W: f32 = 431.0;
pub const RIGHT_PANEL_W: f32 = 432.0;
pub const HEADER_H: f32 = 64.0;
pub const ACTION_SIZE: f32 = 40.0;
pub const CHAT_ROW_H: f32 = 76.0;
pub const CHAT_ROW_CARD_H: f32 = 72.0;
pub const BUBBLE_MAX_W: f32 = 560.0;

// Corner radii
pub const RADIUS_SM: f32 = 7.5;
pub const RADIUS_MD: f32 = 12.0;
pub const RADIUS_LG: f32 = 16.0;
pub const RADIUS_MODAL: f32 = 18.0;
pub const RADIUS_COMPOSER: f32 = 26.0;

// Type scale (px)
pub const TEXT_SIZE: f32 = 14.0;
pub const TEXT_SIZE_SM: f32 = 11.0;
pub const TEXT_PREVIEW: f32 = 14.0;
pub const TEXT_COMPOSER: f32 = 15.0;
pub const TEXT_NAME: f32 = 16.0;
pub const TEXT_TITLE: f32 = 22.0;

// Motion policy. High-frequency navigation remains immediate; these values
// are for press feedback and occasional overlays only.
pub const MOTION_FAST_MS: u64 = 80;
pub const MOTION_PRESS_MS: u64 = MOTION_FAST_MS + 40;
pub const MOTION_STANDARD_MS: u64 = MOTION_PRESS_MS + 30;
pub const MOTION_OVERLAY_MS: u64 = MOTION_STANDARD_MS + 90;

pub fn scaled_text(base: f32, scale: u16) -> f32 {
    base * f32::from(scale.clamp(100, 150)) / 100.0
}
