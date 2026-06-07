//! Bifrost Gate ProS3 bring-up runner.
//!
//! The runner is isolated from [`crate::run_gateway`] (which is hard-wired
//! to the Heltec V4 GPIO / SX1262 / SSD1306 resource map) so the two boards
//! can evolve independently. Architecture:
//!
//! ```text
//!     run_bifrost_pros3
//!            │
//!            ▼
//!     BoardServices::init  ── composes every peripheral handle
//!            │
//!            ▼
//!     poll_loop  ── battery read → WiFi scan (15s) → UI render → LED
//! ```
//!
//! Each capability lives in its own submodule and exposes a typed owning
//! handle that fails gracefully (returns `None`) if the underlying hardware
//! is missing or misbehaving. The runner never panics on a missing
//! peripheral; the relevant `Option<_>` slot just stays `None`.
//!
//! Submodule map:
//!
//! - [`antenna`]    — ProS3[D] 2.4 GHz RF switch (GPIO 11).
//! - [`battery`]    — MAX17048 LiPo fuel gauge on the shared STEMMA I2C bus.
//! - [`board`]      — `BoardServices` facade that owns every peripheral.
//! - [`display`]    — SSD1327 128×128 4-bit grayscale OLED driver.
//! - [`i2c_bus`]    — Shared blocking I2C bus for STEMMA peripherals.
//! - [`power`]      — LDO2 enable line (STEMMA + RGB rail).
//! - [`status_led`] — WS2812 RGB LED driver via RMT.
//! - [`ui`]         — Grayscale screen renderers (boot/header/provisioning/connecting).
//! - [`wifi`]       — esp-wifi station controller, scan-only.
//!
//! Phase machine (runner-level):
//!
//! ```text
//!   init → splash (500ms, USB rx active, advert spinner animates)
//!            │
//!            ├── config received → Connecting (provisioning loop yields)
//!            └── timeout → Unprovisioned (USB rx still polled)
//!                            │
//!                            └── config received later → Connecting
//! ```

use core::fmt::Write as _;

use embassy_futures::block_on;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use muninn_gate_core::config::MAX_TELEMETRY_PRODUCERS;
use muninn_gate_core::poller::PollScheduler;
use muninn_gate_core::telemetry::TelemetryRecord;
use muninn_gate_core::{DisplayTelemetryValue, GateFirmwareVariant, TimeSettings};
use muninn_gate_platform_esp32::http_server::{HttpControlCommand, HttpServer};
use muninn_gate_platform_esp32::meshcore::{self, RadioMeshcoreClient};
use muninn_gate_platform_esp32::power::PowerMonitor;
use muninn_gate_platform_esp32::provisioning::{self, ProvisioningCommand};
use muninn_gate_platform_esp32::radio::mesh_radio_config_from_gateway;
use muninn_gate_platform_esp32::storage::{self, Esp32GatewayConfig};
use muninn_gate_platform_esp32::telemetry_state::{self, SharedTelemetryStore};
use muninn_gate_platform_esp32::wifi_common::{self, DhcpOutcome};
use muninn_gate_platform_esp32::{Esp32BoardVariant, poll_control};
use muninn_gate_time::GatewayTime;

use crate::board::BoardServices;
use crate::power_monitor;
use crate::status_led::{LedDriver, LedPhase};
use crate::ui::{NetworkPhase, OtaUpdateStatus, PollStatus, ProducerSummary, Screen, UiState};

/// Poll loop cadence. Drives the fuel-gauge read, UI render, and LED
/// refresh. Battery + LED are cheap; the I2C flush of the framebuffer is
/// the dominant cost (~200 ms at 400 kHz I2C).
const POLL_INTERVAL_MS: u32 = 1_000;
/// Print a serial status line every N ticks (≈ once per 5 s at 1 Hz poll).
const STATUS_LOG_PERIOD_TICKS: u32 = 5;
/// Re-scan visible WiFi networks at this cadence (only while no config is
/// loaded — once we're trying to associate, the radio is busy with that and
/// extra scans churn the controller).
const WIFI_SCAN_INTERVAL_MS: u64 = 15_000;
/// After this many ms with no association, flip the phase to `Error` and
/// stop animating "Connecting" — gives the operator a clear signal that the
/// credentials probably need a second look.
const WIFI_CONNECT_TIMEOUT_MS: u64 = 30_000;
/// Poll cadence while waiting for `is_connected()` to flip true. The main
/// loop runs at 1 Hz, which would mean we miss the moment of association
/// by up to a second; this inner loop catches it within `100 ms` once
/// we've requested a connect.
const WIFI_CONNECT_POLL_MS: u32 = 100;
/// Bail out of the fast `is_connected()` poll after this many ms — caller
/// falls back to the slow 1 Hz check inside the main poll loop.
const WIFI_CONNECT_POLL_BUDGET_MS: u32 = 15_000;
/// Periodic WiFi/L3 health cadence after HTTP starts serving.
const WIFI_HEALTH_CHECK_MS: u64 = 5_000;
/// Periodic same-SSID AP quality scan while already connected.
///
/// This is intentionally much slower than the health check: active WiFi scans
/// are blocking and can stall smoltcp briefly, so use them only to escape a
/// clearly-worse extender/router association.
const WIFI_ROAM_SCAN_INTERVAL_MS: u64 = 2 * 60 * 1_000;
/// Required RSSI gain before roaming away from a usable AP.
const WIFI_ROAM_MIN_IMPROVEMENT_DB: i16 = 12;
/// RSSI threshold where the current AP is weak enough to accept a smaller gain.
const WIFI_ROAM_WEAK_RSSI_DBM: i8 = -70;
/// Required RSSI gain when the current AP is already weak.
const WIFI_ROAM_WEAK_IMPROVEMENT_DB: i16 = 8;
/// Minimum spacing between disruptive network recovery actions.
const NETWORK_RECOVERY_COOLDOWN_MS: u64 = 30_000;
/// Proactively refresh DHCP/TCP state before short AP leases or stale
/// smoltcp sockets can strand the endpoint.
const NETWORK_PREEMPTIVE_REFRESH_MS: u64 = 30 * 60 * 1_000;
/// Right-shift applied per WS2812 channel to keep the LED comfortably dim.
const RGB_DIM_SHIFT: u8 = 4;
/// How often to emit the USB config-ready marker. Keep this present so
/// `tools/cli.py` can sync to a running device, but avoid flooding normal
/// serial monitors.
const PROVISIONING_READY_INTERVAL_MS: u64 = 5_000;
/// Drop an incomplete USB config document after this much idle time. Without
/// this, one stale byte from an interrupted host upload can keep LoRa paused
/// forever.
const USB_UPLOAD_IDLE_TIMEOUT_MS: u64 =
    muninn_gate_platform_esp32::USB_CONFIG_UPLOAD_IDLE_TIMEOUT_MS;
/// Poll USB this often while a config upload is in flight. The USB-Serial/JTAG
/// FIFO is small enough that waiting for the 1 Hz UI tick can drop host bytes.
const USB_UPLOAD_DRAIN_TICK_MS: u32 = 10;
/// Spend up to one normal outer-loop period draining an incomplete upload
/// before yielding back to the rest of the firmware.
const USB_UPLOAD_DRAIN_BUDGET_MS: u32 = POLL_INTERVAL_MS;
/// Total length of the boot advert splash. This is an actual LoRa service
/// window: a stored-config boot has already queued the gateway advert, and
/// the loop below services the SX1262 while the spinner is visible.
const SPLASH_DURATION_MS: u32 = 500;
/// Splash inner tick rate. At 800 kHz I2C the SSD1327 frame flush fits
/// inside this budget while giving the 500 ms advert spinner four frames.
const SPLASH_TICK_MS: u32 = 125;
/// UI title shown when no user config is loaded yet. The bracketed `???`
/// reads as "device has no identity assigned" and is replaced by
/// `config.name` from the uploaded `config.json` once the USB provisioning
/// flow lands for this board variant.
const PLACEHOLDER_TITLE: &str = "[???]";

/// Backstop delay for the scheduler's normal startup spread. Once the
/// operational producer screen is actually reachable, the runtime forces
/// every producer due immediately so the operator gets live metrics right
/// away; this delay only matters if the device never reaches that screen.
const INITIAL_SCHEDULE_DELAY_MS: u64 = 60_000;
/// Minimum interval between UI refreshes from the telemetry snapshot.
/// Polls take seconds to complete, so a 1 s cadence comfortably catches
/// every transition without burning I2C on identical frames.
const PRODUCER_UI_REFRESH_MS: u64 = 1_000;
/// Drain passive LoRa observations often enough that background MeshCore
/// traffic does not fill the RX queue between active producer polls.
const PASSIVE_OBSERVATION_DRAIN_MS: u64 = 250;
/// Minimum interval between direct gateway adverts queued ahead of poll rounds.
///
/// This is not flood routing; it is a bounded direct advert refresh so producers
/// that missed the boot splash still learn the gateway public key before the
/// next login/telemetry request.
const POLL_DIRECT_ADVERT_MIN_INTERVAL_MS: u64 = 60_000;
#[derive(Default)]
struct UsbProvisioningState
{
    last_activity_ms: Option<u64>,
}

#[derive(Default)]
struct ProvisioningPollResult
{
    config_applied: bool,
    upload_active:  bool,
    reload_network: bool,
    preserved_ipv4: Option<[u8; 4]>,
}

struct PreservedNetworkState
{
    ssid: heapless::String<32>,
    ipv4: [u8; 4],
}

#[derive(Default)]
struct NetworkRecoveryState
{
    last_health_check_ms:  u64,
    last_recovery_ms:      u64,
    last_refresh_ms:       u64,
    last_roam_scan_ms:     u64,
    last_send_error_total: u32,
    last_associated:       Option<bool>,
}

impl NetworkRecoveryState
{
    const fn new(now_ms: u64) -> Self
    {
        Self {
            last_health_check_ms:  now_ms,
            last_recovery_ms:      0,
            last_refresh_ms:       now_ms,
            last_roam_scan_ms:     now_ms,
            last_send_error_total: 0,
            last_associated:       None,
        }
    }

    fn recovery_allowed(&self, now_ms: u64) -> bool
    {
        self.last_recovery_ms == 0
            || now_ms.saturating_sub(self.last_recovery_ms) >= NETWORK_RECOVERY_COOLDOWN_MS
    }

