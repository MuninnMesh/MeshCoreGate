//! WiFi scanner for the Bifrost ProS3 bring-up.
//!
//! Owns the `esp-wifi` controller in station mode but stays well short of an
//! actual connection: we only need to enumerate visible APs so the
//! provisioning screen can show the operator what's nearby. The controller
//! is initialized once at boot and re-used for every scan.
//!
//! Blocking scan API only — the bring-up runner is a single-thread loop. A
//! scan takes ~1-3 s; the runner stalls for that long every cycle which is
//! fine while we don't have time-critical work.

use core::cmp::Ordering;

use esp_hal::peripherals::{RNG, TIMG1, WIFI};
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_wifi::EspWifiController;
use esp_wifi::wifi::{AuthMethod, ClientConfiguration, Configuration, ScanConfig, WifiController};
use heapless::{String, Vec};
use static_cell::StaticCell;

use super::ui::{AuthLabel, MAX_WIFI_APS, WifiAp};

/// Maximum APs we ask the radio for in one scan call. We then sort by RSSI
/// and keep the top `MAX_WIFI_APS` for the UI.
const MAX_SCAN_RESULTS: usize = 12;

/// Errors initializing the WiFi scanner.
#[derive(Debug, Clone, Copy)]
pub enum WifiInitError
{
    /// esp-wifi setup failed (timer/RNG init).
    DriverInit,
    /// WiFi controller (`esp_wifi::wifi::new`) creation failed.
    Controller,
    /// `set_configuration`/`start` failed.
    Configuration,
}

/// Owned WiFi controller + scan helpers.
pub struct WifiScanner
{
    controller: WifiController<'static>,
}

impl WifiScanner
{
    /// Bring up the esp-wifi station controller. Does NOT associate with any
    /// AP; only enables scanning.
    pub fn new(timg1: TIMG1<'static>, rng: RNG<'static>, wifi: WIFI<'static>) -> Result<Self, WifiInitError>
    {
        static WIFI_CTRL: StaticCell<EspWifiController<'static>> = StaticCell::new();
        let timg = TimerGroup::new(timg1);
        let rng = Rng::new(rng);
        let init = esp_wifi::init(timg.timer0, rng).map_err(|_| WifiInitError::DriverInit)?;
        let init: &'static EspWifiController<'static> = WIFI_CTRL.init(init);
        let (mut controller, _interfaces) =
            esp_wifi::wifi::new(init, wifi).map_err(|_| WifiInitError::Controller)?;
        controller
            .set_configuration(&Configuration::Client(ClientConfiguration::default()))
            .map_err(|_| WifiInitError::Configuration)?;
        controller
            .start()
            .map_err(|_| WifiInitError::Configuration)?;
        Ok(Self { controller })
    }

    /// Blocking scan. Returns up to `MAX_WIFI_APS` APs sorted by signal
    /// strength (strongest first).
    pub fn scan(&mut self) -> Result<Vec<WifiAp, { MAX_WIFI_APS }>, esp_wifi::wifi::WifiError>
    {
        let aps = self
            .controller
            .scan_with_config_sync_max(ScanConfig::default(), MAX_SCAN_RESULTS)?;
        let mut results: Vec<WifiAp, { MAX_WIFI_APS }> = Vec::new();

        // Walk the raw scan results, project to our typed `WifiAp`, and
        // maintain a max-by-rssi heap of size `MAX_WIFI_APS`. Simpler O(n*k)
        // approach for the tiny `k` we care about.
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
                auth: ap.auth_method.map(classify_auth).unwrap_or(AuthLabel::Other),
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
