//! Fixed-capacity MeshCore event and packet data types.

/// Maximum stored MeshCore display-name bytes.
pub const MESH_NAME_MAX: usize = 16;
/// Maximum stored MeshCore path or trace-hash bytes.
pub const MESH_PATH_MAX: usize = 16;
/// Maximum LoRa payload bytes accepted by MeshCore TX frames.
pub const MESH_PAYLOAD_MAX: usize = 255;
/// Maximum stored text-preview bytes.
pub const MESH_TEXT_PREVIEW_MAX: usize = 24;

/// MeshCore system channel kind.
pub const MESH_CHANNEL_MESH: u8 = 0;
/// MeshCore public channel kind.
pub const MESH_CHANNEL_PUBLIC: u8 = 1;
/// MeshCore testing channel kind.
pub const MESH_CHANNEL_TESTING: u8 = 2;
/// MeshCore critters channel kind.
pub const MESH_CHANNEL_CRITTERS: u8 = 3;
/// MeshCore private channel kind.
pub const MESH_CHANNEL_PRIV: u8 = 4;

/// MeshCore request payload kind.
pub const MESH_KIND_REQUEST: u8 = 0x00;
/// MeshCore response payload kind.
pub const MESH_KIND_RESPONSE: u8 = 0x01;
/// MeshCore text payload kind.
pub const MESH_KIND_TEXT: u8 = 0x02;
/// MeshCore ACK payload kind.
pub const MESH_KIND_ACK: u8 = 0x03;
/// MeshCore advert payload kind.
pub const MESH_KIND_ADVERT: u8 = 0x04;
/// MeshCore group payload kind.
pub const MESH_KIND_GROUP: u8 = 0x05;
/// MeshCore group-data payload kind.
pub const MESH_KIND_GROUP_DATA: u8 = 0x06;
/// MeshCore anonymous payload kind.
pub const MESH_KIND_ANON: u8 = 0x07;
/// MeshCore path payload kind.
pub const MESH_KIND_PATH: u8 = 0x08;
/// MeshCore trace payload kind.
pub const MESH_KIND_TRACE: u8 = 0x09;
/// MeshCore multipart payload kind.
pub const MESH_KIND_MULTIPART: u8 = 0x0A;
/// MeshCore control payload kind.
pub const MESH_KIND_CONTROL: u8 = 0x0B;
/// MeshCore raw payload kind.
pub const MESH_KIND_RAW: u8 = 0x0C;

/// Parsed MeshCore message metadata used by radio, UI, and storage layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshMessage
{
    /// Message timestamp supplied by the caller.
    pub timestamp:        u32,
    /// Source node identifier or short hash.
    pub src:              u32,
    /// Last repeater node identifier or short hash.
    pub repeater:         u32,
    /// Destination node identifier or short hash.
    pub dst:              u32,
    /// MeshCore payload kind.
    pub kind:             u8,
    /// Number of path hops reported by the payload.
    pub path_hops:        u8,
    /// Stored path bytes, truncated to [`MESH_PATH_MAX`].
    pub path:             [u8; MESH_PATH_MAX],
    /// Number of valid bytes in [`Self::path`].
    pub path_len:         u8,
    /// Last-packet RSSI in dBm.
    pub rssi_dbm:         i16,
    /// Last-packet SNR in tenths of a dB.
    pub snr_tenth_db:     i16,
    /// Original payload length in bytes.
    pub payload_len:      u16,
    /// Sender display-name bytes.
    pub sender_name:      [u8; MESH_NAME_MAX],
    /// Number of valid bytes in [`Self::sender_name`].
    pub sender_len:       u8,
    /// MeshCore channel kind.
    pub channel_kind:     u8,
    /// Whether [`Self::pubkey4`] is valid.
    pub has_pubkey4:      bool,
    /// Four-byte public-key prefix compressed into a displayable value.
    pub pubkey4:          u16,
    /// Stored text-preview bytes.
    pub text_preview:     [u8; MESH_TEXT_PREVIEW_MAX],
    /// Number of valid bytes in [`Self::text_preview`].
    pub text_len:         u8,
    /// Whether latitude and longitude are valid.
    pub has_position:     bool,
    /// Latitude in E7 degrees.
    pub lat_e7:           i32,
    /// Longitude in E7 degrees.
    pub lon_e7:           i32,
    /// TRACE route hashes from payload bytes, not from [`Self::path`].
    pub trace_hashes:     [u8; MESH_PATH_MAX],
    /// Number of valid bytes in [`Self::trace_hashes`].
    pub trace_hashes_len: u8,
    /// Number of bytes per trace hash.
    pub trace_hash_size:  u8,
}

