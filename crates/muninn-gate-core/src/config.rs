//! Gateway and telemetry producer configuration types.

use core::fmt;

use heapless::{String, Vec};

use crate::Error;

/// Maximum number of telemetry producers tracked by one gateway.
pub const MAX_TELEMETRY_PRODUCERS: usize = 16;
/// Maximum gateway display name length.
pub const MAX_GATEWAY_NAME_LEN: usize = 32;
/// Maximum number of HTTP bearer tokens accepted by the gateway.
pub const MAX_HTTP_AUTH_TOKENS: usize = 8;
/// Maximum HTTP bearer token length.
pub const MAX_HTTP_AUTH_TOKEN_LEN: usize = 96;
/// Maximum telemetry producer display name length.
pub const MAX_PRODUCER_NAME_LEN: usize = 32;
/// Maximum explicit producer route/path length.
pub const MAX_PRODUCER_PATH_LEN: usize = 96;
/// Maximum MeshCore key or credential material length.
pub const MAX_MESHCORE_MATERIAL_LEN: usize = 128;
/// Maximum WiFi SSID length.
pub const MAX_WIFI_SSID_LEN: usize = 32;
/// Maximum WiFi password length.
pub const MAX_WIFI_PASSWORD_LEN: usize = 64;
/// Default polling interval for producers without an override.
pub const DEFAULT_POLLING_INTERVAL_SECS: u32 = 600;
/// Minimum allowed polling interval.
pub const DEFAULT_MIN_POLLING_INTERVAL_SECS: u32 = 60;
/// Maximum allowed polling interval.
pub const DEFAULT_MAX_POLLING_INTERVAL_SECS: u32 = 3_600;
/// Default HTTP port for Prometheus metrics.
pub const DEFAULT_HTTP_PORT: u16 = 80;
/// Default failed-poll retry count.
pub const DEFAULT_RETRY_COUNT: u8 = 5;
/// Default scheduler jitter window in seconds.
pub const DEFAULT_JITTER_SECS: u32 = 10;
/// Default OLED brightness percentage.
pub const DEFAULT_DISPLAY_BRIGHTNESS_PERCENT: u8 = 60;
/// Maximum accepted failed-poll retry count.
pub const MAX_RETRY_COUNT: u8 = 10;
/// Maximum accepted scheduler jitter window in seconds.
pub const MAX_JITTER_SECS: u32 = 60;
/// Default MeshCore path hash mode, where mode 2 means 3-byte hashes.
pub const DEFAULT_MESHCORE_PATH_MODE: u8 = 2;
/// Minimum accepted MeshCore path hash mode.
pub const MIN_MESHCORE_PATH_MODE: u8 = 0;
/// Maximum accepted MeshCore path hash mode.
pub const MAX_MESHCORE_PATH_MODE: u8 = 2;
/// Minimum accepted SX126x TX power level.
pub const MIN_TX_POWER_LEVEL: i8 = -9;
/// Maximum accepted SX126x TX power level.
pub const MAX_TX_POWER_LEVEL: i8 = 22;

/// Gateway name shown in diagnostics and UI.
pub type GatewayName = String<MAX_GATEWAY_NAME_LEN>;
/// Bearer token used for HTTP authentication.
pub type HttpAuthToken = String<MAX_HTTP_AUTH_TOKEN_LEN>;
/// Optional producer name; empty means discovery may fill it later.
pub type TelemetryProducerName = String<MAX_PRODUCER_NAME_LEN>;
/// Explicit MeshCore route/path for a producer.
pub type TelemetryProducerPath = String<MAX_PRODUCER_PATH_LEN>;
/// MeshCore public key, private key, seed, or similar key material.
pub type MeshcoreMaterial = String<MAX_MESHCORE_MATERIAL_LEN>;

/// Stable local identifier for a configured telemetry producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TelemetryProducerId(u64);

impl TelemetryProducerId
{
    /// Derive a stable local identifier from a MeshCore public key.
    ///
    /// The public key remains the canonical MeshCore identity. This compact
    /// identifier is only used by in-memory scheduler and telemetry indexes.
    pub fn from_public_key(public_key: &str) -> Self
    {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in public_key.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self(hash)
    }

