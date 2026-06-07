//! Plain-data UI state. The runner builds one of these every tick from
//! live board readings, then hands it to [`super::render`] together with
//! the desired [`super::Screen`]. Nothing here owns peripherals; that keeps
//! screen rendering trivially testable.

use heapless::{String, Vec};
use muninn_driver_max17048::BatterySample;

/// Maximum SSID length we render. WiFi SSIDs are up to 32 bytes; we render
/// truncated with an ellipsis if longer.
pub const MAX_SSID_LEN: usize = 32;
/// Maximum APs we keep in the UI state. The provisioning screen renders the
/// top 3; the dedicated scan screen can show more.
pub const MAX_WIFI_APS: usize = 8;
/// Maximum length of any error / status string the runner may pass in.
pub const MAX_STATUS_LEN: usize = 48;
/// Maximum length of the local wall-clock label rendered on the producer screen.
pub const MAX_CLOCK_TEXT_LEN: usize = 8;
/// Maximum producers tracked in the UI state for the operational screen.
/// `muninn_gate_core::config::MAX_TELEMETRY_PRODUCERS` is 16; we cap our
/// row count below at the on-screen visible limit.
pub const MAX_UI_PRODUCERS: usize = 8;
/// Length cap for the rendered producer name. Matches the source
/// `TelemetryProducerName` width.
pub const MAX_PRODUCER_NAME_LEN: usize = 32;

/// Network bring-up phase used by the header indicator and the body screens.
///
/// The bring-up runner currently only emits `Unprovisioned` and `Scanning`
/// (the latter briefly while a blocking scan is in flight). `Connecting`,
/// `Connected`, and `Error` are wired up for the association/HTTP phase
/// and live behind `dead_code` until that lands.
#[allow(
    dead_code,
    reason = "forward-looking phases consumed after association lands"
)]
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

/// Outcome of the most recent LoRa poll attempt for a producer.
///
/// `Pending` is the boot/config-load state — no poll has been issued
/// yet. The other variants track the lifecycle that the (still-to-land)
/// LoRa poller drives through every cycle: `InProgress` while a request
/// is in flight, then `Success { rssi }` or `Failed` at completion.
#[allow(
    dead_code,
    reason = "InProgress + Failed wired through the UI now; poller lands later"
)]
#[derive(Debug, Clone, Copy)]
pub enum PollStatus
{
    /// Never polled — boot state.
    Pending,
    /// A poll request is currently in flight (drives a small animation).
    InProgress,
    /// Last poll succeeded with the given received-signal strength.
    Success
    {
        /// RSSI in dBm reported by the SX1262 for the response packet.
        rssi: i8,
    },
    /// Last poll failed (timeout, CRC mismatch, etc.).
    Failed,
}

/// Compact projection of a `TelemetryProducerConfig` for the UI. Holds
/// what the operational screen needs to render one row — name, kind
/// label, enabled flag, last-poll status + age, and the single
/// "highlight metric" with its unit suffix.
#[derive(Debug, Clone)]
pub struct ProducerSummary
{
    /// Display name as configured.
    pub name:            String<MAX_PRODUCER_NAME_LEN>,
    /// Short kind label, e.g. `"companion"` or `"repeater"`.
    pub kind_label:      &'static str,
    /// Whether the producer is enabled for polling.
    pub enabled:         bool,
    /// Outcome of the most recent poll attempt.
    pub status:          PollStatus,
    /// Boot-relative wall-clock time when the last poll *completed*
    /// (succeeded or failed), used to render the `5s` / `2m` age tag.
    /// `None` while `status == Pending`.
    pub last_poll_at_ms: Option<u64>,
    /// Selected highlight metric for this producer (e.g. battery V,
    /// solar W, temperature C). `None` until the first successful
    /// poll populates it.
    pub metric_value:    Option<f32>,
    /// Short unit suffix appended to `metric_value` — kept as a
    /// `&'static str` so the UI doesn't allocate per row.
    pub metric_unit:     &'static str,
}

/// Progress state for an in-flight WiFi OTA update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtaUpdateStatus
{
    /// Bytes received from the HTTP upload body. `None` means OTA mode
    /// is active but no binary upload is currently being streamed.
    pub received_bytes: Option<u32>,
    /// Expected firmware body length from `Content-Length`.
    pub total_bytes:    Option<u32>,
}

/// All state needed to render any Bifrost UI screen. Built fresh each tick
/// by the runner. References live data instead of owning it where possible.
#[derive(Debug, Clone)]
pub struct UiState
{
    /// Gateway display name (typically the variant `BOARD` constant).
    pub title:                 String<32>,
    /// First four hex chars of the gateway MeshCore public key, shown in the top bar.
    pub gateway_pubkey_prefix: String<4>,
    /// Latest battery sample, if the fuel gauge is healthy.
    pub battery:               Option<BatterySample>,
    /// `true` while USB 5 V is present on the ProS3 VBUS sense pin —
    /// drives the charging indicator inside the battery icon.
    pub usb_connected:         bool,
    /// Current network phase shown in the header + status body.
    pub network:               NetworkPhase,
    /// Latest set of nearby APs from the most recent scan.
    pub wifi_aps:              Vec<WifiAp, MAX_WIFI_APS>,
    /// Producers from the most recently uploaded config — drives the
    /// operational screen's per-node rows.
    pub producers:             Vec<ProducerSummary, MAX_UI_PRODUCERS>,
    /// `true` once the gate has a LoRa radio peripheral. Bifrost ProS3
    /// leaves this `false` until the Wio-SX1262 wire-check probe
    /// succeeds at boot (see `bifrost_pros3::lora::LoraRadio::probe`).
    /// While `false`, the operational screen renders a clear "LoRa not
    /// wired" warning and the status LED blinks fast-red critical.
    pub lora_available:        bool,
    /// HTTP server port from the uploaded config (default 80). Rendered
    /// next to the IP on the operational screen so the operator gets a
    /// scannable "host:port" address without having to look it up.
    pub http_port:             u16,
    /// Boot-relative wall clock in ms — drives animations (spinner phase etc).
    pub now_ms:                u64,
    /// Free-form line shown in the status footer.
    pub status_line:           String<MAX_STATUS_LEN>,
    /// Local wall-clock label (`HH:MM`) if wall time has been seeded.
    pub clock_text:            String<MAX_CLOCK_TEXT_LEN>,
    /// Active WiFi OTA progress, if a firmware update is in progress.
    pub ota_update:            Option<OtaUpdateStatus>,
}

impl UiState
{
    /// Build an empty state. Caller fills in fields per tick.
    pub fn new() -> Self
    {
        Self {
            title:                 String::new(),
            gateway_pubkey_prefix: String::new(),
            battery:               None,
            usb_connected:         false,
            network:               NetworkPhase::Unprovisioned,
            wifi_aps:              Vec::new(),
            producers:             Vec::new(),
            lora_available:        false,
            http_port:             80,
            now_ms:                0,
            status_line:           String::new(),
            clock_text:            String::new(),
            ota_update:            None,
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