    fn mark_recovery(&mut self, now_ms: u64)
    {
        self.last_recovery_ms = now_ms;
        self.last_refresh_ms = now_ms;
    }
}

/// Run the Bifrost Gate ProS3 firmware forever.
pub fn run<V>() -> !
where
    V: GateFirmwareVariant + Esp32BoardVariant,
{
    let cpu_config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(cpu_config);
    let mut services = BoardServices::init(peripherals);

    services.print_banner::<V>();
    services.dim_led_to_off();

    let mut ui_state = UiState::new();
    let _ = ui_state.title.push_str(PLACEHOLDER_TITLE);
    let mut wall_time = GatewayTime::default();
    let _ = services.init_wall_time_from_rtc(&mut wall_time, ui_state.now_ms);

    let mut led_driver = LedDriver::new();
    let pending_credentials = try_load_stored_config(&mut services, &mut ui_state, &mut wall_time);
    // If a stored config landed, bring the SX1262 up with the radio
    // params from it before we hand off to the splash + WiFi loop.
    // `lora_available` reflects the result so the operational screen
    // can drop the "radio not wired" panel as soon as we hit Online.
    try_init_lora_from_config::<V>(&mut services, ui_state.now_ms);
    ui_state.lora_available = services.lora_owner.is_some();
    // Prime the MeshCore poll plumbing (request tag counter + telemetry
    // store) from whatever config landed. The scheduler keeps recreating
    // itself whenever a new config arrives via USB.
    let mut scheduler = try_bring_up_meshcore::<V>(&mut services, ui_state.now_ms, &wall_time);
    let stored_applied = pending_credentials.is_some();
    let config_received = splash_phase::<V>(
        &mut services,
        &mut ui_state,
        &mut led_driver,
        &mut scheduler,
        &mut wall_time,
        stored_applied,
    );
    // Kick WiFi AFTER splash so the user actually sees the animated splash
    // screen — `try_connect` blocks ~2 s for the pre-scan and we don't want
    // the panel sitting black during that window.
    if let Some((ssid, password)) = pending_credentials {
        kick_wifi(&mut services, &ssid, &password, ui_state.now_ms);
    }
    poll_loop::<V>(
        &mut services,
        &mut ui_state,
        &mut led_driver,
        &mut scheduler,
        &mut wall_time,
        config_received,
    );
}

/// One-shot bring-up for the MeshCore poll plumbing. Initializes the
/// request-tag counter, resets the shared telemetry store from the
/// active gateway config, and constructs a fresh [`PollScheduler`].
/// Returns `None` if there's no active config — the caller retries
/// after USB provisioning lands one.
fn try_bring_up_meshcore<V>(
    services: &mut BoardServices,
    now_ms: u64,
    wall_time: &GatewayTime,
) -> Option<PollScheduler<MAX_TELEMETRY_PRODUCERS>>
where
    V: Esp32BoardVariant,
{
    let config = services.active_config.as_ref()?;
    let tx_power = V::map_tx_power(config.radio.tx_power_level);
    if telemetry_state::reset_from_config(config, now_ms, tx_power).is_err() {
        esp_println::println!("Meshcore: telemetry store init failed");
        return None;
    }
    meshcore::initialize_request_tags_from_time(now_ms, wall_time.unix_seconds(now_ms));
    // Queue one direct-routed signed gateway advert so producers learn
    // our public key before polling.
    let _ =
        meshcore::queue_gateway_advert(config, now_ms, wall_time.unix_seconds(now_ms), "startup");
    let scheduler_start_ms = now_ms.saturating_add(INITIAL_SCHEDULE_DELAY_MS);
    match PollScheduler::new(config, scheduler_start_ms) {
        Ok(scheduler) => {
            esp_println::println!(
                "Meshcore: scheduler ready, first poll forced on operational screen; backstop \
                 t+{}ms ({} producers)",
                INITIAL_SCHEDULE_DELAY_MS,
                config.producers.len(),
            );
            Some(scheduler)
        },
        Err(e) => {
            esp_println::println!("Meshcore: scheduler init failed ({:?})", e);
            None
        },
    }
}

/// Build the radio config from the active gateway config and bring the
/// SX1262 up. No-op if the LoRa owner is already initialized or if no
/// config has been applied yet.
fn try_init_lora_from_config<V>(services: &mut BoardServices, now_ms: u64)
where
    V: Esp32BoardVariant,
{
    if services.lora_owner.is_some() || services.lora_pending.is_none() {
        return;
    }
    // Extract radio config first so the immutable borrow on
    // services.active_config ends before init_lora's mutable borrow
    // begins.
    let derived = services.active_config.as_ref().and_then(|config| {
        match mesh_radio_config_from_gateway::<V>(config) {
            Ok(radio_config) => {
                let tx_power = V::map_tx_power(config.radio.tx_power_level);
                Some((radio_config, tx_power))
            },
            Err(_) => {
                esp_println::println!("LoRa: invalid radio config in gateway config; init skipped");
                None
            },
        }
    });
    if let Some((radio_config, tx_power)) = derived {
        services.init_lora(radio_config, tx_power, now_ms);
    }
}

/// Heapless string pair returned by [`apply_config`] when the loaded
/// config has WiFi credentials to act on.
type WifiCredentials = (heapless::String<32>, heapless::String<64>);

/// Try to load a previously persisted config from the `muninn_cfg` flash
/// partition. On success, applies it to UI state and returns the WiFi
/// credentials so the caller can kick the WiFi controller at the right
/// moment (after splash, so the splash animation is visible).
fn try_load_stored_config(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    wall_time: &mut GatewayTime,
) -> Option<WifiCredentials>
{
    match storage::load_config() {
        Ok(config) => {
            esp_println::println!(
                "Storage: loaded stored config name=\"{}\" producers={}",
                config.name.as_str(),
                config.producers.len(),
            );
            let creds = apply_config(services, ui_state, &config);
            apply_time_settings(
                services,
                ui_state,
                wall_time,
                &config,
                false,
                "stored config",
            );
            creds
        },
        Err(_) => {
            esp_println::println!("Storage: no stored config — awaiting USB provisioning");
            None
        },
    }
}

fn apply_time_settings(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    wall_time: &mut GatewayTime,
    config: &Esp32GatewayConfig,
    persist_to_rtc: bool,
    source: &'static str,
)
{
    wall_time.apply_settings(config.time, ui_state.now_ms);
    sync_clock_text(ui_state, wall_time);
    if persist_to_rtc && config.time.unix_time_seconds.is_some() {
        let rtc_updated = services.update_rtc_from_wall_time(wall_time, ui_state.now_ms);
        esp_println::println!(
            "Time: {} seed applied unix={:?} offset={}min rtc_updated={}",
            source,
            config.time.unix_time_seconds,
            wall_time.utc_offset_minutes(),
            rtc_updated,
        );
    }
}

/// Push a validated config into UI state and stash it on
/// [`BoardServices::active_config`] so the HTTP server can authenticate
/// requests against the configured bearer tokens. Returns the WiFi
/// credentials when the config has them, so the caller can decide
/// when to call [`kick_wifi`].
fn apply_config(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    config: &Esp32GatewayConfig,
) -> Option<WifiCredentials>
{
    services.active_config = Some(config.clone());
    ui_state.title.clear();
    let _ = ui_state.title.push_str(config.name.as_str());
    ui_state.gateway_pubkey_prefix.clear();
    if let Some(public_key) = config.meshcore.public_key.as_ref() {
        for ch in public_key.as_str().chars().take(4) {
            let _ = ui_state.gateway_pubkey_prefix.push(ch.to_ascii_uppercase());
        }
    }

    let mut ssid: heapless::String<32> = heapless::String::new();
    let mut password: heapless::String<64> = heapless::String::new();
    if let Some(http) = config.http.as_ref() {
        let _ = ssid.push_str(http.wifi_ssid.as_str());
        let _ = password.push_str(http.wifi_password.as_str());
        ui_state.http_port = http.port;
    }
    ui_state.network = NetworkPhase::Connecting { ssid: ssid.clone() };

    ui_state.producers.clear();
    for producer in config.producers.iter() {
        let mut name: heapless::String<32> = heapless::String::new();
        let _ = name.push_str(producer.name.as_str());
        // All producers start in `Pending` — the LoRa poller (still to
        // land) drives them through InProgress → Success/Failed each
        // cycle and updates `last_poll_at_ms` + `metric_value`.
        let summary = ProducerSummary {
            name,
            kind_label: producer.kind.as_str(),
            enabled: producer.enabled,
            status: PollStatus::Pending,
            last_poll_at_ms: None,
            metric_value: None,
            metric_unit: "",
        };
        if ui_state.producers.push(summary).is_err() {
            break;
        }
    }

    if ssid.is_empty() {
        None
    } else {
        Some((ssid, password))
    }
}

fn apply_http_time_sync(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    wall_time: &mut GatewayTime,
    settings: TimeSettings,
)
{
    wall_time.apply_settings(settings, ui_state.now_ms);
    sync_clock_text(ui_state, wall_time);
    if let Some(config) = services.active_config.as_mut() {
        config.time = settings;
    }
    let rtc_updated = if settings.unix_time_seconds.is_some() {
        services.update_rtc_from_wall_time(wall_time, ui_state.now_ms)
    } else {
        false
    };
    esp_println::println!(
        "Time: HTTP sync applied unix={:?} offset={}min rtc_updated={}",
        settings.unix_time_seconds,
        wall_time.utc_offset_minutes(),
        rtc_updated,
    );
}

