//! Post-association operational screen — mirrors `Producers` page in
//! `muninn-gate-ui::pages` once we have a LoRa radio, and shows a clear
//! "LoRa not wired" panel while we don't.
//!
//! Layouts:
//!
//! - **No LoRa** (current Bifrost ProS3 milestone): full-body warning panel — triangle icon +
//!   headline + dim subtitle. Producer rows are suppressed because we can't poll anything without
//!   the radio.
//! - **With LoRa** (future): network status row + per-producer rows with small status glyphs
//!   (pending / online / offline / disabled).

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::Gray4;
use embedded_graphics::primitives::{
    Line,
    PrimitiveStyleBuilder,
    Rectangle,
    StyledDrawable,
    Triangle,
};
use embedded_graphics::{Drawable, Pixel};
use heapless::String;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use super::{
    FONT_BODY,
    FONT_HEADLINE,
    FONT_TINY,
    LUMA_ACCENT,
    LUMA_BG,
    LUMA_DIM,
    LUMA_INACTIVE,
    LUMA_TEXT,
    NetworkPhase,
    PollStatus,
    ProducerSummary,
    UiState,
    header,
};

/// Maximum producer rows we attempt to draw on screen (only used when
/// `lora_available` is `true`).
const MAX_VISIBLE_PRODUCERS: usize = 4;
/// Vertical pitch between producer rows.
const PRODUCER_ROW_HEIGHT: i32 = 14;
/// First-row top Y. Lifted off the divider (which sits at
/// `body_top + 18`) by 14 px so there's a clear breathing gap between
/// the IP headline and the per-node rows instead of feeling crammed.
const PRODUCER_LIST_TOP_OFFSET: i32 = 32;
/// Left margin of the per-row signal indicator.
const SIGNAL_X: i32 = 2;
/// Vertical offset of the signal glyph from the row top — nudges the
/// 7-px-tall bars one px below the geometric center so the bottom of
/// the bars aligns with the text baseline.
const SIGNAL_Y_OFFSET: i32 = 4;
/// Left edge of the producer name (7 px gap past the 8-px signal glyph).
const NAME_X: i32 = 17;
/// Right edge of the age cell (`5s` / `2m` / `3h` etc.). Right-aligned.
const AGE_RIGHT_X: i32 = 82;
/// Right edge of the highlight-metric cell. Right-aligned.
const METRIC_RIGHT_X: i32 = 126;
/// Baseline offset for the body row text (FONT_BODY name + FONT_TINY age /
/// metric). 11 px down from the row top puts the baseline below the
/// signal glyph cleanly.
const ROW_TEXT_BASELINE: i32 = 11;
/// Max characters of the producer name before truncation. Sized so the
/// SSID + age + metric cells never collide.
const MAX_NAME_CHARS: usize = 8;
/// Baseline for the local wall-clock footer.
const CLOCK_FOOTER_BASELINE_Y: i32 = 126;

pub fn draw<D, E>(target: &mut D, state: &UiState) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    Rectangle::new(Point::new(0, 0), Size::new(128, 128)).draw_styled(
        &PrimitiveStyleBuilder::new().fill_color(LUMA_BG).build(),
        target,
    )?;
    header::draw(target, state)?;

    let body_top = header::BODY_TOP_Y;

    // Big centered "IP:port" headline — the SSID is intentionally omitted
    // because the operator already knows which network they configured;
    // showing it again would just steal real estate from the actionable
    // address.
    let mut endpoint: String<24> = String::new();
    write_endpoint(&mut endpoint, &state.network, state.http_port);
    let _ = FONT_HEADLINE.render_aligned(
        endpoint.as_str(),
        Point::new(64, body_top + 14),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_TEXT),
        target,
    );
    draw_divider(target, body_top + 18)?;

    if state.lora_available {
        draw_producer_list(target, &state.producers, state.now_ms)?;
    } else {
        draw_lora_unavailable_panel(target, body_top + 20)?;
    }
    draw_clock_footer(target, state)?;

    Ok(())
}

