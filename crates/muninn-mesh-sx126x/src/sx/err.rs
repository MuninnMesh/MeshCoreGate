//! Driver error types.

use core::fmt::{self, Debug};

/// SPI-side errors returned by the driver.
pub enum SpiError<TSPIERR>
{
    /// SPI write operation failed.
    Write(TSPIERR),
    /// SPI transfer operation failed.
    Transfer(TSPIERR),
    /// BUSY pin did not clear before the local timeout expired.
    BusyTimeout,
}

impl<TSPIERR: Debug> Debug for SpiError<TSPIERR>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self {
            Self::Write(err) => write!(f, "Write({:?})", err),
            Self::Transfer(err) => write!(f, "Transfer({:?})", err),
            Self::BusyTimeout => write!(f, "BusyTimeout"),
        }
    }
}

/// GPIO-side errors returned by the driver.
pub enum PinError<TPINERR>
{
    /// Output pin operation failed.
    Output(TPINERR),
    /// Input pin operation failed.
    Input(TPINERR),
    /// DIO1 did not assert before the local timeout expired.
    Dio1Timeout,
}

impl<TPINERR: Debug> Debug for PinError<TPINERR>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self {
            Self::Output(err) => write!(f, "Output({:?})", err),
            Self::Input(err) => write!(f, "Input({:?})", err),
            Self::Dio1Timeout => f.write_str("Dio1Timeout"),
        }
    }
}

/// Combined SX126x driver error.
pub enum SxError<TSPIERR, TPINERR>
{
    /// SPI operation failed.
    Spi(SpiError<TSPIERR>),
    /// GPIO operation failed.
    Pin(PinError<TPINERR>),
    /// A caller provided a payload too large for an SX126x packet length field.
    InvalidPayloadLength,
}

impl<TSPIERR: Debug, TPINERR: Debug> Debug for SxError<TSPIERR, TPINERR>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self {
            Self::Spi(err) => write!(f, "Spi({:?})", err),
            Self::Pin(err) => write!(f, "Pin({:?})", err),
            Self::InvalidPayloadLength => f.write_str("InvalidPayloadLength"),
        }
    }
}

impl<TSPIERR, TPINERR> From<SpiError<TSPIERR>> for SxError<TSPIERR, TPINERR>
{
    fn from(spi_err: SpiError<TSPIERR>) -> Self
    {
        SxError::Spi(spi_err)
    }
}

impl<TSPIERR, TPINERR> From<PinError<TPINERR>> for SxError<TSPIERR, TPINERR>
{
    fn from(spi_err: PinError<TPINERR>) -> Self
    {
        SxError::Pin(spi_err)
    }
}