fn handle_http_control_command<V>(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    led_driver: &mut LedDriver,
    scheduler: &mut Option<PollScheduler<MAX_TELEMETRY_PRODUCERS>>,
    wall_time: &mut GatewayTime,
    network_recovery: &mut NetworkRecoveryState,
    connect_started_ms: &mut Option<u64>,
    initial_operational_poll_requested: &mut bool,
    config_loaded: &mut bool,
    ota_flashing: &mut bool,
    command: HttpControlCommand,
) where
    V: GateFirmwareVariant + Esp32BoardVariant,
{
    match command {
        HttpControlCommand::SyncTime(settings) => {
            apply_http_time_sync(services, ui_state, wall_time, settings);
        },
        HttpControlCommand::OtaRequested => {
            *ota_flashing = true;
            if ui_state.ota_update.is_none() {
                ui_state.ota_update = Some(OtaUpdateStatus {
                    received_bytes: None,
                    total_bytes:    None,
                });
            }
            led_driver.set_phase(LedPhase::OtaFlashing, ui_state.now_ms);
            drive_status_led(services, led_driver, ui_state.now_ms);
            render_screen(services, ui_state, Screen::OtaUpdate, 0);
            esp_println::println!("OTA: request accepted; purple update indicator active");
        },
        HttpControlCommand::OtaFinished => {
            *ota_flashing = false;
            ui_state.ota_update = None;
            let new_phase =
                led_phase_from_state(&ui_state.network, *config_loaded, ui_state.lora_available);
            led_driver.set_phase(new_phase, ui_state.now_ms);
            drive_status_led(services, led_driver, ui_state.now_ms);
            esp_println::println!("OTA: update indicator cleared");
        },
        HttpControlCommand::SetConfig { document, config } => {
            let preserved_network = preserved_network_state_for_config(services, &config);
            esp_println::println!(
                "HTTP config: name=\"{}\" producers={}",
                config.name.as_str(),
                config.producers.len(),
            );
            match storage::save_config_document(document.as_str(), &config) {
                Ok(()) => esp_println::println!(
                    "Storage: persisted HTTP config ({} bytes)",
                    document.len(),
                ),
                Err(err) => esp_println::println!(
                    "Storage: HTTP save failed: {:?} (config kept in RAM only)",
                    err,
                ),
            }

            let creds = apply_config(services, ui_state, &config);
            apply_time_settings(services, ui_state, wall_time, &config, true, "HTTP config");
            let reload_network = preserved_network.is_none();
            let preserved_ipv4 = preserved_network.as_ref().map(|network| network.ipv4);
            if let Some(network) = preserved_network {
                ui_state.network = NetworkPhase::Connected {
                    ssid: network.ssid,
                    rssi: 0,
                    ipv4: network.ipv4,
                };
                esp_println::println!(
                    "Config: preserved WiFi endpoint {}.{}.{}.{} after HTTP update",
                    network.ipv4[0],
                    network.ipv4[1],
                    network.ipv4[2],
                    network.ipv4[3],
                );
            } else if let Some((ssid, password)) = creds {
                kick_wifi(services, &ssid, &password, ui_state.now_ms);
            }

            if reload_network {
                reset_http_server_for_network_change(services);
            }
            *config_loaded = true;
            try_init_lora_from_config::<V>(services, ui_state.now_ms);
            ui_state.lora_available = services.lora_owner.is_some();
            *scheduler = try_bring_up_meshcore::<V>(services, ui_state.now_ms, wall_time);
            if let Some(ipv4) = preserved_ipv4 {
                telemetry_state::record_network_started(ui_state.now_ms);
                telemetry_state::record_wifi_connected_at(ui_state.now_ms);
                wifi_common::record_connected_ap_info("config-preserve");
                telemetry_state::record_dhcp_configured(ui_state.now_ms, 0, ipv4, None);
                telemetry_state::record_http_serving_started(ui_state.now_ms);
            }
            *initial_operational_poll_requested = false;
            *network_recovery = NetworkRecoveryState::new(ui_state.now_ms);
            if reload_network {
                *connect_started_ms = Some(ui_state.now_ms);
                fast_poll_until_associated(services, ui_state, ui_state.now_ms);
            } else {
                *connect_started_ms = None;
            }
        },
    }
}

fn poll_http_once<V>(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    led_driver: &mut LedDriver,
    scheduler: &mut Option<PollScheduler<MAX_TELEMETRY_PRODUCERS>>,
    wall_time: &mut GatewayTime,
    network_recovery: &mut NetworkRecoveryState,
    connect_started_ms: &mut Option<u64>,
    initial_operational_poll_requested: &mut bool,
    config_loaded: &mut bool,
    ota_flashing: &mut bool,
) -> bool
where
    V: GateFirmwareVariant + Esp32BoardVariant,
{
    let tx_power = V::map_tx_power(0);
    let (http_command, ota_upload) = if let (Some(server), Some(cfg)) = (
        services.http_server.as_mut(),
        services.active_config.as_ref(),
    ) {
        if let Err(e) = server.poll(ui_state.now_ms, cfg, tx_power) {
            esp_println::println!("HTTP: poll error {:?}", e);
            server.reset_network();
            (None, None)
        } else {
            (server.take_control_command(), server.ota_upload_snapshot())
        }
    } else {
        (None, None)
    };

    if let Some(ota_upload) = ota_upload {
        ui_state.ota_update = Some(OtaUpdateStatus {
            received_bytes: Some(ota_upload.received_bytes),
            total_bytes:    Some(ota_upload.total_bytes),
        });
    }
    if let Some(command) = http_command {
        handle_http_control_command::<V>(
            services,
            ui_state,
            led_driver,
            scheduler,
            wall_time,
            network_recovery,
            connect_started_ms,
            initial_operational_poll_requested,
            config_loaded,
            ota_flashing,
            command,
        );
    }

    *ota_flashing || ui_state.ota_update.is_some()
}

/// Ask the WiFi controller to associate with the configured SSID. Errors
/// are logged but not returned — the runner's connection-state machine
/// observes the same outcome via `is_connected()`.
fn kick_wifi(services: &mut BoardServices, ssid: &str, password: &str, now_ms: u64)
{
    kick_wifi_with_ap(services, ssid, password, None, now_ms);
}

fn kick_wifi_with_ap(
    services: &mut BoardServices,
    ssid: &str,
    password: &str,
    pinned_ap: Option<wifi_common::PinnedAp>,
    now_ms: u64,
)
{
    if let Some(scanner) = services.wifi.as_mut() {
        telemetry_state::record_network_started(now_ms);
        let connect_result = if let Some(pin) = pinned_ap {
            scanner.try_connect_pinned(ssid, password, pin, now_ms)
        } else {
            scanner.try_connect(ssid, password, now_ms)
        };
        match connect_result {
            Ok(()) => esp_println::println!("WiFi: connect requested ssid=\"{}\"", ssid),
            Err(e) => {
                esp_println::println!("WiFi: try_connect failed for ssid=\"{}\": {:?}", ssid, e,)
            },
        }
    }
}

fn active_wifi_credentials(services: &BoardServices) -> Option<WifiCredentials>
{
    let http = services.active_config.as_ref()?.http.as_ref()?;
    let mut ssid: heapless::String<32> = heapless::String::new();
    let mut password: heapless::String<64> = heapless::String::new();
    let _ = ssid.push_str(http.wifi_ssid.as_str());
    let _ = password.push_str(http.wifi_password.as_str());
    if ssid.is_empty() {
        None
    } else {
        Some((ssid, password))
    }
}

fn preserved_network_state_for_config(
    services: &mut BoardServices,
    config: &Esp32GatewayConfig,
) -> Option<PreservedNetworkState>
{
    let old_http = services.active_config.as_ref()?.http.as_ref()?;
    let new_http = config.http.as_ref()?;
    if old_http.port != new_http.port
        || old_http.wifi_ssid.as_str() != new_http.wifi_ssid.as_str()
        || old_http.wifi_password.as_str() != new_http.wifi_password.as_str()
    {
        return None;
    }

    let endpoint = services.http_server.as_ref()?.endpoint()?;
    let associated = services
        .wifi
        .as_mut()
        .map(|wifi| wifi.is_connected())
        .unwrap_or(false);
    if !associated {
        return None;
    }

    let mut ssid: heapless::String<32> = heapless::String::new();
    if ssid.push_str(new_http.wifi_ssid.as_str()).is_err() {
        return None;
    }
    Some(PreservedNetworkState {
        ssid,
        ipv4: endpoint.ipv4,
    })
}

fn reset_http_server_for_network_change(services: &mut BoardServices)
{
    if let Some(server) = services.http_server.as_mut() {
        server.reset_network();
    }
}

fn maintain_network<V>(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    recovery: &mut NetworkRecoveryState,
    connect_started_ms: &mut Option<u64>,
) where
    V: Esp32BoardVariant,
{
    let now_ms = ui_state.now_ms;
    if now_ms.saturating_sub(recovery.last_health_check_ms) < WIFI_HEALTH_CHECK_MS {
        return;
    }
    recovery.last_health_check_ms = now_ms;

    let Some((ssid, password)) = active_wifi_credentials(services) else {
        return;
    };

    let associated = services
        .wifi
        .as_mut()
        .map(|wifi| wifi.is_connected())
        .unwrap_or(false);
    if recovery.last_associated != Some(associated) {
        recovery.last_associated = Some(associated);
        esp_println::println!(
            "WiFi watchdog: associated={} endpoint={}",
            associated,
            if services
                .http_server
                .as_ref()
                .and_then(|server| server.endpoint())
                .is_some()
            {
                "up"
            } else {
                "down"
            },
        );
    }

    if !associated {
        reconnect_wifi(
            services,
            ui_state,
            recovery,
            connect_started_ms,
            &ssid,
            &password,
            None,
            "association_lost",
        );
        return;
    }

    let Some(server) = services.http_server.as_ref() else {
        reconnect_wifi(
            services,
            ui_state,
            recovery,
            connect_started_ms,
            &ssid,
            &password,
            None,
            "http_server_missing",
        );
        return;
    };
    let endpoint_missing = server.endpoint().is_none();
    let send_error_total = server.send_error_total();
    let send_error_seen = send_error_total != recovery.last_send_error_total;
    let refresh_due =
        now_ms.saturating_sub(recovery.last_refresh_ms) >= NETWORK_PREEMPTIVE_REFRESH_MS;
    recovery.last_send_error_total = send_error_total;

    if endpoint_missing {
        if recover_http_dhcp(services, ui_state, recovery, "endpoint_missing") {
            return;
        }
        reconnect_wifi(
            services,
            ui_state,
            recovery,
            connect_started_ms,
            &ssid,
            &password,
            None,
            "dhcp_endpoint_missing",
        );
    } else if send_error_seen {
        if !recover_http_dhcp(services, ui_state, recovery, "http_send_error") {
            reconnect_wifi(
                services,
                ui_state,
                recovery,
                connect_started_ms,
                &ssid,
                &password,
                None,
                "http_send_error",
            );
        }
    } else if maybe_roam_to_stronger_ap(
        services,
        ui_state,
        recovery,
        connect_started_ms,
        &ssid,
        &password,
    ) {
        return;
    } else if refresh_due {
        let _ = recover_http_dhcp(services, ui_state, recovery, "periodic_refresh");
    }
}