/// "No radio" state: warning triangle icon, "LoRa Radio" headline,
/// "not wired" subhead. Producer rows are suppressed entirely — there's
/// nothing to poll, so showing them would just be cosmetic noise.
fn draw_lora_unavailable_panel<D, E>(target: &mut D, panel_top: i32) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let icon_center = Point::new(64, panel_top + 22);
    draw_warning_triangle(target, icon_center)?;

    let _ = FONT_HEADLINE.render_aligned(
        "LoRa Radio",
        Point::new(64, panel_top + 56),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_ACCENT),
        target,
    );
    let _ = FONT_BODY.render_aligned(
        "not wired",
        Point::new(64, panel_top + 72),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_TEXT),
        target,
    );
    Ok(())
}

/// 22 × 20 warning triangle with an exclamation mark inside, drawn in
/// `LUMA_ACCENT` so it pops against the dark body.
fn draw_warning_triangle<D, E>(target: &mut D, center: Point) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let stroke = PrimitiveStyleBuilder::new()
        .stroke_color(LUMA_ACCENT)
        .stroke_width(2)
        .build();
    let fill = PrimitiveStyleBuilder::new().fill_color(LUMA_ACCENT).build();

    Triangle::new(
        Point::new(center.x, center.y - 10),
        Point::new(center.x - 11, center.y + 9),
        Point::new(center.x + 11, center.y + 9),
    )
    .draw_styled(&stroke, target)?;

    // Exclamation mark: 2 px wide vertical stem + 2 px dot below.
    Rectangle::new(Point::new(center.x - 1, center.y - 4), Size::new(2, 6))
        .draw_styled(&fill, target)?;
    Rectangle::new(Point::new(center.x - 1, center.y + 4), Size::new(2, 2))
        .draw_styled(&fill, target)?;
    Ok(())
}

/// Renders producer rows. One row per node with this layout:
///
/// ```text
///   [signal 8x7] Name···········  [age]  [metric]
/// ```
///
/// The signal cell shows: `?` for `Pending`, a compact spinner for
/// `InProgress`, fill-based-on-RSSI bars for `Success`, and a small `✗`
/// glyph for `Failed`. Age + metric stay `"—"` until the LoRa poller fills
/// `last_poll_at_ms` + `metric_value`.
fn draw_producer_list<D, E>(
    target: &mut D,
    producers: &[ProducerSummary],
    now_ms: u64,
) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let list_top = header::BODY_TOP_Y + PRODUCER_LIST_TOP_OFFSET;
    if producers.is_empty() {
        let _ = FONT_TINY.render_aligned(
            "no producers configured",
            Point::new(64, list_top + 8),
            VerticalPosition::Baseline,
            HorizontalAlignment::Center,
            FontColor::Transparent(LUMA_DIM),
            target,
        );
        return Ok(());
    }

    for (row_index, producer) in producers.iter().take(MAX_VISIBLE_PRODUCERS).enumerate() {
        let row_y = list_top + (row_index as i32) * PRODUCER_ROW_HEIGHT;
        if row_y + PRODUCER_ROW_HEIGHT > 128 {
            break;
        }
        draw_producer_row(target, producer, row_y, now_ms)?;
    }
    Ok(())
}

/// Render one producer row at `row_y` (row top y).
fn draw_producer_row<D, E>(
    target: &mut D,
    producer: &ProducerSummary,
    row_y: i32,
    now_ms: u64,
) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    draw_poll_signal(
        target,
        Point::new(SIGNAL_X, row_y + SIGNAL_Y_OFFSET),
        producer.status,
        now_ms,
    )?;

    let name_color = if producer.enabled {
        LUMA_TEXT
    } else {
        LUMA_DIM
    };
    let mut name: String<32> = String::new();
    let mut taken = 0usize;
    for ch in producer.name.chars() {
        if taken >= MAX_NAME_CHARS {
            let _ = name.push('…');
            break;
        }
        if name.push(ch).is_err() {
            break;
        }
        taken += 1;
    }
    let _ = FONT_BODY.render_aligned(
        if name.is_empty() { "?" } else { name.as_str() },
        Point::new(NAME_X, row_y + ROW_TEXT_BASELINE),
        VerticalPosition::Baseline,
        HorizontalAlignment::Left,
        FontColor::Transparent(name_color),
        target,
    );

    // Age cell — `"5s"` / `"2m"` / `"3h"` / `"—"` if never polled.
    let age = format_age(now_ms, producer.last_poll_at_ms);
    let _ = FONT_TINY.render_aligned(
        age.as_str(),
        Point::new(AGE_RIGHT_X, row_y + ROW_TEXT_BASELINE),
        VerticalPosition::Baseline,
        HorizontalAlignment::Right,
        FontColor::Transparent(LUMA_DIM),
        target,
    );

    // Highlight-metric cell — `"12.4V"` after success, otherwise the compact
    // producer kind so the row still carries useful identity before first RX.
    let metric = if producer.enabled {
        format_metric(
            producer.metric_value,
            producer.metric_unit,
            producer.kind_label,
        )
    } else {
        let mut disabled: String<10> = String::new();
        let _ = disabled.push_str("off");
        disabled
    };
    let _ = FONT_TINY.render_aligned(
        metric.as_str(),
        Point::new(METRIC_RIGHT_X, row_y + ROW_TEXT_BASELINE),
        VerticalPosition::Baseline,
        HorizontalAlignment::Right,
        FontColor::Transparent(LUMA_TEXT),
        target,
    );

    Ok(())
}