impl MeshMessage
{
    /// Create a message with required packet metadata.
    pub const fn new(
        timestamp: u32,
        src: u32,
        dst: u32,
        kind: u8,
        rssi_dbm: i16,
        snr_tenth_db: i16,
        payload_len: u16,
    ) -> Self
    {
        Self {
            timestamp,
            src,
            repeater: src,
            dst,
            kind,
            path_hops: 0,
            path: [0; MESH_PATH_MAX],
            path_len: 0,
            rssi_dbm,
            snr_tenth_db,
            payload_len,
            sender_name: [0; MESH_NAME_MAX],
            sender_len: 0,
            channel_kind: MESH_CHANNEL_MESH,
            has_pubkey4: false,
            pubkey4: 0,
            text_preview: [0; MESH_TEXT_PREVIEW_MAX],
            text_len: 0,
            has_position: false,
            lat_e7: 0,
            lon_e7: 0,
            trace_hashes: [0; MESH_PATH_MAX],
            trace_hashes_len: 0,
            trace_hash_size: 0,
        }
    }

    /// Replace stored path bytes, truncating to [`MESH_PATH_MAX`].
    pub fn set_path(&mut self, hops: &[u8])
    {
        self.path = [0; MESH_PATH_MAX];
        let capped = hops.len().min(MESH_PATH_MAX);
        self.path[..capped].copy_from_slice(&hops[..capped]);
        self.path_len = capped as u8;
    }

    /// Replace the sender display name, truncating to [`MESH_NAME_MAX`].
    pub fn set_sender_name(&mut self, bytes: &[u8])
    {
        self.sender_name = [0; MESH_NAME_MAX];
        let capped = bytes.len().min(MESH_NAME_MAX);
        self.sender_name[..capped].copy_from_slice(&bytes[..capped]);
        self.sender_len = capped as u8;
    }

    /// Return the sender display name as UTF-8 when present and valid.
    pub fn sender_name_str(&self) -> Option<&str>
    {
        let len = (self.sender_len as usize).min(self.sender_name.len());
        if len == 0 {
            return None;
        }
        core::str::from_utf8(&self.sender_name[..len]).ok()
    }

    /// Set the short public-key prefix.
    pub fn set_pubkey4(&mut self, prefix: u16)
    {
        self.has_pubkey4 = true;
        self.pubkey4 = prefix;
    }

    /// Set the MeshCore channel kind.
    pub fn set_channel_kind(&mut self, channel_kind: u8)
    {
        self.channel_kind = channel_kind;
    }

    /// Return a compact channel label for UI display.
    pub fn channel_label(&self) -> &'static str
    {
        match self.channel_kind {
            MESH_CHANNEL_PUBLIC => "public",
            MESH_CHANNEL_TESTING => "testing",
            MESH_CHANNEL_CRITTERS => "critters",
            MESH_CHANNEL_PRIV => "priv",
            _ => "[sys]",
        }
    }

    /// Replace the text preview, truncating to [`MESH_TEXT_PREVIEW_MAX`].
    pub fn set_text_preview(&mut self, bytes: &[u8])
    {
        self.text_preview = [0; MESH_TEXT_PREVIEW_MAX];
        let capped = bytes.len().min(self.text_preview.len());
        self.text_preview[..capped].copy_from_slice(&bytes[..capped]);
        self.text_len = capped as u8;
    }

    /// Return the text preview as UTF-8 when present and valid.
    pub fn text_preview_str(&self) -> Option<&str>
    {
        let len = (self.text_len as usize).min(self.text_preview.len());
        if len == 0 {
            return None;
        }
        core::str::from_utf8(&self.text_preview[..len]).ok()
    }

    /// Set the message position in E7 degrees.
    pub fn set_position(&mut self, lat_e7: i32, lon_e7: i32)
    {
        self.has_position = true;
        self.lat_e7 = lat_e7;
        self.lon_e7 = lon_e7;
    }

    /// Replace TRACE route hashes, truncating to [`MESH_PATH_MAX`].
    pub fn set_trace_hashes(&mut self, hashes: &[u8], hash_size: u8)
    {
        self.trace_hashes = [0; MESH_PATH_MAX];
        let capped = hashes.len().min(MESH_PATH_MAX);
        self.trace_hashes[..capped].copy_from_slice(&hashes[..capped]);
        self.trace_hashes_len = capped as u8;
        self.trace_hash_size = hash_size;
    }
}

/// Aggregated state for a MeshCore node observed on the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshNode
{
    /// Node identifier or short hash.
    pub id:                u32,
    /// Timestamp when the node was last observed.
    pub last_seen:         u32,
    /// Number of times this node has been heard.
    pub heard_count:       u32,
    /// Minimum observed hop count, or 0 when unknown.
    pub min_hops:          u8,
    /// Maximum observed hop count, or 0 when unknown.
    pub max_hops:          u8,
    /// Most recent observed hop count.
    pub last_hops:         u8,
    /// Most recent RSSI in dBm.
    pub last_rssi_dbm:     i16,
    /// Most recent SNR in tenths of a dB.
    pub last_snr_tenth_db: i16,
    /// Whether [`Self::pubkey4`] is valid.
    pub has_pubkey4:       bool,
    /// Short public-key prefix.
    pub pubkey4:           u16,
    /// Whether latitude and longitude are valid.
    pub has_position:      bool,
    /// Latitude in E7 degrees.
    pub lat_e7:            i32,
    /// Longitude in E7 degrees.
    pub lon_e7:            i32,
    /// Node display-name bytes.
    pub name:              [u8; MESH_NAME_MAX],
    /// Number of valid bytes in [`Self::name`].
    pub name_len:          u8,
}