    /// Return the raw numeric producer identifier.
    pub const fn as_u64(self) -> u64
    {
        self.0
    }
}

impl fmt::Display for TelemetryProducerId
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        write!(f, "{:016x}", self.0)
    }
}

/// Global polling policy shared by telemetry producers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollingConfig
{
    /// Default interval used when a producer has no override.
    pub default_interval_secs: u32,
    /// Minimum allowed polling interval.
    pub min_interval_secs:     u32,
    /// Maximum allowed polling interval.
    pub max_interval_secs:     u32,
    /// Number of retries after the initial failed producer poll.
    pub retry_count:           u8,
    /// Maximum per-producer scheduler jitter in seconds.
    pub jitter_secs:           u32,
}

impl PollingConfig
{
    /// Clamp a requested interval to the configured min/max range.
    pub const fn clamp_interval_secs(&self, requested_secs: u32) -> u32
    {
        if requested_secs < self.min_interval_secs {
            self.min_interval_secs
        } else if requested_secs > self.max_interval_secs {
            self.max_interval_secs
        } else {
            requested_secs
        }
    }

    /// Return the clamped default polling interval in milliseconds.
    pub const fn default_interval_ms(&self) -> u64
    {
        self.clamp_interval_secs(self.default_interval_secs) as u64 * 1_000
    }

    /// Return the configured scheduler jitter window in milliseconds.
    pub const fn jitter_ms(&self) -> u64
    {
        self.jitter_secs as u64 * 1_000
    }

    /// Return true when the polling policy is internally consistent.
    pub const fn is_valid(&self) -> bool
    {
        self.min_interval_secs > 0
            && self.min_interval_secs <= self.default_interval_secs
            && self.default_interval_secs <= self.max_interval_secs
            && self.retry_count <= MAX_RETRY_COUNT
            && self.jitter_secs <= MAX_JITTER_SECS
    }
}

impl Default for PollingConfig
{
    fn default() -> Self
    {
        Self {
            default_interval_secs: DEFAULT_POLLING_INTERVAL_SECS,
            min_interval_secs:     DEFAULT_MIN_POLLING_INTERVAL_SECS,
            max_interval_secs:     DEFAULT_MAX_POLLING_INTERVAL_SECS,
            retry_count:           DEFAULT_RETRY_COUNT,
            jitter_secs:           DEFAULT_JITTER_SECS,
        }
    }
}

/// HTTP service configuration; absence means serial-only output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpConfig
{
    /// HTTP server port.
    pub port:          u16,
    /// Whether the HTTP server should use TLS when supported by the platform.
    pub tls:           bool,
    /// WiFi SSID used to connect before serving HTTP.
    pub wifi_ssid:     String<MAX_WIFI_SSID_LEN>,
    /// WiFi password used to connect before serving HTTP.
    pub wifi_password: String<MAX_WIFI_PASSWORD_LEN>,
    /// Bearer tokens accepted by HTTP endpoints.
    pub tokens:        Vec<HttpAuthToken, MAX_HTTP_AUTH_TOKENS>,
}

impl HttpConfig
{
    /// Create HTTP configuration with WiFi credentials and no tokens.
    pub fn new(port: u16, tls: bool, wifi_ssid: &str, wifi_password: &str) -> Result<Self, Error>
    {
        Ok(Self {
            port,
            tls,
            wifi_ssid: fixed_string(wifi_ssid)?,
            wifi_password: fixed_string(wifi_password)?,
            tokens: Vec::new(),
        })
    }

    /// Add one HTTP bearer token.
    pub fn add_token(&mut self, token: &str) -> Result<(), Error>
    {
        self.tokens
            .push(fixed_string(token)?)
            .map_err(|_| Error::Capacity)
    }

    /// Return true when the HTTP config can start WiFi and HTTP serving.
    pub fn is_valid(&self) -> bool
    {
        self.port != 0
            && !self.wifi_ssid.is_empty()
            && self.tokens.iter().all(|token| !token.is_empty())
    }
}

/// Display configuration, including the explicit disabled state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplaySettings
{
    /// Display output is disabled.
    #[default]
    Off,
    /// Display output is enabled.
    Enabled
    {
        /// Telemetry value shown in each node row.
        node_display:       DisplayTelemetryValue,
        /// Display brightness as a percentage from 0 to 100.
        brightness_percent: u8,
    },
}