fn maybe_roam_to_stronger_ap(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    recovery: &mut NetworkRecoveryState,
    connect_started_ms: &mut Option<u64>,
    ssid: &heapless::String<32>,
    password: &heapless::String<64>,
) -> bool
{
    let now_ms = ui_state.now_ms;
    if now_ms.saturating_sub(recovery.last_roam_scan_ms) < WIFI_ROAM_SCAN_INTERVAL_MS {
        let _ = wifi_common::refresh_connected_ap_telemetry();
        return false;
    }
    recovery.last_roam_scan_ms = now_ms;

    let Some(current) = wifi_common::refresh_connected_ap_telemetry() else {
        esp_println::println!("WiFi roam: current AP info unavailable; skipping scan");
        return false;
    };
    let Some(scanner) = services.wifi.as_mut() else {
        return false;
    };
    let Some(best) = scanner.strongest_ap_for(ssid.as_str()) else {
        return false;
    };
    if best.bssid == current.bssid {
        esp_println::println!(
            "WiFi roam: staying on current AP rssi={} best_seen={}",
            current.rssi,
            best.rssi,
        );
        return false;
    }

    let improvement = i16::from(best.rssi) - i16::from(current.rssi);
    let required = if current.rssi <= WIFI_ROAM_WEAK_RSSI_DBM {
        WIFI_ROAM_WEAK_IMPROVEMENT_DB
    } else {
        WIFI_ROAM_MIN_IMPROVEMENT_DB
    };
    if improvement < required {
        esp_println::println!(
            "WiFi roam: candidate bssid={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} rssi={} \
             current={} improvement={}dB required={}dB; staying",
            best.bssid[0],
            best.bssid[1],
            best.bssid[2],
            best.bssid[3],
            best.bssid[4],
            best.bssid[5],
            best.rssi,
            current.rssi,
            improvement,
            required,
        );
        return false;
    }
    if !recovery.recovery_allowed(now_ms) {
        esp_println::println!(
            "WiFi roam: stronger AP found but recovery cooldown is active; deferring"
        );
        return false;
    }

    esp_println::println!(
        "WiFi roam: switching AP current={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}/{}dBm \
         best={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}/{}dBm improvement={}dB",
        current.bssid[0],
        current.bssid[1],
        current.bssid[2],
        current.bssid[3],
        current.bssid[4],
        current.bssid[5],
        current.rssi,
        best.bssid[0],
        best.bssid[1],
        best.bssid[2],
        best.bssid[3],
        best.bssid[4],
        best.bssid[5],
        best.rssi,
        improvement,
    );
    reconnect_wifi(
        services,
        ui_state,
        recovery,
        connect_started_ms,
        ssid,
        password,
        Some(best),
        "stronger_ap",
    );
    true
}

fn reconnect_wifi(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    recovery: &mut NetworkRecoveryState,
    connect_started_ms: &mut Option<u64>,
    ssid: &heapless::String<32>,
    password: &heapless::String<64>,
    pinned_ap: Option<wifi_common::PinnedAp>,
    reason: &'static str,
)
{
    let now_ms = ui_state.now_ms;
    if !recovery.recovery_allowed(now_ms) {
        return;
    }
    esp_println::println!("WiFi watchdog: {} recovery; reconnecting", reason);
    telemetry_state::record_network_recovery();
    telemetry_state::record_wifi_connected(false);
    if let Some(server) = services.http_server.as_mut() {
        server.reset_network();
    }
    if let Some(scanner) = services.wifi.as_mut() {
        scanner.disconnect();
    }
    ui_state.network = NetworkPhase::Connecting { ssid: ssid.clone() };
    kick_wifi_with_ap(
        services,
        ssid.as_str(),
        password.as_str(),
        pinned_ap,
        now_ms,
    );
    *connect_started_ms = Some(now_ms);
    recovery.mark_recovery(now_ms);
}

fn recover_http_dhcp(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    recovery: &mut NetworkRecoveryState,
    reason: &'static str,
) -> bool
{
    let Some((ssid, _)) = active_wifi_credentials(services) else {
        return false;
    };
    let Some(server) = services.http_server.as_mut() else {
        return false;
    };
    let now_ms = ui_state.now_ms;
    if !recovery.recovery_allowed(now_ms) && reason != "periodic_refresh" {
        return false;
    }

    esp_println::println!("Network watchdog: {} recovery; reacquiring DHCP", reason);
    telemetry_state::record_network_recovery();
    telemetry_state::record_dhcp_started(now_ms);
    let started = esp_hal::time::Instant::now();
    match server.reacquire_dhcp(now_ms) {
        DhcpOutcome::Acquired { ip, gateway } => {
            let acquire_ms = started.elapsed().as_millis().min(u64::from(u32::MAX)) as u32;
            let configured_ms = now_ms.saturating_add(u64::from(acquire_ms));
            telemetry_state::record_dhcp_configured(configured_ms, acquire_ms, ip, gateway);
            telemetry_state::record_http_serving_started(configured_ms);
            ui_state.network = NetworkPhase::Connected {
                ssid,
                rssi: 0,
                ipv4: ip,
            };
            esp_println::println!(
                "Network watchdog: DHCP lease {}.{}.{}.{} restored after {}ms",
                ip[0],
                ip[1],
                ip[2],
                ip[3],
                acquire_ms,
            );
            recovery.mark_recovery(configured_ms);
            true
        },
        DhcpOutcome::TimedOut => {
            telemetry_state::record_dhcp_timeout();
            esp_println::println!(
                "Network watchdog: DHCP recovery timed out after {}ms",
                started.elapsed().as_millis(),
            );
            false
        },
    }
}

/// Boot splash + USB provisioning listener.
///
/// Holds the panel on the splash screen for [`SPLASH_DURATION_MS`] while
/// emitting the `set_config` ready marker on USB-Serial/JTAG (only when we
/// don't already have a config — otherwise the cli.py reader would think
/// the device is still unprovisioned). If a complete config document lands
/// before the splash ends, this returns `true` and the post-splash phase
/// jumps straight to "Connecting".
fn splash_phase<V>(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    led_driver: &mut LedDriver,
    scheduler: &mut Option<PollScheduler<MAX_TELEMETRY_PRODUCERS>>,
    wall_time: &mut GatewayTime,
    mut config_loaded: bool,
) -> bool
where
    V: Esp32BoardVariant,
{
    let delay = Delay::new();
    let mut elapsed: u32 = 0;
    let mut last_ready_ms: Option<u64> = None;
    let mut usb_provisioning = UsbProvisioningState::default();

    while elapsed < SPLASH_DURATION_MS {
        ui_state.now_ms = ui_state.now_ms.saturating_add(SPLASH_TICK_MS as u64);

        // Always advertise the `ready` marker — the cli.py tool needs
        // it to know when the firmware will accept a new config, and
        // gating on `!config_loaded` would block re-uploads after the
        // first persisted config has loaded.
        emit_ready_marker(ui_state.now_ms, &mut last_ready_ms);
        sync_clock_text(ui_state, wall_time);
        let mut provisioning_result =
            poll_provisioning(services, ui_state, &mut usb_provisioning, wall_time);
        if provisioning_result.upload_active {
            provisioning_result = drain_usb_upload(
                services,
                ui_state,
                &mut usb_provisioning,
                wall_time,
                provisioning_result,
            );
        }
        if provisioning_result.config_applied {
            config_loaded = true;
            try_init_lora_from_config::<V>(services, ui_state.now_ms);
            ui_state.lora_available = services.lora_owner.is_some();
            *scheduler = try_bring_up_meshcore::<V>(services, ui_state.now_ms, wall_time);
        }

        if !provisioning_result.upload_active
            && let Some(owner) = services.lora_owner.as_mut()
        {
            owner.service(ui_state.now_ms);
        }

        if let Some(disp) = services.display.as_mut() {
            if crate::ui::render(disp, Screen::Booting, ui_state).is_ok() {
                let _ = disp.flush();
            }
        }

        // BootSweep is fine while we're still hunting for a config; if
        // one's already in hand the LED jumps to Connecting so the
        // operator gets immediate "I'm doing something" feedback even
        // though the splash is still painted.
        let splash_phase = if config_loaded {
            LedPhase::Connecting
        } else {
            LedPhase::BootSweep
        };
        led_driver.set_phase(splash_phase, ui_state.now_ms);
        drive_status_led(services, led_driver, ui_state.now_ms);

        delay.delay_millis(SPLASH_TICK_MS);
        elapsed = elapsed.saturating_add(SPLASH_TICK_MS);
    }

    config_loaded
}

