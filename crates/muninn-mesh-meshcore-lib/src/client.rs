//! MeshCore client-side packet helpers.
//!
//! These helpers mirror the official MeshCore firmware request flow used by
//! `BaseChatMesh`: anonymous login requests establish the contact, regular
//! encrypted requests carry a 4-byte tag, and telemetry responses return the
//! same tag followed by Cayenne LPP bytes.

use aes::cipher::{Array, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use ed25519_dalek::hazmat::{ExpandedSecretKey, raw_sign};
use ed25519_dalek::{Sha512, Signer, SigningKey, VerifyingKey};
use heapless::Vec;
use hmac::digest::KeyInit as HmacKeyInit;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::{
    MESH_KIND_ADVERT,
    MESH_KIND_ANON,
    MESH_KIND_PATH,
    MESH_KIND_REQUEST,
    MESH_KIND_RESPONSE,
    MESH_PAYLOAD_MAX,
    MeshTxFrame,
};

/// MeshCore telemetry request command byte.
pub const MESHCORE_REQ_TYPE_GET_TELEMETRY_DATA: u8 = 0x03;
/// MeshCore successful anonymous-login response marker.
pub const MESHCORE_RESP_SERVER_LOGIN_OK: u8 = 0x00;
/// Maximum password bytes accepted by official MeshCore login packets.
pub const MESHCORE_LOGIN_PASSWORD_MAX: usize = 15;
/// MeshCore flood route type.
pub const MESHCORE_ROUTE_FLOOD: u8 = 0x01;
/// MeshCore direct route type.
pub const MESHCORE_ROUTE_DIRECT: u8 = 0x02;
/// MeshCore transport-flood route type.
pub const MESHCORE_ROUTE_TRANSPORT_FLOOD: u8 = 0x00;
/// MeshCore transport-direct route type.
pub const MESHCORE_ROUTE_TRANSPORT_DIRECT: u8 = 0x03;
/// MeshCore advert type used by companion/client-like nodes.
pub const MESHCORE_ADVERT_TYPE_COMPANION: u8 = 0x01;
/// MeshCore advert appdata flag indicating a display name is present.
pub const MESHCORE_ADVERT_HAS_NAME_FLAG: u8 = 0x80;
/// Maximum MeshCore advert appdata bytes accepted by firmware receivers.
pub const MESHCORE_ADVERT_APPDATA_MAX: usize = 32;
/// Default MeshCore LoRa center frequency used by the public US preset.
pub const MESHCORE_DEFAULT_FREQ_HZ: u32 = 910_525_000;
/// Default MeshCore public LoRa sync word.
pub const MESHCORE_DEFAULT_SYNC_WORD: u16 = 0x1424;
/// Default MeshCore LoRa preamble length in symbols.
pub const MESHCORE_DEFAULT_PREAMBLE_LEN: u16 = 16;
/// MeshCore public key byte length.
pub const MESHCORE_PUBLIC_KEY_BYTES: usize = 32;
/// MeshCore seed private key byte length.
pub const MESHCORE_PRIVATE_SEED_BYTES: usize = 32;
/// MeshCore expanded private key byte length.
pub const MESHCORE_EXPANDED_PRIVATE_KEY_BYTES: usize = 64;
/// MeshCore route hash minimum byte length.
pub const MESHCORE_MIN_PATH_HASH_BYTES: u8 = 1;
/// MeshCore route hash maximum byte length.
pub const MESHCORE_MAX_PATH_HASH_BYTES: u8 = 3;
/// MeshCore request and response tag byte length.
pub const MESHCORE_TAG_BYTES: usize = 4;

/// MeshCore request command byte offset in encrypted request payloads.
const REQUEST_COMMAND_OFFSET: usize = MESHCORE_TAG_BYTES;
/// MeshCore telemetry request plaintext byte length.
const TELEMETRY_REQUEST_PLAINTEXT_BYTES: usize = 13;
/// MeshCore telemetry request nonce byte offset.
const TELEMETRY_REQUEST_NONCE_OFFSET: usize = 9;
/// MeshCore login response status byte offset.
const LOGIN_RESPONSE_STATUS_OFFSET: usize = MESHCORE_TAG_BYTES;
/// MeshCore login response "OK" status byte length.
const LOGIN_RESPONSE_OK_BYTES: usize = 2;
/// MeshCore packet route bitmask.
const WIRE_ROUTE_MASK: u8 = 0x03;
/// MeshCore packet payload-kind bit shift.
const WIRE_PAYLOAD_KIND_SHIFT: u8 = 2;
/// MeshCore packet payload-kind bitmask after shifting.
const WIRE_PAYLOAD_KIND_MASK: u8 = 0x0f;
/// MeshCore packet header byte length.
const WIRE_HEADER_BYTES: usize = 1;
/// MeshCore transport route header byte length.
const WIRE_TRANSPORT_HEADER_BYTES: usize = 4;
/// MeshCore encoded path-length byte length.
const WIRE_PATH_LEN_BYTES: usize = 1;
/// MeshCore path hash-count bitmask in the encoded path length byte.
const PATH_HASH_COUNT_MASK: u8 = 0x3f;
/// Maximum path hashes representable in a MeshCore path length byte.
const PATH_HASH_COUNT_MAX: u8 = PATH_HASH_COUNT_MASK;
/// AES block size used by MeshCore AES-128 payloads.
const AES_BLOCK_BYTES: usize = 16;
/// HMAC prefix byte count included before encrypted payload bytes.
const MAC_PREFIX_BYTES: usize = 2;

/// Errors returned while building or parsing MeshCore client packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshcoreClientPacketError
{
    /// A configured key was not valid hex or had an unsupported length.
    InvalidKey,
    /// Packet construction exceeded fixed MeshCore frame capacity.
    PayloadTooLarge,
    /// Route/path settings cannot be represented by this helper.
    UnsupportedRoute,
    /// The response was addressed to another gateway.
    NotForGateway,
    /// The response came from a different producer.
    WrongSource,
    /// The response MAC could not be verified.
    Mac,
    /// The wire payload was malformed or too short.
    Decode,
    /// The producer rejected anonymous login.
    AuthFailed,
    /// The response tag did not match the active request.
    ResponseTagMismatch,
}

/// Gateway identity material needed for MeshCore encrypted requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshcoreIdentity
{
    /// Gateway MeshCore public key.
    pub public_key:      [u8; MESHCORE_PUBLIC_KEY_BYTES],
    /// Gateway Ed25519 clamped private scalar.
    pub private_scalar:  [u8; MESHCORE_PRIVATE_SEED_BYTES],
    /// Gateway Ed25519 material used for signed adverts.
    pub private_signing: MeshcoreSigningKey,
}

/// Gateway signing key material accepted by MeshCore exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshcoreSigningKey
{
    /// A 32-byte Ed25519 seed.
    Seed
    {
        /// Raw Ed25519 seed bytes.
        bytes: [u8; MESHCORE_PRIVATE_SEED_BYTES],
    },
    /// A 64-byte expanded MeshCore Ed25519 private key.
    Expanded
    {
        /// Expanded private scalar and hash-prefix bytes.
        bytes: [u8; MESHCORE_EXPANDED_PRIVATE_KEY_BYTES],
    },
}

impl MeshcoreIdentity
{
    /// Build identity material from provisioned MeshCore hex strings.
    ///
    /// `public_key_hex` must be a 32-byte public key. `private_key_hex` may be
    /// a 32-byte seed or a 64-byte MeshCore expanded private key. Expanded keys
    /// are stored as scalar plus prefix; the first 32 bytes are the scalar used
    /// for X25519 shared-secret derivation.
    pub fn from_hex(
        public_key_hex: &str,
        private_key_hex: &str,
    ) -> Result<Self, MeshcoreClientPacketError>
    {
        let public_key = parse_hex_32(public_key_hex)?;
        let (private_bytes, private_len) =
            parse_hex_bytes::<MESHCORE_EXPANDED_PRIVATE_KEY_BYTES>(private_key_hex)?;
        let (private_scalar, private_signing) = match private_len {
            MESHCORE_PRIVATE_SEED_BYTES => {
                let seed: [u8; MESHCORE_PRIVATE_SEED_BYTES] = private_bytes
                    [..MESHCORE_PRIVATE_SEED_BYTES]
                    .try_into()
                    .map_err(|_| MeshcoreClientPacketError::InvalidKey)?;
                let signing_key = SigningKey::from_bytes(&seed);
                if signing_key.verifying_key().to_bytes() != public_key {
                    return Err(MeshcoreClientPacketError::InvalidKey);
                }
                (
                    signing_key.to_scalar_bytes(),
                    MeshcoreSigningKey::Seed { bytes: seed },
                )
            },
            MESHCORE_EXPANDED_PRIVATE_KEY_BYTES => {
                let expanded = ExpandedSecretKey::from_bytes(&private_bytes);
                if VerifyingKey::from(&expanded).to_bytes() != public_key {
                    return Err(MeshcoreClientPacketError::InvalidKey);
                }
                (
                    private_bytes[..MESHCORE_PRIVATE_SEED_BYTES]
                        .try_into()
                        .map_err(|_| MeshcoreClientPacketError::InvalidKey)?,
                    MeshcoreSigningKey::Expanded {
                        bytes: private_bytes,
                    },
                )
            },
            _ => return Err(MeshcoreClientPacketError::InvalidKey),
        };

        Ok(Self {
            public_key,
            private_scalar,
            private_signing,
        })
    }

    /// Derive the X25519 shared secret used by MeshCore AES/HMAC packets.
    pub fn shared_secret(
        &self,
        peer_public_key: &[u8; MESHCORE_PUBLIC_KEY_BYTES],
    ) -> Result<[u8; MESHCORE_PUBLIC_KEY_BYTES], MeshcoreClientPacketError>
    {
        let peer = VerifyingKey::from_bytes(peer_public_key)
            .map_err(|_| MeshcoreClientPacketError::InvalidKey)?;
        Ok(peer
            .to_montgomery()
            .mul_clamped(self.private_scalar)
            .to_bytes())
    }

    /// Return this gateway's MeshCore route hash for the selected route.
    pub fn hash(
        &self,
        route: MeshcoreRequestRoute,
    ) -> Result<MeshcorePathHash, MeshcoreClientPacketError>
    {
        MeshcorePathHash::from_public_key(&self.public_key, route)
    }

    /// Sign bytes using the provisioned MeshCore Ed25519 identity.
    pub fn sign(&self, message: &[u8]) -> Result<[u8; 64], MeshcoreClientPacketError>
    {
        match self.private_signing {
            MeshcoreSigningKey::Seed { bytes } => {
                let signing_key = SigningKey::from_bytes(&bytes);
                if signing_key.verifying_key().to_bytes() != self.public_key {
                    return Err(MeshcoreClientPacketError::InvalidKey);
                }
                Ok(signing_key.sign(message).to_bytes())
            },
            MeshcoreSigningKey::Expanded { bytes } => {
                let expanded = ExpandedSecretKey::from_bytes(&bytes);
                let verifying_key = VerifyingKey::from(&expanded);
                if verifying_key.to_bytes() != self.public_key {
                    return Err(MeshcoreClientPacketError::InvalidKey);
                }
                Ok(raw_sign::<Sha512>(&expanded, message, &verifying_key).to_bytes())
            },
        }
    }
}

/// Configured route for a MeshCore client request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshcoreRequestRoute
{
    /// Send as a zero-hop direct packet.
    Direct,
    /// Send as a flood packet using the selected path hash size.
    Flood
    {
        /// Number of bytes per path hash, from 1 to 3.
        path_hash_size: u8,
    },
}

impl MeshcoreRequestRoute
{
    /// Return the route bits stored in the MeshCore packet header.
    pub const fn route_type(self) -> u8
    {
        match self {
            Self::Direct => MESHCORE_ROUTE_DIRECT,
            Self::Flood { .. } => MESHCORE_ROUTE_FLOOD,
        }
    }

    /// Return the encoded empty path length for this route.
    pub const fn encoded_empty_path_len(self) -> Result<u8, MeshcoreClientPacketError>
    {
        match self {
            Self::Direct => Ok(0),
            Self::Flood { path_hash_size } => encode_path_len(path_hash_size, 0),
        }
    }

    /// Return the number of public-key hash bytes used for this route.
    pub const fn path_hash_size(self) -> Result<u8, MeshcoreClientPacketError>
    {
        match self {
            Self::Direct => Ok(MESHCORE_MIN_PATH_HASH_BYTES),
            Self::Flood { path_hash_size } => {
                if path_hash_size < MESHCORE_MIN_PATH_HASH_BYTES
                    || path_hash_size > MESHCORE_MAX_PATH_HASH_BYTES
                {
                    Err(MeshcoreClientPacketError::UnsupportedRoute)
                } else {
                    Ok(path_hash_size)
                }
            },
        }
    }
}

/// MeshCore public-key hash bytes used in direct and flood-routed requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshcorePathHash
{
    /// Hash bytes copied from the start of a MeshCore public key.
    bytes: [u8; MESHCORE_MAX_PATH_HASH_BYTES as usize],
    /// Number of valid bytes in [`Self::bytes`].
    len:   u8,
}

impl MeshcorePathHash
{
    /// Build a route hash from a 32-byte MeshCore public key.
    pub fn from_public_key(
        public_key: &[u8; MESHCORE_PUBLIC_KEY_BYTES],
        route: MeshcoreRequestRoute,
    ) -> Result<Self, MeshcoreClientPacketError>
    {
        let len = route.path_hash_size()?;
        let mut bytes = [0u8; MESHCORE_MAX_PATH_HASH_BYTES as usize];
        let mut index = 0;
        while index < len as usize {
            bytes[index] = public_key[index];
            index += 1;
        }
        Ok(Self { bytes, len })
    }

    /// Return the valid route hash bytes.
    pub fn as_slice(&self) -> &[u8]
    {
        &self.bytes[..self.len as usize]
    }

    /// Return the valid route hash byte count.
    pub const fn len(self) -> usize
    {
        self.len as usize
    }

    /// Return true when the route hash has no bytes.
    pub const fn is_empty(self) -> bool
    {
        self.len == 0
    }
}

/// MeshCore producer/contact addressed by a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshcoreContact
{
    /// Producer MeshCore public key.
    pub public_key: [u8; MESHCORE_PUBLIC_KEY_BYTES],
    /// Route used for outbound requests.
    pub route:      MeshcoreRequestRoute,
}

impl MeshcoreContact
{
    /// Build a contact from a provisioned public key.
    pub fn from_hex(
        public_key_hex: &str,
        route: MeshcoreRequestRoute,
    ) -> Result<Self, MeshcoreClientPacketError>
    {
        Ok(Self {
            public_key: parse_hex_32(public_key_hex)?,
            route,
        })
    }

    /// Return the contact's MeshCore route hash for outbound requests.
    pub fn hash(&self) -> Result<MeshcorePathHash, MeshcoreClientPacketError>
    {
        MeshcorePathHash::from_public_key(&self.public_key, self.route)
    }
}

/// Decrypted MeshCore response data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshcoreResponse
{
    /// Decrypted response bytes.
    pub data: [u8; MESH_PAYLOAD_MAX],
    /// Number of valid response bytes.
    pub len:  usize,
}

impl MeshcoreResponse
{
    /// Return the valid decrypted response bytes.
    pub fn as_slice(&self) -> &[u8]
    {
        &self.data[..self.len.min(self.data.len())]
    }
}

/// Build an official MeshCore anonymous login request.
pub fn build_login_request(
    identity: &MeshcoreIdentity,
    contact: &MeshcoreContact,
    password: &str,
    tag: u32,
) -> Result<MeshTxFrame, MeshcoreClientPacketError>
{
    let secret = identity.shared_secret(&contact.public_key)?;
    let mut plain = Vec::<u8, 24>::new();
    plain
        .extend_from_slice(&tag.to_le_bytes())
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    let password = password.as_bytes();
    let password_len = password.len().min(MESHCORE_LOGIN_PASSWORD_MAX);
    plain
        .extend_from_slice(&password[..password_len])
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;

    let mut body = Vec::<u8, MESH_PAYLOAD_MAX>::new();
    body.extend_from_slice(contact.hash()?.as_slice())
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    body.extend_from_slice(&identity.public_key)
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    encrypt_then_mac(&secret, plain.as_slice(), &mut body)?;
    build_wire_frame(MESH_KIND_ANON, contact.route, body.as_slice())
}

/// Build an official MeshCore telemetry request.
pub fn build_telemetry_request(
    identity: &MeshcoreIdentity,
    contact: &MeshcoreContact,
    tag: u32,
    nonce: u32,
) -> Result<MeshTxFrame, MeshcoreClientPacketError>
{
    let secret = identity.shared_secret(&contact.public_key)?;
    let mut plain = [0u8; TELEMETRY_REQUEST_PLAINTEXT_BYTES];
    plain[..MESHCORE_TAG_BYTES].copy_from_slice(&tag.to_le_bytes());
    plain[REQUEST_COMMAND_OFFSET] = MESHCORE_REQ_TYPE_GET_TELEMETRY_DATA;
    plain[TELEMETRY_REQUEST_NONCE_OFFSET..TELEMETRY_REQUEST_NONCE_OFFSET + MESHCORE_TAG_BYTES]
        .copy_from_slice(&nonce.to_le_bytes());

    let mut body = Vec::<u8, MESH_PAYLOAD_MAX>::new();
    body.extend_from_slice(contact.hash()?.as_slice())
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    body.extend_from_slice(identity.hash(contact.route)?.as_slice())
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    encrypt_then_mac(&secret, &plain, &mut body)?;
    build_wire_frame(MESH_KIND_REQUEST, contact.route, body.as_slice())
}

/// Build a signed MeshCore startup advert for the gateway identity.
pub fn build_gateway_advert(
    identity: &MeshcoreIdentity,
    name: &str,
    timestamp_secs: u32,
) -> Result<MeshTxFrame, MeshcoreClientPacketError>
{
    let mut appdata = Vec::<u8, MESHCORE_ADVERT_APPDATA_MAX>::new();
    appdata
        .push(MESHCORE_ADVERT_TYPE_COMPANION | MESHCORE_ADVERT_HAS_NAME_FLAG)
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    push_advert_name(&mut appdata, name)?;

    let timestamp_secs = timestamp_secs.max(1);
    let mut signed = Vec::<u8, 68>::new();
    signed
        .extend_from_slice(&identity.public_key)
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    signed
        .extend_from_slice(&timestamp_secs.to_le_bytes())
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    signed
        .extend_from_slice(appdata.as_slice())
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    let signature = identity.sign(signed.as_slice())?;

    let mut body = Vec::<u8, MESH_PAYLOAD_MAX>::new();
    body.extend_from_slice(&identity.public_key)
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    body.extend_from_slice(&timestamp_secs.to_le_bytes())
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    body.extend_from_slice(&signature)
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    body.extend_from_slice(appdata.as_slice())
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;

    build_advert_wire_frame(body.as_slice())
}

/// Decrypt a direct response or path-return response for this contact.
pub fn decrypt_response(
    identity: &MeshcoreIdentity,
    contact: &MeshcoreContact,
    raw: &[u8],
) -> Result<MeshcoreResponse, MeshcoreClientPacketError>
{
    let wire = parse_wire_packet(raw)?;
    match wire.payload_type {
        MESH_KIND_RESPONSE => decrypt_datagram_payload(identity, contact, wire.payload),
        MESH_KIND_PATH => decrypt_path_response(identity, contact, wire.payload),
        _ => Err(MeshcoreClientPacketError::Decode),
    }
}

/// Return the MeshCore payload type and body from a raw wire packet.
pub fn meshcore_payload_body(raw: &[u8]) -> Result<(u8, &[u8]), MeshcoreClientPacketError>
{
    let wire = parse_wire_packet(raw)?;
    Ok((wire.payload_type, wire.payload))
}

/// Check whether a decrypted login response indicates success.
pub fn login_response_is_success(
    response: &MeshcoreResponse,
) -> Result<(), MeshcoreClientPacketError>
{
    let data = response.as_slice();
    if data.len() <= LOGIN_RESPONSE_STATUS_OFFSET {
        return Err(MeshcoreClientPacketError::Decode);
    }

    if data
        .get(LOGIN_RESPONSE_STATUS_OFFSET..LOGIN_RESPONSE_STATUS_OFFSET + LOGIN_RESPONSE_OK_BYTES)
        == Some(b"OK")
        || data[LOGIN_RESPONSE_STATUS_OFFSET] == MESHCORE_RESP_SERVER_LOGIN_OK
    {
        Ok(())
    } else {
        Err(MeshcoreClientPacketError::AuthFailed)
    }
}

/// Return true when a decrypted response is a repeater anonymous-login success.
pub fn response_is_login_success(response: &MeshcoreResponse) -> bool
{
    login_response_is_success(response).is_ok()
}

/// Return telemetry LPP bytes when `response` matches the request tag.
pub fn telemetry_response_lpp(
    response: &MeshcoreResponse,
    expected_tag: u32,
) -> Result<&[u8], MeshcoreClientPacketError>
{
    let data = response.as_slice();
    if data.len() <= MESHCORE_TAG_BYTES {
        return Err(MeshcoreClientPacketError::Decode);
    }
    let tag = u32::from_le_bytes(
        data[..MESHCORE_TAG_BYTES]
            .try_into()
            .map_err(|_| MeshcoreClientPacketError::Decode)?,
    );
    if tag != expected_tag {
        return Err(MeshcoreClientPacketError::ResponseTagMismatch);
    }
    Ok(&data[MESHCORE_TAG_BYTES..])
}

/// Parse a 32-byte lowercase or uppercase hex value.
pub fn parse_hex_32(
    value: &str,
) -> Result<[u8; MESHCORE_PUBLIC_KEY_BYTES], MeshcoreClientPacketError>
{
    let (bytes, len) = parse_hex_bytes::<MESHCORE_PUBLIC_KEY_BYTES>(value)?;
    if len == MESHCORE_PUBLIC_KEY_BYTES {
        Ok(bytes)
    } else {
        Err(MeshcoreClientPacketError::InvalidKey)
    }
}

/// Parsed raw MeshCore packet view.
struct WirePacket<'a>
{
    /// MeshCore payload type from the packet header.
    payload_type: u8,
    /// MeshCore payload body after route and path metadata.
    payload:      &'a [u8],
}

/// Parse a raw MeshCore packet into payload type and body.
fn parse_wire_packet(raw: &[u8]) -> Result<WirePacket<'_>, MeshcoreClientPacketError>
{
    let header = *raw.first().ok_or(MeshcoreClientPacketError::Decode)?;
    let route_type = header & WIRE_ROUTE_MASK;
    let payload_type = (header >> WIRE_PAYLOAD_KIND_SHIFT) & WIRE_PAYLOAD_KIND_MASK;
    let mut index = WIRE_HEADER_BYTES;

    if route_type == MESHCORE_ROUTE_TRANSPORT_FLOOD || route_type == MESHCORE_ROUTE_TRANSPORT_DIRECT
    {
        index = index.saturating_add(WIRE_TRANSPORT_HEADER_BYTES);
    }

    let path_len = *raw.get(index).ok_or(MeshcoreClientPacketError::Decode)?;
    index = index.saturating_add(WIRE_PATH_LEN_BYTES);
    let path_bytes = path_byte_len(path_len)?;
    index = index.saturating_add(path_bytes);
    let payload = raw.get(index..).ok_or(MeshcoreClientPacketError::Decode)?;
    if payload.is_empty() {
        return Err(MeshcoreClientPacketError::Decode);
    }

    Ok(WirePacket {
        payload_type,
        payload,
    })
}

/// Decrypt a MeshCore datagram body.
fn decrypt_datagram_payload(
    identity: &MeshcoreIdentity,
    contact: &MeshcoreContact,
    payload: &[u8],
) -> Result<MeshcoreResponse, MeshcoreClientPacketError>
{
    let dst_hash = identity.hash(contact.route)?;
    let src_hash = contact.hash()?;
    let headers_len = dst_hash.len().saturating_add(src_hash.len());
    if payload.len() <= headers_len {
        return Err(MeshcoreClientPacketError::Decode);
    }
    if payload.get(..dst_hash.len()) != Some(dst_hash.as_slice()) {
        return Err(MeshcoreClientPacketError::NotForGateway);
    }
    let src_start = dst_hash.len();
    let src_end = src_start.saturating_add(src_hash.len());
    if payload.get(src_start..src_end) != Some(src_hash.as_slice()) {
        return Err(MeshcoreClientPacketError::WrongSource);
    }

    let secret = identity.shared_secret(&contact.public_key)?;
    decrypt_mac_then_decrypt(&secret, &payload[src_end..])
}

/// Decrypt a MeshCore PATH response and extract its embedded response body.
fn decrypt_path_response(
    identity: &MeshcoreIdentity,
    contact: &MeshcoreContact,
    payload: &[u8],
) -> Result<MeshcoreResponse, MeshcoreClientPacketError>
{
    let plain = decrypt_datagram_payload(identity, contact, payload)?;
    let data = plain.as_slice();
    let encoded_path_len = *data.first().ok_or(MeshcoreClientPacketError::Decode)?;
    let mut index = WIRE_PATH_LEN_BYTES.saturating_add(path_byte_len(encoded_path_len)?);
    let extra_type =
        *data.get(index).ok_or(MeshcoreClientPacketError::Decode)? & WIRE_PAYLOAD_KIND_MASK;
    index = index.saturating_add(WIRE_HEADER_BYTES);
    if extra_type != MESH_KIND_RESPONSE {
        return Err(MeshcoreClientPacketError::Decode);
    }
    let extra = data.get(index..).ok_or(MeshcoreClientPacketError::Decode)?;
    let mut out = [0u8; MESH_PAYLOAD_MAX];
    let len = extra.len().min(out.len());
    out[..len].copy_from_slice(&extra[..len]);
    Ok(MeshcoreResponse { data: out, len })
}

/// Build a complete raw MeshCore wire frame.
fn build_wire_frame(
    payload_type: u8,
    route: MeshcoreRequestRoute,
    body: &[u8],
) -> Result<MeshTxFrame, MeshcoreClientPacketError>
{
    let mut raw = Vec::<u8, MESH_PAYLOAD_MAX>::new();
    raw.push((payload_type << WIRE_PAYLOAD_KIND_SHIFT) | route.route_type())
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    raw.push(route.encoded_empty_path_len()?)
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    raw.extend_from_slice(body)
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    MeshTxFrame::from_slice(payload_type, raw.as_slice())
        .ok_or(MeshcoreClientPacketError::PayloadTooLarge)
}

/// Build a flood advert frame with MeshCore's untyped zero-hop advert path.
fn build_advert_wire_frame(body: &[u8]) -> Result<MeshTxFrame, MeshcoreClientPacketError>
{
    let mut raw = Vec::<u8, MESH_PAYLOAD_MAX>::new();
    raw.push((MESH_KIND_ADVERT << WIRE_PAYLOAD_KIND_SHIFT) | MESHCORE_ROUTE_FLOOD)
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    raw.push(0)
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    raw.extend_from_slice(body)
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    MeshTxFrame::from_slice(MESH_KIND_ADVERT, raw.as_slice())
        .ok_or(MeshcoreClientPacketError::PayloadTooLarge)
}

/// Encrypt with AES-128-ECB and append MeshCore's 2-byte HMAC prefix.
fn encrypt_then_mac(
    shared_secret: &[u8; 32],
    plain: &[u8],
    out_payload: &mut Vec<u8, MESH_PAYLOAD_MAX>,
) -> Result<(), MeshcoreClientPacketError>
{
    let key: &[u8; AES_BLOCK_BYTES] = shared_secret[..AES_BLOCK_BYTES]
        .try_into()
        .map_err(|_| MeshcoreClientPacketError::InvalidKey)?;
    let cipher = aes::Aes128::new(key.into());

    let mac_start = out_payload.len();
    for _ in 0..MAC_PREFIX_BYTES {
        out_payload
            .push(0)
            .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    }
    let cipher_start = out_payload.len();

    let full_blocks = plain.len() / AES_BLOCK_BYTES;
    for index in 0..full_blocks {
        let mut block = [0u8; AES_BLOCK_BYTES];
        let start = index * AES_BLOCK_BYTES;
        block.copy_from_slice(&plain[start..start + AES_BLOCK_BYTES]);
        let mut block = Array::from(block);
        cipher.encrypt_block(&mut block);
        out_payload
            .extend_from_slice(block.as_slice())
            .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    }

    let remainder = plain.len() % AES_BLOCK_BYTES;
    if remainder > 0 {
        let mut block = [0u8; AES_BLOCK_BYTES];
        block[..remainder].copy_from_slice(&plain[full_blocks * AES_BLOCK_BYTES..]);
        let mut block = Array::from(block);
        cipher.encrypt_block(&mut block);
        out_payload
            .extend_from_slice(block.as_slice())
            .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    }

    type HmacSha256 = Hmac<Sha256>;
    let mut hmac = <HmacSha256 as HmacKeyInit>::new_from_slice(shared_secret)
        .map_err(|_| MeshcoreClientPacketError::InvalidKey)?;
    hmac.update(&out_payload[cipher_start..]);
    let digest = hmac.finalize().into_bytes();
    out_payload[mac_start] = digest[0];
    out_payload[mac_start + 1] = digest[1];
    Ok(())
}

/// Append a null-terminated advert name within MeshCore appdata capacity.
fn push_advert_name(
    appdata: &mut Vec<u8, MESHCORE_ADVERT_APPDATA_MAX>,
    name: &str,
) -> Result<(), MeshcoreClientPacketError>
{
    let reserve = 1;
    for ch in name.chars() {
        let mut utf8 = [0u8; 4];
        let bytes = ch.encode_utf8(&mut utf8).as_bytes();
        if appdata.len() + bytes.len() + reserve > appdata.capacity() {
            break;
        }
        appdata
            .extend_from_slice(bytes)
            .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)?;
    }
    appdata
        .push(0)
        .map_err(|_| MeshcoreClientPacketError::PayloadTooLarge)
}

