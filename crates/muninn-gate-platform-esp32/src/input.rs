//! ESP32 local input controls.

use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::peripherals::GPIO0;

/// Minimum interval between accepted button presses.
pub const USER_BUTTON_DEBOUNCE_MS: u64 = 250;

/// Local user button connected to the selected ESP32 board.
pub struct UserButton
{
    pin:             Input<'static>,
    was_pressed:     bool,
    last_pressed_ms: u64,
}

impl UserButton
{
    /// Create a user button from the Heltec V4.x PRG/user-button pin.
    pub fn new(pin: GPIO0<'static>) -> Self
    {
        Self {
            pin:             Input::new(pin, InputConfig::default().with_pull(Pull::Up)),
            was_pressed:     false,
            last_pressed_ms: 0,
        }
    }

    /// Return true once for each debounced button press.
    pub fn poll_pressed(&mut self, now_ms: u64) -> bool
    {
        let pressed = self.pin.is_low();
        let accepted = pressed
            && !self.was_pressed
            && now_ms.saturating_sub(self.last_pressed_ms) >= USER_BUTTON_DEBOUNCE_MS;

        self.was_pressed = pressed;
        if accepted {
            self.last_pressed_ms = now_ms;
        }
        accepted
    }
}
