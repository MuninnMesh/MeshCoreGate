//! ESP app image descriptor required by the ESP32-S3 bootloader.
//!
//! This is boot metadata only. It provides the descriptor that the second-stage
//! bootloader validates before jumping to the bare-metal `esp-hal` application.

use core::ffi::c_char;

const APP_DESC_MAGIC: u32 = 0xABCD_5432;
const MMU_PAGE_SIZE_LOG2: u8 = 16;

#[repr(C)]
pub struct EspAppDesc
{
    magic_word:              u32,
    secure_version:          u32,
    reserved_1:              [u32; 2],
    version:                 [c_char; 32],
    project_name:            [c_char; 32],
    time:                    [c_char; 16],
    date:                    [c_char; 16],
    idf_ver:                 [c_char; 32],
    app_elf_sha256:          [u8; 32],
    min_efuse_blk_rev_full:  u16,
    max_efuse_blk_rev_full:  u16,
    mmu_page_size:           u8,
    reserved_3:              [u8; 3],
    reserved_2:              [u32; 18],
}

#[used]
#[unsafe(export_name = "esp_app_desc")]
#[unsafe(link_section = ".rodata_desc.appdesc")]
static ESP_APP_DESC: EspAppDesc = EspAppDesc {
    magic_word:              APP_DESC_MAGIC,
    secure_version:          0,
    reserved_1:              [0; 2],
    version:                 cstr(env!("CARGO_PKG_VERSION")),
    project_name:            cstr("muninn-gate"),
    time:                    cstr("00:00:00"),
    date:                    cstr("May 19 2026"),
    idf_ver:                 cstr("none"),
    app_elf_sha256:          [0; 32],
    min_efuse_blk_rev_full:  0,
    max_efuse_blk_rev_full:  u16::MAX,
    mmu_page_size:           MMU_PAGE_SIZE_LOG2,
    reserved_3:              [0; 3],
    reserved_2:              [0; 18],
};

const fn cstr<const N: usize>(value: &str) -> [c_char; N]
{
    let bytes = value.as_bytes();
    let mut out = [0 as c_char; N];
    let mut index = 0;
    while index < bytes.len() && index + 1 < N {
        out[index] = bytes[index] as c_char;
        index += 1;
    }
    out
}
