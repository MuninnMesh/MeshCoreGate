//! Lightweight MeshCore packet metadata decoding.
//!
//! The radio layer needs a small amount of metadata for diagnostics, metrics,
//! and UI state before higher-level MeshCore handling is wired in.

use muninn_mesh_meshcore_lib::MESH_PATH_MAX;

/// Small metadata summary extracted from raw MeshCore payload bytes.
#[derive(Clone, Copy)]
pub struct MeshPacketMeta
{
    /// Source node hash or address fragment.
    pub src:              u32,
    /// Last non-zero path hop, used as repeater hint.
    pub repeater:         u32,
    /// Destination node hash or address fragment.
    pub dst:              u32,
    /// MeshCore payload type.
    pub kind:             u8,
    /// Number of hops advertised by the path header.
    pub path_hops:        u8,
    /// Stored path bytes, truncated to [`MESH_PATH_MAX`].
    pub path:             [u8; MESH_PATH_MAX],
    /// Number of valid path bytes stored.
    pub path_len:         u8,
    /// Sender display name bytes from advert payloads.
    pub sender_name:      [u8; 16],
    /// Number of valid bytes in [`Self::sender_name`].
    pub sender_len:       u8,
    /// Whether the advert included a short public-key hash.
    pub has_pubkey4:      bool,
    /// Short public-key hash from advert payloads.
    pub pubkey4:          u16,
    /// Whether [`Self::full_pubkey`] is valid.
    pub has_full_pubkey:  bool,
    /// Full public key from advert payloads.
    pub full_pubkey:      [u8; 32],
    /// Sanitized preview of text payload content.
    pub text_preview:     [u8; 48],
    /// Number of valid bytes in [`Self::text_preview`].
    pub text_len:         u8,
    /// Whether latitude and longitude are valid.
    pub has_position:     bool,
    /// Latitude in E7 degrees.
    pub lat_e7:           i32,
    /// Longitude in E7 degrees.
    pub lon_e7:           i32,
    /// TRACE route hashes from payload; SNR stays in `path`.
    pub trace_hashes:     [u8; MESH_PATH_MAX],
    /// Number of valid bytes in [`Self::trace_hashes`].
    pub trace_hashes_len: u8,
    /// TRACE hash element size in bytes.
    pub trace_hash_size:  u8,
}

impl Default for MeshPacketMeta
{
    fn default() -> Self
    {
        Self {
            src:              0,
            repeater:         0,
            dst:              0,
            kind:             0,
            path_hops:        0,
            path:             [0; MESH_PATH_MAX],
            path_len:         0,
            sender_name:      [0; 16],
            sender_len:       0,
            has_pubkey4:      false,
            pubkey4:          0,
            has_full_pubkey:  false,
            full_pubkey:      [0; 32],
            text_preview:     [0; 48],
            text_len:         0,
            has_position:     false,
            lat_e7:           0,
            lon_e7:           0,
            trace_hashes:     [0; MESH_PATH_MAX],
            trace_hashes_len: 0,
            trace_hash_size:  0,
        }
    }
}

