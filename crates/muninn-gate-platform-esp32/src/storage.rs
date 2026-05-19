//! ESP32 flash-backed configuration storage.
//!
//! The ESP32 platform stores the validated provisioning document in the
//! dedicated `muninn_cfg` data partition. Boot loads that document and passes
//! it through the same USB provisioning parser, so there is one config format
//! and one validation path.

use core::cmp::min;

use esp_rom_sys::rom::spiflash::{
    ESP_ROM_SPIFLASH_RESULT_OK,
    esp_rom_spiflash_erase_sector,
    esp_rom_spiflash_read,
    esp_rom_spiflash_unlock,
    esp_rom_spiflash_write,
};
use heapless::Vec;
use muninn_gate_core::config::MAX_TELEMETRY_PRODUCERS;
use muninn_gate_core::{Error, GatewayConfig};

use crate::provisioning::{ProvisioningCommand, USB_CONFIG_JSON_BYTES, parse_usb_document};

/// Gateway config type stored by the ESP32 platform.
pub type Esp32GatewayConfig = GatewayConfig<MAX_TELEMETRY_PRODUCERS>;

/// ESP-IDF partition table flash address.
const PART_TABLE_ADDR: u32 = 0x8000;
/// Maximum entries in the ESP-IDF partition table sector.
const PART_TABLE_ENTRIES: usize = 96;
/// ESP-IDF partition table entry size in bytes.
const PART_TABLE_ENTRY_SIZE: usize = 32;
/// ESP-IDF partition entry magic value.
const PART_MAGIC: u16 = 0x50AA;
/// ESP-IDF data partition type.
const PART_TYPE_DATA: u8 = 0x01;
/// One SPI flash sector in bytes.
const SECTOR_SIZE: u32 = 4096;
/// Two-sector A/B config storage footprint.
const CONFIG_STORAGE_BYTES: u32 = SECTOR_SIZE * 2;
/// One config storage slot in bytes.
const CONFIG_SLOT_BYTES: usize = SECTOR_SIZE as usize;
/// Number of words in one config storage slot.
const CONFIG_SLOT_WORDS: usize = CONFIG_SLOT_BYTES / 4;
/// Record header size in bytes.
const CONFIG_HEADER_BYTES: usize = 32;
/// Maximum bytes that fit in one persisted provisioning document.
const CONFIG_PAYLOAD_BYTES: usize = CONFIG_SLOT_BYTES - CONFIG_HEADER_BYTES;
/// Magic value for persisted Muninn Gate config records.
const CONFIG_MAGIC: u32 = 0x4643_474D;
/// Persisted config record version.
const CONFIG_VERSION: u16 = 1;
/// Flash address cache sentinel.
const INVALID_ADDR: u32 = u32::MAX;

static mut CONFIG_BASE_ADDR: u32 = INVALID_ADDR;

/// Load gateway configuration from flash storage.
pub fn load_config() -> Result<Esp32GatewayConfig, Error>
{
    let document = load_config_document().map_err(|_| Error::Storage)?;
    let text = core::str::from_utf8(document.as_slice()).map_err(|_| Error::Storage)?;
    let command = parse_usb_document(text).map_err(|_| Error::Storage)?;
    let ProvisioningCommand::SetConfig(config) = command else {
        return Err(Error::Storage);
    };
    let config = *config;
    config.validate()?;
    Ok(config)
}

/// Save a validated gateway configuration and its source document to flash.
pub fn save_config_document(document: &str, config: &Esp32GatewayConfig) -> Result<(), Error>
{
    config.validate()?;
    save_config_bytes(document.as_bytes()).map_err(|_| Error::Storage)
}

