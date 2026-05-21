//! Runtime USB configuration update handling.
//!
//! The ESP32 firmware keeps the USB config receiver active after provisioning.
//! A new config is validated and written to flash, then applied on reboot.

use esp_hal::delay::Delay;
use muninn_gate_core::config::MAX_TELEMETRY_PRODUCERS;
use muninn_gate_core::{
    Clock,
    DiagnosticLevel,
    DiagnosticSubsystem,
    GateFirmwareVariant,
    GatewayConfig,
    GatewayRuntimeError,
};

use crate::display::LocalDisplay;
use crate::platform::Esp32Platform;
use crate::provisioning::{ProvisioningCommand, ProvisioningError};
use crate::serial::UsbJsonReceiver;
use crate::{diagnostics, poll_control, provisioning, serial, storage};

/// Runtime USB config update receiver.
pub struct UsbConfigUpdateService
{
    receiver:             UsbJsonReceiver,
    last_activity_ms:     Option<u64>,
    last_displayed_bytes: usize,
    reboot_required:      bool,
}

impl UsbConfigUpdateService
{
    /// Create a runtime config update service around an existing USB receiver.
    pub fn new(receiver: UsbJsonReceiver) -> Self
    {
        Self {
            receiver,
            last_activity_ms: None,
            last_displayed_bytes: 0,
            reboot_required: false,
        }
    }

    /// Return true after a valid config update has been persisted.
    pub const fn reboot_required(&self) -> bool
    {
        self.reboot_required
    }

    /// Poll USB for config update and control commands.
    pub fn poll<V>(&mut self, platform: &Esp32Platform, display: Option<&mut LocalDisplay>)
    where
        V: GateFirmwareVariant,
    {
        match self.receiver.poll_document() {
            Ok(Some(document)) => {
                self.last_activity_ms = None;
                self.last_displayed_bytes = 0;
                self.handle_document::<V>(platform, display, document.as_str());
            },
            Ok(None) => {
                self.handle_incomplete_upload::<V>(platform, display);
            },
            Err(_) => {
                self.reject_upload(platform, display, ProvisioningError::InvalidConfig);
                self.receiver.discard_partial();
            },
        }
    }

    fn handle_incomplete_upload<V>(
        &mut self,
        platform: &Esp32Platform,
        display: Option<&mut LocalDisplay>,
    ) where
        V: GateFirmwareVariant,
    {
        let mut display = display;
        if self.receiver.take_activity() {
            self.last_activity_ms = Some(platform.now_ms());
            let buffered = self.receiver.buffered_bytes();
            if buffered != self.last_displayed_bytes {
                self.last_displayed_bytes = buffered;
                if let Some(display) = display.as_mut() {
                    let _ = display.show_config_receiving(buffered);
                }
            }
        }

        if self.receiver.buffered_bytes() == 0 {
            return;
        }

        let Some(last_ms) = self.last_activity_ms else {
            return;
        };

        if platform.now_ms().saturating_sub(last_ms) < crate::USB_CONFIG_UPLOAD_IDLE_TIMEOUT_MS {
            return;
        }

        self.last_activity_ms = None;
        self.last_displayed_bytes = 0;
        match self.receiver.take_buffered_document() {
            Ok(document) => self.handle_document::<V>(platform, display, document.as_str()),
            Err(_) => self.reject_upload(platform, display, ProvisioningError::InvalidConfig),
        }
    }

