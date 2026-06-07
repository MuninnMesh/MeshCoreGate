//! Chip-level WiFi primitives shared by every ESP32 board variant.
//!
//! Both [`crate::wifi`] (Heltec V4 + HTTP server stack) and
//! [`crate::bifrost_pros3::wifi`] (Bifrost Gate ProS3 bring-up) target the
//! same esp-wifi 0.15 driver on the same family of chips. The
//! board-specific modules differ in how they orchestrate the radio (HTTP
//! socket pool, scheduler hooks, diagnostics rings, UI integration) but
//! the **low-level setup is identical**: wait for the controller to
//! report started, disable modem power-save, cap TX power, scan for the
//! configured SSID, pin the strongest BSSID + channel, and apply the
//! same DHCP retry budget. This module owns those primitives so neither
//! board copies them out of date.
//!
//! Every helper here is intentionally side-effect minimal: it talks to
//! the esp-wifi controller (or `esp-wifi-sys` C bindings) and returns a
//! plain status. Callers layer their own diagnostics, telemetry, and UI
//! reporting on top.

use alloc::string::String as AllocString;
use core::mem::MaybeUninit;

use esp_hal::delay::Delay;
use esp_hal::peripherals::{RNG, TIMG1, WIFI};
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_wifi::EspWifiController;
use esp_wifi::config::PowerSaveMode;
use esp_wifi::wifi::{
    AuthMethod,
    ClientConfiguration,
    Configuration,
    ScanConfig,
    WifiController,
    WifiDevice,
    WifiError,
};
use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::socket::dhcpv4;
use smoltcp::time::{Duration as SmoltcpDuration, Instant as SmoltcpInstant};
use smoltcp::wire::{DhcpOption, IpCidr};
use static_cell::StaticCell;

/// Maximum WiFi TX power in 0.25 dBm units (20 = 5 dBm).
/// Keep this modest on the shared ESP32-S3 + SX1262 builds: high WiFi PA
/// levels couple into the LoRa prototype strongly enough to corrupt SX1262
/// status reads and collapse TX reliability after association.
pub const TX_POWER_QUARTER_DBM: i8 = 20;

/// Number of `is_started()` polls we accept before giving up. Combined
/// with [`START_POLL_INTERVAL_MS`] this gives a 2 s budget which is
/// enough headroom for both the Heltec and Bifrost paths even on a
/// cold-boot recalibration cycle.
pub const START_POLL_MAX: u32 = 200;
/// Delay (ms) between consecutive `is_started()` polls.
pub const START_POLL_INTERVAL_MS: u32 = 10;

/// Maximum APs we retain from a `scan_with_config_sync_max` call when
/// pre-pinning the strongest BSSID. 12 is comfortable headroom for a
/// dense 2.4 GHz environment without bloating the per-scan heap usage.
pub const SCAN_PIN_RESULTS: usize = 12;

/// DHCP DISCOVER timeout — tightened from smoltcp's 10 s default so a
/// single dropped beacon doesn't blow the entire DHCP budget.
pub const DHCP_DISCOVER_TIMEOUT_SECS: u64 = 2;
/// DHCP initial REQUEST timeout — same tightening rationale as DISCOVER.
pub const DHCP_INITIAL_REQUEST_TIMEOUT_SECS: u64 = 2;
/// Number of DHCP REQUEST retries before deconfiguring the socket.
pub const DHCP_REQUEST_RETRIES: u16 = 3;
/// DHCP option carrying the client host name.
pub const DHCP_HOST_NAME_OPTION: u8 = 12;
/// Maximum sanitized DHCP hostname length sent to the AP/router.
pub const DHCP_HOSTNAME_MAX_LEN: usize = 32;
const DEFAULT_DHCP_HOSTNAME: &str = "muninn-gate";

