//! ESP32 128x64 OLED display transport.

use display_interface_i2c::I2CInterface;
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::time::Rate;
use heapless::String;
use muninn_gate_core::config::MAX_TELEMETRY_PRODUCERS;
use muninn_gate_core::{
    DisplaySettings,
    DisplayVariant,
    GateFirmwareVariant,
    GatewayConfig,
    GatewayRuntimeState,
    ServingInterfaces,
    TelemetryProducerId,
    TelemetrySnapshot,
};
use muninn_gate_ui::{
    GraphicsError,
    Oled128x64Dashboard,
    draw_dashboard_128x64,
    draw_framed_notice_128x64,
    draw_wifi_connecting_128x64,
};
use ssd1306::mode::{BufferedGraphicsMode, DisplayConfig};
use ssd1306::prelude::{Brightness, DisplayRotation, DisplaySize128x64};
use ssd1306::{I2CDisplayInterface, Ssd1306};

use crate::platform::Esp32DisplayResources;

/// I2C address used by the WiFi LoRa 32 V4.x SSD1306 display.
pub const OLED_I2C_ADDRESS: u8 = 0x3c;
/// Display I2C bus frequency.
pub const OLED_I2C_FREQUENCY_KHZ: u32 = 400;
/// Number of SSD1306 init attempts before the OLED is treated as unavailable.
pub const OLED_INIT_ATTEMPTS: usize = 3;
/// Display reset low pulse duration.
pub const OLED_RESET_LOW_MS: u32 = 25;
/// Display settle time after reset is released.
pub const OLED_RESET_HIGH_MS: u32 = 100;
/// Delay between failed OLED init attempts.
pub const OLED_INIT_RETRY_MS: u32 = 50;
type OledDisplay = Ssd1306<
    I2CInterface<I2c<'static, Blocking>>,
    DisplaySize128x64,
    BufferedGraphicsMode<DisplaySize128x64>,
>;

/// Errors returned while initializing or drawing the local display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayStartError
{
    /// The selected board has no 128x64 OLED display.
    UnsupportedVariant,
    /// The I2C peripheral could not be configured.
    I2cConfig,
    /// The SSD1306 controller did not initialize.
    Init,
    /// A frame could not be flushed to the display.
    Flush,
    /// The UI renderer failed to create a text frame.
    Render,
}

impl DisplayStartError
{
    /// Return a compact display startup error label.
    pub const fn as_str(self) -> &'static str
    {
        match self {
            Self::UnsupportedVariant => "display unsupported",
            Self::I2cConfig => "display i2c failed",
            Self::Init => "display init failed",
            Self::Flush => "display flush failed",
            Self::Render => "display render failed",
        }
    }
}

/// Local OLED display handle for ESP32 board variants.
pub struct LocalDisplay
{
    display: OledDisplay,
    _reset:  Output<'static>,
}

impl LocalDisplay
{
    /// Initialize the 128x64 OLED connected to the board display pins.
    pub fn new<V>(resources: Esp32DisplayResources) -> Result<Self, DisplayStartError>
    where
        V: GateFirmwareVariant,
    {
        if V::CAPABILITIES.display != DisplayVariant::Oled128x64 {
            return Err(DisplayStartError::UnsupportedVariant);
        }

        let i2c = I2c::new(
            resources.i2c0,
            I2cConfig::default().with_frequency(Rate::from_khz(OLED_I2C_FREQUENCY_KHZ)),
        )
        .map_err(|_| DisplayStartError::I2cConfig)?
        .with_sda(resources.sda)
        .with_scl(resources.scl);

        let interface = I2CDisplayInterface::new_custom_address(i2c, OLED_I2C_ADDRESS);
        let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
            .into_buffered_graphics_mode();
        let mut reset = Output::new(resources.reset, Level::High, OutputConfig::default());
        let delay = Delay::new();

        for attempt in 0..OLED_INIT_ATTEMPTS {
            reset_oled(&mut reset, &delay);
            if display.init().is_ok() {
                display.clear_buffer();
                if display.flush().is_ok() {
                    return Ok(Self {
                        display,
                        _reset: reset,
                    });
                }
            }
            if attempt + 1 < OLED_INIT_ATTEMPTS {
                delay.delay_millis(OLED_INIT_RETRY_MS);
            }
        }

        Err(DisplayStartError::Init)
    }

