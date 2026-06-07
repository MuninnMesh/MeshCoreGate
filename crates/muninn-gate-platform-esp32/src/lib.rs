#![no_std]
#![warn(missing_docs)]
//! Common ESP32-S3 platform services for Muninn Gate board variants.
//!
//! This crate owns reusable ESP32 startup, diagnostics, serial, WiFi, HTTP
//! metrics, and storage hooks. Concrete board crates select which services are
//! used.

extern crate alloc;

/// ESP32 runtime USB configuration updates.
pub mod config_update;
/// ESP32 diagnostic event retention.
pub mod diagnostics;
/// ESP32 local display transport and pages.
pub mod display;
/// ESP32 HTTP response helpers and Prometheus rendering bridge.
pub mod http_metrics;
/// Chip-level HTTP server (smoltcp socket pool, request handling)
/// shared by every ESP32 board variant.
pub mod http_server;
/// ESP32 local input controls.
pub mod input;
/// ESP32 MeshCore adapter over the radio owner queues.
pub mod meshcore;
/// ESP32 platform initialization and clock hooks.
pub mod platform;
/// ESP32 poll-on-demand request accounting.
pub mod poll_control;
/// Board-agnostic power-monitoring trait (SOC + USB presence).
pub mod power;
/// ESP32 USB provisioning parser.
pub mod provisioning;
/// ESP32 radio configuration and cooperative owner hooks.
pub mod radio;
/// ESP32 scheduler runtime integration.
pub mod scheduler;
/// ESP32 serial output helpers.
pub mod serial;
/// ESP32 configuration loading backend.
pub mod storage;
/// ESP32 shared telemetry store.
pub mod telemetry_state;
/// ESP32 WiFi station and blocking HTTP service.
pub mod wifi;
/// Chip-level WiFi primitives shared by every ESP32 board variant.
pub mod wifi_common;

use esp_hal::delay::Delay;
use muninn_gate_core::config::MAX_TELEMETRY_PRODUCERS;
use muninn_gate_core::output::serial_json_enabled;
use muninn_gate_core::{
    Clock,
    DiagnosticLevel,
    DiagnosticSubsystem,
    DisplaySettings,
    GateFirmwareVariant,
    GatewayConfig,
    GatewayRuntimeError,
    GatewayRuntimeState,
    ServingInterfaces,
    TxPowerMapping,
};
use muninn_mesh_radio::MeshRadioConfig;
pub use muninn_mesh_sx126x::op::tcxo::TcxoVoltage;

use crate::config_update::UsbConfigUpdateService;
use crate::display::LocalDisplay;
use crate::platform::Esp32RadioResources;
use crate::provisioning::{ProvisioningCommand, ProvisioningError};

/// Board radio hardware settings consumed by the common ESP32 runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Esp32RadioHardware
{
    /// Whether the radio path uses a TCXO controlled by the radio driver.
    pub tcxo_enabled:  bool,
    /// DIO3 voltage used to power/control the TCXO.
    pub tcxo_voltage:  TcxoVoltage,
    /// TCXO startup delay in milliseconds when [`Self::tcxo_enabled`] is true.
    pub tcxo_delay_ms: u32,
}

/// ESP32 board variant supported by the common ESP32 runner.
pub trait Esp32BoardVariant: GateFirmwareVariant
{
    /// Board-level radio hardware settings.
    const RADIO_HARDWARE: Esp32RadioHardware;
}