impl DisplaySettings
{
    /// Return the selected node telemetry value for display rows.
    pub const fn node_display(self) -> DisplayTelemetryValue
    {
        match self {
            Self::Off => DisplayTelemetryValue::StateOfCharge,
            Self::Enabled { node_display, .. } => node_display,
        }
    }

    /// Return the requested display brightness percentage.
    pub const fn brightness_percent(self) -> u8
    {
        match self {
            Self::Off => 0,
            Self::Enabled {
                brightness_percent, ..
            } => brightness_percent,
        }
    }

    /// Return true when the display settings are internally valid.
    pub const fn is_valid(self) -> bool
    {
        match self {
            Self::Off => true,
            Self::Enabled {
                brightness_percent, ..
            } => brightness_percent <= 100,
        }
    }
}

/// Producer telemetry value shown in local display node rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayTelemetryValue
{
    /// Show temperature in Celsius.
    TemperatureCelsius,
    /// Show relative humidity percentage.
    HumidityPercent,
    /// Show battery state of charge, preferring percentage and falling back to voltage.
    #[default]
    StateOfCharge,
    /// Show battery voltage.
    BatteryVoltage,
    /// Show pressure converted to hPa.
    PressureHpa,
    /// Show last received RSSI.
    Rssi,
    /// Show latest gateway poll latency.
    PollLatency,
}

impl DisplayTelemetryValue
{
    /// Return the stable config/wire label for this value.
    pub const fn as_str(self) -> &'static str
    {
        match self {
            Self::TemperatureCelsius => "temperature",
            Self::HumidityPercent => "humidity",
            Self::StateOfCharge => "soc",
            Self::BatteryVoltage => "battery_voltage",
            Self::PressureHpa => "pressure",
            Self::Rssi => "rssi",
            Self::PollLatency => "latency",
        }
    }
}

/// LoRa radio parameters supplied by gateway configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RadioParams
{
    /// LoRa center frequency in hertz.
    pub frequency_hz:     u32,
    /// LoRa bandwidth in hertz.
    pub bandwidth_hz:     u32,
    /// LoRa spreading factor.
    pub spreading_factor: u8,
    /// LoRa coding-rate denominator, for example `5` for 4/5.
    pub coding_rate:      u8,
    /// Requested TX power level before board-specific mapping.
    pub tx_power_level:   i8,
    /// LoRa sync word.
    pub sync_word:        u16,
    /// LoRa preamble length in symbols.
    pub preamble_len:     u16,
    /// Whether LoRa IQ should be inverted.
    pub iq_inverted:      bool,
    /// SX126x TX ramp time in microseconds.
    pub tx_ramp_time_us:  u16,
}

impl Default for RadioParams
{
    fn default() -> Self
    {
        Self {
            frequency_hz:     910_525_000,
            bandwidth_hz:     62_500,
            spreading_factor: 7,
            coding_rate:      5,
            tx_power_level:   14,
            sync_word:        0x1424,
            preamble_len:     16,
            iq_inverted:      false,
            tx_ramp_time_us:  200,
        }
    }
}

impl RadioParams
{
    /// Return true when all radio fields are supported by the SX126x bridge.
    pub const fn is_supported(self) -> bool
    {
        self.frequency_hz > 0
            && supported_bandwidth_hz(self.bandwidth_hz)
            && supported_spreading_factor(self.spreading_factor)
            && supported_coding_rate(self.coding_rate)
            && self.tx_power_level >= MIN_TX_POWER_LEVEL
            && self.tx_power_level <= MAX_TX_POWER_LEVEL
            && self.preamble_len > 0
            && supported_tx_ramp_time_us(self.tx_ramp_time_us)
    }
}

/// MeshCore routing parameters for the gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshcoreRoutingConfig
{
    /// MeshCore path hash mode: 0 is 1 byte, 1 is 2 bytes, 2 is 3 bytes.
    pub path_mode: u8,
}

impl MeshcoreRoutingConfig
{
    /// Return true when the routing parameters are supported.
    pub const fn is_valid(self) -> bool
    {
        self.path_mode <= MAX_MESHCORE_PATH_MODE
    }
}

