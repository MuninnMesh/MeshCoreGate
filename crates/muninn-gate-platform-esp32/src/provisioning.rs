//! USB provisioning command parsing and validation.

use alloc::boxed::Box;
use alloc::string::String as AllocString;
use alloc::vec::Vec as AllocVec;

use muninn_gate_core::config::{
    DEFAULT_DISPLAY_BRIGHTNESS_PERCENT,
    DEFAULT_HTTP_PORT,
    DEFAULT_MESHCORE_PATH_MODE,
    MAX_TELEMETRY_PRODUCERS,
    fixed_string,
};
use muninn_gate_core::{
    DisplaySettings,
    DisplayTelemetryValue,
    GatewayConfig,
    HttpConfig,
    MeshcoreConfig,
    MeshcoreRoutingConfig,
    PollingConfig,
    RadioParams,
    TelemetryProducerConfig,
    TelemetryProducerKind,
    TelemetryRoute,
};
use serde::Deserialize;

/// Maximum bytes accepted for one USB provisioning JSON document.
///
/// ESP32 storage currently reserves one 4 KiB flash slot per config record
/// including a small header, so accepting more than 4 KiB over USB only wastes
/// RAM and stack space before storage rejects it.
pub const USB_CONFIG_JSON_BYTES: usize = 4096;

/// Parsed USB provisioning command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisioningCommand
{
    /// Replace the active gateway configuration.
    SetConfig(Box<GatewayConfig<MAX_TELEMETRY_PRODUCERS>>),
    /// Print the active provisioning instructions.
    Help,
    /// Print the current runtime status.
    Status,
    /// Request an immediate producer poll.
    PollNow,
    /// Reboot the device after a saved configuration change.
    Reboot,
}

/// Error returned while parsing or validating USB provisioning input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisioningError
{
    /// The uploaded document is not valid JSON or JSONC.
    Json,
    /// The command `type` field is missing or unsupported.
    Command,
    /// A required field is missing.
    MissingField,
    /// A field value is outside the supported range or capacity.
    InvalidConfig,
}

impl ProvisioningError
{
    /// Return a compact error label for display and serial responses.
    pub const fn as_str(self) -> &'static str
    {
        match self {
            Self::Json => "invalid json",
            Self::Command => "unsupported command",
            Self::MissingField => "missing required field",
            Self::InvalidConfig => "invalid config",
        }
    }
}

/// Parse one JSON or JSONC USB provisioning document.
pub fn parse_usb_document(input: &str) -> Result<ProvisioningCommand, ProvisioningError>
{
    let json = strip_jsonc_comments(input)?;

    if let Ok(config) = parse_config_document(json.as_str()) {
        return Ok(ProvisioningCommand::SetConfig(config));
    }

    let command: WireProvisioningCommand =
        serde_json::from_str(json.as_str()).map_err(|_| ProvisioningError::Json)?;

    match command.command_type.as_str() {
        "set_config" => {
            let config = command.config.ok_or(ProvisioningError::MissingField)?;
            parse_config_wire(config).map(ProvisioningCommand::SetConfig)
        },
        "get_help" | "help" => Ok(ProvisioningCommand::Help),
        "status" | "get_status" => Ok(ProvisioningCommand::Status),
        "poll_now" => Ok(ProvisioningCommand::PollNow),
        "reboot" => Ok(ProvisioningCommand::Reboot),
        _ => Err(ProvisioningError::Command),
    }
}

fn parse_config_document(
    input: &str,
) -> Result<Box<GatewayConfig<MAX_TELEMETRY_PRODUCERS>>, ProvisioningError>
{
    let wire: Box<WireGatewayConfig> =
        serde_json::from_str(input).map_err(|_| ProvisioningError::Json)?;
    parse_config_wire(wire)
}

fn parse_config_wire(
    wire: Box<WireGatewayConfig>,
) -> Result<Box<GatewayConfig<MAX_TELEMETRY_PRODUCERS>>, ProvisioningError>
{
    let config = wire.into_config()?;
    config
        .validate()
        .map_err(|_| ProvisioningError::InvalidConfig)?;
    Ok(config)
}