/// Idle time before an incomplete USB config upload is rejected.
pub const USB_CONFIG_UPLOAD_IDLE_TIMEOUT_MS: u64 = 2_000;
/// Boot-time window for replacing saved config before WiFi starts.
pub const USB_CONFIG_UPDATE_WINDOW_MS: u64 = 5_000;
/// Interval between provisioning-ready markers while the USB window is open.
pub const USB_CONFIG_READY_INTERVAL_MS: u64 = 1_000;
/// Whether to start the LoRa/MeshCore owner.
pub const START_RADIO_OWNER: bool = true;
/// Delay the first automatic scheduled producer poll after boot.
///
/// This keeps WiFi association, DHCP, and initial HTTP serving from competing
/// with LoRa request/response airtime during the demo-critical first minute.
/// Operators can still force an immediate poll through `/poll` or the button.
pub const INITIAL_SCHEDULE_DELAY_MS: u64 = 60_000;
/// Whether to queue a MeshCore startup advert at boot.
///
/// The gateway does not need an unsolicited advert to serve HTTP telemetry.
/// Keeping this off avoids a radio TX burst while WiFi is still settling.
pub const QUEUE_STARTUP_ADVERT: bool = false;

/// Run common ESP32 gateway startup for a concrete board variant.
pub fn run_gateway<V>() -> !
where
    V: Esp32BoardVariant,
{
    let mut platform = platform::init();

    record_diagnostic(
        platform.now_ms(),
        DiagnosticLevel::Info,
        DiagnosticSubsystem::Runtime,
        "boot",
        "ESP32 platform startup",
    );
    esp_println::println!("Muninn Gate platform=ESP32 variant={}", V::NAME);

    let Some(board_resources) = platform.take_board_resources() else {
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Error,
            DiagnosticSubsystem::Runtime,
            "board_resources_unavailable",
            "board resources were unavailable for radio/display startup",
        );
        report_state(GatewayRuntimeState::Error {
            reason: GatewayRuntimeError::Platform,
        });
        loop {
            core::hint::spin_loop();
        }
    };
    let powered_board = board_resources.enable_external_power();
    let radio_resources = powered_board.radio;
    let mut user_button = input::UserButton::new(powered_board.input.user_button);
    let mut display = match LocalDisplay::new::<V>(powered_board.display) {
        Ok(display) => Some(display),
        Err(error) => {
            esp_println::println!("Display: unavailable ({})", error.as_str());
            record_diagnostic(
                platform.now_ms(),
                DiagnosticLevel::Warn,
                DiagnosticSubsystem::Display,
                "display_start_failed",
                error.as_str(),
            );
            None
        },
    };
    let _external_power = powered_board.power;

    let loaded_config = load_or_provision::<V>(&mut platform, display.as_mut());
    let config = loaded_config.config;

    if let Err(reason) = config_update::validate_outputs::<V>(&config) {
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Error,
            DiagnosticSubsystem::Config,
            "output_unsupported",
            "configured output interface is unsupported or invalid",
        );
        report_state(GatewayRuntimeState::Error { reason });
        loop {
            core::hint::spin_loop();
        }
    }
    if let Some(display) = display.as_mut() {
        let _ = display.apply_settings(config.display);
    }

    let tx_power = V::map_tx_power(config.radio.tx_power_level);
    print_config_summary::<V>(&config, tx_power);
    if telemetry_state::reset_from_config(&config, platform.now_ms(), tx_power).is_err() {
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Error,
            DiagnosticSubsystem::Runtime,
            "telemetry_store_init_failed",
            "shared telemetry store could not be initialized from config",
        );
        report_state(GatewayRuntimeState::Error {
            reason: GatewayRuntimeError::Platform,
        });
        loop {
            core::hint::spin_loop();
        }
    }
    meshcore::initialize_request_tags(platform.now_ms());
    let radio_config = match radio::mesh_radio_config_from_gateway::<V>(&config) {
        Ok(config) => config,
        Err(_) => {
            record_diagnostic(
                platform.now_ms(),
                DiagnosticLevel::Error,
                DiagnosticSubsystem::Radio,
                "radio_config_invalid",
                "radio configuration could not be mapped to the ESP32 radio driver",
            );
            report_state(GatewayRuntimeState::Error {
                reason: GatewayRuntimeError::InvalidConfig,
            });
            loop {
                core::hint::spin_loop();
            }
        },
    };

    report_state(GatewayRuntimeState::Provisioned);
    if let Some(display) = display.as_mut() {
        let _ = display.show_config_accepted();
    }
    record_diagnostic(
        platform.now_ms(),
        DiagnosticLevel::Info,
        DiagnosticSubsystem::Config,
        "provisioned",
        "valid gateway configuration loaded",
    );

    telemetry_state::refresh_gateway_metrics(platform.now_ms(), tx_power);
    let snapshot = telemetry_state::snapshot();
    if let Some(display) = display.as_mut() {
        let _ = display.show_status::<V>(
            &config,
            &snapshot,
            GatewayRuntimeState::Provisioned,
            platform.now_ms(),
        );
    }
    if V::CAPABILITIES.usb_serial && serial::write_snapshot_json(&snapshot).is_err() {
        serial::write_line("Serial: failed to render telemetry snapshot");
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Error,
            DiagnosticSubsystem::Serial,
            "serial_render_failed",
            "bring-up serial JSON snapshot render failed",
        );
    }

    let scheduler_start_ms = platform.now_ms().saturating_add(INITIAL_SCHEDULE_DELAY_MS);
    let mut scheduler = match scheduler::GatewayScheduler::new(&config, scheduler_start_ms) {
        Ok(scheduler) => scheduler,
        Err(_) => {
            record_diagnostic(
                platform.now_ms(),
                DiagnosticLevel::Error,
                DiagnosticSubsystem::Poller,
                "scheduler_init_failed",
                "poll scheduler could not be initialized",
            );
            report_state(GatewayRuntimeState::Error {
                reason: GatewayRuntimeError::Platform,
            });
            loop {
                core::hint::spin_loop();
            }
        },
    };
    let mut radio_owner = maybe_start_radio_or_halt(
        &mut platform,
        radio_resources,
        radio_config,
        tx_power,
        &config,
    );

    if config.http.is_some() && V::CAPABILITIES.http_server {
        wifi::serve_http_forever::<V>(
            &mut platform,
            &config,
            tx_power,
            display.as_mut(),
            &mut scheduler,
            &mut user_button,
            radio_owner.as_mut(),
        );
    } else {
        let serial_state = GatewayRuntimeState::Serving {
            interfaces: ServingInterfaces::new(None, V::CAPABILITIES.usb_serial),
        };
        report_state(serial_state);
        if let Some(display) = display.as_mut() {
            let _ = display.show_status::<V>(&config, &snapshot, serial_state, platform.now_ms());
        }

        let delay = Delay::new();
        loop {
            {
                let mut maintenance = || {
                    if let Some(owner) = radio_owner.as_mut() {
                        owner.service();
                    }
                };
                scheduler.tick_with_maintenance::<V, _>(
                    &platform,
                    &config,
                    tx_power,
                    serial_state,
                    display.as_mut(),
                    Some(&mut user_button),
                    &mut maintenance,
                );
            }
            if let Some(owner) = radio_owner.as_mut() {
                owner.service();
            }
            delay.delay_millis(5);
        }
    }
}

