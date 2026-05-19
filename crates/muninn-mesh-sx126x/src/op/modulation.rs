//! Modulation parameter types.

/// Raw modulation parameters encoded for `SetModulationParams`.
#[derive(Copy, Clone, Debug)]
pub struct ModParams
{
    /// Encoded modulation parameter bytes.
    inner: [u8; 8],
    /// Number of encoded bytes to write.
    len:   usize,
}

impl From<ModParams> for [u8; 8]
{
    fn from(val: ModParams) -> Self
    {
        val.inner
    }
}

impl ModParams
{
    /// Return the encoded parameter bytes that should be written to the radio.
    pub fn as_bytes(&self) -> &[u8]
    {
        &self.inner[..self.len]
    }
}

pub use lora::*;

mod lora
{
    //! LoRa modulation parameter types.

    use super::ModParams;

    /// LoRa spreading factor.
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    #[repr(u8)]
    pub enum LoRaSpreadFactor
    {
        /// Spreading factor 5.
        SF5  = 0x05,
        /// Spreading factor 6.
        SF6  = 0x06,
        /// Spreading factor 7.
        SF7  = 0x07,
        /// Spreading factor 8.
        SF8  = 0x08,
        /// Spreading factor 9.
        SF9  = 0x09,
        /// Spreading factor 10.
        SF10 = 0x0A,
        /// Spreading factor 11.
        SF11 = 0x0B,
        /// Spreading factor 12.
        SF12 = 0x0C,
    }

    /// Error returned when decoding an invalid LoRa spreading factor.
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub struct InvalidLoRaSpreadFactor;

    impl TryFrom<u8> for LoRaSpreadFactor
    {
        type Error = InvalidLoRaSpreadFactor;

        fn try_from(value: u8) -> Result<Self, Self::Error>
        {
            match value {
                0x05 => Ok(Self::SF5),
                0x06 => Ok(Self::SF6),
                0x07 => Ok(Self::SF7),
                0x08 => Ok(Self::SF8),
                0x09 => Ok(Self::SF9),
                0x0A => Ok(Self::SF10),
                0x0B => Ok(Self::SF11),
                0x0C => Ok(Self::SF12),
                _ => Err(InvalidLoRaSpreadFactor),
            }
        }
    }

    /// LoRa signal bandwidth.
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    #[repr(u8)]
    pub enum LoRaBandWidth
    {
        /// 7.81 kHz.
        BW7   = 0x00,
        /// 10.42 kHz.
        BW10  = 0x08,
        /// 15.63 kHz.
        BW15  = 0x01,
        /// 20.83 kHz.
        BW20  = 0x09,
        /// 31.25 kHz.
        BW31  = 0x02,
        /// 41.67 kHz.
        BW41  = 0x0A,
        /// 62.50 kHz.
        BW62  = 0x03,
        /// 125 kHz.
        BW125 = 0x04,
        /// 250 kHz.
        BW250 = 0x05,
        /// 500 kHz.
        BW500 = 0x06,
    }

    /// LoRa coding rate.
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    #[repr(u8)]
    pub enum LoraCodingRate
    {
        /// Coding rate 4/5.
        CR4_5 = 0x01,
        /// Coding rate 4/6.
        CR4_6 = 0x02,
        /// Coding rate 4/7.
        CR4_7 = 0x03,
        /// Coding rate 4/8.
        CR4_8 = 0x04,
    }

    /// Builder for LoRa modulation parameters.
    #[derive(Copy, Clone, Debug)]
    pub struct LoraModParams
    {
        /// LoRa spreading factor.
        spread_factor: LoRaSpreadFactor,
        /// LoRa signal bandwidth.
        bandwidth:     LoRaBandWidth,
        /// LoRa coding rate.
        coding_rate:   LoraCodingRate,
        /// Low-data-rate optimization flag.
        low_dr_opt:    bool,
    }

    impl Default for LoraModParams
    {
        fn default() -> Self
        {
            Self {
                spread_factor: LoRaSpreadFactor::SF7,
                bandwidth:     LoRaBandWidth::BW125,
                coding_rate:   LoraCodingRate::CR4_5,
                low_dr_opt:    false,
            }
        }
    }

    impl LoraModParams
    {
        /// Set LoRa spreading factor.
        pub fn set_spread_factor(mut self, spread_factor: LoRaSpreadFactor) -> Self
        {
            self.spread_factor = spread_factor;
            self
        }

        /// Set LoRa bandwidth.
        pub fn set_bandwidth(mut self, bandwidth: LoRaBandWidth) -> Self
        {
            self.bandwidth = bandwidth;
            self
        }

        /// Set LoRa coding rate.
        pub fn set_coding_rate(mut self, coding_rate: LoraCodingRate) -> Self
        {
            self.coding_rate = coding_rate;
            self
        }

        /// Enable or disable low-data-rate optimization.
        pub fn set_low_dr_opt(mut self, low_dr_opt: bool) -> Self
        {
            self.low_dr_opt = low_dr_opt;
            self
        }
    }

    impl From<LoraModParams> for ModParams
    {
        fn from(val: LoraModParams) -> Self
        {
            ModParams {
                inner: [
                    val.spread_factor as u8,
                    val.bandwidth as u8,
                    val.coding_rate as u8,
                    val.low_dr_opt as u8,
                    0x00,
                    0x00,
                    0x00,
                    0x00,
                ],
                len:   4,
            }
        }
    }
}