fn strip_jsonc_comments(input: &str) -> Result<AllocString, ProvisioningError>
{
    let mut out = AllocString::new();
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    let mut in_line_comment = false;

    while let Some(ch) = chars.next() {
        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
                out.push('\n');
            }
            continue;
        }

        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            out.push(ch);
            continue;
        }

        if ch == '/' && chars.peek() == Some(&'/') {
            let _ = chars.next();
            in_line_comment = true;
            continue;
        }

        out.push(ch);
    }

    if in_string || escaped {
        return Err(ProvisioningError::Json);
    }

    Ok(out)
}

#[derive(Debug, Deserialize)]
struct WireProvisioningCommand
{
    #[serde(rename = "type")]
    command_type: AllocString,
    config:       Option<Box<WireGatewayConfig>>,
}

#[derive(Debug, Deserialize)]
struct WireGatewayConfig
{
    name:      AllocString,
    http:      Option<WireHttpConfig>,
    display:   Option<WireDisplayConfig>,
    polling:   Option<WirePollingConfig>,
    radio:     Option<WireRadioConfig>,
    meshcore:  WireMeshcoreConfig,
    producers: Option<AllocVec<WireProducerConfig>>,
}

impl WireGatewayConfig
{
    fn into_config(
        self: Box<Self>,
    ) -> Result<Box<GatewayConfig<MAX_TELEMETRY_PRODUCERS>>, ProvisioningError>
    {
        let wire = *self;
        let mut config = Box::<GatewayConfig<MAX_TELEMETRY_PRODUCERS>>::default();
        config.name =
            fixed_string(wire.name.as_str()).map_err(|_| ProvisioningError::InvalidConfig)?;
        config.http = wire.http.map(WireHttpConfig::into_config).transpose()?;
        config.display = wire
            .display
            .map(WireDisplayConfig::into_config)
            .transpose()?
            .unwrap_or(DisplaySettings::Off);
        config.polling = wire
            .polling
            .map(WirePollingConfig::into_config)
            .unwrap_or_default();
        config.radio = wire
            .radio
            .map(WireRadioConfig::into_config)
            .unwrap_or_default();
        config.meshcore = wire.meshcore.into_config()?;

        if let Some(producers) = wire.producers {
            for producer in producers {
                config
                    .add_producer(producer.into_config()?)
                    .map_err(|_| ProvisioningError::InvalidConfig)?;
            }
        }

        Ok(config)
    }
}

#[derive(Debug, Deserialize)]
struct WireHttpConfig
{
    port:          Option<u16>,
    tls:           Option<bool>,
    wifi_ssid:     AllocString,
    wifi_password: Option<AllocString>,
    tokens:        Option<AllocVec<AllocString>>,
}

impl WireHttpConfig
{
    fn into_config(self) -> Result<HttpConfig, ProvisioningError>
    {
        let mut http = HttpConfig::new(
            self.port.unwrap_or(DEFAULT_HTTP_PORT),
            self.tls.unwrap_or(false),
            self.wifi_ssid.as_str(),
            self.wifi_password.as_deref().unwrap_or(""),
        )
        .map_err(|_| ProvisioningError::InvalidConfig)?;

        if let Some(tokens) = self.tokens {
            for token in tokens {
                http.add_token(token.as_str())
                    .map_err(|_| ProvisioningError::InvalidConfig)?;
            }
        }

        Ok(http)
    }
}

#[derive(Debug, Deserialize)]
struct WireDisplayConfig
{
    enabled:            bool,
    node_display:       Option<AllocString>,
    brightness_percent: Option<u8>,
}

impl WireDisplayConfig
{
    fn into_config(self) -> Result<DisplaySettings, ProvisioningError>
    {
        if !self.enabled {
            return Ok(DisplaySettings::Off);
        }

        let node_display = self
            .node_display
            .as_deref()
            .map(parse_display_value)
            .transpose()?
            .unwrap_or_default();
        let brightness_percent = self
            .brightness_percent
            .unwrap_or(DEFAULT_DISPLAY_BRIGHTNESS_PERCENT);
        if brightness_percent > 100 {
            return Err(ProvisioningError::InvalidConfig);
        }
        Ok(DisplaySettings::Enabled {
            node_display,
            brightness_percent,
        })
    }
}