fn poll_loop<V>(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    led_driver: &mut LedDriver,
    scheduler: &mut Option<PollScheduler<MAX_TELEMETRY_PRODUCERS>>,
    wall_time: &mut GatewayTime,
    started_with_config: bool,
) -> !
where
    V: GateFirmwareVariant + Esp32BoardVariant,
{
    let delay = Delay::new();
    let mut tick: u32 = 0;
    let mut last_scan_ms: Option<u64> = None;
    let mut last_ready_ms: Option<u64> = None;
    let mut config_loaded = started_with_config;
    let mut connect_started_ms: Option<u64> = None;
    let mut last_ui_sync_ms: u64 = 0;
    let mut last_observation_drain_ms: u64 = 0;
    let mut last_poll_advert_ms: u64 = 0;
    let mut initial_operational_poll_requested = false;
    let mut ota_flashing = false;
    let mut usb_provisioning = UsbProvisioningState::default();
    let mut network_recovery = NetworkRecoveryState::new(ui_state.now_ms);
    if !config_loaded {
        ui_state.network = NetworkPhase::Unprovisioned;
    } else {
        // Stored-config boot path: `apply_config` already kicked the WiFi
        // controller. Mirror the mid-loop USB-upload behaviour by running
        // the same fast association poll + inline DHCP, so the screen
        // doesn't sit on "Connecting" for the full 1 Hz outer-loop tick.
        connect_started_ms = Some(ui_state.now_ms);
        fast_poll_until_associated(services, ui_state, ui_state.now_ms);
    }

    loop {
        // `now_ms` is advanced by the HTTP inner loop at end of each
        // tick (every 10 ms), not here — keeping the increment once
        // per outer tick would back-date all the per-tick work to the
        // start of the previous wall-second.

        // 1. Battery sample + USB sense. The snapshot is the concrete Bifrost impl of the platform
        //    `PowerMonitor` trait; the runtime fans it out to UI state (which still wants the typed
        //    `BatterySample`) and to the heartbeat (which takes the trait surface so future boards
        //    can plug in their own impl without changing the logger).
        let power_snapshot = power_monitor::BifrostPowerSnapshot::refresh_from_services(services);
        let latest_sample = power_snapshot.battery_sample();
        ui_state.battery = latest_sample;
        ui_state.usb_connected = power_snapshot.usb_connected();

        // 2. WiFi scan every WIFI_SCAN_INTERVAL_MS — only while unprovisioned. Once we're trying to
        //    associate, scans starve the controller.
        if !config_loaded {
            maybe_rescan(services, ui_state, &mut last_scan_ms);
        }

        // 3. USB provisioning. Emit the ready marker later in the loop, after LoRa work, so the
        //    host does not start uploading immediately before a multi-second radio wait.
        //    `poll_provisioning` sets `ui_state.network` to `Connecting { ssid }`, applies title,
        //    and triggers `WifiScanner::try_connect` on success.
        let provisioning_result =
            poll_provisioning(services, ui_state, &mut usb_provisioning, wall_time);
        if provisioning_result.config_applied {
            // A new config landed. Only tear down WiFi/DHCP when the
            // upload actually changes network identity; time sync and
            // display/radio updates should not strand the UI at
            // "Acquiring IP" while the existing endpoint is healthy.
            if provisioning_result.reload_network {
                reset_http_server_for_network_change(services);
            }
            config_loaded = true;
            try_init_lora_from_config::<V>(services, ui_state.now_ms);
            ui_state.lora_available = services.lora_owner.is_some();
            *scheduler = try_bring_up_meshcore::<V>(services, ui_state.now_ms, wall_time);
            if let Some(ipv4) = provisioning_result.preserved_ipv4 {
                telemetry_state::record_network_started(ui_state.now_ms);
                telemetry_state::record_wifi_connected_at(ui_state.now_ms);
                wifi_common::record_connected_ap_info("config-preserve");
                telemetry_state::record_dhcp_configured(ui_state.now_ms, 0, ipv4, None);
                telemetry_state::record_http_serving_started(ui_state.now_ms);
            }
            initial_operational_poll_requested = false;
            last_poll_advert_ms = 0;
            network_recovery = NetworkRecoveryState::new(ui_state.now_ms);
            if provisioning_result.reload_network {
                connect_started_ms = Some(ui_state.now_ms);
                fast_poll_until_associated(services, ui_state, ui_state.now_ms);
            } else {
                connect_started_ms = None;
            }
        }
        if provisioning_result.upload_active {
            continue;
        }

        // 4. Track WiFi association state after a connect was requested.
        if let Some(started) = connect_started_ms {
            update_connection_phase(services, ui_state, started);
            if matches!(
                ui_state.network,
                NetworkPhase::Connected { .. } | NetworkPhase::Error { .. },
            ) {
                connect_started_ms = None;
            }
        }
        if connect_started_ms.is_none() {
            maintain_network::<V>(
                services,
                ui_state,
                &mut network_recovery,
                &mut connect_started_ms,
            );
        }

        let http_busy = poll_http_once::<V>(
            services,
            ui_state,
            led_driver,
            scheduler,
            wall_time,
            &mut network_recovery,
            &mut connect_started_ms,
            &mut initial_operational_poll_requested,
            &mut config_loaded,
            &mut ota_flashing,
        );

        let operational_ready = is_operational_screen_ready(ui_state, config_loaded);
        if operational_ready
            && !initial_operational_poll_requested
            && let Some(scheduler) = scheduler.as_mut()
        {
            scheduler.force_all_due(ui_state.now_ms);
            initial_operational_poll_requested = true;
            esp_println::println!(
                "Poll startup: forced all producers due at first operational screen"
            );
        }

        poll_on_demand_button(services, ui_state.now_ms);
        let poll_now_requests = poll_control::take_poll_now_requests();
        if poll_now_requests > 0 {
            if let Some(scheduler) = scheduler.as_mut() {
                scheduler.force_all_due(ui_state.now_ms);
                esp_println::println!(
                    "Poll now: forced all producers due ({} pending request(s))",
                    poll_now_requests,
                );
            } else {
                esp_println::println!(
                    "Poll now: ignored {} request(s); scheduler not ready",
                    poll_now_requests,
                );
            }
        }

        let new_phase = if ota_flashing {
            LedPhase::OtaFlashing
        } else {
            led_phase_from_state(&ui_state.network, config_loaded, ui_state.lora_available)
        };
        led_driver.set_phase(new_phase, ui_state.now_ms);
        drive_status_led(services, led_driver, ui_state.now_ms);

        // 4.4. MeshCore producer polling. Walks any scheduled producers
        //      whose `next_poll_ms` has passed, fires a poll request per
        //      producer over LoRa, and waits up to ~10 s per producer for
        //      the response (RX is serviced during the wait by the
        //      maintenance closure). Skipped until the radio is listening
        //      and a scheduler exists.
        sync_clock_text(ui_state, wall_time);
        if !ota_flashing
            && ui_state.now_ms.saturating_sub(last_observation_drain_ms)
                >= PASSIVE_OBSERVATION_DRAIN_MS
        {
            if let Some(config) = services.active_config.as_ref() {
                let _ = meshcore::drain_observations(config, ui_state.now_ms);
            }
            last_observation_drain_ms = ui_state.now_ms;
        }
        if !ota_flashing && !http_busy {
            run_poll_round_if_due::<V>(
                services,
                ui_state,
                scheduler.as_mut(),
                led_driver,
                wall_time,
                &mut last_poll_advert_ms,
            );
        }

        emit_ready_marker(ui_state.now_ms, &mut last_ready_ms);
        // Sync the OLED producer rows from the platform telemetry store
        // throttled to PRODUCER_UI_REFRESH_MS so this work doesn't show
        // up as a duty-cycle hit on the I2C bus.
        if ui_state.now_ms.saturating_sub(last_ui_sync_ms) >= PRODUCER_UI_REFRESH_MS {
            sync_producers_from_snapshot(ui_state, active_node_display(services));
            last_ui_sync_ms = ui_state.now_ms;
        }

        // `Connected { ipv4: [0,0,0,0], .. }` is the brief
        // associated-but-no-DHCP-yet window — keep on the Connecting
        // screen (which has its own "Acquiring IP" sub-state with
        // spinner) instead of flipping to Operational and showing the
        // ugly `"DHCP…"` placeholder where the IP belongs.
        let screen = screen_from_state(&ui_state.network, config_loaded, ota_flashing);

        // 5. Periodic serial heartbeat.
        if tick.is_multiple_of(STATUS_LOG_PERIOD_TICKS) {
            log_status(tick, &power_snapshot, services, ui_state.wifi_aps.len());
        }

        // 6. UI render + 7. LED color.
        render_screen(services, ui_state, screen, tick);
        drive_status_led(services, led_driver, ui_state.now_ms);

        // 8. HTTP serving — drain smoltcp + serve requests at sub-tick cadence so TCP handshakes /
        //    retransmits don't time out. We replace the single end-of-tick `delay_millis` with an
        //    inner loop that polls the HTTP server every `HTTP_POLL_INTERVAL_MS` for the remainder
        //    of the outer tick. UI render + battery + WiFi still run at 1 Hz — EXCEPT for
        //    spinner-bearing screens (Connecting, Acquiring IP), which re-render every
        //    `SPINNER_REFRESH_MS` so the animation doesn't appear frozen at 1 Hz (Braille cycle =
        //    1000 ms, which would land on the same frame every tick).
        const HTTP_POLL_INTERVAL_MS: u32 = 10;
        const SPINNER_REFRESH_MS: u64 = 250;
        let inner_target_ms = ui_state.now_ms.saturating_add(POLL_INTERVAL_MS as u64);
        let spinner_active = matches!(
            ui_state.network,
            NetworkPhase::Connecting { .. }
                | NetworkPhase::Connected {
                    ipv4: [0, 0, 0, 0],
                    ..
                }
        );
        let mut last_spinner_render_ms = ui_state.now_ms;
        while ui_state.now_ms < inner_target_ms {
            let _ = poll_http_once::<V>(
                services,
                ui_state,
                led_driver,
                scheduler,
                wall_time,
                &mut network_recovery,
                &mut connect_started_ms,
                &mut initial_operational_poll_requested,
                &mut config_loaded,
                &mut ota_flashing,
            );
            // Service the LoRa owner at HTTP-poll cadence (~10 ms).
            // start_rx fires once the boot-delay window closes; from
            // then on poll_receive drains the SX1262 IRQ + buffer, and
            // a queued TX frame is dispatched if the airtime budget
            // permits.
            if let Some(owner) = services.lora_owner.as_mut() {
                owner.service(ui_state.now_ms);
            }
            poll_on_demand_button(services, ui_state.now_ms);
            delay.delay_millis(HTTP_POLL_INTERVAL_MS);
            ui_state.now_ms = ui_state.now_ms.saturating_add(HTTP_POLL_INTERVAL_MS as u64);

            if (spinner_active || ota_flashing)
                && ui_state.now_ms.saturating_sub(last_spinner_render_ms) >= SPINNER_REFRESH_MS
            {
                let animated_screen = if ota_flashing {
                    Screen::OtaUpdate
                } else {
                    screen
                };
                render_screen(services, ui_state, animated_screen, tick);
                last_spinner_render_ms = ui_state.now_ms;
            }
            if ota_flashing {
                led_driver.set_phase(LedPhase::OtaFlashing, ui_state.now_ms);
                drive_status_led(services, led_driver, ui_state.now_ms);
            }
        }
        tick = tick.wrapping_add(1);
    }
}