/// Verify MeshCore's 2-byte HMAC prefix and decrypt AES-128-ECB blocks.
fn decrypt_mac_then_decrypt(
    shared_secret: &[u8; 32],
    source: &[u8],
) -> Result<MeshcoreResponse, MeshcoreClientPacketError>
{
    if source.len() <= MAC_PREFIX_BYTES {
        return Err(MeshcoreClientPacketError::Decode);
    }
    let mac = &source[..MAC_PREFIX_BYTES];
    let ciphertext = &source[MAC_PREFIX_BYTES..];
    if ciphertext.is_empty() || !ciphertext.len().is_multiple_of(AES_BLOCK_BYTES) {
        return Err(MeshcoreClientPacketError::Decode);
    }

    type HmacSha256 = Hmac<Sha256>;
    let mut hmac = <HmacSha256 as HmacKeyInit>::new_from_slice(shared_secret)
        .map_err(|_| MeshcoreClientPacketError::InvalidKey)?;
    hmac.update(ciphertext);
    let digest = hmac.finalize().into_bytes();
    if digest[0] != mac[0] || digest[1] != mac[1] {
        return Err(MeshcoreClientPacketError::Mac);
    }

    let key: &[u8; AES_BLOCK_BYTES] = shared_secret[..AES_BLOCK_BYTES]
        .try_into()
        .map_err(|_| MeshcoreClientPacketError::InvalidKey)?;
    let cipher = aes::Aes128::new(key.into());
    let mut data = [0u8; MESH_PAYLOAD_MAX];
    if ciphertext.len() > data.len() {
        return Err(MeshcoreClientPacketError::PayloadTooLarge);
    }
    data[..ciphertext.len()].copy_from_slice(ciphertext);
    for block in data[..ciphertext.len()].chunks_exact_mut(AES_BLOCK_BYTES) {
        let mut raw_block = [0u8; AES_BLOCK_BYTES];
        raw_block.copy_from_slice(block);
        let mut decoded = Array::from(raw_block);
        cipher.decrypt_block(&mut decoded);
        block.copy_from_slice(decoded.as_slice());
    }

    Ok(MeshcoreResponse {
        data,
        len: ciphertext.len(),
    })
}