/// Load the newest valid config document from flash.
fn load_config_document() -> Result<Vec<u8, USB_CONFIG_JSON_BYTES>, StorageError>
{
    let base = resolve_config_base_addr()?;
    let slot_a = read_slot_header(base)?;
    let slot_b = read_slot_header(base + SECTOR_SIZE)?;

    let chosen = match (slot_a, slot_b) {
        (Some(a), Some(b)) => {
            if b.seq.wrapping_sub(a.seq) < (1_u32 << 31) {
                Some(b)
            } else {
                Some(a)
            }
        },
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
    .ok_or(StorageError::NotFound)?;

    read_slot_payload(chosen)
}

/// Save one config document with A/B sector rollover.
fn save_config_bytes(document: &[u8]) -> Result<(), StorageError>
{
    if document.is_empty() || document.len() > CONFIG_PAYLOAD_BYTES {
        return Err(StorageError::TooLarge);
    }

    let base = resolve_config_base_addr()?;
    let addr_a = base;
    let addr_b = base + SECTOR_SIZE;
    let slot_a = read_slot_header(addr_a)?;
    let slot_b = read_slot_header(addr_b)?;

    let (active_addr, next_seq) = match (slot_a, slot_b) {
        (Some(a), Some(b)) => {
            if b.seq.wrapping_sub(a.seq) < (1_u32 << 31) {
                (addr_b, b.seq.wrapping_add(1))
            } else {
                (addr_a, a.seq.wrapping_add(1))
            }
        },
        (Some(a), None) => (addr_a, a.seq.wrapping_add(1)),
        (None, Some(b)) => (addr_b, b.seq.wrapping_add(1)),
        (None, None) => (addr_a, 1),
    };
    let target_addr = if active_addr == addr_a {
        addr_b
    } else {
        addr_a
    };

    let mut words = [0xFFFF_FFFF_u32; CONFIG_SLOT_WORDS];
    write_record_header(&mut words, next_seq, document.len() as u32, crc32(document));
    write_bytes(&mut words, CONFIG_HEADER_BYTES, document);

    erase_and_write_slot(target_addr / SECTOR_SIZE, target_addr, &words)?;

    let verify = read_slot_header(target_addr)?.ok_or(StorageError::Corrupt)?;
    if verify.seq != next_seq || verify.payload_len as usize != document.len() {
        return Err(StorageError::Corrupt);
    }
    let verify_payload = read_slot_payload(verify)?;
    if verify_payload.as_slice() != document {
        return Err(StorageError::Corrupt);
    }

    Ok(())
}

/// Write record header bytes into the slot image.
fn write_record_header(words: &mut [u32; CONFIG_SLOT_WORDS], seq: u32, payload_len: u32, crc: u32)
{
    let mut header = [0xFF_u8; CONFIG_HEADER_BYTES];
    header[0..4].copy_from_slice(&CONFIG_MAGIC.to_le_bytes());
    header[4..6].copy_from_slice(&CONFIG_VERSION.to_le_bytes());
    header[6..8].copy_from_slice(&(CONFIG_HEADER_BYTES as u16).to_le_bytes());
    header[8..12].copy_from_slice(&seq.to_le_bytes());
    header[12..16].copy_from_slice(&payload_len.to_le_bytes());
    header[16..20].copy_from_slice(&crc.to_le_bytes());
    write_bytes(words, 0, &header);
}

/// Write bytes into a little-endian flash word image.
fn write_bytes(words: &mut [u32; CONFIG_SLOT_WORDS], offset: usize, bytes: &[u8])
{
    for (index, byte) in bytes.iter().copied().enumerate() {
        let byte_offset = offset + index;
        let word_index = byte_offset / 4;
        let shift = (byte_offset % 4) * 8;
        let mask = !(0xFF_u32 << shift);
        words[word_index] = (words[word_index] & mask) | (u32::from(byte) << shift);
    }
}

/// Read and validate one slot header.
fn read_slot_header(addr: u32) -> Result<Option<StoredSlot>, StorageError>
{
    let mut words = [0_u32; CONFIG_HEADER_BYTES / 4];
    let rc = unsafe { esp_rom_spiflash_read(addr, words.as_mut_ptr(), CONFIG_HEADER_BYTES as u32) };
    if rc != ESP_ROM_SPIFLASH_RESULT_OK {
        return Err(StorageError::Read(rc));
    }

    let mut header = [0_u8; CONFIG_HEADER_BYTES];
    for (index, word) in words.iter().enumerate() {
        let offset = index * 4;
        header[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
    }

    let magic = u32::from_le_bytes(header[0..4].try_into().map_err(|_| StorageError::Corrupt)?);
    if magic != CONFIG_MAGIC {
        return Ok(None);
    }
    let version = u16::from_le_bytes(header[4..6].try_into().map_err(|_| StorageError::Corrupt)?);
    let header_len =
        u16::from_le_bytes(header[6..8].try_into().map_err(|_| StorageError::Corrupt)?);
    let seq = u32::from_le_bytes(
        header[8..12]
            .try_into()
            .map_err(|_| StorageError::Corrupt)?,
    );
    let payload_len = u32::from_le_bytes(
        header[12..16]
            .try_into()
            .map_err(|_| StorageError::Corrupt)?,
    );
    let payload_crc = u32::from_le_bytes(
        header[16..20]
            .try_into()
            .map_err(|_| StorageError::Corrupt)?,
    );

    if version != CONFIG_VERSION
        || usize::from(header_len) != CONFIG_HEADER_BYTES
        || payload_len == 0
        || payload_len as usize > CONFIG_PAYLOAD_BYTES
    {
        return Ok(None);
    }

    Ok(Some(StoredSlot {
        addr,
        seq,
        payload_len,
        payload_crc,
    }))
}

/// Read and CRC-check one slot payload.
fn read_slot_payload(slot: StoredSlot) -> Result<Vec<u8, USB_CONFIG_JSON_BYTES>, StorageError>
{
    let mut payload = Vec::new();
    let mut offset = 0_usize;
    let total_len = slot.payload_len as usize;

    while offset < total_len {
        let data_len = min(256, total_len - offset);
        let raw_len = align4(data_len);
        let mut words = [0_u32; 64];
        let rc = unsafe {
            esp_rom_spiflash_read(
                slot.addr + CONFIG_HEADER_BYTES as u32 + offset as u32,
                words.as_mut_ptr(),
                raw_len as u32,
            )
        };
        if rc != ESP_ROM_SPIFLASH_RESULT_OK {
            return Err(StorageError::Read(rc));
        }

        for byte_index in 0..data_len {
            let word = words[byte_index / 4];
            let shift = (byte_index % 4) * 8;
            let byte = ((word >> shift) & 0xFF) as u8;
            payload.push(byte).map_err(|_| StorageError::TooLarge)?;
        }

        offset += data_len;
    }

    if crc32(payload.as_slice()) != slot.payload_crc {
        return Err(StorageError::Corrupt);
    }

    Ok(payload)
}

/// Round byte count up to a flash-word boundary.
const fn align4(value: usize) -> usize
{
    (value + 3) & !3
}

/// Resolve the base address reserved for config storage.
fn resolve_config_base_addr() -> Result<u32, StorageError>
{
    let cached = unsafe { CONFIG_BASE_ADDR };
    if cached != INVALID_ADDR {
        return Ok(cached);
    }

    let mut chosen: Option<(u32, u32)> = None;
    for index in 0..PART_TABLE_ENTRIES {
        let addr = PART_TABLE_ADDR + (index * PART_TABLE_ENTRY_SIZE) as u32;
        let mut words = [0_u32; PART_TABLE_ENTRY_SIZE / 4];
        let rc = unsafe {
            esp_rom_spiflash_read(addr, words.as_mut_ptr(), PART_TABLE_ENTRY_SIZE as u32)
        };
        if rc != ESP_ROM_SPIFLASH_RESULT_OK {
            return Err(StorageError::Read(rc));
        }

        let mut entry = [0_u8; PART_TABLE_ENTRY_SIZE];
        for (word_index, word) in words.iter().enumerate() {
            let offset = word_index * 4;
            entry[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }

        let magic = u16::from_le_bytes([entry[0], entry[1]]);
        if magic == 0xFFFF {
            break;
        }
        if magic != PART_MAGIC {
            continue;
        }

        let partition_type = entry[2];
        let offset = u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]);
        let size = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]);
        let label_raw = &entry[12..28];
        let label_len = label_raw
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(label_raw.len());
        let label = core::str::from_utf8(&label_raw[..label_len]).unwrap_or("");

        if partition_type == PART_TYPE_DATA && label == "muninn_cfg" && size >= CONFIG_STORAGE_BYTES
        {
            chosen = Some((offset, size));
            break;
        }
    }

    let (partition_offset, partition_size) = chosen.ok_or(StorageError::PartitionNotFound)?;
    let base = align_down_sector(partition_offset + partition_size - CONFIG_STORAGE_BYTES);
    unsafe {
        CONFIG_BASE_ADDR = base;
    }
    Ok(base)
}

