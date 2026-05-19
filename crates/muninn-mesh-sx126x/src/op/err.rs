//! Device error response type.

/// Device error bitfield returned by `GetDeviceErrors`.
#[derive(Copy, Clone)]
pub struct DeviceErrors
{
    /// Raw device-error bits.
    inner: u16,
}

impl core::fmt::Debug for DeviceErrors
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        let rc64k_calib_err = self.rc64k_calib_err();
        let rc13m_calib_err = self.rc13m_calib_err();
        let pll_calib_err = self.pll_calib_err();
        let adc_calib_err = self.adc_calib_err();
        let img_calib_err = self.img_calib_err();
        let xosc_start_err = self.xosc_start_err();
        let pll_lock_err = self.pll_lock_err();
        let pa_ramp_err = self.pa_ramp_err();
        write!(
            f,
            "DeviceErrors {{inner: {:#016b}, rc64k_calib_err: {}, rc13m_calib_err: {}, \
             pll_calib_err: {}, adc_calib_err: {}, img_calib_err: {}, xosc_start_err: {}, \
             pll_lock_err: {}, pa_ramp_err: {}}}",
            self.inner,
            rc64k_calib_err,
            rc13m_calib_err,
            pll_calib_err,
            adc_calib_err,
            img_calib_err,
            xosc_start_err,
            pll_lock_err,
            pa_ramp_err,
        )
    }
}

impl From<u16> for DeviceErrors
{
    fn from(val: u16) -> Self
    {
        Self { inner: val }
    }
}

impl DeviceErrors
{
    /// Return the raw device-error bit mask.
    pub fn bits(self) -> u16
    {
        self.inner
    }

    /// Return true when RC64K calibration failed.
    pub fn rc64k_calib_err(self) -> bool
    {
        (self.inner & 1 << 0) > 0
    }

    /// Return true when RC13M calibration failed.
    pub fn rc13m_calib_err(self) -> bool
    {
        (self.inner & 1 << 1) > 0
    }

    /// Return true when PLL calibration failed.
    pub fn pll_calib_err(self) -> bool
    {
        (self.inner & 1 << 2) > 0
    }

    /// Return true when ADC calibration failed.
    pub fn adc_calib_err(self) -> bool
    {
        (self.inner & 1 << 3) > 0
    }

    /// Return true when image calibration failed.
    pub fn img_calib_err(self) -> bool
    {
        (self.inner & 1 << 4) > 0
    }

    /// Return true when the crystal oscillator failed to start.
    pub fn xosc_start_err(self) -> bool
    {
        (self.inner & 1 << 5) > 0
    }

    /// Return true when PLL lock failed.
    pub fn pll_lock_err(self) -> bool
    {
        (self.inner & 1 << 6) > 0
    }

    /// Return true when PA ramping failed.
    pub fn pa_ramp_err(self) -> bool
    {
        (self.inner & 1 << 8) > 0
    }
}