impl Default for MeshcoreRoutingConfig
{
    fn default() -> Self
    {
        Self {
            path_mode: DEFAULT_MESHCORE_PATH_MODE,
        }
    }
}

/// MeshCore identity and routing material for the gateway itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MeshcoreConfig
{
    /// Gateway MeshCore public key, when provisioned.
    pub public_key:  Option<MeshcoreMaterial>,
    /// Gateway MeshCore private key or seed, when stored by this firmware.
    pub private_key: Option<MeshcoreMaterial>,
    /// MeshCore route diagnostics and path hash settings.
    pub routing:     MeshcoreRoutingConfig,
}

/// Route policy for polling one telemetry producer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TelemetryRoute
{
    /// Poll the producer directly.
    #[default]
    Direct,
    /// Legacy value accepted by older configs; firmware normalizes it to direct.
    Flood,
    /// Legacy value accepted by older configs; firmware normalizes it to direct.
    Path(TelemetryProducerPath),
}

/// MeshCore producer behavior used by active telemetry polling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TelemetryProducerKind
{
    /// Companion node; send the encrypted telemetry request directly.
    #[default]
    Companion,
    /// Repeater node; log in first, then send the telemetry request.
    Repeater,
}

impl TelemetryProducerKind
{
    /// Return the stable config/wire label for this producer kind.
    pub const fn as_str(self) -> &'static str
    {
        match self {
            Self::Companion => "companion",
            Self::Repeater => "repeater",
        }
    }
}

/// MeshCore-compatible telemetry producer polled by the gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryProducerConfig
{
    /// Producer MeshCore public key.
    pub public_key:            MeshcoreMaterial,
    /// MeshCore producer behavior used by active polling.
    pub kind:                  TelemetryProducerKind,
    /// Optional MeshCore repeater password for authenticated polling.
    pub password:              Option<MeshcoreMaterial>,
    /// Optional producer name; empty means it can be discovered later.
    pub name:                  TelemetryProducerName,
    /// Whether this producer should be polled.
    pub enabled:               bool,
    /// Optional producer polling interval override.
    pub polling_interval_secs: Option<u32>,
    /// Route policy for this producer.
    pub route:                 TelemetryRoute,
}

impl TelemetryProducerConfig
{
    /// Create a producer config with an optional display name.
    pub fn new(public_key: &str, name: &str) -> Result<Self, Error>
    {
        Ok(Self {
            public_key:            fixed_string(public_key)?,
            kind:                  TelemetryProducerKind::Companion,
            password:              None,
            name:                  fixed_trimmed_string(name)?,
            enabled:               true,
            polling_interval_secs: None,
            route:                 TelemetryRoute::Direct,
        })
    }

    /// Return the stable local identifier derived from the public key.
    pub fn id(&self) -> TelemetryProducerId
    {
        TelemetryProducerId::from_public_key(self.public_key.as_str())
    }

    /// Return the effective polling interval after applying global policy.
    pub const fn effective_polling_interval_ms(&self, polling: PollingConfig) -> u64
    {
        let requested = match self.polling_interval_secs {
            Some(interval_secs) => interval_secs,
            None => polling.default_interval_secs,
        };
        polling.clamp_interval_secs(requested) as u64 * 1_000
    }
}

/// Complete gateway configuration after provisioning and validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayConfig<const N: usize = MAX_TELEMETRY_PRODUCERS>
{
    /// Gateway display name.
    pub name:      GatewayName,
    /// HTTP service config; `None` means serial-only output.
    pub http:      Option<HttpConfig>,
    /// Display settings or explicit off state.
    pub display:   DisplaySettings,
    /// Configured telemetry producers.
    pub producers: Vec<TelemetryProducerConfig, N>,
    /// Polling scheduler policy.
    pub polling:   PollingConfig,
    /// LoRa radio settings.
    pub radio:     RadioParams,
    /// MeshCore gateway identity and routing settings.
    pub meshcore:  MeshcoreConfig,
}

impl<const N: usize> GatewayConfig<N>
{
    /// Create a gateway config with the required display name.
    pub fn new(name: &str) -> Result<Self, Error>
    {
        Ok(Self {
            name:      fixed_trimmed_string(name)?,
            http:      None,
            display:   DisplaySettings::Off,
            producers: Vec::new(),
            polling:   PollingConfig::default(),
            radio:     RadioParams::default(),
            meshcore:  MeshcoreConfig::default(),
        })
    }