    /// Apply configured display hardware settings.
    pub fn apply_settings(&mut self, settings: DisplaySettings) -> Result<(), DisplayStartError>
    {
        match settings {
            DisplaySettings::Off => Ok(()),
            DisplaySettings::Enabled {
                brightness_percent, ..
            } => self.set_brightness_percent(brightness_percent),
        }
    }

    /// Set SSD1306 brightness from a 0-100 percentage.
    pub fn set_brightness_percent(
        &mut self,
        brightness_percent: u8,
    ) -> Result<(), DisplayStartError>
    {
        let percent = brightness_percent.min(100);
        let contrast = ((u16::from(percent) * u16::from(u8::MAX)) / 100) as u8;
        let precharge = if percent <= 10 { 1 } else { 2 };
        self.display
            .set_brightness(Brightness::custom(precharge, contrast))
            .map_err(|_| DisplayStartError::Render)
    }

    /// Render an unprovisioned status page with USB upload instructions.
    pub fn show_unprovisioned(&mut self) -> Result<(), DisplayStartError>
    {
        self.draw_framed_notice("Muninn Gate v0.1", "PROVISION REQUIRED", "SEE README.md")
    }

    /// Render progress after USB provisioning bytes have arrived.
    pub fn show_config_receiving(&mut self, bytes: usize) -> Result<(), DisplayStartError>
    {
        let mut received = String::<32>::new();
        core::fmt::write(&mut received, format_args!("{} bytes", bytes))
            .map_err(|_| DisplayStartError::Render)?;
        self.draw_framed_notice("Muninn Gate v0.1", "RECEIVING CONFIG", received.as_str())
    }

    /// Render a malformed configuration error page.
    pub fn show_config_error(&mut self, reason: &str) -> Result<(), DisplayStartError>
    {
        self.draw_framed_notice(
            "Muninn Gate v0.1",
            "CONFIG REJECTED",
            rejection_reason_label(reason),
        )
    }

    /// Render a configuration accepted page.
    pub fn show_config_accepted(&mut self) -> Result<(), DisplayStartError>
    {
        self.draw_framed_notice("Muninn Gate v0.1", "CONFIG ACCEPTED", "STARTING")
    }

    /// Render a short acknowledgement after the user requests a poll.
    pub fn show_poll_requested(&mut self) -> Result<(), DisplayStartError>
    {
        self.draw_framed_notice("Muninn Gate", "POLL REQUESTED", "STARTING NOW")
    }

    /// Render a persisted configuration update that needs a reboot.
    pub fn show_config_update_reboot_required(&mut self) -> Result<(), DisplayStartError>
    {
        self.draw_framed_notice("Muninn Gate v0.1", "CONFIG UPDATED", "RESET DEVICE")
    }

    /// Render a WiFi connection progress page.
    pub fn show_wifi_connecting(
        &mut self,
        ssid: &str,
        detail: &str,
        step: u8,
    ) -> Result<(), DisplayStartError>
    {
        draw_wifi_connecting_128x64(&mut self.display, ssid, detail, step)
            .map_err(map_graphics_error)?;
        self.display.flush().map_err(|_| DisplayStartError::Flush)
    }

    /// Render a terminal WiFi startup failure page.
    pub fn show_wifi_error(&mut self, detail: &str) -> Result<(), DisplayStartError>
    {
        self.draw_framed_notice("Muninn Gate", "WIFI FAILED", detail)
    }

    /// Render the operational HTTP dashboard with endpoint, nodes, and poll hint.
    pub fn show_http_dashboard<V>(
        &mut self,
        config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
        snapshot: &TelemetrySnapshot<MAX_TELEMETRY_PRODUCERS>,
        state: GatewayRuntimeState,
        now_ms: u64,
    ) -> Result<(), DisplayStartError>
    where
        V: GateFirmwareVariant,
    {
        let endpoint = match state {
            GatewayRuntimeState::Serving { interfaces } => interfaces.http,
            GatewayRuntimeState::Unprovisioned
            | GatewayRuntimeState::Provisioned
            | GatewayRuntimeState::Error { .. } => None,
        };
        self.draw_dashboard(Oled128x64Dashboard {
            config,
            snapshot,
            state,
            now_ms,
            endpoint,
            board_name: V::BOARD,
            active_poll: None,
        })
    }

