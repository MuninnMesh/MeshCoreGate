//! RX and TX command parameter types.

/// Timeout value encoded for `SetRx` and `SetTx`.
#[derive(Copy, Clone)]
pub struct RxTxTimeout
{
    /// Encoded timeout bytes.
    inner: [u8; 3],
}

impl From<RxTxTimeout> for [u8; 3]
{
    fn from(val: RxTxTimeout) -> Self
    {
        val.inner
    }
}

impl RxTxTimeout
{
    /// Encode a timeout in milliseconds.
    pub const fn from_ms(ms: u32) -> Self
    {
        let inner = ms << 6;
        let inner = inner.to_le_bytes();
        let inner = [inner[2], inner[1], inner[0]];
        Self { inner }
    }

    /// Encode continuous receive mode.
    pub const fn continuous_rx() -> Self
    {
        Self {
            inner: [0xFF, 0xFF, 0xFF],
        }
    }
}

impl From<u32> for RxTxTimeout
{
    fn from(val: u32) -> Self
    {
        let bytes = val.to_be_bytes();
        Self {
            inner: [bytes[1], bytes[2], bytes[3]],
        }
    }
}

/// TX power ramp time.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RampTime
{
    /// 10 microseconds.
    Ramp10u   = 0x00,
    /// 20 microseconds.
    Ramp20u   = 0x01,
    /// 40 microseconds.
    Ramp40u   = 0x02,
    /// 80 microseconds.
    Ramp80u   = 0x03,
    /// 200 microseconds.
    Ramp200u  = 0x04,
    /// 800 microseconds.
    Ramp800u  = 0x05,
    /// 1700 microseconds.
    Ramp1700u = 0x06,
    /// 3400 microseconds.
    Ramp3400u = 0x07,
}

/// Parameters passed to `SetTxParams`.
#[derive(Copy, Clone, Debug)]
pub struct TxParams
{
    /// Output power in dBm.
    power_dbm: i8,
    /// Power ramp time.
    ramp_time: RampTime,
}

impl Default for TxParams
{
    fn default() -> Self
    {
        Self {
            power_dbm: 0,
            ramp_time: RampTime::Ramp200u,
        }
    }
}

impl From<TxParams> for [u8; 2]
{
    fn from(val: TxParams) -> Self
    {
        [val.power_dbm as u8, val.ramp_time as u8]
    }
}

impl TxParams
{
    /// Set output power in dBm.
    pub fn set_power_dbm(mut self, power_dbm: i8) -> Self
    {
        debug_assert!(power_dbm >= -17);
        debug_assert!(power_dbm <= 22);
        self.power_dbm = power_dbm;
        self
    }

    /// Set power ramp time.
    pub fn set_ramp_time(mut self, ramp_time: RampTime) -> Self
    {
        self.ramp_time = ramp_time;
        self
    }
}

/// Power amplifier device selection.
#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum DeviceSel
{
    /// High-power SX1262 PA path.
    SX1262 = 0x00,
    /// Low-power SX1261 PA path.
    SX1261 = 0x01,
}

/// Parameters passed to `SetPaConfig`.
#[derive(Copy, Clone, Debug)]
pub struct PaConfig
{
    /// PA duty-cycle selector.
    pa_duty_cycle: u8,
    /// High-power PA maximum output selector.
    hp_max:        u8,
    /// SX1261/SX1262 PA device selector.
    device_sel:    DeviceSel,
}

impl From<PaConfig> for [u8; 4]
{
    fn from(val: PaConfig) -> Self
    {
        [val.pa_duty_cycle, val.hp_max, val.device_sel as u8, 0x01]
    }
}

impl Default for PaConfig
{
    fn default() -> Self
    {
        Self {
            pa_duty_cycle: 0x00,
            hp_max:        0x00,
            device_sel:    DeviceSel::SX1262,
        }
    }
}

impl PaConfig
{
    /// Set PA duty cycle.
    pub fn set_pa_duty_cycle(mut self, pa_duty_cycle: u8) -> Self
    {
        self.pa_duty_cycle = pa_duty_cycle;
        self
    }

    /// Set high-power PA maximum output selector.
    pub fn set_hp_max(mut self, hp_max: u8) -> Self
    {
        self.hp_max = hp_max;
        self
    }

    /// Set SX1261/SX1262 PA device selection.
    pub fn set_device_sel(mut self, device_sel: DeviceSel) -> Self
    {
        self.device_sel = device_sel;
        self
    }
}

/// RX buffer status returned by `GetRxBufferStatus`.
#[derive(Debug, Copy, Clone)]
pub struct RxBufferStatus
{
    /// Number of received bytes.
    payload_length_rx:       u8,
    /// RX buffer start pointer.
    rx_start_buffer_pointer: u8,
}

impl From<[u8; 2]> for RxBufferStatus
{
    fn from(raw: [u8; 2]) -> Self
    {
        Self {
            payload_length_rx:       raw[0],
            rx_start_buffer_pointer: raw[1],
        }
    }
}

impl RxBufferStatus
{
    /// Return the number of received payload bytes.
    pub fn payload_length_rx(&self) -> u8
    {
        self.payload_length_rx
    }

    /// Return the RX buffer start pointer.
    pub fn rx_start_buffer_pointer(&self) -> u8
    {
        self.rx_start_buffer_pointer
    }
}