/// Parse up to `N` bytes from an exact hex string.
fn parse_hex_bytes<const N: usize>(
    value: &str,
) -> Result<([u8; N], usize), MeshcoreClientPacketError>
{
    let bytes = value.as_bytes();
    if !bytes.len().is_multiple_of(2) || bytes.len() / 2 > N {
        return Err(MeshcoreClientPacketError::InvalidKey);
    }

    let mut out = [0u8; N];
    for (index, chunk) in bytes.chunks_exact(2).enumerate() {
        let high = hex_nibble(chunk[0]).ok_or(MeshcoreClientPacketError::InvalidKey)?;
        let low = hex_nibble(chunk[1]).ok_or(MeshcoreClientPacketError::InvalidKey)?;
        out[index] = (high << 4) | low;
    }
    Ok((out, bytes.len() / 2))
}

/// Convert one ASCII hex digit to a nibble.
fn hex_nibble(byte: u8) -> Option<u8>
{
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Encode MeshCore path length mode and path count.
const fn encode_path_len(
    path_hash_size: u8,
    path_hash_count: u8,
) -> Result<u8, MeshcoreClientPacketError>
{
    if path_hash_size < MESHCORE_MIN_PATH_HASH_BYTES
        || path_hash_size > MESHCORE_MAX_PATH_HASH_BYTES
        || path_hash_count > PATH_HASH_COUNT_MAX
    {
        return Err(MeshcoreClientPacketError::UnsupportedRoute);
    }
    Ok(((path_hash_size - MESHCORE_MIN_PATH_HASH_BYTES) << 6)
        | (path_hash_count & PATH_HASH_COUNT_MASK))
}

/// Return path byte length for an encoded MeshCore path length byte.
const fn path_byte_len(path_len: u8) -> Result<usize, MeshcoreClientPacketError>
{
    let hash_size = ((path_len >> 6) & WIRE_ROUTE_MASK) + MESHCORE_MIN_PATH_HASH_BYTES;
    if hash_size > MESHCORE_MAX_PATH_HASH_BYTES {
        return Err(MeshcoreClientPacketError::UnsupportedRoute);
    }
    let hash_count = path_len & PATH_HASH_COUNT_MASK;
    Ok(hash_size as usize * hash_count as usize)
}

#[cfg(test)]
mod tests
{
    use ed25519_dalek::SigningKey;
    use heapless::String;

    use super::{
        MeshcoreContact,
        MeshcoreIdentity,
        MeshcoreRequestRoute,
        MeshcoreResponse,
        build_gateway_advert,
        build_login_request,
        build_telemetry_request,
        login_response_is_success,
        response_is_login_success,
    };
    use crate::MESH_PAYLOAD_MAX;

    #[test]
    fn builds_login_and_telemetry_frames()
    {
        let identity = MeshcoreIdentity::from_hex(
            "f582bb6c7ac6f647c187c57b85906f9cbf51d4871ee41d080d4e65bec6cc37a8",
            "d8fc9a3ad0d8d018d2ca85dfc8eb9ecab3fa7ad5cb17c521288f6995326a1f784360c5a2dc907c653b0ffdb55e87b02cb245197a3768ecddef9347546c72491e",
        )
        .unwrap();
        let contact = MeshcoreContact::from_hex(
            "f582bb6c7ac6f647c187c57b85906f9cbf51d4871ee41d080d4e65bec6cc37a8",
            MeshcoreRequestRoute::Flood { path_hash_size: 3 },
        )
        .unwrap();

        let login = build_login_request(&identity, &contact, "secret", 1).unwrap();
        let telemetry = build_telemetry_request(&identity, &contact, 2, 3).unwrap();

        assert_eq!(login.payload_slice()[0], (0x07 << 2) | 0x01);
        assert_eq!(login.payload_slice()[1], 0x80);
        assert_eq!(telemetry.payload_slice()[0], 0x01);
        assert_eq!(telemetry.payload_slice()[1], 0x80);
    }

    #[test]
    fn accepts_seed_private_key_form()
    {
        let seed = [7u8; 32];
        let signing = SigningKey::from_bytes(&seed);
        let public_hex = hex(&signing.verifying_key().to_bytes());
        let seed_hex = hex(&seed);

        assert!(MeshcoreIdentity::from_hex(&public_hex, &seed_hex).is_ok());
    }

    #[test]
    fn builds_signed_gateway_advert()
    {
        let identity = MeshcoreIdentity::from_hex(
            "f582bb6c7ac6f647c187c57b85906f9cbf51d4871ee41d080d4e65bec6cc37a8",
            "d8fc9a3ad0d8d018d2ca85dfc8eb9ecab3fa7ad5cb17c521288f6995326a1f784360c5a2dc907c653b0ffdb55e87b02cb245197a3768ecddef9347546c72491e",
        )
        .unwrap();

        let advert = build_gateway_advert(&identity, "gate", 7).unwrap();
        let payload = advert.payload_slice();

        assert_eq!(advert.kind, 0x04);
        assert_eq!(payload[0], (0x04 << 2) | 0x01);
        assert_eq!(payload[1], 0);
        assert_eq!(&payload[2..34], &identity.public_key);
        assert_eq!(&payload[34..38], &7_u32.to_le_bytes());
        assert_eq!(payload[102], 0x81);
        assert_eq!(&payload[103..108], b"gate\0");
    }

    #[test]
    fn accepts_login_ok_with_repeater_timestamp()
    {
        let response = login_response(0x1234_5678, b"OK");

        assert_eq!(login_response_is_success(&response), Ok(()));
        assert!(response_is_login_success(&response));
    }

    #[test]
    fn accepts_legacy_binary_login_ok_marker()
    {
        let response = login_response(0x90ab_cdef, &[0]);

        assert_eq!(login_response_is_success(&response), Ok(()));
    }

    fn login_response(tag: u32, status: &[u8]) -> MeshcoreResponse
    {
        let mut data = [0u8; MESH_PAYLOAD_MAX];
        data[..4].copy_from_slice(&tag.to_le_bytes());
        data[4..4 + status.len()].copy_from_slice(status);
        MeshcoreResponse {
            data,
            len: 4 + status.len(),
        }
    }

    fn hex(bytes: &[u8]) -> String<128>
    {
        let mut out = String::new();
        for byte in bytes {
            out.push(nibble(byte >> 4)).unwrap();
            out.push(nibble(byte & 0x0f)).unwrap();
        }
        out
    }

    fn nibble(value: u8) -> char
    {
        match value {
            0..=9 => char::from(b'0' + value),
            _ => char::from(b'a' + value - 10),
        }
    }
}
