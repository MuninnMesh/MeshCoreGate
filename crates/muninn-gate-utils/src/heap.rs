//! ESP32-S3 heap setup helpers.
//!
//! Embedded firmware on esp-hal needs to register one or more heap
//! regions with `esp_alloc::HEAP` early in `main` so subsequent
//! allocations (e.g. esp-wifi DMA descriptors, display framebuffers
//! kept off the stack, MeshCore identity blobs) actually succeed.
//!
//! Boards typically want two regions:
//!
//! - **DRAM2**: linked via the `.dram2_uninit` section. Used by esp-wifi for DMA-safe buffers. ~70
//!   KiB is the conservative size for STA-only WiFi with a modest connection count.
//! - **DRAM**: a regular bss region for everything else (HTTP server buffers, scratch space, etc.).
//!   32 KiB is the typical Muninn Gate size.
//!
//! Each board can call [`register_dram_region`] and
//! [`register_dram2_region`] (or roll its own with `esp_alloc::HEAP`
//! directly) at boot before any other allocation runs.
//!
//! The actual `static mut HEAP_*: MaybeUninit<[u8; N]>` storage MUST be
//! declared in the board crate so the linker can place it correctly via
//! `#[link_section = ".dram2_uninit"]` etc. — this helper exists to
//! centralize the unsafe ceremony, not to own the bytes themselves.

use core::mem::MaybeUninit;

/// Register a regular DRAM heap region with `esp_alloc::HEAP`.
///
/// Caller passes a `&'static mut MaybeUninit<[u8; N]>` that the linker
/// has placed in standard `.bss`. The size `N` is the region's byte
/// length — pass `core::mem::size_of_val(buf)` if you don't have the
/// const at hand.
///
/// # Safety
///
/// - `region` must point to writable, uninitialized storage that lives for the entire program
///   lifetime (typically a `static mut`).
/// - The same region must not already have been registered.
pub unsafe fn register_dram_region<const N: usize>(region: &'static mut MaybeUninit<[u8; N]>)
{
    unsafe {
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            region.as_mut_ptr().cast(),
            N,
            esp_alloc::MemoryCapability::Internal.into(),
        ));
    }
}

/// Register a DRAM2 heap region with `esp_alloc::HEAP`. Same shape as
/// [`register_dram_region`]; the only difference is that the caller's
/// `static mut` should carry `#[unsafe(link_section = ".dram2_uninit")]`
/// so the linker actually places the bytes in DRAM2.
///
/// # Safety
///
/// Same constraints as [`register_dram_region`].
pub unsafe fn register_dram2_region<const N: usize>(region: &'static mut MaybeUninit<[u8; N]>)
{
    unsafe {
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            region.as_mut_ptr().cast(),
            N,
            esp_alloc::MemoryCapability::Internal.into(),
        ));
    }
}