fn maybe_start_radio_or_halt(
    platform: &mut platform::Esp32Platform,
    radio_resources: Esp32RadioResources,
    radio_config: MeshRadioConfig,
    tx_power: TxPowerMapping,
    config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
) -> Option<radio::CooperativeRadioOwner>
{
    if START_RADIO_OWNER {
        Some(start_radio_or_halt(
            platform,
            radio_resources,
            radio_config,
            tx_power,
            config,
        ))
    } else {
        drop((radio_resources, radio_config, tx_power));
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Warn,
            DiagnosticSubsystem::Radio,
            "radio_owner_disabled",
            "LoRa/MeshCore task disabled for WiFi isolation test",
        );
        None
    }
}

fn start_radio_or_halt(
    platform: &mut platform::Esp32Platform,
    radio_resources: Esp32RadioResources,
    radio_config: MeshRadioConfig,
    tx_power: TxPowerMapping,
    config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
) -> radio::CooperativeRadioOwner
{
    let owner = match radio::CooperativeRadioOwner::new(
        radio_resources,
        radio_config,
        tx_power,
        platform.now_ms(),
    ) {
        Ok(owner) => owner,
        Err(error) => {
            let (code, message) = match error {
                radio::RadioOwnerStartError::SpiInit => (
                    "radio_spi_init_failed",
                    "SX126x SPI bus initialization failed",
                ),
                radio::RadioOwnerStartError::SpiDevice => (
                    "radio_spi_device_failed",
                    "SX126x SPI device initialization failed",
                ),
                radio::RadioOwnerStartError::RadioInit => {
                    ("radio_init_failed", "SX1262 radio initialization failed")
                },
            };
            record_diagnostic(
                platform.now_ms(),
                DiagnosticLevel::Error,
                DiagnosticSubsystem::Radio,
                code,
                message,
            );
            report_state(GatewayRuntimeState::Error {
                reason: GatewayRuntimeError::Platform,
            });
            loop {
                core::hint::spin_loop();
            }
        },
    };
    record_diagnostic(
        platform.now_ms(),
        DiagnosticLevel::Info,
        DiagnosticSubsystem::Radio,
        "radio_owner_started",
        "LoRa/MeshCore owner started cooperatively on WiFi core",
    );
    if QUEUE_STARTUP_ADVERT {
        meshcore::queue_startup_advert(config, platform.now_ms());
    }

    if tx_power.selected_level != tx_power.requested_level {
        esp_println::println!(
            "Radio: TX power mapped requested={} selected={}",
            tx_power.requested_level,
            tx_power.selected_level,
        );
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Info,
            DiagnosticSubsystem::Radio,
            "tx_power_mapped",
            "board-specific TX power mapping applied",
        );
    }
    owner
}

