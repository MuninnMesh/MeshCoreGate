//! SX126x calibration command parameters.
//!
//! The SX126x exposes two related calibration commands:
//!
//! - `Calibrate` runs internal block calibration for RC oscillators, PLL, ADC, and image-rejection
//!   circuitry. Firmware usually runs this once during radio initialization after reset and before
//!   normal RX/TX operation.
//! - `CalibrateImage` tunes image rejection for the active RF band. Firmware should run it during
//!   bring-up and again whenever the configured RF frequency moves into a different
//!   image-calibration band.
//!
//! This module only encodes command parameters. Higher-level radio code decides
//! when calibration is needed and which RF frequency or board policy applies.

/// Internal block-selection mask passed to the `Calibrate` command.
///
/// Each bit enables one SX126x calibration block. Most firmware should use
/// [`Self::all`] during a cold radio initialization. Selective masks are useful
/// only for advanced recovery paths where one block needs to be re-run without
/// disturbing the rest of the radio state.
#[derive(Copy, Clone)]
pub struct CalibParam
{
    /// Encoded SX126x calibration bit mask.
    inner: u8,
}

impl From<CalibParam> for u8
{
    fn from(val: CalibParam) -> Self
    {
        val.inner
    }
}

impl From<u8> for CalibParam
{
    fn from(val: u8) -> Self
    {
        // The SX126x calibration mask uses bits 0..=6. Bit 7 is reserved.
        Self { inner: val & 0x7F }
    }
}

impl CalibParam
{
    /// Build a calibration mask from individual SX126x block-enable bits.
    ///
    /// Arguments map directly to the datasheet `Calibrate` command fields:
    ///
    /// - `rc64k_en`: 64 kHz RC oscillator calibration.
    /// - `rc13_en`: 13 MHz RC oscillator calibration.
    /// - `pll_en`: PLL calibration.
    /// - `adc_pulse_en`: ADC pulse calibration.
    /// - `adc_bulk_n_en`: ADC bulk-N calibration.
    /// - `adc_bulk_p_en`: ADC bulk-P calibration.
    /// - `image_en`: image calibration block. This does not replace `CalibrateImage`, which still
    ///   needs the RF-band pair.
    pub const fn new(
        rc64k_en: bool,
        rc13_en: bool,
        pll_en: bool,
        adc_pulse_en: bool,
        adc_bulk_n_en: bool,
        adc_bulk_p_en: bool,
        image_en: bool,
    ) -> Self
    {
        let inner = (rc64k_en as u8)
            | (rc13_en as u8) << 1
            | (pll_en as u8) << 2
            | (adc_pulse_en as u8) << 3
            | (adc_bulk_n_en as u8) << 4
            | (adc_bulk_p_en as u8) << 5
            | (image_en as u8) << 6;
        Self { inner }
    }

    /// Enable all internal calibration blocks for cold radio initialization.
    pub const fn all() -> Self
    {
        Self::new(true, true, true, true, true, true, true)
    }
}

/// RF-band parameter pairs accepted by the `CalibrateImage` command.
///
/// The command does not take the exact carrier frequency. It takes two encoded
/// bytes selected from the SX126x datasheet for a supported frequency range.
/// Pick the variant that contains the configured LoRa center frequency.
#[derive(Copy, Clone)]
#[repr(u16)]
pub enum CalibImageFreq
{
    /// 430 to 440 MHz image-calibration pair.
    MHz430_440 = 0x6B_6F,
    /// 470 to 510 MHz image-calibration pair.
    MHz470_510 = 0x75_81,
    /// 779 to 787 MHz image-calibration pair.
    MHz779_787 = 0xC1_C5,
    /// 863 to 870 MHz image-calibration pair.
    MHz863_870 = 0xD7_DB,
    /// 902 to 928 MHz image-calibration pair.
    MHz902_928 = 0xE1_E9,
}

impl From<CalibImageFreq> for [u8; 2]
{
    fn from(val: CalibImageFreq) -> Self
    {
        (val as u16).to_be_bytes()
    }
}

impl CalibImageFreq
{
    /// Select an image-calibration bucket for an RF frequency in hertz.
    ///
    /// Frequencies inside a documented SX126x range return that range. Unknown
    /// frequencies fall back to [`Self::MHz902_928`] so callers that already
    /// validated their region still get a deterministic command parameter.
    pub const fn from_rf_frequency(rf_frequency: u32) -> Self
    {
        match rf_frequency / 1000000 {
            902..=928 => Self::MHz902_928,
            863..=870 => Self::MHz863_870,
            779..=787 => Self::MHz779_787,
            470..=510 => Self::MHz470_510,
            430..=440 => Self::MHz430_440,
            _ => Self::MHz902_928, // Default
        }
    }
}
