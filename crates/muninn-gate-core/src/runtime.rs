//! Runtime state reported by the gateway while it provisions and serves.

/// Network endpoint for an HTTP service exposed by the gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpEndpoint
{
    /// IPv4 address as four octets.
    pub ipv4: [u8; 4],
    /// TCP port number.
    pub port: u16,
}

impl HttpEndpoint
{
    /// Create an HTTP endpoint from an IPv4 address and port.
    pub const fn new(ipv4: [u8; 4], port: u16) -> Self
    {
        Self { ipv4, port }
    }
}

/// Interfaces currently serving telemetry or provisioning traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ServingInterfaces
{
    /// HTTP endpoint, when WiFi or another network interface is serving metrics.
    pub http:       Option<HttpEndpoint>,
    /// Whether USB or UART serial is active.
    pub usb_serial: bool,
}

impl ServingInterfaces
{
    /// Create a serving interface set from active runtime interfaces.
    pub const fn new(http: Option<HttpEndpoint>, usb_serial: bool) -> Self
    {
        Self { http, usb_serial }
    }

    /// Return true when at least one interface is serving.
    pub const fn any(self) -> bool
    {
        self.http.is_some() || self.usb_serial
    }
}

/// High-level runtime state for status displays, health endpoints, and logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayRuntimeState
{
    /// No valid persistent configuration is available.
    Unprovisioned,
    /// Valid configuration is loaded but external telemetry interfaces are not serving yet.
    Provisioned,
    /// The gateway is provisioned and serving through one or more interfaces.
    Serving
    {
        /// Active serving interfaces.
        interfaces: ServingInterfaces,
    },
    /// The gateway cannot continue normal operation.
    Error
    {
        /// Coarse error reason suitable for display.
        reason: GatewayRuntimeError,
    },
}

impl GatewayRuntimeState
{
    /// Return a compact state label for displays, logs, and health output.
    pub const fn label(self) -> &'static str
    {
        match self {
            Self::Unprovisioned => "not provisioned",
            Self::Provisioned => "provisioned",
            Self::Serving { .. } => "serving",
            Self::Error { .. } => "error",
        }
    }

    /// Return a concise next-step message for a display or serial status command.
    pub const fn next_step(self) -> &'static str
    {
        match self {
            Self::Unprovisioned => "upload config JSON over USB serial",
            Self::Provisioned => "starting telemetry services",
            Self::Serving { .. } => "ready",
            Self::Error { reason } => reason.next_step(),
        }
    }
}

/// Coarse runtime error reason suitable for status displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayRuntimeError
{
    /// Persistent config is missing, invalid, or unsupported.
    InvalidConfig,
    /// Persistent storage failed.
    Storage,
    /// Radio initialization or operation failed.
    Radio,
    /// MeshCore protocol operation failed.
    Meshcore,
    /// Network startup or serving failed.
    Network,
    /// Config requested an output that the selected board cannot provide.
    UnsupportedOutput,
    /// A board-specific task failed.
    Platform,
}

impl GatewayRuntimeError
{
    /// Return a concise next-step message for the error.
    pub const fn next_step(self) -> &'static str
    {
        match self {
            Self::InvalidConfig => "Fix configuration over USB serial",
            Self::Storage => "Check persistent storage and reprovision",
            Self::Radio => "Check radio wiring and configuration",
            Self::Meshcore => "Check MeshCore credentials and producer config",
            Self::Network => "Check network configuration",
            Self::UnsupportedOutput => "Select an output supported by this board",
            Self::Platform => "Check board platform initialization",
        }
    }
}