#[derive(Debug, Deserialize)]
struct WirePollingConfig
{
    default_interval_secs: Option<u32>,
    jitter_secs:           Option<u32>,
}

impl WirePollingConfig
{
    fn into_config(self) -> PollingConfig
    {
        let mut polling = PollingConfig::default();
        if let Some(default_interval_secs) = self.default_interval_secs {
            polling.default_interval_secs = default_interval_secs;
        }
        if let Some(jitter_secs) = self.jitter_secs {
            polling.jitter_secs = jitter_secs;
        }
        polling
    }
}

#[derive(Debug, Deserialize)]
struct WireRadioConfig
{
    frequency_hz:     Option<u32>,
    bandwidth_hz:     Option<u32>,
    spreading_factor: Option<u8>,
    coding_rate:      Option<u8>,
    tx_power_level:   Option<i8>,
    sync_word:        Option<u16>,
    preamble_len:     Option<u16>,
    iq_inverted:      Option<bool>,
    tx_ramp_time_us:  Option<u16>,
}

impl WireRadioConfig
{
    fn into_config(self) -> RadioParams
    {
        let mut radio = RadioParams::default();
        if let Some(frequency_hz) = self.frequency_hz {
            radio.frequency_hz = frequency_hz;
        }
        if let Some(bandwidth_hz) = self.bandwidth_hz {
            radio.bandwidth_hz = bandwidth_hz;
        }
        if let Some(spreading_factor) = self.spreading_factor {
            radio.spreading_factor = spreading_factor;
        }
        if let Some(coding_rate) = self.coding_rate {
            radio.coding_rate = coding_rate;
        }
        if let Some(tx_power_level) = self.tx_power_level {
            radio.tx_power_level = tx_power_level;
        }
        if let Some(sync_word) = self.sync_word {
            radio.sync_word = sync_word;
        }
        if let Some(preamble_len) = self.preamble_len {
            radio.preamble_len = preamble_len;
        }
        if let Some(iq_inverted) = self.iq_inverted {
            radio.iq_inverted = iq_inverted;
        }
        if let Some(tx_ramp_time_us) = self.tx_ramp_time_us {
            radio.tx_ramp_time_us = tx_ramp_time_us;
        }
        radio
    }
}

#[derive(Debug, Deserialize)]
struct WireMeshcoreConfig
{
    public_key:  AllocString,
    private_key: Option<AllocString>,
    routing:     Option<WireMeshcoreRoutingConfig>,
}

