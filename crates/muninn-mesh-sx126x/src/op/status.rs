//! Status and statistics response types.

/// Raw status byte returned by SX126x commands.
#[derive(Copy, Clone)]
pub struct Status
{
    /// Raw command status byte.
    inner: u8,
}

impl core::fmt::Debug for Status
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        let chip_mode = self.chip_mode();
        let command_status = self.command_status();
        write!(
            f,
            "Status {{inner: {:#08b}, chip_mode: {:?}, command_status: {:?}}}",
            self.inner, chip_mode, command_status
        )
    }
}

/// Current chip operating mode.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ChipMode
{
    /// Standby using the RC oscillator.
    StbyRC   = 0x02,
    /// Standby using the external crystal oscillator.
    StbyXOSC = 0x03,
    /// Frequency-synthesis mode.
    FS       = 0x04,
    /// Receive mode.
    RX       = 0x05,
    /// Transmit mode.
    TX       = 0x06,
}

/// Command-processing status reported by the radio.
#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum CommandStatus
{
    /// Command completed and data is available.
    DataAvailable          = 0x02,
    /// Command timed out.
    CommandTimeout         = 0x03,
    /// Command processing failed.
    CommandProcessingError = 0x04,
    /// Command could not be executed.
    FailureToExecute       = 0x05,
    /// TX command completed.
    CommandTxDone          = 0x06,
}

impl From<u8> for Status
{
    fn from(b: u8) -> Self
    {
        Self { inner: b }
    }
}

impl Status
{
    /// Decode the current chip operating mode, if the status byte is valid.
    pub fn chip_mode(&self) -> Option<ChipMode>
    {
        use ChipMode::*;
        match (self.inner & 0x70) >> 4 {
            0x02 => Some(StbyRC),
            0x03 => Some(StbyXOSC),
            0x04 => Some(FS),
            0x05 => Some(RX),
            0x06 => Some(TX),
            _ => None,
        }
    }

    /// Decode the command status, if the status byte is valid.
    pub fn command_status(self) -> Option<CommandStatus>
    {
        use CommandStatus::*;
        match (self.inner & 0x0E) >> 1 {
            0x02 => Some(DataAvailable),
            0x03 => Some(CommandTimeout),
            0x04 => Some(CommandProcessingError),
            0x05 => Some(FailureToExecute),
            0x06 => Some(CommandTxDone),
            _ => None,
        }
    }
}

/// Packet statistics returned by `GetStats`.
#[derive(Copy, Clone, Debug)]
pub struct Stats
{
    /// Command status byte returned with the stats payload.
    pub status:       Status,
    /// Number of received packets.
    pub rx_pkt:       u16,
    /// Number of received packets with CRC errors.
    pub crc_error:    u16,
    /// Number of received packets with header errors.
    pub header_error: u16,
}

impl From<[u8; 7]> for Stats
{
    fn from(b: [u8; 7]) -> Self
    {
        Self {
            status:       b[0].into(),
            rx_pkt:       u16::from_be_bytes([b[1], b[2]]),
            crc_error:    u16::from_be_bytes([b[3], b[4]]),
            header_error: u16::from_be_bytes([b[5], b[6]]),
        }
    }
}

/// Last-packet status returned by `GetPacketStatus`.
#[derive(Copy, Clone, Debug)]
pub struct PacketStatus
{
    /// Raw packet RSSI byte.
    rssi_pkt:        u8,
    /// Raw packet SNR byte.
    snr_pkt:         i8,
    /// Raw signal RSSI byte.
    signal_rssi_pkt: u8,
}

impl From<[u8; 3]> for PacketStatus
{
    fn from(b: [u8; 3]) -> Self
    {
        Self {
            rssi_pkt:        b[0],
            snr_pkt:         i8::from_be_bytes([b[1]]),
            signal_rssi_pkt: b[2],
        }
    }
}

impl PacketStatus
{
    /// Return packet RSSI in dBm.
    pub fn rssi_pkt(&self) -> f32
    {
        self.rssi_pkt as f32 / -2.0
    }

    /// Return packet SNR in dB.
    pub fn snr_pkt(&self) -> f32
    {
        self.snr_pkt as f32 / 4.0
    }

    /// Return signal RSSI in dBm.
    pub fn signal_rssi_pkt(&self) -> f32
    {
        self.signal_rssi_pkt as f32 / -2.0
    }
}