/// 8 × 7 px status indicator at `top_left`.
///
/// Status mapping:
///
/// - `Pending`     → compact dim `?`.
/// - `InProgress`  → compact 4-dot spinner.
/// - `Success`     → bars filled based on `rssi` thresholds.
/// - `Failed`      → replaced entirely with a small `✗` glyph.
fn draw_poll_signal<D, E>(
    target: &mut D,
    top_left: Point,
    status: PollStatus,
    now_ms: u64,
) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    if matches!(status, PollStatus::Failed) {
        return draw_x_glyph(target, top_left);
    }
    if matches!(status, PollStatus::Pending) {
        // No poll has happened yet — render an explicit dim "?" so
        // the cell isn't mistaken for "indicator absent" the way an
        // all-dim 3-bar outline can be.
        let _ = FONT_TINY.render_aligned(
            "?",
            Point::new(top_left.x + 4, top_left.y + 7),
            VerticalPosition::Baseline,
            HorizontalAlignment::Center,
            FontColor::Transparent(LUMA_DIM),
            target,
        );
        return Ok(());
    }
    if matches!(status, PollStatus::InProgress) {
        return draw_poll_spinner(target, top_left, now_ms);
    }
    let lit_count: u8 = match status {
        PollStatus::Success { rssi } => match rssi {
            r if r >= -90 => 3,
            r if r >= -100 => 2,
            r if r >= -110 => 1,
            _ => 0,
        },
        PollStatus::Pending | PollStatus::InProgress | PollStatus::Failed => 0,
    };
    for bar in 0..3 {
        draw_signal_bar(target, top_left, bar, bar < lit_count)?;
    }
    Ok(())
}

/// 8 × 7 px four-dot spinner used while a producer poll is in flight.
fn draw_poll_spinner<D, E>(target: &mut D, top_left: Point, now_ms: u64) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    const DOTS: [(i32, i32); 4] = [(3, 0), (6, 2), (3, 5), (0, 2)];
    let active = ((now_ms / 250) as usize) % DOTS.len();

    for (index, (dx, dy)) in DOTS.iter().enumerate() {
        let color = if index == active {
            LUMA_ACCENT
        } else {
            LUMA_INACTIVE
        };
        Rectangle::new(
            Point::new(top_left.x + dx, top_left.y + dy),
            Size::new(2, 2),
        )
        .draw_styled(
            &PrimitiveStyleBuilder::new().fill_color(color).build(),
            target,
        )?;
    }
    Ok(())
}

/// Helper for [`draw_poll_signal`] — draws one of the three bars at a
/// fixed offset inside the 8 × 7 cell.
fn draw_signal_bar<D, E>(
    target: &mut D,
    cell_top_left: Point,
    bar_idx: u8,
    lit: bool,
) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let (rel_x, rel_y, h) = match bar_idx {
        0 => (0, 4, 3),
        1 => (3, 2, 5),
        _ => (6, 0, 7),
    };
    let color = if lit { LUMA_ACCENT } else { LUMA_INACTIVE };
    Rectangle::new(
        Point::new(cell_top_left.x + rel_x, cell_top_left.y + rel_y),
        Size::new(2, h),
    )
    .draw_styled(
        &PrimitiveStyleBuilder::new().fill_color(color).build(),
        target,
    )?;
    Ok(())
}