impl WireMeshcoreConfig
{
    fn into_config(self) -> Result<MeshcoreConfig, ProvisioningError>
    {
        Ok(MeshcoreConfig {
            public_key:  Some(
                fixed_string(self.public_key.as_str())
                    .map_err(|_| ProvisioningError::InvalidConfig)?,
            ),
            private_key: self
                .private_key
                .map(|key| fixed_string(key.as_str()))
                .transpose()
                .map_err(|_| ProvisioningError::InvalidConfig)?,
            routing:     self
                .routing
                .map(WireMeshcoreRoutingConfig::into_config)
                .unwrap_or_default(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct WireMeshcoreRoutingConfig
{
    path_mode: Option<u8>,
}

impl WireMeshcoreRoutingConfig
{
    fn into_config(self) -> MeshcoreRoutingConfig
    {
        MeshcoreRoutingConfig {
            path_mode: self.path_mode.unwrap_or(DEFAULT_MESHCORE_PATH_MODE),
        }
    }
}

#[derive(Debug, Deserialize)]
struct WireProducerConfig
{
    public_key:            AllocString,
    kind:                  Option<AllocString>,
    password:              Option<AllocString>,
    name:                  Option<AllocString>,
    enabled:               Option<bool>,
    polling_interval_secs: Option<u32>,
    route:                 Option<AllocString>,
}

impl WireProducerConfig
{
    fn into_config(self) -> Result<TelemetryProducerConfig, ProvisioningError>
    {
        let mut producer = TelemetryProducerConfig::new(
            self.public_key.as_str(),
            self.name.as_deref().unwrap_or(""),
        )
        .map_err(|_| ProvisioningError::InvalidConfig)?;
        producer.kind = self
            .kind
            .as_deref()
            .map(parse_producer_kind)
            .transpose()?
            .unwrap_or_default();
        producer.password = self
            .password
            .map(|password| fixed_string(password.as_str()))
            .transpose()
            .map_err(|_| ProvisioningError::InvalidConfig)?;
        producer.enabled = self.enabled.unwrap_or(true);
        producer.polling_interval_secs = self.polling_interval_secs;
        producer.route = self
            .route
            .as_deref()
            .map(parse_route)
            .transpose()?
            .unwrap_or_default();
        Ok(producer)
    }
}

fn parse_producer_kind(value: &str) -> Result<TelemetryProducerKind, ProvisioningError>
{
    match value {
        "companion" => Ok(TelemetryProducerKind::Companion),
        "repeater" => Ok(TelemetryProducerKind::Repeater),
        _ => Err(ProvisioningError::InvalidConfig),
    }
}

fn parse_display_value(value: &str) -> Result<DisplayTelemetryValue, ProvisioningError>
{
    match value {
        "temperature" => Ok(DisplayTelemetryValue::TemperatureCelsius),
        "humidity" => Ok(DisplayTelemetryValue::HumidityPercent),
        "soc" => Ok(DisplayTelemetryValue::StateOfCharge),
        "battery_voltage" => Ok(DisplayTelemetryValue::BatteryVoltage),
        "pressure" => Ok(DisplayTelemetryValue::PressureHpa),
        "rssi" => Ok(DisplayTelemetryValue::Rssi),
        "latency" => Ok(DisplayTelemetryValue::PollLatency),
        _ => Err(ProvisioningError::InvalidConfig),
    }
}

fn parse_route(value: &str) -> Result<TelemetryRoute, ProvisioningError>
{
    match value {
        "direct" => Ok(TelemetryRoute::Direct),
        "flood" => Ok(TelemetryRoute::Flood),
        "" => Err(ProvisioningError::InvalidConfig),
        path => Ok(TelemetryRoute::Path(
            fixed_string(path).map_err(|_| ProvisioningError::InvalidConfig)?,
        )),
    }
}

#[cfg(test)]
mod tests
{
    use muninn_gate_core::TelemetryProducerKind;

    use super::{ProvisioningCommand, parse_usb_document};

    #[test]
    fn parses_raw_config_with_comments()
    {
        let command = parse_usb_document(
            r#"{
              "name": "MC Telemetry Gate V4", // comment
              "display": {"enabled": true, "node_display": "temperature", "brightness_percent": 60},
              "polling": {"default_interval_secs": 600, "jitter_secs": 10},
              "radio": {"frequency_hz": 910525000, "bandwidth_hz": 62500},
              "meshcore": {"public_key": "gateway-public-key", "routing": {"path_mode": 2}},
              "producers": [{"public_key": "producer-public-key", "kind": "companion", "route": "direct"}]
            }"#,
        )
        .unwrap();

        match command {
            ProvisioningCommand::SetConfig(config) => {
                assert_eq!(config.name.as_str(), "MC Telemetry Gate V4");
                assert_eq!(config.producers.len(), 1);
                assert_eq!(config.producers[0].kind, TelemetryProducerKind::Companion);
            },
            _ => panic!("unexpected command"),
        }
    }

    #[test]
    fn parses_set_config_command()
    {
        let command = parse_usb_document(
            r#"{"type":"set_config","config":{
              "name":"gate",
              "meshcore":{"public_key":"gateway-public-key"},
              "producers":[{"public_key":"producer-public-key","kind":"repeater","password":"admin"}]
            }}"#,
        )
        .unwrap();

        match command {
            ProvisioningCommand::SetConfig(config) => {
                assert_eq!(config.producers[0].kind, TelemetryProducerKind::Repeater);
            },
            _ => panic!("unexpected command"),
        }
    }

    #[test]
    fn rejects_companion_password()
    {
        let result = parse_usb_document(
            r#"{"name":"gate",
              "meshcore":{"public_key":"gateway-public-key"},
              "producers":[{"public_key":"producer-public-key","kind":"companion","password":"admin"}]
            }"#,
        );

        assert!(result.is_err());
    }
}