fn print_config_summary<V>(
    config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
    tx_power: TxPowerMapping,
) where
    V: GateFirmwareVariant,
{
    esp_println::println!("Config: board={}", V::NAME);
    esp_println::println!("Config: name=\"{}\"", config.name.as_str());

    match &config.http {
        Some(http) => {
            let password = if http.wifi_password.is_empty() {
                "<empty>"
            } else {
                "<redacted>"
            };
            esp_println::println!(
                "Config: http enabled port={} tls={} wifi_ssid=\"{}\" wifi_password={} tokens={}",
                http.port,
                http.tls,
                http.wifi_ssid.as_str(),
                password,
                http.tokens.len(),
            );
        },
        None => esp_println::println!("Config: http disabled serial_only=true"),
    }

    match config.display {
        DisplaySettings::Off => esp_println::println!("Config: display disabled"),
        DisplaySettings::Enabled {
            node_display,
            brightness_percent,
        } => {
            esp_println::println!(
                "Config: display enabled node_display={} brightness={}%",
                node_display.as_str(),
                brightness_percent,
            );
        },
    }

    esp_println::println!(
        "Config: polling default_interval={}s min={}s max={}s retry_count={} jitter={}s",
        config.polling.default_interval_secs,
        config.polling.min_interval_secs,
        config.polling.max_interval_secs,
        config.polling.retry_count,
        config.polling.jitter_secs,
    );
    esp_println::println!(
        "Config: radio frequency={}Hz bandwidth={}Hz sf={} cr={} sync=0x{:04x} preamble={} \
         iq_inverted={} tx_power_level={} tx_selected={} tx_ramp={}us",
        config.radio.frequency_hz,
        config.radio.bandwidth_hz,
        config.radio.spreading_factor,
        config.radio.coding_rate,
        config.radio.sync_word,
        config.radio.preamble_len,
        config.radio.iq_inverted,
        tx_power.requested_level,
        tx_power.selected_level,
        config.radio.tx_ramp_time_us,
    );
    esp_println::println!(
        "Config: meshcore public_key={} private_key={} path_mode={}",
        config
            .meshcore
            .public_key
            .as_ref()
            .map(|key| key.as_str())
            .unwrap_or("<missing>"),
        if config.meshcore.private_key.is_some() {
            "<redacted>"
        } else {
            "<missing>"
        },
        config.meshcore.routing.path_mode,
    );
    esp_println::println!("Config: producers={}", config.producers.len());
    for (index, producer) in config.producers.iter().enumerate() {
        let name = if producer.name.is_empty() {
            "<discover>"
        } else {
            producer.name.as_str()
        };
        let password = if producer.password.is_some() {
            "<redacted>"
        } else {
            "<none>"
        };
        let route = "direct";
        esp_println::println!(
            "Config: producer[{}] kind={} enabled={} name=\"{}\" public_key={} password={} \
             interval={}s route={}",
            index,
            producer.kind.as_str(),
            producer.enabled,
            name,
            producer.public_key.as_str(),
            password,
            producer
                .polling_interval_secs
                .unwrap_or(config.polling.default_interval_secs),
            route,
        );
    }
}