/// 8 × 7 px `✗` glyph used in place of the signal bars when the last
/// poll failed. Drawn in `LUMA_TEXT` so it reads as a clear "not OK"
/// without screaming for attention the way a bright accent would.
fn draw_x_glyph<D, E>(target: &mut D, top_left: Point) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    const PIXELS: &[(i32, i32)] = &[
        (0, 0),
        (7, 0),
        (1, 1),
        (6, 1),
        (2, 2),
        (5, 2),
        (3, 3),
        (4, 3),
        (2, 4),
        (5, 4),
        (1, 5),
        (6, 5),
        (0, 6),
        (7, 6),
    ];
    for (dx, dy) in PIXELS {
        Pixel(Point::new(top_left.x + dx, top_left.y + dy), LUMA_TEXT).draw(target)?;
    }
    Ok(())
}

/// Format `(now_ms - last_poll_at_ms)` as a compact `Ns` / `Nm` / `Nh`
/// / `Nd` tag. Returns `"?"` when no poll has completed yet — using
/// ASCII so the cell actually renders (the em-dash isn't in
/// `profont10`).
fn format_age(now_ms: u64, last_poll_at_ms: Option<u64>) -> String<6>
{
    let mut out: String<6> = String::new();
    let Some(t) = last_poll_at_ms else {
        let _ = out.push('?');
        return out;
    };
    let age_s = now_ms.saturating_sub(t) / 1_000;
    let _ = if age_s < 60 {
        core::fmt::write(&mut out, format_args!("{}s", age_s))
    } else if age_s < 3_600 {
        core::fmt::write(&mut out, format_args!("{}m", age_s / 60))
    } else if age_s < 86_400 {
        core::fmt::write(&mut out, format_args!("{}h", age_s / 3_600))
    } else {
        core::fmt::write(&mut out, format_args!("{}d", age_s / 86_400))
    };
    out
}

/// Format the highlight metric as `"{value:.1}{unit}"`, returning a compact
/// fallback label when no value has been measured yet.
fn format_metric(value: Option<f32>, unit: &str, fallback: &str) -> String<10>
{
    let mut out: String<10> = String::new();
    let Some(v) = value else {
        for ch in fallback.chars().take(9) {
            let _ = out.push(ch);
        }
        if out.is_empty() {
            let _ = out.push('?');
        }
        return out;
    };
    let _ = core::fmt::write(&mut out, format_args!("{:.1}{}", v, unit));
    out
}

fn draw_clock_footer<D, E>(target: &mut D, state: &UiState) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    if state.clock_text.is_empty() {
        return Ok(());
    }
    let _ = FONT_TINY.render_aligned(
        state.clock_text.as_str(),
        Point::new(64, CLOCK_FOOTER_BASELINE_Y),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_DIM),
        target,
    );
    Ok(())
}

fn draw_divider<D, E>(target: &mut D, y: i32) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    Line::new(Point::new(4, y), Point::new(123, y)).draw_styled(
        &PrimitiveStyleBuilder::new()
            .stroke_color(LUMA_DIM)
            .stroke_width(1)
            .build(),
        target,
    )?;
    Ok(())
}

/// Render `"IP:port"` for the prominent operational headline. Falls back
/// to short status text while DHCP/association is in flight so the cell
/// never sits empty.
fn write_endpoint(out: &mut String<24>, phase: &NetworkPhase, port: u16)
{
    use core::fmt::Write as _;
    match phase {
        NetworkPhase::Connected { ipv4, .. } if *ipv4 != [0, 0, 0, 0] => {
            let _ = write!(
                out,
                "{}.{}.{}.{}:{}",
                ipv4[0], ipv4[1], ipv4[2], ipv4[3], port,
            );
        },
        NetworkPhase::Connected { .. } => {
            let _ = out.push_str("DHCP…");
        },
        NetworkPhase::Connecting { .. } => {
            let _ = out.push_str("connecting");
        },
        NetworkPhase::Error { .. } => {
            let _ = out.push_str("offline");
        },
        _ => {
            let _ = out.push_str("offline");
        },
    }
}
