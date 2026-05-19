//! SX126x register addresses.

/// Register addresses from the SX126x datasheet.
#[repr(u16)]
pub enum Register
{
    /// DIO output-enable register.
    DioxOutputEnable         = 0x0580,
    /// DIO input-enable register.
    DioxInputEnable          = 0x0583,
    /// DIO pull-up control register.
    DioxPullUpControl        = 0x0584,
    /// DIO pull-down control register.
    DioxPullDownControl      = 0x0585,
    /// FSK whitening LFSR initial value MSB.
    WhiteningInitialValueMsb = 0x06B8,
    /// FSK whitening LFSR initial value LSB.
    WhiteningInitialValueLsb = 0x06B9,
    /// FSK CRC initial value MSB.
    CrcMsbInitialValue       = 0x06BC,
    /// FSK CRC initial value LSB.
    CrcLsbInitialValue       = 0x06BD,
    /// FSK CRC polynomial MSB.
    CrcMsbPolynomialValue    = 0x06BE,
    /// FSK CRC polynomial LSB.
    CrcLsbPolynomialValue    = 0x06BF,
    /// FSK sync word byte 0.
    SyncWord0                = 0x06C0,
    /// FSK sync word byte 1.
    SyncWord1                = 0x06C1,
    /// FSK sync word byte 2.
    SyncWord2                = 0x06C2,
    /// FSK sync word byte 3.
    SyncWord3                = 0x06C3,
    /// FSK sync word byte 4.
    SyncWord4                = 0x06C4,
    /// FSK sync word byte 5.
    SyncWord5                = 0x06C5,
    /// FSK sync word byte 6.
    SyncWord6                = 0x06C6,
    /// FSK sync word byte 7.
    SyncWord7                = 0x06C7,
    /// FSK node address.
    NodeAddress              = 0x06CD,
    /// FSK broadcast address.
    BroadcastAddress         = 0x06CE,
    /// LoRa inverted-IQ polarity setup register.
    IqPolaritySetup          = 0x0736,
    /// LoRa sync word MSB.
    LoRaSyncWordMsb          = 0x0740,
    /// LoRa sync word LSB.
    LoRaSyncWordLsb          = 0x0741,
    /// Random-number generator byte 0.
    RandomNumberGen0         = 0x0819,
    /// Random-number generator byte 1.
    RandomNumberGen1         = 0x081A,
    /// Random-number generator byte 2.
    RandomNumberGen2         = 0x081B,
    /// Random-number generator byte 3.
    RandomNumberGen3         = 0x081C,
    /// Board-specific RX sensitivity patch register.
    RxSensitivityPatch       = 0x08B5,
    /// TX modulation workaround register.
    TxModulation             = 0x0889,
    /// RX gain control register.
    RxGain                   = 0x08AC,
    /// TX clamp configuration register.
    TxClampConfig            = 0x08D8,
    /// Over-current protection configuration register.
    OcpConfiguration         = 0x08E7,
    /// RTC timer control register.
    RtcControl               = 0x0902,
    /// XTA trimming capacitor register.
    XtaTrim                  = 0x0911,
    /// XTB trimming capacitor register.
    XtbTrim                  = 0x0912,
    /// DIO3 output-voltage control register.
    Dio3OutputVoltageControl = 0x0920,
    /// Event mask register.
    EventMask                = 0x0944,
}