/// Decode useful MeshCore metadata from a raw LoRa payload.
pub fn decode_packet_meta(payload: &[u8]) -> MeshPacketMeta
{
    if payload.is_empty() {
        return MeshPacketMeta::default();
    }

    let header = payload[0];
    let route_type = header & 0x03;
    let payload_type = (header & 0x3C) >> 2;
    let mut src = 0u32;
    let mut dst = 0u32;
    let mut idx = 1usize;

    if route_type == 0x00 || route_type == 0x03 {
        if payload.len() < idx + 4 {
            return MeshPacketMeta {
                kind: payload_type,
                ..MeshPacketMeta::default()
            };
        }
        idx += 4;
    }

    if payload.len() <= idx {
        return MeshPacketMeta {
            kind: payload_type,
            ..MeshPacketMeta::default()
        };
    }
    let raw_path_len = payload[idx];
    let segment_size = (raw_path_len & 0xC0) >> 6;
    let path_count = (raw_path_len & 0x3F) as usize;

    if segment_size != 0 {
        return MeshPacketMeta {
            kind: payload_type,
            ..MeshPacketMeta::default()
        };
    }

    let path_hops = path_count.min(u8::MAX as usize) as u8;
    idx += 1;
    if payload.len() < idx + path_count {
        return MeshPacketMeta {
            kind: payload_type,
            path_hops,
            ..MeshPacketMeta::default()
        };
    }
    let path = &payload[idx..idx + path_count];
    let mut last_hop = 0u32;

    let mut stored_path = [0u8; MESH_PATH_MAX];
    let stored_len = path_count.min(MESH_PATH_MAX);
    stored_path[..stored_len].copy_from_slice(&path[..stored_len]);

    for &hop in path {
        if hop != 0 {
            last_hop = hop as u32;
        }
    }

    let repeater = if last_hop != 0 { last_hop } else { src };
    idx += path_count;

    let mut sender_name = [0u8; 16];
    let mut sender_len = 0u8;
    let mut has_pubkey4 = false;
    let mut pubkey4 = 0u16;
    let mut has_full_pubkey = false;
    let mut full_pubkey = [0u8; 32];
    let mut has_position = false;
    let mut lat_e7 = 0i32;
    let mut lon_e7 = 0i32;
    let mut trace_hashes = [0u8; MESH_PATH_MAX];
    let mut trace_hashes_len = 0u8;
    let mut trace_hash_size = 0u8;
    let payload_body = &payload[idx..];

    if (payload_type == 0x00
        || payload_type == 0x01
        || payload_type == 0x02
        || payload_type == 0x08)
        && payload_body.len() >= 2
    {
        dst = payload_body[0] as u32;
        src = payload_body[1] as u32;
    }

    let (text_preview, text_len) = parse_text_preview(payload_type, payload_body);

    if payload_type == 0x04 {
        let advert = parse_advert_meta(payload_body);
        sender_name = advert.sender_name;
        sender_len = advert.sender_len;
        has_pubkey4 = advert.has_pubkey4;
        pubkey4 = advert.pubkey4;
        has_full_pubkey = advert.has_full_pubkey;
        full_pubkey = advert.full_pubkey;
        has_position = advert.has_position;
        lat_e7 = advert.lat_e7;
        lon_e7 = advert.lon_e7;
    }

    if payload_type == 0x09 && payload_body.len() >= 9 {
        let flags = payload_body[8];
        let path_sz = flags & 0x03;
        trace_hash_size = 1u8 << path_sz;
        let hash_data = &payload_body[9..];
        let hlen = hash_data.len().min(MESH_PATH_MAX);
        trace_hashes[..hlen].copy_from_slice(&hash_data[..hlen]);
        trace_hashes_len = hlen as u8;

        let num_hashes = if trace_hash_size > 0 {
            hlen / trace_hash_size as usize
        } else {
            0
        };
        log::debug!(
            "TRACE rx: flags={:#04x} hsz={} raw={} hashes={} path_len={} snr={:?}",
            flags,
            trace_hash_size,
            hlen,
            num_hashes,
            path_count,
            &path[..path_count.min(MESH_PATH_MAX)]
        );
    }

    if src == 0 && payload_type != 0x04 && payload_type != 0x09 {
        for &hop in path {
            if hop != 0 {
                src = hop as u32;
                break;
            }
        }
    }

    MeshPacketMeta {
        src,
        repeater,
        dst,
        kind: payload_type,
        path_hops,
        path: stored_path,
        path_len: stored_len as u8,
        sender_name,
        sender_len,
        has_pubkey4,
        pubkey4,
        has_full_pubkey,
        full_pubkey,
        text_preview,
        text_len,
        has_position,
        lat_e7,
        lon_e7,
        trace_hashes,
        trace_hashes_len,
        trace_hash_size,
    }
}

/// Metadata extracted from a MeshCore advert payload.
#[derive(Clone, Copy)]
struct AdvertMeta
{
    /// Sender display name bytes.
    sender_name:     [u8; 16],
    /// Number of valid bytes in [`Self::sender_name`].
    sender_len:      u8,
    /// Whether [`Self::pubkey4`] is valid.
    has_pubkey4:     bool,
    /// Short public-key hash.
    pubkey4:         u16,
    /// Whether [`Self::full_pubkey`] is valid.
    has_full_pubkey: bool,
    /// Full public key bytes.
    full_pubkey:     [u8; 32],
    /// Whether latitude and longitude are valid.
    has_position:    bool,
    /// Latitude in E7 degrees.
    lat_e7:          i32,
    /// Longitude in E7 degrees.
    lon_e7:          i32,
}