fn poll_on_demand_button(services: &mut BoardServices, now_ms: u64)
{
    if services.poll_button.poll_pressed(now_ms) {
        services.buzzer.chirp();
        let count = poll_control::request_poll_now();
        esp_println::println!(
            "Poll button: GPIO15 press queued immediate producer poll (count={})",
            count,
        );
    }
}

fn screen_from_state(network: &NetworkPhase, config_loaded: bool, ota_flashing: bool) -> Screen
{
    if ota_flashing {
        return Screen::OtaUpdate;
    }
    match (network, config_loaded) {
        (NetworkPhase::Connected { ipv4, .. }, _) if *ipv4 == [0, 0, 0, 0] => Screen::Connecting,
        (NetworkPhase::Connected { .. }, _) => Screen::Operational,
        (_, true) => Screen::Connecting,
        (_, false) => Screen::Provisioning,
    }
}

fn is_operational_screen_ready(ui_state: &UiState, config_loaded: bool) -> bool
{
    matches!(
        screen_from_state(&ui_state.network, config_loaded, false),
        Screen::Operational
    )
}

fn sync_clock_text(ui_state: &mut UiState, wall_time: &GatewayTime)
{
    ui_state.clock_text.clear();
    let Some(local) = wall_time.local_datetime(ui_state.now_ms) else {
        return;
    };
    let mut buf = [0u8; 5];
    local.write_hh_mm(&mut buf);
    if let Ok(text) = core::str::from_utf8(&buf) {
        let _ = ui_state.clock_text.push_str(text);
    }
}

/// Map the high-level WiFi / provisioning state onto an [`LedPhase`].
/// While we're still waiting on a config the LED keeps animating the
/// boot sweep so the operator knows the device is alive and looking
/// for input.
///
/// `lora_available` overrides the "Online" phase with a fast red blink
/// — a gateway with WiFi up but no LoRa radio can't fulfill its job,
/// so the operator should see that as a critical (not informational)
/// state the moment WiFi recovers.
fn led_phase_from_state(
    network: &NetworkPhase,
    config_loaded: bool,
    lora_available: bool,
) -> LedPhase
{
    if !config_loaded {
        return LedPhase::BootSweep;
    }
    match network {
        NetworkPhase::Connecting { .. } | NetworkPhase::Unprovisioned | NetworkPhase::Scanning => {
            LedPhase::Connecting
        },
        NetworkPhase::Connected { ipv4, .. } => {
            if *ipv4 == [0, 0, 0, 0] {
                LedPhase::AcquiringIp
            } else if !lora_available {
                LedPhase::CriticalError
            } else {
                LedPhase::Online
            }
        },
        NetworkPhase::Error { .. } => LedPhase::FatalError,
    }
}

/// Watch the WiFi controller for association events and update the UI
/// phase. On the first time it flips to "connected", runs DHCP inline so
/// the Connected screen can show a real IPv4 address instead of an
/// `Acquiring IP` spinner.
fn update_connection_phase(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    connect_started_ms: u64,
)
{
    // Snapshot the SSID we were trying to connect to — needed both for the
    // success state and the timeout error message.
    let ssid_snapshot: heapless::String<32> = match &ui_state.network {
        NetworkPhase::Connecting { ssid } => ssid.clone(),
        NetworkPhase::Connected { ssid, .. } => ssid.clone(),
        _ => heapless::String::new(),
    };

    let connected = services
        .wifi
        .as_mut()
        .map(|w| w.is_connected())
        .unwrap_or(false);

    if connected {
        if !matches!(ui_state.network, NetworkPhase::Connected { .. }) {
            let assoc_ms = ui_state.now_ms.saturating_sub(connect_started_ms);
            esp_println::println!(
                "WiFi: associated with ssid=\"{}\" after {}ms",
                ssid_snapshot.as_str(),
                assoc_ms,
            );
            telemetry_state::record_wifi_connected_at(ui_state.now_ms);
            wifi_common::record_connected_ap_info("bifrost-associated");
            // Promote to Connected first so the OLED flips off the spinner
            // immediately; the DHCP wait below runs blocking and can take
            // multiple seconds.
            ui_state.network = NetworkPhase::Connected {
                ssid: ssid_snapshot.clone(),
                rssi: 0,
                ipv4: [0, 0, 0, 0],
            };

            // Hand the WifiDevice to the shared HTTP server, run
            // DHCP through it, and stash the server on BoardServices
            // so subsequent poll_loop ticks can keep serving requests.
            // From here on the device is owned by the HttpServer —
            // smoltcp drives all packet I/O on it.
            if services.http_server.is_none() {
                let mut configured_name: heapless::String<32> = heapless::String::new();
                if let Some(config) = services.active_config.as_ref() {
                    let _ = configured_name.push_str(config.name.as_str());
                }
                if configured_name.is_empty() {
                    let _ = configured_name.push_str("muninn-gate");
                }
                if let Some(scanner) = services.wifi.as_mut()
                    && let Some(device) = scanner.take_device()
                {
                    let port = ui_state.http_port;
                    let mut server =
                        HttpServer::new(device, port, ui_state.now_ms, configured_name.as_str());
                    let started = esp_hal::time::Instant::now();
                    let dhcp_started_ms = ui_state.now_ms;
                    telemetry_state::record_dhcp_started(dhcp_started_ms);
                    match server.acquire_dhcp(dhcp_started_ms) {
                        DhcpOutcome::Acquired { ip, gateway } => {
                            let acquire_ms =
                                started.elapsed().as_millis().min(u64::from(u32::MAX)) as u32;
                            let configured_ms =
                                dhcp_started_ms.saturating_add(u64::from(acquire_ms));
                            telemetry_state::record_dhcp_configured(
                                configured_ms,
                                acquire_ms,
                                ip,
                                gateway,
                            );
                            telemetry_state::record_http_serving_started(configured_ms);
                            esp_println::println!(
                                "DHCP: lease acquired {}.{}.{}.{} after {}ms",
                                ip[0],
                                ip[1],
                                ip[2],
                                ip[3],
                                acquire_ms,
                            );
                            if let Some(gw) = gateway {
                                esp_println::println!(
                                    "DHCP: gateway {}.{}.{}.{}",
                                    gw[0],
                                    gw[1],
                                    gw[2],
                                    gw[3],
                                );
                            }
                            ui_state.network = NetworkPhase::Connected {
                                ssid: ssid_snapshot,
                                rssi: 0,
                                ipv4: ip,
                            };
                            esp_println::println!(
                                "HTTP: serving on http://{}.{}.{}.{}:{}/",
                                ip[0],
                                ip[1],
                                ip[2],
                                ip[3],
                                port,
                            );
                        },
                        DhcpOutcome::TimedOut => {
                            telemetry_state::record_dhcp_timeout();
                            esp_println::println!(
                                "DHCP: gave up after {}ms — keeping network stack for retry",
                                started.elapsed().as_millis(),
                            );
                        },
                    }
                    services.http_server = Some(server);
                }
            }
        }
        return;
    }

    let elapsed = ui_state.now_ms.saturating_sub(connect_started_ms);
    if elapsed >= WIFI_CONNECT_TIMEOUT_MS
        && matches!(ui_state.network, NetworkPhase::Connecting { .. })
    {
        esp_println::println!(
            "WiFi: association timeout after {}ms for ssid=\"{}\"",
            elapsed,
            ssid_snapshot.as_str(),
        );
        telemetry_state::record_wifi_connected(false);
        let mut reason: heapless::String<48> = heapless::String::new();
        let _ = reason.push_str("Cannot reach AP");
        ui_state.network = NetworkPhase::Error { reason };
    }
}

/// Tight `is_connected()` poll right after a `try_connect`. Without this,
/// the 1 Hz main loop misses the actual association moment by up to a
/// second; here we poll every `WIFI_CONNECT_POLL_MS` for at most
/// `WIFI_CONNECT_POLL_BUDGET_MS`. On success we run DHCP inline.
///
/// Re-renders the Connecting screen every `FAST_POLL_RENDER_MS` so the
/// spinner keeps animating during the wait — without this the user sees
/// a frozen frame for the whole association window.
fn fast_poll_until_associated(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    connect_started_ms: u64,
)
{
    /// How often to repaint the Connecting screen during the poll. A
    /// full SSD1327 flush is ~200 ms over I2C, so 400 ms (≈ 2.5 fps)
    /// keeps the spinner visibly animating without dominating the poll
    /// budget.
    const FAST_POLL_RENDER_MS: u32 = 400;

    let delay = Delay::new();
    let mut elapsed: u32 = 0;
    let mut last_render_ms: u32 = 0;
    esp_println::println!(
        "WiFi: fast-polling for association (budget {}ms)",
        WIFI_CONNECT_POLL_BUDGET_MS,
    );

    // Initial paint — the splash screen was the previous frame, so the
    // panel needs the Connecting screen up front before we start the
    // blocking poll.
    ui_state.now_ms = ui_state.now_ms.saturating_add(WIFI_CONNECT_POLL_MS as u64);
    render_screen(services, ui_state, Screen::Connecting, 0);

    while elapsed < WIFI_CONNECT_POLL_BUDGET_MS {
        if let Some(scanner) = services.wifi.as_mut()
            && scanner.is_connected()
        {
            esp_println::println!("WiFi: is_connected() true after {}ms (fast poll)", elapsed,);
            ui_state.now_ms = ui_state.now_ms.saturating_add(elapsed as u64);
            update_connection_phase(services, ui_state, connect_started_ms);
            return;
        }
        delay.delay_millis(WIFI_CONNECT_POLL_MS);
        elapsed = elapsed.saturating_add(WIFI_CONNECT_POLL_MS);

        if elapsed.saturating_sub(last_render_ms) >= FAST_POLL_RENDER_MS {
            ui_state.now_ms = ui_state.now_ms.saturating_add(FAST_POLL_RENDER_MS as u64);
            render_screen(services, ui_state, Screen::Connecting, 0);
            last_render_ms = elapsed;
        }
    }
    esp_println::println!("WiFi: fast-poll budget exhausted, falling back to 1 Hz");
    ui_state.now_ms = ui_state.now_ms.saturating_add(elapsed as u64);
}

