//! Poll-on-demand request accounting shared by USB and HTTP handlers.

use core::sync::atomic::{AtomicU32, Ordering};

static POLL_NOW_REQUESTS: AtomicU32 = AtomicU32::new(0);

/// Record one operator request to poll producers immediately.
pub fn request_poll_now() -> u32
{
    POLL_NOW_REQUESTS
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1)
}

/// Return the number of poll-on-demand requests observed since boot.
pub fn poll_now_total() -> u32
{
    POLL_NOW_REQUESTS.load(Ordering::Relaxed)
}

/// Return and clear pending poll-on-demand requests.
pub fn take_poll_now_requests() -> u32
{
    POLL_NOW_REQUESTS.swap(0, Ordering::Relaxed)
}