/// Poll the controller's `is_started()` until it reports `true` or the
/// shared poll budget expires. Returns `true` on success, `false` on
/// timeout. Callers map the boolean onto their own error type.
pub fn wait_for_started(controller: &WifiController<'_>) -> bool
{
    let delay = Delay::new();
    for _ in 0..START_POLL_MAX {
        if matches!(controller.is_started(), Ok(true)) {
            return true;
        }
        delay.delay_millis(START_POLL_INTERVAL_MS);
    }
    false
}

/// Disable WiFi modem power-save. esp-wifi defaults to a minimum-sleep
/// power save policy; that's a battery win but it slows every WiFi RTT
/// because the radio briefly powers down between beacons. Every gateway
/// firmware we ship wants `None` for predictable network timing.
pub fn disable_power_save(controller: &mut WifiController<'_>) -> Result<(), WifiError>
{
    controller.set_power_saving(PowerSaveMode::None)
}

/// Outcome of [`set_max_tx_power`].
pub enum SetTxPowerResult
{
    /// Driver accepted the cap; payload is the actual applied value in
    /// 0.25 dBm units (the driver may clamp it lower on some chips).
    Applied(i8),
    /// `esp_wifi_set_max_tx_power` failed with the given return code.
    SetFailed(i32),
    /// Set succeeded but the readback via `esp_wifi_get_max_tx_power`
    /// failed (the cap is still in effect; we just can't confirm it).
    GetFailed,
}

/// Cap WiFi TX power at [`TX_POWER_QUARTER_DBM`] and read the
/// driver-applied value back. Wraps the `esp-wifi-sys` C bindings so
/// the unsafe block lives in one place.
pub fn set_max_tx_power() -> SetTxPowerResult
{
    let set_result =
        unsafe { esp_wifi_sys::include::esp_wifi_set_max_tx_power(TX_POWER_QUARTER_DBM) };
    if set_result != 0 {
        return SetTxPowerResult::SetFailed(set_result);
    }
    let mut applied = 0_i8;
    let get_result = unsafe { esp_wifi_sys::include::esp_wifi_get_max_tx_power(&mut applied) };
    if get_result != 0 {
        return SetTxPowerResult::GetFailed;
    }
    SetTxPowerResult::Applied(applied)
}

/// Snapshot of the AP the station is currently associated to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectedAp
{
    /// 6-byte BSSID of the currently-associated AP.
    pub bssid:     [u8; 6],
    /// Primary channel of the currently-associated AP.
    pub channel:   u8,
    /// Current AP RSSI in dBm.
    pub rssi:      i8,
    /// Driver auth-mode value, if available.
    pub auth_mode: u8,
}

/// Read the currently-associated AP record without logging.
pub fn connected_ap_info() -> Option<ConnectedAp>
{
    let mut ap_info = MaybeUninit::<esp_wifi_sys::include::wifi_ap_record_t>::uninit();
    let result = unsafe { esp_wifi_sys::include::esp_wifi_sta_get_ap_info(ap_info.as_mut_ptr()) };
    if result != 0 {
        return None;
    }

    let ap_info = unsafe { ap_info.assume_init() };
    Some(ConnectedAp {
        bssid:     ap_info.bssid,
        channel:   ap_info.primary,
        rssi:      ap_info.rssi,
        auth_mode: ap_info.authmode as u8,
    })
}

/// Read current AP info and publish it to the shared telemetry snapshot.
pub fn refresh_connected_ap_telemetry() -> Option<ConnectedAp>
{
    let ap_info = connected_ap_info()?;
    crate::telemetry_state::record_wifi_ap_info(
        i16::from(ap_info.rssi),
        ap_info.channel,
        ap_info.bssid,
        Some(ap_info.auth_mode),
    );
    Some(ap_info)
}

/// Read current AP info, publish it to telemetry, and log a one-line summary.
pub fn record_connected_ap_info(phase: &str) -> Option<ConnectedAp>
{
    let Some(ap_info) = refresh_connected_ap_telemetry() else {
        esp_println::println!("WiFi: AP info unavailable during {}", phase);
        return None;
    };
    esp_println::println!(
        "WiFi: AP phase={} bssid={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} channel={} rssi={} \
         auth={}",
        phase,
        ap_info.bssid[0],
        ap_info.bssid[1],
        ap_info.bssid[2],
        ap_info.bssid[3],
        ap_info.bssid[4],
        ap_info.bssid[5],
        ap_info.channel,
        ap_info.rssi,
        ap_info.auth_mode,
    );
    Some(ap_info)
}

/// Single scan match — the data needed to pin the next `connect()` to a
/// specific AP. Returned by [`scan_strongest_bssid`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedAp
{
    /// 6-byte BSSID of the chosen AP.
    pub bssid:   [u8; 6],
    /// 2.4 GHz / 5 GHz channel the AP advertises.
    pub channel: u8,
    /// Signal strength in dBm.
    pub rssi:    i8,
}

/// Scan for the configured SSID and return the strongest match. The
/// driver still owns auth + the 4-way handshake but it doesn't waste
/// 1-2 s on its own AP discovery when we hand it a pinned BSSID +
/// channel. Returns `None` on scan error or no SSID match.
pub fn scan_strongest_bssid(
    controller: &mut WifiController<'_>,
    ssid: &str,
    max_results: usize,
) -> Option<PinnedAp>
{
    let scan_config = ScanConfig {
        ssid: Some(ssid),
        ..Default::default()
    };
    let aps = match controller.scan_with_config_sync_max(scan_config, max_results) {
        Ok(aps) => aps,
        Err(error) => {
            esp_println::println!("WiFi: pre-scan failed: {:?}", error);
            return None;
        },
    };

    let mut best: Option<PinnedAp> = None;
    let mut matches = 0_u8;
    for ap in aps.iter().filter(|ap| ap.ssid.as_str() == ssid) {
        matches = matches.saturating_add(1);
        if best
            .as_ref()
            .map(|current| ap.signal_strength > current.rssi)
            .unwrap_or(true)
        {
            best = Some(PinnedAp {
                bssid:   ap.bssid,
                channel: ap.channel,
                rssi:    ap.signal_strength,
            });
        }
    }
    if let Some(best) = best {
        esp_println::println!(
            "WiFi: strongest AP for ssid=\"{}\" bssid={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} \
             channel={} rssi={} matches={}",
            ssid,
            best.bssid[0],
            best.bssid[1],
            best.bssid[2],
            best.bssid[3],
            best.bssid[4],
            best.bssid[5],
            best.channel,
            best.rssi,
            matches,
        );
        Some(best)
    } else {
        esp_println::println!("WiFi: no scan result matched configured SSID \"{}\"", ssid);
        None
    }
}

/// Convenience wrapper around [`scan_strongest_bssid`] that returns the
/// `base` configuration with `bssid` + `channel` filled in. Returns the
/// original config unchanged when no match is found, so the caller can
/// always pass the result to `set_configuration` without branching.
pub fn config_pinned_to_strongest_ap(
    controller: &mut WifiController<'_>,
    base: &ClientConfiguration,
) -> Option<ClientConfiguration>
{
    let pin = scan_strongest_bssid(controller, base.ssid.as_str(), SCAN_PIN_RESULTS)?;
    let mut pinned = base.clone();
    pinned.bssid = Some(pin.bssid);
    pinned.channel = Some(pin.channel);
    Some(pinned)
}

/// Apply the shared DHCP retry budget to a freshly-created socket.
/// Tighter than smoltcp's defaults so a single dropped DISCOVER doesn't
/// blow the whole DHCP timeout window.
pub fn configure_dhcp_retry(socket: &mut dhcpv4::Socket<'_>)
{
    let mut retry = socket.get_retry_config();
    retry.discover_timeout = SmoltcpDuration::from_secs(DHCP_DISCOVER_TIMEOUT_SECS);
    retry.initial_request_timeout = SmoltcpDuration::from_secs(DHCP_INITIAL_REQUEST_TIMEOUT_SECS);
    retry.request_retries = DHCP_REQUEST_RETRIES;
    socket.set_retry_config(retry);
}

/// Add DHCP option 12 Host Name to a socket using a sanitized form of
/// the configured gateway name. Consumer router apps usually display
/// this value in their device list.
pub fn configure_dhcp_hostname(
    socket: &mut dhcpv4::Socket<'static>,
    configured_name: &str,
    hostname_storage: &'static StaticCell<[u8; DHCP_HOSTNAME_MAX_LEN]>,
    options_storage: &'static StaticCell<[DhcpOption<'static>; 1]>,
) -> &'static str
{
    let storage = hostname_storage.init_with(|| [0_u8; DHCP_HOSTNAME_MAX_LEN]);
    let len = write_dhcp_hostname(storage, configured_name);
    let hostname_bytes: &'static [u8] = &storage[..len];
    let options = options_storage.init_with(|| {
        [DhcpOption {
            kind: DHCP_HOST_NAME_OPTION,
            data: hostname_bytes,
        }]
    });
    socket.set_outgoing_options(options);
    core::str::from_utf8(hostname_bytes).unwrap_or(DEFAULT_DHCP_HOSTNAME)
}

fn write_dhcp_hostname(out: &mut [u8; DHCP_HOSTNAME_MAX_LEN], configured_name: &str) -> usize
{
    let mut len = 0_usize;
    let mut pending_dash = false;

    for byte in configured_name.bytes() {
        let mapped = match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => byte,
            b' ' | b'-' | b'_' => b'-',
            _ => continue,
        };

        if mapped == b'-' {
            if len > 0 {
                pending_dash = true;
            }
            continue;
        }

        if pending_dash && len < out.len().saturating_sub(1) {
            out[len] = b'-';
            len += 1;
        }
        pending_dash = false;

        if len >= out.len() {
            break;
        }
        out[len] = mapped;
        len += 1;
    }

    while len > 0 && out[len - 1] == b'-' {
        len -= 1;
    }

    if len == 0 {
        let fallback = DEFAULT_DHCP_HOSTNAME.as_bytes();
        let fallback_len = fallback.len().min(out.len());
        out[..fallback_len].copy_from_slice(&fallback[..fallback_len]);
        return fallback_len;
    }

    len
}

/// Settle delay after `is_connected = true` flips before we start
/// hammering DHCPDISCOVER. Without it the first attempt often races
/// the AP's still-finalizing 4-way handshake and gets dropped.
pub const POST_ASSOC_SETTLE_MS: u32 = 500;
/// smoltcp poll cadence while waiting for a DHCP lease.
pub const DHCP_POLL_INTERVAL_MS: u32 = 25;
/// Default total DHCP poll budget — 40 s at 25 ms ticks.
pub const DHCP_DEFAULT_TIMEOUT_POLLS: u32 = 1_600;

/// Outcome of [`run_dhcp_blocking`].
#[derive(Debug, Clone, Copy)]
pub enum DhcpOutcome
{
    /// Lease acquired; IPv4 octets + optional gateway IPv4.
    Acquired
    {
        /// Allocated IPv4 address.
        ip:      [u8; 4],
        /// Default gateway IPv4, if the DHCP server included it.
        gateway: Option<[u8; 4]>,
    },
    /// Poll budget exhausted before any `Configured` event arrived.
    TimedOut,
}

/// Errors returned by [`init_station`].
#[derive(Debug, Clone, Copy)]
pub enum WifiSetupError
{
    /// `esp_wifi::init` rejected the timer + RNG combination.
    DriverInit,
    /// `esp_wifi::wifi::new` failed.
    Controller,
    /// `set_configuration` / `start` failed.
    Configuration,
    /// `is_started()` never went true within the start budget.
    StartTimeout,
}

/// Bring up the esp-wifi station controller with the standard
/// reliability tweaks (power-save off, TX power capped). After this
/// returns, the controller is started; the caller can immediately
/// call [`associate`] or set a scan-only configuration.
///
/// Shared between every ESP32 board variant — bumping reliability
/// defaults here improves every board at once instead of each board
/// re-doing the same setup with subtle differences.
pub fn init_station(
    timg1: TIMG1<'static>,
    rng: RNG<'static>,
    wifi: WIFI<'static>,
) -> Result<(WifiController<'static>, WifiDevice<'static>), WifiSetupError>
{
    static WIFI_CTRL: StaticCell<EspWifiController<'static>> = StaticCell::new();
    let timg = TimerGroup::new(timg1);
    let rng = Rng::new(rng);
    let init = esp_wifi::init(timg.timer0, rng).map_err(|_| WifiSetupError::DriverInit)?;
    let init: &'static EspWifiController<'static> = WIFI_CTRL.init(init);
    let (mut controller, interfaces) =
        esp_wifi::wifi::new(init, wifi).map_err(|_| WifiSetupError::Controller)?;
    controller
        .set_configuration(&Configuration::Client(ClientConfiguration::default()))
        .map_err(|_| WifiSetupError::Configuration)?;
    controller
        .start()
        .map_err(|_| WifiSetupError::Configuration)?;
    if !wait_for_started(&controller) {
        esp_println::println!(
            "WiFi: is_started() never went true ({}ms budget)",
            START_POLL_MAX * START_POLL_INTERVAL_MS,
        );
        return Err(WifiSetupError::StartTimeout);
    }
    if let Err(e) = disable_power_save(&mut controller) {
        esp_println::println!("WiFi: set_power_saving failed: {:?}", e);
    }
    match set_max_tx_power() {
        SetTxPowerResult::Applied(applied) => esp_println::println!(
            "WiFi: TX power cap requested={} applied={} (0.25 dBm units)",
            TX_POWER_QUARTER_DBM,
            applied,
        ),
        SetTxPowerResult::SetFailed(rc) => {
            esp_println::println!("WiFi: esp_wifi_set_max_tx_power failed: {}", rc);
        },
        SetTxPowerResult::GetFailed => {
            esp_println::println!(
                "WiFi: TX power cap requested={} (readback failed, cap in effect)",
                TX_POWER_QUARTER_DBM,
            );
        },
    }
    Ok((controller, interfaces.sta))
}

/// Configure the station for a given SSID + password, pin the
/// strongest matching BSSID + channel into the config (skipping the
/// driver's own AP discovery), and call `connect()`. Returns the
/// moment `connect()` returns — caller polls `is_connected()` to
/// observe the association.
pub fn associate(
    controller: &mut WifiController<'static>,
    ssid: &str,
    password: &str,
) -> Result<(), WifiError>
{
    let mut client = client_configuration(ssid, password);
    controller.set_configuration(&Configuration::Client(client.clone()))?;
    if let Some(pinned) = config_pinned_to_strongest_ap(controller, &client) {
        client = pinned;
        if let Err(e) = controller.set_configuration(&Configuration::Client(client)) {
            esp_println::println!(
                "WiFi: applying pinned config failed: {:?} (continuing with unpinned)",
                e,
            );
        }
    }
    controller.connect()
}

/// Configure and connect to a specific AP from a previous scan.
pub fn associate_pinned(
    controller: &mut WifiController<'static>,
    ssid: &str,
    password: &str,
    pin: PinnedAp,
) -> Result<(), WifiError>
{
    let mut client = client_configuration(ssid, password);
    client.bssid = Some(pin.bssid);
    client.channel = Some(pin.channel);
    esp_println::println!(
        "WiFi: connecting pinned AP bssid={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} channel={} \
         rssi={}",
        pin.bssid[0],
        pin.bssid[1],
        pin.bssid[2],
        pin.bssid[3],
        pin.bssid[4],
        pin.bssid[5],
        pin.channel,
        pin.rssi,
    );
    controller.set_configuration(&Configuration::Client(client))?;
    controller.connect()
}

fn client_configuration(ssid: &str, password: &str) -> ClientConfiguration
{
    let auth_method = if password.is_empty() {
        AuthMethod::None
    } else {
        AuthMethod::WPAWPA2Personal
    };
    ClientConfiguration {
        ssid: AllocString::from(ssid),
        bssid: None,
        auth_method,
        password: AllocString::from(password),
        channel: None,
    }
}

/// Run the DHCP client on `interface` until a lease lands or the poll
/// budget is exhausted. On `Acquired`, the IPv4 + default route are
/// installed on the interface so subsequent TCP traffic can route.
///
/// `now_ms_at_call` is the host's monotonic millisecond clock at the
/// moment of the call — used to seed the smoltcp clock so the DHCP
/// state machine ticks at real wall-clock rate.
pub fn run_dhcp_blocking(
    interface: &mut Interface,
    device: &mut WifiDevice<'_>,
    sockets: &mut SocketSet<'_>,
    dhcp_handle: SocketHandle,
    now_ms_at_call: u64,
    timeout_polls: u32,
) -> DhcpOutcome
{
    let delay = Delay::new();
    delay.delay_millis(POST_ASSOC_SETTLE_MS);

    let mut elapsed_ms: u64 = 0;
    for _ in 0..timeout_polls {
        let now = SmoltcpInstant::from_millis((now_ms_at_call + elapsed_ms) as i64);
        let _ = interface.poll(now, device, sockets);
        let socket = sockets.get_mut::<dhcpv4::Socket>(dhcp_handle);
        if let Some(dhcpv4::Event::Configured(config)) = socket.poll() {
            let octets = config.address.address().octets();
            let gateway = config.router.map(|r| r.octets());
            if let Some(router) = config.router {
                let _ = interface.routes_mut().add_default_ipv4_route(router);
            }
            interface.update_ip_addrs(|addrs| {
                let _ = addrs.push(IpCidr::Ipv4(config.address));
            });
            return DhcpOutcome::Acquired {
                ip: octets,
                gateway,
            };
        }
        delay.delay_millis(DHCP_POLL_INTERVAL_MS);
        elapsed_ms = elapsed_ms.saturating_add(DHCP_POLL_INTERVAL_MS as u64);
    }
    DhcpOutcome::TimedOut
}