    /// Render the dashboard while a producer poll is actively waiting.
    pub fn show_polling_dashboard(
        &mut self,
        config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
        snapshot: &TelemetrySnapshot<MAX_TELEMETRY_PRODUCERS>,
        state: GatewayRuntimeState,
        now_ms: u64,
        board_name: &'static str,
        active_poll: (TelemetryProducerId, u8),
    ) -> Result<(), DisplayStartError>
    {
        let endpoint = match state {
            GatewayRuntimeState::Serving { interfaces } => interfaces.http,
            GatewayRuntimeState::Unprovisioned
            | GatewayRuntimeState::Provisioned
            | GatewayRuntimeState::Error { .. } => None,
        };
        self.draw_dashboard(Oled128x64Dashboard {
            config,
            snapshot,
            state,
            now_ms,
            endpoint,
            board_name,
            active_poll: Some(active_poll),
        })
    }

    /// Render the configured 128x64 gateway status frame.
    pub fn show_status<V>(
        &mut self,
        config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
        snapshot: &TelemetrySnapshot<MAX_TELEMETRY_PRODUCERS>,
        state: GatewayRuntimeState,
        now_ms: u64,
    ) -> Result<(), DisplayStartError>
    where
        V: GateFirmwareVariant,
    {
        let endpoint = match state {
            GatewayRuntimeState::Serving { interfaces } => interfaces.http,
            GatewayRuntimeState::Unprovisioned
            | GatewayRuntimeState::Provisioned
            | GatewayRuntimeState::Error { .. } => None,
        };
        self.draw_dashboard(Oled128x64Dashboard {
            config,
            snapshot,
            state,
            now_ms,
            endpoint,
            board_name: V::BOARD,
            active_poll: None,
        })
    }

    fn draw_dashboard(
        &mut self,
        context: Oled128x64Dashboard<'_, MAX_TELEMETRY_PRODUCERS>,
    ) -> Result<(), DisplayStartError>
    {
        draw_dashboard_128x64(&mut self.display, context).map_err(map_graphics_error)?;
        self.display.flush().map_err(|_| DisplayStartError::Flush)
    }

    fn draw_framed_notice(
        &mut self,
        top: &str,
        middle: &str,
        bottom: &str,
    ) -> Result<(), DisplayStartError>
    {
        draw_framed_notice_128x64(&mut self.display, top, middle, bottom)
            .map_err(map_graphics_error)?;
        self.display.flush().map_err(|_| DisplayStartError::Flush)
    }
}

fn map_graphics_error<E>(_error: GraphicsError<E>) -> DisplayStartError
{
    DisplayStartError::Render
}

fn rejection_reason_label(reason: &str) -> &'static str
{
    match reason {
        "invalid json" => "INVALID JSON",
        "unsupported command" => "BAD COMMAND",
        "missing required field" => "MISSING FIELD",
        "invalid config" => "INVALID CONFIG",
        "upload timeout" => "UPLOAD TIMEOUT",
        "usb buffer" => "USB BUFFER",
        "usb unavailable" => "USB UNAVAILABLE",
        _ => "CHECK CONFIG",
    }
}

/// Render a serial-only serving state for displays without an HTTP endpoint.
pub fn serial_only_state() -> GatewayRuntimeState
{
    GatewayRuntimeState::Serving {
        interfaces: ServingInterfaces::new(None, true),
    }
}

fn reset_oled(reset: &mut Output<'static>, delay: &Delay)
{
    reset.set_high();
    delay.delay_millis(1);
    reset.set_low();
    delay.delay_millis(OLED_RESET_LOW_MS);
    reset.set_high();
    delay.delay_millis(OLED_RESET_HIGH_MS);
}
