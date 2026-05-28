//! Plain-data UI state. The runner builds one of these every tick from
//! live board readings, then hands it to [`super::render`] together with
//! the desired [`super::Screen`]. Nothing here owns peripherals; that keeps
//! screen rendering trivially testable.

use heapless::String;
use heapless::Vec;

use crate::bifrost_pros3::battery::BatterySample;

/// Maximum SSID length we render. WiFi SSIDs are up to 32 bytes; we render
/// truncated with an ellipsis if longer.
pub const MAX_SSID_LEN: usize = 32;
/// Maximum APs we keep in the UI state. The provisioning screen renders the
/// top 3; the dedicated scan screen can show more.
pub const MAX_WIFI_APS: usize = 8;
/// Maximum length of any error / status string the runner may pass in.
pub const MAX_STATUS_LEN: usize = 48;

/// Network bring-up phase used by the header indicator and the body screens.
///
/// The bring-up runner currently only emits `Unprovisioned` and `Scanning`
/// (the latter briefly while a blocking scan is in flight). `Connecting`,
/// `Connected`, and `Error` are wired up for the association/HTTP phase
/// and live behind `dead_code` until that lands.
#[allow(dead_code, reason = "forward-looking phases consumed after association lands")]
#[derive(Debug, Clone)]
pub enum NetworkPhase
{
    /// No WiFi configured yet (device is awaiting provisioning).
    Unprovisioned,
    /// Scanning for nearby APs.
    Scanning,
    /// Trying to associate with a configured AP.
    Connecting
    {
        /// SSID the radio is associating with.
        ssid: String<MAX_SSID_LEN>,
    },
    /// Connected with an active DHCP lease.
    Connected
    {
        /// SSID the radio is associated with.
        ssid: String<MAX_SSID_LEN>,
        /// Current signal strength in dBm (typically -100..-30).
        rssi: i8,
        /// IPv4 address from DHCP.
        ipv4: [u8; 4],
    },
    /// WiFi configured but currently not connected.
    Error
    {
        /// Short reason string to display.
        reason: String<MAX_STATUS_LEN>,
    },
}

/// Short label for the auth method, suitable to show in tight UI cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthLabel
{
    /// Open / no encryption.
    Open,
    /// WEP (legacy).
    Wep,
    /// WPA-PSK.
    Wpa,
    /// WPA2-PSK (the common case in 2026).
    Wpa2,
    /// WPA3-PSK.
    Wpa3,
    /// Enterprise (802.1x) — requires more than a static PSK.
    Enterprise,
    /// Anything we don't recognize.
    Other,
}

impl AuthLabel
{
    /// Human-readable 3..4 char label.
    pub const fn short(self) -> &'static str
    {
        match self {
            Self::Open => "Open",
            Self::Wep => "WEP",
            Self::Wpa => "WPA",
            Self::Wpa2 => "WPA2",
            Self::Wpa3 => "WPA3",
            Self::Enterprise => "ENT",
            Self::Other => "?",
        }
    }
}

/// Single AP observed during a WiFi scan.
#[derive(Debug, Clone)]
pub struct WifiAp
{
    /// SSID (truncated to `MAX_SSID_LEN`).
    pub ssid:    String<MAX_SSID_LEN>,
    /// Signal strength in dBm.
    pub rssi:    i8,
    /// Auth method observed in the scan beacon.
    pub auth:    AuthLabel,
    /// 2.4 GHz channel (1..=13) or 5 GHz channel (36..=165).
    pub channel: u8,
}

/// All state needed to render any Bifrost UI screen. Built fresh each tick
/// by the runner. References live data instead of owning it where possible.
#[derive(Debug, Clone)]
pub struct UiState
{
    /// Gateway display name (typically the variant `BOARD` constant).
    pub title:         String<32>,
    /// Latest battery sample, if the fuel gauge is healthy.
    pub battery:       Option<BatterySample>,
    /// `true` while USB 5 V is present on the ProS3 VBUS sense pin —
    /// drives the charging indicator inside the battery icon.
    pub usb_connected: bool,
    /// Current network phase shown in the header + status body.
    pub network:       NetworkPhase,
    /// Latest set of nearby APs from the most recent scan.
    pub wifi_aps:      Vec<WifiAp, MAX_WIFI_APS>,
    /// Boot-relative wall clock in ms — drives animations (spinner phase etc).
    pub now_ms:        u64,
    /// Free-form line shown in the status footer.
    pub status_line:   String<MAX_STATUS_LEN>,
}

impl UiState
{
    /// Build an empty state. Caller fills in fields per tick.
    pub fn new() -> Self
    {
        Self {
            title:         String::new(),
            battery:       None,
            usb_connected: false,
            network:       NetworkPhase::Unprovisioned,
            wifi_aps:      Vec::new(),
            now_ms:        0,
            status_line:   String::new(),
        }
    }
}

impl Default for UiState
{
    fn default() -> Self
    {
        Self::new()
    }
}