/// Return an empty advert metadata value.
fn empty_advert_meta() -> AdvertMeta
{
    AdvertMeta {
        sender_name:     [0; 16],
        sender_len:      0,
        has_pubkey4:     false,
        pubkey4:         0,
        has_full_pubkey: false,
        full_pubkey:     [0; 32],
        has_position:    false,
        lat_e7:          0,
        lon_e7:          0,
    }
}

/// Parse display, key, and position metadata from an advert body.
fn parse_advert_meta(payload_body: &[u8]) -> AdvertMeta
{
    if payload_body.len() < (32 + 4 + 64 + 1) {
        return empty_advert_meta();
    }
    let mut full_pubkey = [0u8; 32];
    full_pubkey.copy_from_slice(&payload_body[..32]);
    let appdata = &payload_body[32 + 4 + 64..];
    let flags = appdata[0];
    let mut idx = 1usize;
    let mut has_position = false;
    let mut lat_e7 = 0i32;
    let mut lon_e7 = 0i32;

    if (flags & 0x10) != 0 {
        if appdata.len() < idx + 8 {
            return empty_advert_meta();
        }
        lat_e7 = i32::from_le_bytes([
            appdata[idx],
            appdata[idx + 1],
            appdata[idx + 2],
            appdata[idx + 3],
        ]);
        lon_e7 = i32::from_le_bytes([
            appdata[idx + 4],
            appdata[idx + 5],
            appdata[idx + 6],
            appdata[idx + 7],
        ]);
        lat_e7 = scale_e6_to_e7(lat_e7);
        lon_e7 = scale_e6_to_e7(lon_e7);
        has_position = true;
        idx += 8;
    }
    if (flags & 0x20) != 0 {
        idx = idx.saturating_add(2);
    }
    if (flags & 0x40) != 0 {
        idx = idx.saturating_add(2);
    }
    if (flags & 0x80) == 0 || idx >= appdata.len() {
        return AdvertMeta {
            sender_name: [0; 16],
            sender_len: 0,
            has_pubkey4: true,
            pubkey4: u16::from_be_bytes([payload_body[0], payload_body[1]]),
            has_full_pubkey: true,
            full_pubkey,
            has_position,
            lat_e7,
            lon_e7,
        };
    }

    let mut out = [0u8; 16];
    let mut out_len = 0usize;
    for &b in &appdata[idx..] {
        if b == 0 || out_len >= out.len() {
            break;
        }
        if (32..=126).contains(&b) && b != b',' {
            out[out_len] = b;
            out_len += 1;
        }
    }

    AdvertMeta {
        sender_name: out,
        sender_len: out_len as u8,
        has_pubkey4: true,
        pubkey4: u16::from_be_bytes([payload_body[0], payload_body[1]]),
        has_full_pubkey: true,
        full_pubkey,
        has_position,
        lat_e7,
        lon_e7,
    }
}

/// Convert E6 coordinate units to E7 coordinate units.
fn scale_e6_to_e7(v_e6: i32) -> i32
{
    v_e6.saturating_mul(10)
}

/// Parse a printable preview from a text payload.
fn parse_text_preview(kind: u8, payload_body: &[u8]) -> ([u8; 48], u8)
{
    if kind != 0x02 {
        return ([0; 48], 0);
    }

    let mut out = [0u8; 48];
    let mut out_len = 0usize;
    let mut scanned = 0usize;
    let mut printable = 0usize;

    for &b in payload_body.iter().take(96) {
        if b == 0 {
            break;
        }
        scanned += 1;
        let ascii = (32..=126).contains(&b);
        if ascii || b == b'\n' || b == b'\r' || b == b'\t' {
            printable += 1;
            if out_len < out.len() {
                out[out_len] = if ascii { b } else { b' ' };
                out_len += 1;
            }
        } else if out_len < out.len() {
            out[out_len] = b'?';
            out_len += 1;
        }
    }

    if scanned == 0 {
        return ([0; 48], 0);
    }
    if (printable * 100 / scanned) < 85 || out_len == 0 {
        return ([0; 48], 0);
    }

    while out_len > 0 && out[out_len - 1] == b' ' {
        out_len -= 1;
    }
    (out, out_len as u8)
}

/// Compute the 32-bit FNV-1a hash for a received payload.
pub fn fnv1a32(data: &[u8]) -> u32
{
    let mut hash = 0x811C9DC5u32;
    for &b in data {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}
