//! Reusable display components shared by text and graphics frontends.

/// Convert an RSSI value in dBm into a 0-3 producer signal-strength bucket.
pub fn rssi_bars(rssi: i16) -> u8
{
    if rssi <= -125 {
        0
    } else if rssi <= -110 {
        1
    } else if rssi <= -95 {
        2
    } else {
        3
    }
}

/// Convert battery charge percent into a 0-7 vertical icon fill height.
pub fn battery_fill_height(percent: u8) -> u32
{
    let percent = percent.min(100);
    if percent == 0 {
        return 0;
    }

    let height = (u32::from(percent) * 7).div_ceil(100);
    height.clamp(1, 7)
}
