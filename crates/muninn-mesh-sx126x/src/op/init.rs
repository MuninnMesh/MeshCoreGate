//! Initialization and standby command parameters.

/// Standby clock source used by `SetStandby`.
#[repr(u8)]
#[derive(Copy, Clone)]
pub enum StandbyConfig
{
    /// Standby using the internal RC oscillator.
    StbyRc   = 0x00,
    /// Standby using the external crystal oscillator.
    StbyXOSC = 0x01,
}