    /// Add a telemetry producer.
    pub fn add_producer(&mut self, producer: TelemetryProducerConfig) -> Result<(), Error>
    {
        self.producers.push(producer).map_err(|_| Error::Capacity)
    }

    /// Find one telemetry producer by local ID.
    pub fn producer(&self, producer_id: TelemetryProducerId) -> Option<&TelemetryProducerConfig>
    {
        self.producers
            .iter()
            .find(|producer| producer.id() == producer_id)
    }

    /// Iterate over enabled telemetry producers.
    pub fn enabled_producers(&self) -> impl Iterator<Item = &TelemetryProducerConfig>
    {
        self.producers.iter().filter(|producer| producer.enabled)
    }

    /// Validate this configuration before starting gateway services.
    pub fn validate(&self) -> Result<(), Error>
    {
        if self.name.is_empty()
            || !self.polling.is_valid()
            || !self.display.is_valid()
            || !self.radio.is_supported()
            || !self.meshcore.routing.is_valid()
        {
            return Err(Error::InvalidConfig);
        }

        match self.meshcore.public_key.as_ref() {
            Some(public_key) if !public_key.is_empty() => {},
            _ => return Err(Error::InvalidConfig),
        }

        if let Some(http) = &self.http
            && !http.is_valid()
        {
            return Err(Error::InvalidConfig);
        }

        for (index, producer) in self.producers.iter().enumerate() {
            if producer.public_key.is_empty() {
                return Err(Error::InvalidConfig);
            }

            if let Some(password) = producer.password.as_ref()
                && password.is_empty()
            {
                return Err(Error::InvalidConfig);
            }

            if producer.kind == TelemetryProducerKind::Companion && producer.password.is_some() {
                return Err(Error::InvalidConfig);
            }

            if let Some(interval_secs) = producer.polling_interval_secs
                && (interval_secs < self.polling.min_interval_secs
                    || interval_secs > self.polling.max_interval_secs)
            {
                return Err(Error::InvalidConfig);
            }

            if self
                .producers
                .iter()
                .skip(index + 1)
                .any(|other| other.public_key.as_str() == producer.public_key.as_str())
            {
                return Err(Error::InvalidConfig);
            }
        }

        Ok(())
    }
}

impl<const N: usize> Default for GatewayConfig<N>
{
    fn default() -> Self
    {
        Self {
            name:      String::new(),
            http:      None,
            display:   DisplaySettings::Off,
            producers: Vec::new(),
            polling:   PollingConfig::default(),
            radio:     RadioParams::default(),
            meshcore:  MeshcoreConfig::default(),
        }
    }
}

/// Copy a string into a fixed-capacity heapless string.
pub fn fixed_string<const N: usize>(value: &str) -> Result<String<N>, Error>
{
    let mut out = String::new();
    out.push_str(value).map_err(|_| Error::InvalidConfig)?;
    Ok(out)
}

/// Copy a human-facing display string after trimming surrounding whitespace.
pub fn fixed_trimmed_string<const N: usize>(value: &str) -> Result<String<N>, Error>
{
    fixed_string(value.trim())
}

const fn supported_bandwidth_hz(value: u32) -> bool
{
    matches!(
        value,
        7_810 | 10_420 | 15_630 | 20_830 | 31_250 | 41_670 | 62_500 | 125_000 | 250_000 | 500_000
    )
}

const fn supported_spreading_factor(value: u8) -> bool
{
    matches!(value, 5..=12)
}

const fn supported_coding_rate(value: u8) -> bool
{
    matches!(value, 5..=8)
}

const fn supported_tx_ramp_time_us(value: u16) -> bool
{
    matches!(value, 10 | 20 | 40 | 80 | 200 | 800 | 1700 | 3400)
}

#[cfg(test)]
mod tests
{
    use super::{GatewayConfig, TelemetryProducerConfig, TelemetryProducerKind};

    #[test]
    fn default_gateway_config_requires_name_and_public_key()
    {
        assert!(GatewayConfig::<1>::default().validate().is_err());
    }