    fn handle_document<V>(
        &mut self,
        platform: &Esp32Platform,
        display: Option<&mut LocalDisplay>,
        document: &str,
    ) where
        V: GateFirmwareVariant,
    {
        let mut display = display;
        esp_println::println!(
            r#"{{"type":"progress","request":"set_config","stage":"received","bytes":{}}}"#,
            document.len(),
        );
        match provisioning::parse_usb_document(document) {
            Ok(ProvisioningCommand::SetConfig(config)) => {
                serial::write_line(
                    r#"{"type":"progress","request":"set_config","stage":"validated"}"#,
                );
                if validate_outputs::<V>(&config).is_err() {
                    self.reject_upload(platform, display.take(), ProvisioningError::InvalidConfig);
                    return;
                }

                serial::write_line(
                    r#"{"type":"progress","request":"set_config","stage":"saving"}"#,
                );
                match storage::save_config_document(document, &config) {
                    Ok(()) => {},
                    Err(error) => {
                        self.reject_storage_upload(platform, display.take(), error);
                        return;
                    },
                }

                self.reboot_required = true;
                serial::write_line(
                    r#"{"type":"ok","request":"set_config","action":"reboot_required"}"#,
                );
                record_diagnostic(
                    platform.now_ms(),
                    DiagnosticLevel::Info,
                    DiagnosticSubsystem::Config,
                    "config_updated",
                    "valid USB config update persisted; reboot required",
                );
                if let Some(display) = display.as_mut() {
                    let _ = display.show_config_update_reboot_required();
                }
            },
            Ok(ProvisioningCommand::Help) => write_runtime_help(),
            Ok(ProvisioningCommand::Status) => write_runtime_status(self.reboot_required),
            Ok(ProvisioningCommand::PollNow) => {
                let count = poll_control::request_poll_now();
                esp_println::println!(r#"{{"type":"ok","request":"poll_now","count":{}}}"#, count);
            },
            Ok(ProvisioningCommand::Reboot) => reboot(),
            Err(error) => self.reject_upload(platform, display, error),
        }
    }

    fn reject_upload(
        &mut self,
        platform: &Esp32Platform,
        display: Option<&mut LocalDisplay>,
        error: ProvisioningError,
    )
    {
        esp_println::println!(
            r#"{{"type":"error","request":"set_config","code":"{}"}}"#,
            error.as_str(),
        );
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Error,
            DiagnosticSubsystem::Config,
            "config_update_failed",
            error.as_str(),
        );
        if let Some(display) = display {
            let _ = display.show_config_error(error.as_str());
        }
    }

    fn reject_storage_upload(
        &mut self,
        platform: &Esp32Platform,
        display: Option<&mut LocalDisplay>,
        error: storage::StorageError,
    )
    {
        if let Some(rom_code) = error.rom_code() {
            esp_println::println!(
                r#"{{"type":"error","request":"set_config","code":"{}","rom_code":{}}}"#,
                error.as_code(),
                rom_code,
            );
        } else {
            esp_println::println!(
                r#"{{"type":"error","request":"set_config","code":"{}"}}"#,
                error.as_code(),
            );
        }
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Error,
            DiagnosticSubsystem::Config,
            "config_update_storage_failed",
            error.as_code(),
        );
        if let Some(display) = display {
            let _ = display.show_config_error(error.as_code());
        }
    }
}

/// Validate that a config only enables interfaces supported by the variant.
pub fn validate_outputs<V>(
    config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
) -> Result<(), GatewayRuntimeError>
where
    V: GateFirmwareVariant,
{
    if config.http.is_some() && (!V::CAPABILITIES.http_server || !V::CAPABILITIES.wifi) {
        return Err(GatewayRuntimeError::UnsupportedOutput);
    }

    if !V::CAPABILITIES.usb_serial {
        return Err(GatewayRuntimeError::UnsupportedOutput);
    }

    Ok(())
}

fn write_runtime_help()
{
    serial::write_line(
        r#"{"type":"help","commands":["set_config","status","help","poll_now","reboot"]}"#,
    );
    serial::write_line("upload raw config JSON or JSONC to replace saved config");
}

fn write_runtime_status(reboot_required: bool)
{
    if reboot_required {
        serial::write_line(r#"{"type":"status","state":"provisioned","config":"reboot_required"}"#);
    } else {
        serial::write_line(r#"{"type":"status","state":"provisioned","config":"active"}"#);
    }
}

fn reboot() -> !
{
    serial::write_line(r#"{"type":"ok","request":"reboot"}"#);
    let delay = Delay::new();
    delay.delay_millis(100);
    esp_rom_sys::rom::software_reset()
}

fn record_diagnostic(
    timestamp_ms: u64,
    level: DiagnosticLevel,
    subsystem: DiagnosticSubsystem,
    code: &str,
    message: &str,
)
{
    let _ = diagnostics::record(timestamp_ms, level, subsystem, code, message);
}