impl MeshNode
{
    /// Create a node record from the first observation.
    pub const fn new(id: u32, last_seen: u32, rssi_dbm: i16, snr_tenth_db: i16) -> Self
    {
        Self {
            id,
            last_seen,
            heard_count: 1,
            min_hops: 0,
            max_hops: 0,
            last_hops: 0,
            last_rssi_dbm: rssi_dbm,
            last_snr_tenth_db: snr_tenth_db,
            has_pubkey4: false,
            pubkey4: 0,
            has_position: false,
            lat_e7: 0,
            lon_e7: 0,
            name: [0; MESH_NAME_MAX],
            name_len: 0,
        }
    }

    /// Replace the node display name, truncating to [`MESH_NAME_MAX`].
    pub fn set_name(&mut self, bytes: &[u8])
    {
        self.name = [0; MESH_NAME_MAX];
        let capped = bytes.len().min(MESH_NAME_MAX);
        self.name[..capped].copy_from_slice(&bytes[..capped]);
        self.name_len = capped as u8;
    }

    /// Return the node display name as UTF-8 when present and valid.
    pub fn name_str(&self) -> Option<&str>
    {
        let len = (self.name_len as usize).min(self.name.len());
        if len == 0 {
            return None;
        }
        core::str::from_utf8(&self.name[..len]).ok()
    }

    /// Update hop-count statistics from one observation.
    pub fn update_hops(&mut self, hops: u8)
    {
        if hops == 0 {
            return;
        }
        self.last_hops = hops;
        if self.min_hops == 0 || hops < self.min_hops {
            self.min_hops = hops;
        }
        if hops > self.max_hops {
            self.max_hops = hops;
        }
    }

    /// Set the node position in E7 degrees.
    pub fn set_position(&mut self, lat_e7: i32, lon_e7: i32)
    {
        self.has_position = true;
        self.lat_e7 = lat_e7;
        self.lon_e7 = lon_e7;
    }

    /// Set the short public-key prefix.
    pub fn set_pubkey4(&mut self, prefix: u16)
    {
        self.has_pubkey4 = true;
        self.pubkey4 = prefix;
    }
}

/// MeshCore frame prepared for transmission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshTxFrame
{
    /// MeshCore payload kind.
    pub kind:        u8,
    /// Number of valid bytes in [`Self::payload`].
    pub payload_len: u8,
    /// Raw MeshCore payload bytes.
    pub payload:     [u8; MESH_PAYLOAD_MAX],
}

impl MeshTxFrame
{
    /// Create a frame from a fixed-capacity payload buffer.
    pub const fn new(kind: u8, payload: [u8; MESH_PAYLOAD_MAX], payload_len: u8) -> Self
    {
        Self {
            kind,
            payload_len,
            payload,
        }
    }

    /// Create a frame by copying a payload slice, returning `None` if too large.
    pub fn from_slice(kind: u8, payload: &[u8]) -> Option<Self>
    {
        if payload.len() > MESH_PAYLOAD_MAX {
            return None;
        }
        let mut out = [0u8; MESH_PAYLOAD_MAX];
        out[..payload.len()].copy_from_slice(payload);
        Some(Self::new(kind, out, payload.len() as u8))
    }

    /// Return the valid payload bytes.
    pub fn payload_slice(&self) -> &[u8]
    {
        let len = (self.payload_len as usize).min(self.payload.len());
        &self.payload[..len]
    }
}

/// Storage record for persisted MeshCore observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshStorageRecord
{
    /// Persisted message record.
    Message(MeshMessage),
    /// Persisted node record.
    Node(MeshNode),
}

/// Return a compact display name for a MeshCore payload kind.
pub fn mesh_kind_name(kind: u8) -> &'static str
{
    match kind {
        MESH_KIND_REQUEST => "REQ",
        MESH_KIND_RESPONSE => "RESP",
        MESH_KIND_TEXT => "TXT",
        MESH_KIND_ACK => "ACK",
        MESH_KIND_ADVERT => "ADVERT",
        MESH_KIND_GROUP => "GRP",
        MESH_KIND_GROUP_DATA => "GRP",
        MESH_KIND_ANON => "ANON",
        MESH_KIND_PATH => "PATH",
        MESH_KIND_TRACE => "TRACE",
        MESH_KIND_MULTIPART => "MULTIPART",
        MESH_KIND_CONTROL => "CONTROL",
        MESH_KIND_RAW => "RAW",
        _ => "UNK",
    }
}