    #[test]
    fn validates_supported_config()
    {
        let mut config = supported_config();
        config
            .add_producer(TelemetryProducerConfig::new("producer-public-key", "roof").unwrap())
            .unwrap();

        assert!(config.validate().is_ok());
    }

    #[test]
    fn defaults_producer_kind_to_companion()
    {
        let producer = TelemetryProducerConfig::new("producer-public-key", "roof").unwrap();

        assert_eq!(producer.kind, TelemetryProducerKind::Companion);
    }

    #[test]
    fn trims_configured_display_names()
    {
        let config = GatewayConfig::<1>::new(" gate ").unwrap();
        let producer = TelemetryProducerConfig::new("producer-public-key", " roof ").unwrap();

        assert_eq!(config.name.as_str(), "gate");
        assert_eq!(producer.name.as_str(), "roof");
    }

    #[test]
    fn rejects_unsupported_radio_bandwidth()
    {
        let mut config = supported_config();
        config.radio.bandwidth_hz = 123_456;

        assert_eq!(config.validate(), Err(crate::Error::InvalidConfig));
    }

    #[test]
    fn accepts_http_without_tokens()
    {
        let mut config = supported_config();
        config.http = Some(super::HttpConfig::new(80, false, "ssid", "password").unwrap());

        assert!(config.validate().is_ok());
    }

    #[test]
    fn rejects_tx_power_outside_driver_range()
    {
        let mut config = supported_config();
        config.radio.tx_power_level = super::MAX_TX_POWER_LEVEL + 1;

        assert_eq!(config.validate(), Err(crate::Error::InvalidConfig));
    }

    #[test]
    fn rejects_excessive_retry_count()
    {
        let mut config = supported_config();
        config.polling.retry_count = super::MAX_RETRY_COUNT + 1;

        assert_eq!(config.validate(), Err(crate::Error::InvalidConfig));
    }

    #[test]
    fn rejects_producer_interval_outside_policy()
    {
        let mut config = supported_config();
        let mut producer = TelemetryProducerConfig::new("producer-public-key", "").unwrap();
        producer.polling_interval_secs = Some(config.polling.min_interval_secs - 1);
        config.add_producer(producer).unwrap();

        assert_eq!(config.validate(), Err(crate::Error::InvalidConfig));
    }

    #[test]
    fn rejects_duplicate_producer_public_keys()
    {
        let mut config = GatewayConfig::<2>::new("gate").unwrap();
        config.meshcore.public_key = Some(super::fixed_string("gateway-public-key").unwrap());
        config
            .add_producer(TelemetryProducerConfig::new("producer-public-key", "roof").unwrap())
            .unwrap();
        config
            .add_producer(TelemetryProducerConfig::new("producer-public-key", "yard").unwrap())
            .unwrap();

        assert_eq!(config.validate(), Err(crate::Error::InvalidConfig));
    }

    #[test]
    fn rejects_empty_producer_password()
    {
        let mut config = supported_config();
        let mut producer = TelemetryProducerConfig::new("producer-public-key", "roof").unwrap();
        producer.kind = TelemetryProducerKind::Repeater;
        producer.password = Some(super::fixed_string("").unwrap());
        config.add_producer(producer).unwrap();

        assert_eq!(config.validate(), Err(crate::Error::InvalidConfig));
    }

    #[test]
    fn rejects_companion_password()
    {
        let mut config = supported_config();
        let mut producer = TelemetryProducerConfig::new("producer-public-key", "roof").unwrap();
        producer.password = Some(super::fixed_string("password").unwrap());
        config.add_producer(producer).unwrap();

        assert_eq!(config.validate(), Err(crate::Error::InvalidConfig));
    }

    #[test]
    fn derives_producer_id_from_public_key()
    {
        let a = TelemetryProducerConfig::new("producer-public-key-a", "").unwrap();
        let b = TelemetryProducerConfig::new("producer-public-key-b", "").unwrap();

        assert_eq!(a.id(), a.id());
        assert_ne!(a.id(), b.id());
    }

    fn supported_config() -> GatewayConfig<1>
    {
        let mut config = GatewayConfig::<1>::new("gate").unwrap();
        config.meshcore.public_key = Some(super::fixed_string("gateway-public-key").unwrap());
        config
    }
}