/// Emit the JSON ready marker `tools/cli.py` waits for, throttled to one
/// per `PROVISIONING_READY_INTERVAL_MS`.
fn emit_ready_marker(now_ms: u64, last_ready_ms: &mut Option<u64>)
{
    let due = last_ready_ms
        .map(|last| now_ms.saturating_sub(last) >= PROVISIONING_READY_INTERVAL_MS)
        .unwrap_or(true);
    if due {
        esp_println::println!(r#"{{"type":"ready","request":"set_config"}}"#);
        *last_ready_ms = Some(now_ms);
    }
}

/// Drain a tick's worth of USB-Serial/JTAG input and react to any complete
/// JSON document. Reports both config acceptance and whether LoRa should stay
/// out of the way while a live upload is incomplete.
fn poll_provisioning(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    provisioning_state: &mut UsbProvisioningState,
    wall_time: &mut GatewayTime,
) -> ProvisioningPollResult
{
    let Some(rx) = services.usb_rx.as_mut() else {
        return ProvisioningPollResult::default();
    };
    match rx.poll_document() {
        Ok(Some(document)) => match provisioning::parse_usb_document(document.as_str()) {
            Ok(ProvisioningCommand::SetConfig(config)) => {
                provisioning_state.last_activity_ms = None;
                let preserved_network = preserved_network_state_for_config(services, &config);
                esp_println::println!(
                    r#"{{"type":"progress","request":"set_config","stage":"received","bytes":{}}}"#,
                    document.len(),
                );
                esp_println::println!(
                    "Config: name=\"{}\" producers={}",
                    config.name.as_str(),
                    config.producers.len(),
                );

                // Persist before responding `ok` so cli.py + the operator
                // know the config will survive a power cycle. A flash
                // failure is logged but doesn't abort — the device still
                // operates with the in-RAM config for this boot.
                match storage::save_config_document(document.as_str(), &config) {
                    Ok(()) => esp_println::println!(
                        "Storage: persisted config ({} bytes)",
                        document.len(),
                    ),
                    Err(err) => esp_println::println!(
                        "Storage: save failed: {:?} (config kept in RAM only)",
                        err,
                    ),
                }

                let creds = apply_config(services, ui_state, &config);
                apply_time_settings(services, ui_state, wall_time, &config, true, "USB");
                esp_println::println!(r#"{{"type":"ok","request":"set_config"}}"#);
                let reload_network = preserved_network.is_none();
                let preserved_ipv4 = preserved_network.as_ref().map(|network| network.ipv4);
                if let Some(network) = preserved_network {
                    ui_state.network = NetworkPhase::Connected {
                        ssid: network.ssid,
                        rssi: 0,
                        ipv4: network.ipv4,
                    };
                    esp_println::println!(
                        "Config: preserved WiFi endpoint {}.{}.{}.{} after USB update",
                        network.ipv4[0],
                        network.ipv4[1],
                        network.ipv4[2],
                        network.ipv4[3],
                    );
                } else if let Some((ssid, password)) = creds {
                    kick_wifi(services, &ssid, &password, ui_state.now_ms);
                }
                ProvisioningPollResult {
                    config_applied: true,
                    upload_active: false,
                    reload_network,
                    preserved_ipv4,
                }
            },
            Ok(ProvisioningCommand::PollNow) => {
                provisioning_state.last_activity_ms = None;
                let count = poll_control::request_poll_now();
                esp_println::println!(r#"{{"type":"ok","request":"poll_now","count":{}}}"#, count,);
                ProvisioningPollResult::default()
            },
            Ok(_) => {
                provisioning_state.last_activity_ms = None;
                esp_println::println!(
                    r#"{{"type":"error","request":"set_config","code":"unsupported_command"}}"#,
                );
                ProvisioningPollResult::default()
            },
            Err(err) => {
                provisioning_state.last_activity_ms = None;
                esp_println::println!(
                    r#"{{"type":"error","request":"set_config","code":"{}"}}"#,
                    err.as_str(),
                );
                ProvisioningPollResult::default()
            },
        },
        Ok(None) => handle_incomplete_usb_upload(rx, ui_state.now_ms, provisioning_state),
        Err(_) => {
            provisioning_state.last_activity_ms = None;
            esp_println::println!(
                r#"{{"type":"error","request":"set_config","code":"buffer_overrun"}}"#,
            );
            rx.discard_partial();
            ProvisioningPollResult::default()
        },
    }
}

fn drain_usb_upload(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    provisioning_state: &mut UsbProvisioningState,
    wall_time: &mut GatewayTime,
    mut result: ProvisioningPollResult,
) -> ProvisioningPollResult
{
    let delay = Delay::new();
    let start_ms = ui_state.now_ms;
    while result.upload_active
        && !result.config_applied
        && ui_state.now_ms.saturating_sub(start_ms) < USB_UPLOAD_DRAIN_BUDGET_MS as u64
    {
        delay.delay_millis(USB_UPLOAD_DRAIN_TICK_MS);
        ui_state.now_ms = ui_state
            .now_ms
            .saturating_add(USB_UPLOAD_DRAIN_TICK_MS as u64);
        result = poll_provisioning(services, ui_state, provisioning_state, wall_time);
    }
    result
}

fn handle_incomplete_usb_upload(
    rx: &mut muninn_gate_platform_esp32::serial::UsbJsonReceiver,
    now_ms: u64,
    provisioning_state: &mut UsbProvisioningState,
) -> ProvisioningPollResult
{
    if rx.take_activity() {
        provisioning_state.last_activity_ms = Some(now_ms);
    }

    if rx.buffered_bytes() == 0 {
        provisioning_state.last_activity_ms = None;
        return ProvisioningPollResult::default();
    }

    let Some(last_activity_ms) = provisioning_state.last_activity_ms else {
        provisioning_state.last_activity_ms = Some(now_ms);
        return ProvisioningPollResult {
            config_applied: false,
            upload_active:  true,
            reload_network: false,
            preserved_ipv4: None,
        };
    };

    if now_ms.saturating_sub(last_activity_ms) < USB_UPLOAD_IDLE_TIMEOUT_MS {
        return ProvisioningPollResult {
            config_applied: false,
            upload_active:  true,
            reload_network: false,
            preserved_ipv4: None,
        };
    }

    esp_println::println!(
        "USB: dropping stale partial config upload ({} bytes buffered)",
        rx.buffered_bytes(),
    );
    rx.discard_partial();
    provisioning_state.last_activity_ms = None;
    ProvisioningPollResult::default()
}

/// Walk the scheduler for any producers whose `next_poll_ms` has
/// elapsed, run a blocking MeshCore round per producer (radio is
/// serviced from the maintenance callback during the wait), then
/// advance `ui_state.now_ms` by the actual wall-clock elapsed so
/// downstream timing (WiFi timeouts, LED phase, UI refresh) doesn't
/// drift backwards.
fn run_poll_round_if_due<V>(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    scheduler: Option<&mut PollScheduler<MAX_TELEMETRY_PRODUCERS>>,
    led_driver: &LedDriver,
    wall_time: &GatewayTime,
    last_poll_advert_ms: &mut u64,
) where
    V: Esp32BoardVariant,
{
    let Some(scheduler) = scheduler else {
        return;
    };
    let now_ms = ui_state.now_ms;
    let due = scheduler.due_producers(now_ms);
    if due.is_empty() {
        return;
    }
    let listening = services
        .lora_owner
        .as_ref()
        .is_some_and(|owner| owner.is_listening());
    if !listening {
        return;
    }

    // Clone the active config so the immutable borrow it needs for the
    // poll doesn't tie up `services` — that frees `services.display` for
    // the in-poll animation repaints below.
    let Some(config) = services.active_config.clone() else {
        return;
    };

    // Clear stale background frames before opening the active response
    // window. During the poll itself, RadioMeshcoreClient drains and
    // classifies RX frames so response candidates are preserved.
    let _ = meshcore::drain_observations(&config, now_ms);

    if *last_poll_advert_ms == 0
        || now_ms.saturating_sub(*last_poll_advert_ms) >= POLL_DIRECT_ADVERT_MIN_INTERVAL_MS
    {
        if meshcore::queue_gateway_advert(&config, now_ms, wall_time.unix_seconds(now_ms), "poll") {
            *last_poll_advert_ms = now_ms;
        }
    }

    // Flip the producer rows to InProgress so the operational screen
    // shows the in-flight row spinner while the (blocking) poll runs.
    // sync_producers_from_snapshot() overwrites each row with its real
    // Success/Failed result once the round returns.
    for ui_producer in ui_state.producers.iter_mut() {
        let row_is_due = config
            .producers
            .iter()
            .find(|producer| producer.name.as_str() == ui_producer.name.as_str())
            .is_some_and(|producer| due.iter().any(|id| *id == producer.id()));
        if row_is_due {
            ui_producer.status = PollStatus::InProgress;
        }
    }
    render_screen(services, ui_state, Screen::Operational, 0);

    // Take the LoRa owner, display, and RGB LED out of `services` so the
    // maintenance closure can hold them across the blocking poll. The
    // meshcore client borrows `&config` for its whole lifetime `'a`, and
    // the maintenance ref must also live `'a` — so the closure may only
    // capture LOCALS (capturing the `services`/`ui_state` params would
    // make their borrows escape the function). All handles are restored
    // after the round.
    let mut owner_opt = services.lora_owner.take();
    let mut display_opt = services.display.take();
    let mut status_led_opt = services.status_led.take();
    let mut http_server_opt = services.http_server.take();
    // Render onto a clone of the UI state so the closure doesn't capture
    // the `ui_state` param. Cheap relative to the multi-second poll.
    let mut ui_anim = ui_state.clone();
    let started = esp_hal::time::Instant::now();
    // Throttle the in-poll repaint so the animation costs at most one
    // OLED flush per ANIM_INTERVAL_MS. 200 ms gives ~5 fps — enough for
    // the 250 ms row spinner cycle to read as motion without dominating
    // the poll's cooperative time budget.
    const ANIM_INTERVAL_MS: u64 = 200;
    let mut last_anim_ms: u64 = now_ms;
    let tx_power = V::map_tx_power(config.radio.tx_power_level);
    let entry_green_until_ms = led_driver.operational_entry_pulse_until_ms();
    if let (Some(owner), Some(led)) = (owner_opt.as_mut(), status_led_opt.take()) {
        owner.attach_poll_led(led, entry_green_until_ms);
    }

    let summary = {
        let mut maintenance = || {
            let maint_now = now_ms.saturating_add(started.elapsed().as_millis());
            // Service the radio first (RX drain / TX), then animate.
            if let Some(owner) = owner_opt.as_mut() {
                owner.service(maint_now);
            }
            if let Some(server) = http_server_opt.as_mut()
                && let Err(e) = server.poll(maint_now, &config, tx_power)
            {
                esp_println::println!("HTTP: poll error during LoRa wait {:?}", e);
                server.reset_network();
            }
            if maint_now.saturating_sub(last_anim_ms) >= ANIM_INTERVAL_MS {
                last_anim_ms = maint_now;
                // Advance the clone's clock so draw_poll_signal's
                // `(now_ms / 250) % 4` bar cycle steps, then repaint the
                // operational screen straight onto the taken display.
                ui_anim.now_ms = maint_now;
                sync_clock_text(&mut ui_anim, wall_time);
                if let Some(disp) = display_opt.as_mut()
                    && crate::ui::render(disp, Screen::Operational, &ui_anim).is_ok()
                {
                    let _ = disp.flush();
                }
            }
        };
        let mut client = RadioMeshcoreClient::new(&config, now_ms);
        client.set_now_ms(now_ms);
        client.set_network_maintenance(&mut maintenance);
        let mut store = SharedTelemetryStore;
        block_on(scheduler.poll_due_producers(&config, &mut client, &mut store, now_ms))
    };

    let elapsed_ms = started.elapsed().as_millis();
    if let Some(owner) = owner_opt.as_mut()
        && status_led_opt.is_none()
    {
        status_led_opt = owner.detach_poll_led();
    }
    services.lora_owner = owner_opt;
    services.display = display_opt;
    services.status_led = status_led_opt;
    services.http_server = http_server_opt;
    // Drain any non-response frames that piggybacked on the same RX
    // window — passively records producer observations even when their
    // scheduled poll didn't fire this round.
    let _ = meshcore::drain_observations(&config, now_ms);
    // Absolute (not additive) so the in-closure animation clock writes
    // don't double-count the elapsed time.
    ui_state.now_ms = now_ms.saturating_add(elapsed_ms);
    sync_clock_text(ui_state, wall_time);

    if summary.attempted > 0 {
        esp_println::println!(
            "Poll round: attempted={} succeeded={} failed={} retrying={} (took {}ms)",
            summary.attempted,
            summary.succeeded,
            summary.failed,
            summary.retrying,
            elapsed_ms,
        );
        log_latest_poll_errors(&config);
    }

    // Pull the fresh Success/Failed results into the producer rows and
    // repaint immediately so the operator sees the outcome without
    // waiting for the next 1 Hz UI tick.
    sync_producers_from_snapshot(ui_state, config.display.node_display());
    render_screen(services, ui_state, Screen::Operational, 0);
    drive_status_led(services, led_driver, ui_state.now_ms);
}

fn log_latest_poll_errors(config: &Esp32GatewayConfig)
{
    let snapshot = telemetry_state::snapshot();
    for producer in config.enabled_producers() {
        let producer_id = producer.id();
        let Some(record) = snapshot
            .records
            .iter()
            .find(|record| record.config.id() == producer_id)
        else {
            continue;
        };
        let Some(error) = record.poll.last_poll_error else {
            continue;
        };
        esp_println::println!(
            "Poll detail: producer=\"{}\" last_error={}",
            producer.name.as_str(),
            error.as_str(),
        );
    }
}

/// Mirror the shared telemetry store snapshot into the UI's
/// `ProducerSummary` list. Producer order in `ui_state.producers` was
/// established by `apply_config`; we look up each producer by name and
/// copy across the freshest `PollStatus`, `last_poll_at_ms`, and a
/// single highlight metric appropriate to the data the producer
/// actually reports.
fn sync_producers_from_snapshot(ui_state: &mut UiState, node_display: DisplayTelemetryValue)
{
    let snapshot = telemetry_state::snapshot();
    for ui_producer in ui_state.producers.iter_mut() {
        let Some(record) = snapshot
            .records
            .iter()
            .find(|record| record.config.name.as_str() == ui_producer.name.as_str())
        else {
            continue;
        };
        apply_record_to_ui_producer(ui_producer, record, node_display);
    }
}

fn active_node_display(services: &BoardServices) -> DisplayTelemetryValue
{
    services
        .active_config
        .as_ref()
        .map(|config| config.display.node_display())
        .unwrap_or_default()
}

fn apply_record_to_ui_producer(
    ui_producer: &mut ProducerSummary,
    record: &TelemetryRecord,
    node_display: DisplayTelemetryValue,
)
{
    ui_producer.last_poll_at_ms = record.poll.last_poll_ms;
    ui_producer.status = match (record.poll.last_poll_success, record.telemetry.as_ref()) {
        (Some(true), Some(telemetry)) => PollStatus::Success {
            rssi: telemetry
                .rssi
                .map(|r| r.clamp(i16::from(i8::MIN), i16::from(i8::MAX)) as i8)
                .unwrap_or(-127),
        },
        (Some(false), _) => PollStatus::Failed,
        _ => PollStatus::Pending,
    };
    if let Some(telemetry) = record.telemetry.as_ref() {
        let metrics = telemetry.metrics;
        let (value, unit) = match node_display {
            DisplayTelemetryValue::TemperatureCelsius => (metrics.temperature_celsius, "C"),
            DisplayTelemetryValue::HumidityPercent => (metrics.humidity_percent, "%"),
            DisplayTelemetryValue::StateOfCharge => match metrics.battery_percent {
                Some(percent) => (Some(percent), "%"),
                None => (metrics.battery_voltage, "V"),
            },
            DisplayTelemetryValue::BatteryVoltage => (metrics.battery_voltage, "V"),
            DisplayTelemetryValue::PressureHpa => (metrics.pressure_pa.map(|pa| pa / 100.0), "hPa"),
            DisplayTelemetryValue::LuminosityLux => (metrics.luminosity_lux, "lx"),
            DisplayTelemetryValue::Rssi => (telemetry.rssi.map(f32::from), "dBm"),
            DisplayTelemetryValue::PollLatency => (None, ""),
        };
        ui_producer.metric_value = value;
        ui_producer.metric_unit = if value.is_some() { unit } else { "" };
    } else {
        ui_producer.metric_value = None;
        ui_producer.metric_unit = "";
    }
}

fn maybe_rescan(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    last_scan_ms: &mut Option<u64>,
)
{
    let Some(scanner) = services.wifi.as_mut() else {
        return;
    };

    let now = ui_state.now_ms;
    let due = last_scan_ms
        .map(|last| now.saturating_sub(last) >= WIFI_SCAN_INTERVAL_MS)
        .unwrap_or(true);
    if !due {
        return;
    }

    // Blocking scan — stalls the loop for ~1-3 s but we tolerate it at this
    // milestone. We deliberately do NOT touch `ui_state.network` or render
    // an inline "scanning" frame: that would clobber the post-provisioning
    // `Connecting { ssid }` phase and flip the screen back to the
    // provisioning chrome every 15 s.
    match scanner.scan() {
        Ok(aps) => {
            ui_state.wifi_aps.clear();
            for ap in aps.iter() {
                let _ = ui_state.wifi_aps.push(ap.clone());
            }
            esp_println::println!("WiFi: scan complete, {} APs:", aps.len());
            for ap in aps.iter() {
                esp_println::println!(
                    "  ssid=\"{}\" rssi={}dBm ch={} auth={:?}",
                    ap.ssid.as_str(),
                    ap.rssi,
                    ap.channel,
                    ap.auth,
                );
            }
        },
        Err(e) => {
            esp_println::println!("WiFi: scan failed: {:?}", e);
        },
    }
    *last_scan_ms = Some(now);
}

fn render_screen(services: &mut BoardServices, ui_state: &UiState, screen: Screen, tick: u32)
{
    let Some(disp) = services.display.as_mut() else {
        return;
    };
    if crate::ui::render(disp, screen, ui_state).is_err() {
        esp_println::println!("Display: render failed at tick={}", tick);
        return;
    }
    if disp.flush().is_err() {
        esp_println::println!("Display: flush failed at tick={}; UI disabled", tick);
        services.display = None;
    }
}

/// Write the LED color driven by the current [`LedDriver`] state. Errors
/// (RMT busy / transmit failure) drop the channel for the rest of the
/// boot — the firmware doesn't fight to keep the LED alive because the
/// indicator is purely informational.
fn drive_status_led(services: &mut BoardServices, led_driver: &LedDriver, now_ms: u64)
{
    let color = led_driver.current_color(now_ms).dim(RGB_DIM_SHIFT);
    let Some(mut led) = services.status_led.take() else {
        return;
    };
    match led.set_color(color) {
        Ok(()) => services.status_led = Some(led),
        Err(_) => {
            esp_println::println!("RGB: set_color failed at now_ms={}; LED disabled", now_ms);
        },
    }
}

fn log_status(tick: u32, power: &dyn PowerMonitor, services: &BoardServices, ap_count: usize)
{
    let mv = power.voltage_mv().map(|v| v as i32).unwrap_or(-1);
    let soc = power.soc_percent().map(|s| s as i32).unwrap_or(-1);
    let mut line: heapless::String<192> = heapless::String::new();
    let lora_label = if let Some(owner) = services.lora_owner.as_ref() {
        if owner.is_listening() { "rx" } else { "init" }
    } else if services.lora_pending.is_some() {
        "wait"
    } else {
        "none"
    };
    let _ = write!(
        &mut line,
        "Bifrost tick={} display={} led={} wifi={} aps={} lora={} usb={} battery_mv={} soc={}%",
        tick,
        if services.display.is_some() {
            "ok"
        } else {
            "gone"
        },
        if services.status_led.is_some() {
            "ok"
        } else {
            "gone"
        },
        if services.wifi.is_some() {
            "ok"
        } else {
            "gone"
        },
        ap_count,
        lora_label,
        if power.usb_connected() { "on" } else { "off" },
        mv,
        soc,
    );
    esp_println::println!("{}", line.as_str());
}