/// Print a concise runtime state line for serial/display bring-up.
pub fn report_state(state: GatewayRuntimeState)
{
    match state {
        GatewayRuntimeState::Serving { interfaces } => {
            serial::write_line("Status: serving");
            if let Some(endpoint) = interfaces.http {
                esp_println::println!(
                    "Metrics: http://{}.{}.{}.{}:{}/metrics",
                    endpoint.ipv4[0],
                    endpoint.ipv4[1],
                    endpoint.ipv4[2],
                    endpoint.ipv4[3],
                    endpoint.port,
                );
                esp_println::println!(
                    "Logs: http://{}.{}.{}.{}:{}/logs",
                    endpoint.ipv4[0],
                    endpoint.ipv4[1],
                    endpoint.ipv4[2],
                    endpoint.ipv4[3],
                    endpoint.port,
                );
                esp_println::println!(
                    "Poll now: GET http://{}.{}.{}.{}:{}/poll",
                    endpoint.ipv4[0],
                    endpoint.ipv4[1],
                    endpoint.ipv4[2],
                    endpoint.ipv4[3],
                    endpoint.port,
                );
            }
            if interfaces.usb_serial && serial_json_enabled() {
                serial::write_line("Serial: JSON telemetry enabled");
            }
        },
        GatewayRuntimeState::Unprovisioned
        | GatewayRuntimeState::Provisioned
        | GatewayRuntimeState::Error { .. } => {
            esp_println::println!("Status: {}", state.label());
            esp_println::println!("Next: {}", state.next_step());
        },
    }
}

struct LoadedConfig
{
    config: GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
}

fn load_or_provision<V>(
    platform: &mut platform::Esp32Platform,
    display: Option<&mut LocalDisplay>,
) -> LoadedConfig
where
    V: GateFirmwareVariant,
{
    match storage::load_config() {
        Ok(config) => {
            run_usb_config_update_window::<V>(platform, display);
            LoadedConfig { config }
        },
        Err(_) => enter_usb_provisioning::<V>(platform, display),
    }
}

fn run_usb_config_update_window<V>(
    platform: &mut platform::Esp32Platform,
    display: Option<&mut LocalDisplay>,
) where
    V: GateFirmwareVariant,
{
    let Some(usb_serial) = platform.take_usb_serial() else {
        return;
    };

    let mut service = UsbConfigUpdateService::new(serial::UsbJsonReceiver::new(usb_serial));
    let start_ms = platform.now_ms();
    let mut last_ready_ms = start_ms.saturating_sub(USB_CONFIG_READY_INTERVAL_MS);
    let delay = Delay::new();
    let mut display = display;

    while platform.now_ms().saturating_sub(start_ms) < USB_CONFIG_UPDATE_WINDOW_MS {
        let now_ms = platform.now_ms();
        if now_ms.saturating_sub(last_ready_ms) >= USB_CONFIG_READY_INTERVAL_MS {
            write_config_ready();
            last_ready_ms = now_ms;
        }
        service.poll::<V>(platform, display.as_deref_mut());
        if service.reboot_required() {
            delay.delay_millis(1_000);
            reboot_device();
        }
        delay.delay_millis(1);
    }
}

