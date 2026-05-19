//! TCXO control command parameters.

/// DIO3 voltage level used to power or control a TCXO.
#[repr(u8)]
#[derive(Copy, Clone)]
pub enum TcxoVoltage
{
    /// 1.6 V TCXO control voltage.
    Volt1_6 = 0x00,
    /// 1.7 V TCXO control voltage.
    Volt1_7 = 0x01,
    /// 1.8 V TCXO control voltage.
    Volt1_8 = 0x02,
    /// 2.2 V TCXO control voltage.
    Volt2_2 = 0x03,
    /// 2.4 V TCXO control voltage.
    Volt2_4 = 0x04,
    /// 2.7 V TCXO control voltage.
    Volt2_7 = 0x05,
    /// 3.0 V TCXO control voltage.
    Volt3_0 = 0x06,
    /// 3.3 V TCXO control voltage.
    Volt3_3 = 0x07,
}

/// TCXO startup delay encoded for `SetDio3AsTcxoCtrl`.
#[derive(Copy, Clone)]
pub struct TcxoDelay
{
    /// Encoded TCXO delay bytes.
    inner: [u8; 3],
}

impl From<TcxoDelay> for [u8; 3]
{
    fn from(val: TcxoDelay) -> Self
    {
        val.inner
    }
}

impl From<[u8; 3]> for TcxoDelay
{
    fn from(b: [u8; 3]) -> Self
    {
        Self { inner: b }
    }
}

impl TcxoDelay
{
    /// Encode a TCXO startup delay in milliseconds.
    pub const fn from_ms(ms: u32) -> Self
    {
        let inner = ms << 6;
        let inner = inner.to_le_bytes();
        let inner = [inner[2], inner[1], inner[0]];
        Self { inner }
    }
}
