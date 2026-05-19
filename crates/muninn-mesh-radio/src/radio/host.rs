//! Host callback registry for board inputs and side effects.
//!
//! The radio wrapper owns the SX126x device, but board firmware owns inputs
//! such as configured TX power and public-key retention. This module keeps
//! those callbacks in one critical-section protected slot.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

/// Host-provided functions used by radio setup and side effects.
#[derive(Clone, Copy)]
pub struct RadioHostFns
{
    /// Return the configured LoRa TX power in dBm.
    pub lora_tx_power_dbm:  fn() -> i8,
    /// Persist an observed public key for later MeshCore address lookup.
    pub remember_pubkey:    fn(u16, [u8; 32], u32),
    /// Notify the host when LoRa TX is active.
    pub set_lora_tx_active: fn(bool),
}

/// Default TX-power callback for hosts that have not registered one.
fn default_lora_tx_power_dbm() -> i8
{
    0
}

/// Default public-key callback for hosts that have not registered one.
fn default_remember_pubkey(_pubkey4: u16, _full_pubkey: [u8; 32], _last_seen: u32) {}

/// Default TX activity callback for hosts that have not registered one.
fn default_set_lora_tx_active(_active: bool) {}

impl RadioHostFns
{
    /// Return inert callbacks suitable before platform setup registers hooks.
    pub const fn defaults() -> Self
    {
        Self {
            lora_tx_power_dbm:  default_lora_tx_power_dbm,
            remember_pubkey:    default_remember_pubkey,
            set_lora_tx_active: default_set_lora_tx_active,
        }
    }
}

/// Host callbacks guarded by a critical-section mutex.
static RADIO_HOST: BlockingMutex<CriticalSectionRawMutex, RefCell<RadioHostFns>> =
    BlockingMutex::new(RefCell::new(RadioHostFns::defaults()));

/// Replace the host callbacks used by radio-side host integration.
pub fn register_radio_host(host: RadioHostFns)
{
    RADIO_HOST.lock(|slot| *slot.borrow_mut() = host);
}

/// Invoke a closure with the currently registered host callbacks.
pub fn with_radio_host<R>(f: impl FnOnce(&RadioHostFns) -> R) -> R
{
    RADIO_HOST.lock(|slot| {
        let host = slot.borrow();
        f(&host)
    })
}