fn enter_usb_provisioning<V>(
    platform: &mut platform::Esp32Platform,
    mut display: Option<&mut LocalDisplay>,
) -> LoadedConfig
where
    V: GateFirmwareVariant,
{
    report_state(GatewayRuntimeState::Unprovisioned);
    if let Some(display) = display.as_deref_mut() {
        let _ = display.show_unprovisioned();
    }
    record_diagnostic(
        platform.now_ms(),
        DiagnosticLevel::Warn,
        DiagnosticSubsystem::Config,
        "missing_config",
        "no valid saved config; waiting for USB provisioning",
    );

    let Some(usb_serial) = platform.take_usb_serial() else {
        serial::write_line("Config: USB Serial/JTAG unavailable");
        if let Some(display) = display.as_deref_mut() {
            let _ = display.show_config_error("usb unavailable");
        }
        report_state(GatewayRuntimeState::Error {
            reason: GatewayRuntimeError::Platform,
        });
        loop {
            core::hint::spin_loop();
        }
    };

    let mut receiver = serial::UsbJsonReceiver::new(usb_serial);
    write_config_ready();
    let mut last_activity_ms = None;
    let mut last_displayed_bytes = 0_usize;
    let mut last_ready_ms = platform.now_ms();
    let delay = Delay::new();

    loop {
        match receiver.poll_document() {
            Ok(Some(document)) => {
                last_activity_ms = None;
                last_displayed_bytes = 0;
                match handle_provisioning_document::<V>(
                    platform,
                    display.as_deref_mut(),
                    document.as_str(),
                ) {
                    Ok(Some(config)) => {
                        return LoadedConfig { config };
                    },
                    Ok(None) => {},
                    Err(error) => {
                        write_provisioning_error(error);
                        record_diagnostic(
                            platform.now_ms(),
                            DiagnosticLevel::Error,
                            DiagnosticSubsystem::Config,
                            "provisioning_parse_failed",
                            error.as_str(),
                        );
                        if let Some(display) = display.as_deref_mut() {
                            let _ = display.show_config_error(error.as_str());
                        }
                    },
                }
            },
            Ok(None) => {
                if receiver.take_activity() {
                    last_activity_ms = Some(platform.now_ms());
                    let buffered = receiver.buffered_bytes();
                    if buffered != last_displayed_bytes {
                        last_displayed_bytes = buffered;
                        if let Some(display) = display.as_deref_mut() {
                            let _ = display.show_config_receiving(buffered);
                        }
                    }
                }

                if receiver.buffered_bytes() > 0
                    && let Some(last_ms) = last_activity_ms
                    && platform.now_ms().saturating_sub(last_ms)
                        >= USB_CONFIG_UPLOAD_IDLE_TIMEOUT_MS
                {
                    last_activity_ms = None;
                    last_displayed_bytes = 0;
                    match receiver.take_buffered_document() {
                        Ok(document) => {
                            match handle_provisioning_document::<V>(
                                platform,
                                display.as_deref_mut(),
                                document.as_str(),
                            ) {
                                Ok(Some(config)) => {
                                    return LoadedConfig { config };
                                },
                                Ok(None) => {},
                                Err(error) => {
                                    write_provisioning_error(error);
                                    record_diagnostic(
                                        platform.now_ms(),
                                        DiagnosticLevel::Error,
                                        DiagnosticSubsystem::Config,
                                        "provisioning_parse_failed",
                                        error.as_str(),
                                    );
                                    if let Some(display) = display.as_deref_mut() {
                                        let _ = display.show_config_error(error.as_str());
                                    }
                                },
                            }
                        },
                        Err(_) => {
                            write_provisioning_error(ProvisioningError::InvalidConfig);
                            if let Some(display) = display.as_deref_mut() {
                                let _ = display.show_config_error("upload timeout");
                            }
                        },
                    }
                }
            },
            Err(_) => {
                write_provisioning_error(ProvisioningError::InvalidConfig);
                receiver.discard_partial();
                last_activity_ms = None;
                last_displayed_bytes = 0;
                if let Some(display) = display.as_deref_mut() {
                    let _ = display.show_config_error("usb buffer");
                }
            },
        }
        let now_ms = platform.now_ms();
        if receiver.buffered_bytes() == 0
            && now_ms.saturating_sub(last_ready_ms) >= USB_CONFIG_READY_INTERVAL_MS
        {
            write_config_ready();
            last_ready_ms = now_ms;
        }
        delay.delay_millis(1);
    }
}

