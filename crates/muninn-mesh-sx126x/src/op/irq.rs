//! IRQ mask and status types.

/// Individual IRQ mask bits used by `SetDioIrqParams`.
#[repr(u16)]
#[derive(Copy, Clone)]
pub enum IrqMaskBit
{
    /// No IRQ bits set.
    None             = 0x0000,
    /// TX completion IRQ.
    TxDone           = 1 << 0,
    /// RX completion IRQ.
    RxDone           = 1 << 1,
    /// Preamble-detected IRQ.
    PreambleDetected = 1 << 2,
    /// Sync-word-valid IRQ.
    SyncwordValid    = 1 << 3,
    /// Header-valid IRQ.
    HeaderValid      = 1 << 4,
    /// Header-error IRQ.
    HeaderError      = 1 << 5,
    /// CRC-error IRQ.
    CrcErr           = 1 << 6,
    /// Channel-activity-detection complete IRQ.
    CadDone          = 1 << 7,
    /// Channel activity detected IRQ.
    CadDetected      = 1 << 8,
    /// RX or TX timeout IRQ.
    Timeout          = 1 << 9,
    /// All IRQ bits set.
    All              = 0xFFFF,
}

/// IRQ bit mask used for global and DIO-specific IRQ mappings.
#[derive(Copy, Clone)]
pub struct IrqMask
{
    /// Raw IRQ mask bits.
    inner: u16,
}

impl IrqMask
{
    /// Create an empty IRQ mask.
    pub const fn none() -> Self
    {
        Self {
            inner: IrqMaskBit::None as u16,
        }
    }

    /// Create a mask with every IRQ bit set.
    pub const fn all() -> Self
    {
        Self {
            inner: IrqMaskBit::All as u16,
        }
    }

    /// Return a new mask with one additional IRQ bit enabled.
    pub const fn combine(self, bit: IrqMaskBit) -> Self
    {
        let inner = self.inner | bit as u16;
        Self { inner }
    }

    /// Return a new mask containing bits from both masks.
    pub const fn union(self, other: Self) -> Self
    {
        Self {
            inner: self.inner | other.inner,
        }
    }
}

impl From<IrqMask> for u16
{
    fn from(val: IrqMask) -> Self
    {
        val.inner
    }
}

impl From<u16> for IrqMask
{
    fn from(mask: u16) -> Self
    {
        Self { inner: mask }
    }
}

impl Default for IrqMask
{
    fn default() -> Self
    {
        Self::none()
    }
}

/// IRQ status bitfield returned by `GetIrqStatus`.
#[derive(Copy, Clone)]
pub struct IrqStatus
{
    /// Raw IRQ status bits.
    inner: u16,
}

impl From<u16> for IrqStatus
{
    fn from(status: u16) -> Self
    {
        Self { inner: status }
    }
}

impl core::fmt::Debug for IrqStatus
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        write!(
            f,
            "IrqStatus {{inner: {:#016b}, tx_done: {}, rx_done: {}, preamble_detected: {}, \
             syncword_valid: {}, header_valid: {}, header_error: {}, crc_err: {}, cad_done: {}, \
             cad_detected: {}, timeout : {}}}",
            self.inner,
            self.tx_done(),
            self.rx_done(),
            self.preamble_detected(),
            self.syncword_valid(),
            self.header_valid(),
            self.header_error(),
            self.crc_err(),
            self.cad_done(),
            self.cad_detected(),
            self.timeout(),
        )
    }
}

impl IrqStatus
{
    /// Raw IRQ-status bits as returned by `GetIrqStatus`. Exposed for
    /// boards that want to surface the bitmask in diagnostic logs.
    pub fn raw(self) -> u16
    {
        self.inner
    }

    /// Return true when TX completed.
    pub fn tx_done(self) -> bool
    {
        (self.inner & IrqMaskBit::TxDone as u16) > 0
    }

    /// Return true when RX completed.
    pub fn rx_done(self) -> bool
    {
        (self.inner & IrqMaskBit::RxDone as u16) > 0
    }

    /// Return true when a preamble was detected.
    pub fn preamble_detected(self) -> bool
    {
        (self.inner & IrqMaskBit::PreambleDetected as u16) > 0
    }

    /// Return true when a sync word was accepted.
    pub fn syncword_valid(self) -> bool
    {
        (self.inner & IrqMaskBit::SyncwordValid as u16) > 0
    }

    /// Return true when a header was accepted.
    pub fn header_valid(self) -> bool
    {
        (self.inner & IrqMaskBit::HeaderValid as u16) > 0
    }

    /// Return true when a header error occurred.
    pub fn header_error(self) -> bool
    {
        (self.inner & IrqMaskBit::HeaderError as u16) > 0
    }

    /// Return true when a CRC error occurred.
    pub fn crc_err(self) -> bool
    {
        (self.inner & IrqMaskBit::CrcErr as u16) > 0
    }

    /// Return true when channel-activity detection completed.
    pub fn cad_done(self) -> bool
    {
        (self.inner & IrqMaskBit::CadDone as u16) > 0
    }

    /// Return true when channel activity was detected.
    pub fn cad_detected(self) -> bool
    {
        (self.inner & IrqMaskBit::CadDetected as u16) > 0
    }

    /// Return true when RX or TX timed out.
    pub fn timeout(self) -> bool
    {
        (self.inner & IrqMaskBit::Timeout as u16) > 0
    }
}
