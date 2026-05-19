//! Packet parameter types.

/// Packet type selected with `SetPacketType`.
#[repr(u8)]
#[derive(Copy, Clone)]
pub enum PacketType
{
    /// GFSK packet mode.
    GFSK = 0x00,
    /// LoRa packet mode.
    LoRa = 0x01,
}

/// Raw packet parameters encoded for `SetPacketParams`.
#[derive(Copy, Clone, Debug)]
pub struct PacketParams
{
    /// Encoded packet parameter bytes.
    inner: [u8; 9],
    /// Number of encoded bytes to write.
    len:   usize,
}

impl From<PacketParams> for [u8; 9]
{
    fn from(val: PacketParams) -> Self
    {
        val.inner
    }
}

impl PacketParams
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
    //! LoRa packet parameter types.

    use super::PacketParams;

    /// LoRa header mode.
    #[repr(u8)]
    #[derive(Copy, Clone, Debug)]
    pub enum LoRaHeaderType
    {
        /// Variable-length packet with explicit header.
        VarLen   = 0x00,
        /// Fixed-length packet with implicit header.
        FixedLen = 0x01,
    }

    /// LoRa CRC setting.
    #[repr(u8)]
    #[derive(Copy, Clone, Debug)]
    pub enum LoRaCrcType
    {
        /// Disable payload CRC.
        CrcOff = 0x00,
        /// Enable payload CRC.
        CrcOn  = 0x01,
    }

    /// LoRa IQ polarity setting.
    #[repr(u8)]
    #[derive(Copy, Clone, Debug)]
    pub enum LoRaInvertIq
    {
        /// Standard IQ polarity.
        Standard = 0x00,
        /// Inverted IQ polarity.
        Inverted = 0x01,
    }

    /// Builder for LoRa packet parameters.
    #[derive(Copy, Clone, Debug)]
    pub struct LoRaPacketParams
    {
        /// Preamble length in symbols.
        pub preamble_len: u16, // 1, 2
        /// Header type.
        pub header_type:  LoRaHeaderType, // 3
        /// Payload length in bytes.
        pub payload_len:  u8, // 4
        /// CRC setting.
        pub crc_type:     LoRaCrcType, // 5
        /// IQ polarity setting.
        pub invert_iq:    LoRaInvertIq,
    }

    impl From<LoRaPacketParams> for PacketParams
    {
        fn from(val: LoRaPacketParams) -> Self
        {
            let preamble_len = val.preamble_len.to_be_bytes();

            PacketParams {
                inner: [
                    preamble_len[0],
                    preamble_len[1],
                    val.header_type as u8,
                    val.payload_len,
                    val.crc_type as u8,
                    val.invert_iq as u8,
                    0x00,
                    0x00,
                    0x00,
                ],
                len:   6,
            }
        }
    }

    impl Default for LoRaPacketParams
    {
        fn default() -> Self
        {
            Self {
                preamble_len: 16,
                header_type:  LoRaHeaderType::VarLen,
                payload_len:  0x00,
                crc_type:     LoRaCrcType::CrcOff,
                invert_iq:    LoRaInvertIq::Standard,
            }
        }
    }

    impl LoRaPacketParams
    {
        /// Set preamble length in symbols.
        pub fn set_preamble_len(mut self, preamble_len: u16) -> Self
        {
            self.preamble_len = preamble_len;
            self
        }

        /// Set LoRa header type.
        pub fn set_header_type(mut self, header_type: LoRaHeaderType) -> Self
        {
            self.header_type = header_type;
            self
        }

        /// Set payload length in bytes.
        pub fn set_payload_len(mut self, payload_len: u8) -> Self
        {
            self.payload_len = payload_len;
            self
        }

        /// Set payload CRC mode.
        pub fn set_crc_type(mut self, crc_type: LoRaCrcType) -> Self
        {
            self.crc_type = crc_type;
            self
        }

        /// Set IQ polarity.
        pub fn set_invert_iq(mut self, invert_iq: LoRaInvertIq) -> Self
        {
            self.invert_iq = invert_iq;
            self
        }
    }
}