/// Align a flash address down to the nearest sector.
const fn align_down_sector(addr: u32) -> u32
{
    (addr / SECTOR_SIZE) * SECTOR_SIZE
}

/// Erase one target sector and write one complete slot image.
#[esp_hal::ram]
fn erase_and_write_slot(
    target_sector: u32,
    target_addr: u32,
    words: &[u32; CONFIG_SLOT_WORDS],
) -> Result<(), StorageError>
{
    critical_section::with(|_| {
        let rc = unsafe { esp_rom_spiflash_unlock() };
        if rc != ESP_ROM_SPIFLASH_RESULT_OK {
            return Err(StorageError::Write(rc));
        }

        let rc = unsafe { esp_rom_spiflash_erase_sector(target_sector) };
        if rc != ESP_ROM_SPIFLASH_RESULT_OK {
            return Err(StorageError::Erase(rc));
        }

        let rc = unsafe {
            esp_rom_spiflash_write(target_addr, words.as_ptr(), CONFIG_SLOT_BYTES as u32)
        };
        if rc != ESP_ROM_SPIFLASH_RESULT_OK {
            return Err(StorageError::Write(rc));
        }

        Ok(())
    })
}

/// Compute CRC32 using the IEEE polynomial.
fn crc32(data: &[u8]) -> u32
{
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Header metadata for one stored config slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StoredSlot
{
    /// Absolute flash address of the slot.
    addr:        u32,
    /// Monotonic rollover sequence.
    seq:         u32,
    /// Payload length in bytes.
    payload_len: u32,
    /// CRC32 of the payload.
    payload_crc: u32,
}

/// Internal storage failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageError
{
    /// Config partition could not be found.
    PartitionNotFound,
    /// No valid stored config exists.
    NotFound,
    /// Persisted config is too large for the reserved slot.
    TooLarge,
    /// Persisted record failed validation.
    Corrupt,
    /// Flash read failed with ROM return code.
    Read(i32),
    /// Flash erase failed with ROM return code.
    Erase(i32),
    /// Flash write failed with ROM return code.
    Write(i32),
}
