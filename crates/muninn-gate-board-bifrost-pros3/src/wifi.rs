//! Bifrost WiFi orchestrator.
//!
//! Nothing about WiFi setup is Bifrost-specific — every step here is a
//! thin call into [`muninn_gate_platform_esp32::wifi_common`], which
//! Heltec uses for exactly the same flow. The board crate only owns the
//! scan-result projection into UI-friendly types and the inline
//! smoltcp + DHCP setup (which uses the shared retry config).

use core::cmp::Ordering;

use esp_hal::peripherals::{RNG, TIMG1, WIFI};
use esp_wifi::wifi::{AuthMethod, ScanConfig, WifiController, WifiDevice};
use heapless::{String, Vec};
use muninn_gate_platform_esp32::{telemetry_state, wifi_common};

use crate::ui::{AuthLabel, MAX_WIFI_APS, WifiAp};

/// Maximum APs we ask the radio for in one provisioning-page scan.
const MAX_SCAN_RESULTS: usize = 12;

/// Owned WiFi controller + station device. Construction goes through
/// [`wifi_common::init_station`] so the chip-level setup (power save
/// off, TX power cap, started gate) is identical to Heltec's.
pub struct WifiScanner
{
    controller: WifiController<'static>,
    /// Consumed by `acquire_dhcp` on the first call — smoltcp owns it
    /// for the lifetime of the DHCP poll, then drops it back into the
    /// kernel's hands.
    device:     Option<WifiDevice<'static>>,
}

impl WifiScanner
{
    /// Bring up the station controller via the shared platform helper.
    pub fn new(
        timg1: TIMG1<'static>,
        rng: RNG<'static>,
        wifi: WIFI<'static>,
    ) -> Result<Self, wifi_common::WifiSetupError>
    {
        let (controller, device) = wifi_common::init_station(timg1, rng, wifi)?;
        Ok(Self {
            controller,
            device: Some(device),
        })
    }

    /// Configure the station for the configured AP and call `connect()`.
    /// Returns immediately; the caller polls [`Self::is_connected`].
    /// Delegates the credential set + BSSID pin + connect to
    /// [`wifi_common::associate`] so Heltec and Bifrost both go through
    /// the same code path.
    pub fn try_connect(
        &mut self,
        ssid: &str,
        password: &str,
        now_ms: u64,
    ) -> Result<(), esp_wifi::wifi::WifiError>
    {
        let started = esp_hal::time::Instant::now();
        telemetry_state::record_wifi_connect_started(now_ms);
        esp_println::println!(
            "WiFi: try_connect ssid=\"{}\" password_len={}",
            ssid,
            password.len(),
        );
        match wifi_common::associate(&mut self.controller, ssid, password) {
            Ok(()) => {
                telemetry_state::record_wifi_connect_request(false);
                esp_println::println!(
                    "WiFi: connect requested ssid=\"{}\" (total {}ms)",
                    ssid,
                    started.elapsed().as_millis(),
                );
                Ok(())
            },
            Err(error) => {
                telemetry_state::record_wifi_connect_request(true);
                Err(error)
            },
        }
    }

    /// Configure the station for a specific scanned AP and call `connect()`.
    pub fn try_connect_pinned(
        &mut self,
        ssid: &str,
        password: &str,
        pin: wifi_common::PinnedAp,
        now_ms: u64,
    ) -> Result<(), esp_wifi::wifi::WifiError>
    {
        let started = esp_hal::time::Instant::now();
        telemetry_state::record_wifi_connect_started(now_ms);
        esp_println::println!(
            "WiFi: try_connect pinned ssid=\"{}\" password_len={}",
            ssid,
            password.len(),
        );
        match wifi_common::associate_pinned(&mut self.controller, ssid, password, pin) {
            Ok(()) => {
                telemetry_state::record_wifi_connect_request(false);
                esp_println::println!(
                    "WiFi: pinned connect requested ssid=\"{}\" (total {}ms)",
                    ssid,
                    started.elapsed().as_millis(),
                );
                Ok(())
            },
            Err(error) => {
                telemetry_state::record_wifi_connect_request(true);
                Err(error)
            },
        }
    }

    /// True once the station is associated with the configured AP.
    pub fn is_connected(&mut self) -> bool
    {
        self.controller.is_connected().unwrap_or(false)
    }

    /// Ask the station controller to disconnect from the current AP.
    /// Recovery code uses this before issuing a fresh connect request.
    pub fn disconnect(&mut self)
    {
        let _ = self.controller.disconnect();
    }

    /// Hand the WifiDevice off to the caller — typically the HTTP
    /// server, which owns smoltcp and needs to drive the device for
    /// the lifetime of the network stack. Returns `None` on subsequent
    /// calls (device can only be taken once).
    pub fn take_device(&mut self) -> Option<esp_wifi::wifi::WifiDevice<'static>>
    {
        self.device.take()
    }

    /// Scan the configured SSID and return its strongest BSSID/channel.
    pub fn strongest_ap_for(&mut self, ssid: &str) -> Option<wifi_common::PinnedAp>
    {
        wifi_common::scan_strongest_bssid(&mut self.controller, ssid, wifi_common::SCAN_PIN_RESULTS)
    }

    /// Blocking scan returning the top APs by signal strength. Used by
    /// the provisioning UI; HTTP/Heltec uses a different scan path.
    pub fn scan(&mut self) -> Result<Vec<WifiAp, { MAX_WIFI_APS }>, esp_wifi::wifi::WifiError>
    {
        let aps = self
            .controller
            .scan_with_config_sync_max(ScanConfig::default(), MAX_SCAN_RESULTS)?;
        let mut results: Vec<WifiAp, { MAX_WIFI_APS }> = Vec::new();
        for ap in aps.iter() {
            if ap.ssid.is_empty() {
                continue;
            }
            let mut ssid: String<32> = String::new();
            for ch in ap.ssid.chars() {
                if ssid.push(ch).is_err() {
                    break;
                }
            }
            let entry = WifiAp {
                ssid,
                rssi: ap.signal_strength,
                auth: ap
                    .auth_method
                    .map(classify_auth)
                    .unwrap_or(AuthLabel::Other),
                channel: ap.channel,
            };
            if results.len() < results.capacity() {
                let _ = results.push(entry);
            } else if let Some(weakest_index) = weakest_index(&results) {
                if results[weakest_index].rssi < entry.rssi {
                    results[weakest_index] = entry;
                }
            }
        }
        results.sort_unstable_by(|a, b| match b.rssi.cmp(&a.rssi) {
            Ordering::Equal => a.ssid.as_str().cmp(b.ssid.as_str()),
            other => other,
        });
        Ok(results)
    }
}

fn weakest_index(aps: &[WifiAp]) -> Option<usize>
{
    aps.iter()
        .enumerate()
        .min_by_key(|(_, ap)| ap.rssi)
        .map(|(i, _)| i)
}

fn classify_auth(method: AuthMethod) -> AuthLabel
{
    match method {
        AuthMethod::None => AuthLabel::Open,
        AuthMethod::WEP => AuthLabel::Wep,
        AuthMethod::WPA => AuthLabel::Wpa,
        AuthMethod::WPA2Personal | AuthMethod::WPAWPA2Personal => AuthLabel::Wpa2,
        AuthMethod::WPA3Personal | AuthMethod::WPA2WPA3Personal => AuthLabel::Wpa3,
        AuthMethod::WPA2Enterprise => AuthLabel::Enterprise,
        _ => AuthLabel::Other,
    }
}
