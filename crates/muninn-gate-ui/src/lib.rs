#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]
//! Platform-independent display and local UI rendering helpers.
//!
//! The crate keeps reusable UI pieces separate from screen layout code:
//! frames own fixed-capacity text buffers, components own small reusable
//! indicators, formatting owns row text helpers, and pages assemble complete
//! gateway screens.

/// Reusable display indicators and tiny animations.
pub mod components;
/// Text and telemetry formatting helpers.
pub mod format;
/// Fixed-capacity text frame types.
pub mod frame;
/// Reusable monochrome graphics primitives.
pub mod graphics;
/// Complete gateway screen renderers.
pub mod pages;

pub use components::{battery_fill_height, rssi_bars};
pub use format::{format_display_value, format_last_heard_age, format_producer_label};
pub use frame::{
    DISPLAY_LINE_CHARS,
    DisplaySize,
    NODE_ROW_NAME_CHARS,
    OLED_128X64_TEXT_LINES,
    OLED_128X128_TEXT_LINES,
    Oled128x64Frame,
    Oled128x128Frame,
    TextFrame,
    UI_SCRATCH_CHARS,
};
pub use graphics::{
    GraphicsError,
    Oled128x64Dashboard,
    draw_bar_indicator,
    draw_dashboard_128x64,
    draw_framed_notice_128x64,
    draw_poll_activity,
    draw_text_frame_128x64,
    draw_vertical_battery,
    draw_wifi_connecting_128x64,
};
pub use pages::{
    DisplayContext,
    DisplayPage,
    render_oled_128x64,
    render_oled_128x128,
    render_page,
};