fn handle_provisioning_document<V>(
    platform: &mut platform::Esp32Platform,
    display: Option<&mut LocalDisplay>,
    document: &str,
) -> Result<Option<GatewayConfig<MAX_TELEMETRY_PRODUCERS>>, ProvisioningError>
where
    V: GateFirmwareVariant,
{
    match provisioning::parse_usb_document(document)? {
        ProvisioningCommand::SetConfig(config) => {
            esp_println::println!(
                r#"{{"type":"progress","request":"set_config","stage":"received","bytes":{}}}"#,
                document.len(),
            );
            if config_update::validate_outputs::<V>(&config).is_err() {
                return Err(ProvisioningError::InvalidConfig);
            }
            serial::write_line(r#"{"type":"progress","request":"set_config","stage":"saving"}"#);
            if let Err(error) = storage::save_config_document(document, &config) {
                write_storage_error(error);
                record_diagnostic(
                    platform.now_ms(),
                    DiagnosticLevel::Error,
                    DiagnosticSubsystem::Config,
                    "config_storage_failed",
                    error.as_code(),
                );
                if let Some(display) = display {
                    let _ = display.show_config_error(error.as_code());
                }
                return Ok(None);
            }
            serial::write_line(r#"{"type":"ok","request":"set_config"}"#);
            record_diagnostic(
                platform.now_ms(),
                DiagnosticLevel::Info,
                DiagnosticSubsystem::Config,
                "config_accepted",
                "valid USB config accepted",
            );
            if let Some(display) = display {
                let _ = display.show_config_accepted();
            }
            Ok(Some(config.as_ref().clone()))
        },
        ProvisioningCommand::Help => {
            write_provisioning_help();
            Ok(None)
        },
        ProvisioningCommand::Status => {
            serial::write_line(r#"{"type":"status","state":"not_provisioned"}"#);
            report_state(GatewayRuntimeState::Unprovisioned);
            Ok(None)
        },
        ProvisioningCommand::PollNow => {
            serial::write_line(r#"{"type":"error","request":"poll_now","code":"not_provisioned"}"#);
            Ok(None)
        },
        ProvisioningCommand::Reboot => reboot_device(),
    }
}

fn write_provisioning_help()
{
    serial::write_line(r#"{"type":"help","commands":["set_config","status","help","poll_now"]}"#);
    serial::write_line("upload raw config JSON or JSONC to provision the gateway");
}

fn write_config_ready()
{
    serial::write_line(r#"{"type":"ready","request":"set_config"}"#);
}

fn write_provisioning_error(error: ProvisioningError)
{
    esp_println::println!(
        r#"{{"type":"error","request":"set_config","code":"{}"}}"#,
        error.as_str(),
    );
}

fn write_storage_error(error: storage::StorageError)
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
}

fn reboot_device() -> !
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
