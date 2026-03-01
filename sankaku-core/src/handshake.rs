use anyhow::{Result, bail};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy)]
pub struct StaticSecret([u8; 32]);

#[derive(Debug, Clone, Copy)]
pub struct PublicKey([u8; 32]);

#[derive(Debug, Clone, Copy)]
pub struct SharedSecret([u8; 32]);

impl StaticSecret {
    pub fn random_from_rng(mut rng: impl RngCore) -> Self {
        let mut secret = [0u8; 32];
        rng.fill_bytes(&mut secret);
        Self(secret)
    }

    pub fn diffie_hellman(&self, peer: &PublicKey) -> SharedSecret {
        // Compatibility-only symmetric shared secret derivation for deprecated handshake APIs.
        let (first, second) = if self.0 <= peer.0 {
            (self.0, peer.0)
        } else {
            (peer.0, self.0)
        };
        let mut material = [0u8; 64];
        material[..32].copy_from_slice(&first);
        material[32..].copy_from_slice(&second);
        let derived = pseudo_prf(
            &material,
            b"sankaku/legacy-kx/shared/v1",
            b"legacy-kx",
            32,
        );
        let mut shared = [0u8; 32];
        shared.copy_from_slice(&derived);
        SharedSecret(shared)
    }
}

impl PublicKey {
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }
}

impl SharedSecret {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<&StaticSecret> for PublicKey {
    fn from(secret: &StaticSecret) -> Self {
        let derived = pseudo_prf(
            &secret.0,
            b"sankaku/legacy-kx/public/v1",
            b"legacy-public",
            32,
        );
        let mut public = [0u8; 32];
        public.copy_from_slice(&derived);
        Self(public)
    }
}

impl From<[u8; 32]> for PublicKey {
    fn from(value: [u8; 32]) -> Self {
        Self(value)
    }
}

/// Protocol version used by the handshake.
pub const PROTOCOL_VERSION: u16 = 2;
/// Baseline capability bit for interoperable peers.
pub const PROTOCOL_BASELINE_CAPS: u16 = 0x0001;
/// Capability bit signaling support for ticket-based session resumption.
pub const PROTOCOL_CAP_RESUMPTION: u16 = 0x0002;
/// Cipher-suite negotiation slot (current implementation default).
pub const CIPHER_SUITE_DEFAULT: u8 = 0x01;
const HANDSHAKE_DOMAIN: &[u8] = b"sankaku/handshake/v2";
const TICKET_DOMAIN: &[u8] = b"sankaku/ticket/v1";
const RESUME_DOMAIN: &[u8] = b"sankaku/resume/v1";

const TAG_SIZE: usize = 16;
const TAG_LABEL_CLIENT: u8 = 0x43; // 'C'
const TAG_LABEL_SERVER: u8 = 0x53; // 'S'

/// The initiator/respondent role used for directional key assignment.
#[derive(Debug, Clone, Copy)]
pub enum HandshakeRole {
    Client,
    Server,
}

/// Transcript fields used for context binding.
#[derive(Debug, Clone, Copy)]
pub struct HandshakeContext {
    pub protocol_version: u16,
    pub capabilities: u16,
    pub cipher_suite: u8,
    pub session_id: u64,
    pub client_public: [u8; 32],
    pub server_public: [u8; 32],
}

/// Directional keys split by protocol purpose.
#[derive(Debug, Clone, Copy)]
pub struct SessionKeys {
    pub payload_tx: [u8; 32],
    pub payload_rx: [u8; 32],
    pub header_tx: [u8; 32],
    pub header_rx: [u8; 32],
}

/// Client-storable resumption ticket.
///
/// `identity` is an opaque server-encrypted blob.
/// `resumption_secret` is the client-side secret used to build a resume binder and keys.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionTicket {
    pub identity: Vec<u8>,
    pub resumption_secret: [u8; 32],
    pub expires_at: u64,
}

/// Client hello used for 0-RTT ticket resumption.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ResumePacket {
    pub protocol_version: u16,
    pub session_id: u64,
    pub ticket_identity: Vec<u8>,
    pub expires_at: u64,
    pub client_nonce: [u8; 24],
    pub binder: [u8; TAG_SIZE],
}

impl ResumePacket {
    /// Builds a 0-RTT resume packet from a previously stored ticket.
    pub fn new_client(session_id: u64, ticket: &SessionTicket) -> Self {
        let mut client_nonce = [0u8; 24];
        OsRng.fill_bytes(&mut client_nonce);

        let binder = compute_resumption_binder(
            &ticket.resumption_secret,
            session_id,
            &ticket.identity,
            ticket.expires_at,
            client_nonce,
        );

        Self {
            protocol_version: PROTOCOL_VERSION,
            session_id,
            ticket_identity: ticket.identity.clone(),
            expires_at: ticket.expires_at,
            client_nonce,
            binder,
        }
    }

    /// Verifies ticket freshness and binder validity using the ticket's resumption secret.
    pub fn verify(&self, resumption_secret: &[u8; 32], now_secs: u64) -> bool {
        if self.protocol_version != PROTOCOL_VERSION {
            return false;
        }
        if self.expires_at < now_secs {
            return false;
        }

        let expected = compute_resumption_binder(
            resumption_secret,
            self.session_id,
            &self.ticket_identity,
            self.expires_at,
            self.client_nonce,
        );
        constant_time_eq(&expected, &self.binder)
    }
}

/// Result of validating an opaque ticket identity on the server side.
#[derive(Debug, Clone, Copy)]
pub struct ValidatedTicket {
    /// Server-generated ticket identifier used for anti-replay tracking.
    pub ticket_id: [u8; 16],
    /// Stable client identifier used for bounded server-side known-client maps.
    pub client_id: [u8; 16],
    pub resumption_secret: [u8; 32],
    pub expires_at: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
struct TicketIdentityFields {
    protocol_version: u16,
    #[serde(default)]
    ticket_id: [u8; 16],
    #[serde(default)]
    client_id: [u8; 16],
    expires_at: u64,
    resumption_secret: [u8; 32],
}

/// The packet sent over the wire to establish and authenticate a session.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HandshakePacket {
    pub protocol_version: u16,
    pub capabilities: u16,
    #[serde(default = "default_cipher_suite")]
    pub cipher_suite: u8,
    pub session_id: u64,
    pub public_key: [u8; 32],
    pub auth_tag: [u8; TAG_SIZE],
    #[serde(default)]
    pub session_ticket: Option<SessionTicket>,
}

impl HandshakePacket {
    /// Builds a client hello carrying the ephemeral public key.
    pub fn new_client(session_id: u64, public_key: [u8; 32]) -> Self {
        let protocol_version = PROTOCOL_VERSION;
        let capabilities = PROTOCOL_BASELINE_CAPS | PROTOCOL_CAP_RESUMPTION;
        let cipher_suite = CIPHER_SUITE_DEFAULT;
        let auth_tag = compute_client_tag(
            protocol_version,
            capabilities,
            cipher_suite,
            session_id,
            public_key,
        );
        Self {
            protocol_version,
            capabilities,
            cipher_suite,
            session_id,
            public_key,
            auth_tag,
            session_ticket: None,
        }
    }

    /// Builds a server hello carrying the ephemeral public key.
    pub fn new_server(
        session_id: u64,
        server_public: [u8; 32],
        client_public: [u8; 32],
        session_ticket: Option<SessionTicket>,
    ) -> Self {
        let protocol_version = PROTOCOL_VERSION;
        let capabilities = PROTOCOL_BASELINE_CAPS | PROTOCOL_CAP_RESUMPTION;
        let cipher_suite = CIPHER_SUITE_DEFAULT;
        let auth_tag = compute_server_tag(
            protocol_version,
            capabilities,
            cipher_suite,
            session_id,
            client_public,
            server_public,
        );
        Self {
            protocol_version,
            capabilities,
            cipher_suite,
            session_id,
            public_key: server_public,
            auth_tag,
            session_ticket,
        }
    }

    /// Verifies the client hello tag and mandatory capability bits.
    pub fn verify_client(&self) -> bool {
        if self.protocol_version != PROTOCOL_VERSION {
            return false;
        }
        if self.capabilities & PROTOCOL_BASELINE_CAPS == 0 {
            return false;
        }
        if !is_supported_cipher_suite(self.cipher_suite) {
            return false;
        }

        let expected = compute_client_tag(
            self.protocol_version,
            self.capabilities,
            self.cipher_suite,
            self.session_id,
            self.public_key,
        );
        constant_time_eq(&expected, &self.auth_tag)
    }

    /// Verifies the server hello tag against the known client key.
    pub fn verify_server(&self, client_public: [u8; 32]) -> bool {
        if self.protocol_version != PROTOCOL_VERSION {
            return false;
        }
        if self.capabilities & PROTOCOL_BASELINE_CAPS == 0 {
            return false;
        }
        if !is_supported_cipher_suite(self.cipher_suite) {
            return false;
        }

        let expected = compute_server_tag(
            self.protocol_version,
            self.capabilities,
            self.cipher_suite,
            self.session_id,
            client_public,
            self.public_key,
        );
        constant_time_eq(&expected, &self.auth_tag)
    }
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn default_cipher_suite() -> u8 {
    CIPHER_SUITE_DEFAULT
}

fn is_supported_cipher_suite(cipher_suite: u8) -> bool {
    cipher_suite == CIPHER_SUITE_DEFAULT
}

fn constant_time_eq(left: &[u8; TAG_SIZE], right: &[u8; TAG_SIZE]) -> bool {
    let mut diff = 0u8;
    for index in 0..TAG_SIZE {
        diff |= left[index] ^ right[index];
    }
    diff == 0
}

fn build_nonce(label: u8, protocol_version: u16, capabilities: u16, session_id: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[0] = label;
    nonce[1..9].copy_from_slice(&session_id.to_le_bytes());
    nonce[9..11].copy_from_slice(&protocol_version.to_le_bytes());
    nonce[11] = (capabilities as u8) ^ ((capabilities >> 8) as u8);
    nonce
}

fn build_resumption_nonce(label: u8, session_id: u64, client_nonce: [u8; 24]) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[0] = label;
    nonce[1..9].copy_from_slice(&session_id.to_le_bytes());
    nonce[9..12].copy_from_slice(&client_nonce[..3]);
    nonce
}

fn pseudo_prf(material: &[u8], aad: &[u8], nonce: &[u8], out_len: usize) -> Vec<u8> {
    let mut seed = std::collections::hash_map::DefaultHasher::new();
    material.hash(&mut seed);
    aad.hash(&mut seed);
    nonce.hash(&mut seed);
    let mut state = seed.finish();

    let mut out = vec![0u8; out_len];
    for (idx, byte) in out.iter_mut().enumerate() {
        state ^= ((idx as u64) + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        state = state.rotate_left(17) ^ 0xA076_1D64_78BD_642F;
        state = state.wrapping_mul(0x94D0_49BB_1331_11EB);
        *byte = (state >> ((idx % 8) * 8)) as u8;
    }
    out
}

fn build_client_tag_aad(
    protocol_version: u16,
    capabilities: u16,
    cipher_suite: u8,
    session_id: u64,
    client_public: [u8; 32],
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(HANDSHAKE_DOMAIN.len() + 2 + 2 + 1 + 8 + 32 + 6);
    aad.extend_from_slice(HANDSHAKE_DOMAIN);
    aad.extend_from_slice(b"/client");
    aad.extend_from_slice(&protocol_version.to_le_bytes());
    aad.extend_from_slice(&capabilities.to_le_bytes());
    aad.push(cipher_suite);
    aad.extend_from_slice(&session_id.to_le_bytes());
    aad.extend_from_slice(&client_public);
    aad
}

fn build_server_tag_aad(
    protocol_version: u16,
    capabilities: u16,
    cipher_suite: u8,
    session_id: u64,
    client_public: [u8; 32],
    server_public: [u8; 32],
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(HANDSHAKE_DOMAIN.len() + 2 + 2 + 1 + 8 + 32 + 32 + 6);
    aad.extend_from_slice(HANDSHAKE_DOMAIN);
    aad.extend_from_slice(b"/server");
    aad.extend_from_slice(&protocol_version.to_le_bytes());
    aad.extend_from_slice(&capabilities.to_le_bytes());
    aad.push(cipher_suite);
    aad.extend_from_slice(&session_id.to_le_bytes());
    aad.extend_from_slice(&client_public);
    aad.extend_from_slice(&server_public);
    aad
}

fn compute_tag(material: &[u8; 32], nonce: [u8; 12], aad: &[u8]) -> [u8; TAG_SIZE] {
    let tag = pseudo_prf(material, aad, &nonce, TAG_SIZE);
    let mut out = [0u8; TAG_SIZE];
    out.copy_from_slice(&tag);
    out
}

fn compute_client_tag(
    protocol_version: u16,
    capabilities: u16,
    cipher_suite: u8,
    session_id: u64,
    client_public: [u8; 32],
) -> [u8; TAG_SIZE] {
    let nonce = build_nonce(TAG_LABEL_CLIENT, protocol_version, capabilities, session_id);
    let aad = build_client_tag_aad(
        protocol_version,
        capabilities,
        cipher_suite,
        session_id,
        client_public,
    );
    compute_tag(&client_public, nonce, &aad)
}

fn compute_server_tag(
    protocol_version: u16,
    capabilities: u16,
    cipher_suite: u8,
    session_id: u64,
    client_public: [u8; 32],
    server_public: [u8; 32],
) -> [u8; TAG_SIZE] {
    let mut material = client_public;
    for (index, byte) in server_public.iter().enumerate() {
        material[index] ^= byte;
    }

    let nonce = build_nonce(TAG_LABEL_SERVER, protocol_version, capabilities, session_id);
    let aad = build_server_tag_aad(
        protocol_version,
        capabilities,
        cipher_suite,
        session_id,
        client_public,
        server_public,
    );
    compute_tag(&material, nonce, &aad)
}

fn transcript_aad(context: &HandshakeContext, label: &[u8]) -> Vec<u8> {
    let mut aad =
        Vec::with_capacity(HANDSHAKE_DOMAIN.len() + 2 + 2 + 1 + 8 + 32 + 32 + label.len());
    aad.extend_from_slice(HANDSHAKE_DOMAIN);
    aad.extend_from_slice(label);
    aad.extend_from_slice(&context.protocol_version.to_le_bytes());
    aad.extend_from_slice(&context.capabilities.to_le_bytes());
    aad.push(context.cipher_suite);
    aad.extend_from_slice(&context.session_id.to_le_bytes());
    aad.extend_from_slice(&context.client_public);
    aad.extend_from_slice(&context.server_public);
    aad
}

fn derive_key_material(
    shared_secret: [u8; 32],
    context: &HandshakeContext,
    nonce_label: u8,
    label: &[u8],
) -> Result<[u8; 32]> {
    let nonce = build_nonce(
        nonce_label,
        context.protocol_version,
        context.capabilities,
        context.session_id,
    );
    let aad = transcript_aad(context, label);
    let encrypted = pseudo_prf(&shared_secret, &aad, &nonce, 32);
    let mut out = [0u8; 32];
    out.copy_from_slice(&encrypted);
    Ok(out)
}

fn resumption_binder_aad(
    session_id: u64,
    ticket_identity: &[u8],
    expires_at: u64,
    client_nonce: [u8; 24],
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(RESUME_DOMAIN.len() + 8 + 8 + 24 + ticket_identity.len());
    aad.extend_from_slice(RESUME_DOMAIN);
    aad.extend_from_slice(b"/binder");
    aad.extend_from_slice(&session_id.to_le_bytes());
    aad.extend_from_slice(&expires_at.to_le_bytes());
    aad.extend_from_slice(&client_nonce);
    aad.extend_from_slice(ticket_identity);
    aad
}

fn compute_resumption_binder(
    resumption_secret: &[u8; 32],
    session_id: u64,
    ticket_identity: &[u8],
    expires_at: u64,
    client_nonce: [u8; 24],
) -> [u8; TAG_SIZE] {
    let nonce = build_resumption_nonce(0xD1, session_id, client_nonce);
    let aad = resumption_binder_aad(session_id, ticket_identity, expires_at, client_nonce);
    let tag = pseudo_prf(resumption_secret, &aad, &nonce, TAG_SIZE);
    let mut out = [0u8; TAG_SIZE];
    out.copy_from_slice(&tag);
    out
}

fn derive_resumption_key_material(
    resumption_secret: [u8; 32],
    session_id: u64,
    client_nonce: [u8; 24],
    nonce_label: u8,
    label: &[u8],
) -> Result<[u8; 32]> {
    let nonce = build_resumption_nonce(nonce_label, session_id, client_nonce);

    let mut aad = Vec::with_capacity(RESUME_DOMAIN.len() + label.len() + 8 + 24);
    aad.extend_from_slice(RESUME_DOMAIN);
    aad.extend_from_slice(label);
    aad.extend_from_slice(&session_id.to_le_bytes());
    aad.extend_from_slice(&client_nonce);
    let encrypted = pseudo_prf(&resumption_secret, &aad, &nonce, 32);
    let mut out = [0u8; 32];
    out.copy_from_slice(&encrypted);
    Ok(out)
}

/// Issues a resumable session ticket for a recently authenticated client.
pub fn issue_session_ticket(ticket_key: &[u8; 32], lifetime_secs: u64) -> Result<SessionTicket> {
    if lifetime_secs == 0 {
        bail!("ticket lifetime must be greater than zero");
    }

    let now = unix_now_secs();
    let expires_at = now.saturating_add(lifetime_secs);

    let mut resumption_secret = [0u8; 32];
    OsRng.fill_bytes(&mut resumption_secret);
    if resumption_secret == [0u8; 32] {
        resumption_secret[0] = 1;
    }
    let mut ticket_id = [0u8; 16];
    OsRng.fill_bytes(&mut ticket_id);
    let mut client_id = [0u8; 16];
    OsRng.fill_bytes(&mut client_id);

    let fields = TicketIdentityFields {
        protocol_version: PROTOCOL_VERSION,
        ticket_id,
        client_id,
        expires_at,
        resumption_secret,
    };
    let plaintext = bincode::serialize(&fields)?;

    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);

    let mut aad = Vec::with_capacity(TICKET_DOMAIN.len() + plaintext.len());
    aad.extend_from_slice(TICKET_DOMAIN);
    aad.extend_from_slice(&plaintext);
    let mac = pseudo_prf(ticket_key, &aad, &nonce_bytes, TAG_SIZE);

    let mut identity = Vec::with_capacity(12 + plaintext.len() + TAG_SIZE);
    identity.extend_from_slice(&nonce_bytes);
    identity.extend_from_slice(&plaintext);
    identity.extend_from_slice(&mac);

    Ok(SessionTicket {
        identity,
        resumption_secret,
        expires_at,
    })
}

/// Validates and decrypts an opaque ticket identity on the server.
pub fn validate_ticket_identity(
    ticket_key: &[u8; 32],
    identity: &[u8],
    now_secs: u64,
) -> Option<ValidatedTicket> {
    if identity.len() <= 12 + TAG_SIZE {
        return None;
    }
    let (nonce_bytes, payload_with_mac) = identity.split_at(12);
    let (plaintext, mac_bytes) = payload_with_mac.split_at(payload_with_mac.len() - TAG_SIZE);

    let mut aad = Vec::with_capacity(TICKET_DOMAIN.len() + plaintext.len());
    aad.extend_from_slice(TICKET_DOMAIN);
    aad.extend_from_slice(plaintext);
    let expected_mac = pseudo_prf(ticket_key, &aad, nonce_bytes, TAG_SIZE);
    let mut expected = [0u8; TAG_SIZE];
    expected.copy_from_slice(&expected_mac);
    let provided: [u8; TAG_SIZE] = mac_bytes.try_into().ok()?;
    if !constant_time_eq(&expected, &provided) {
        return None;
    }

    let fields: TicketIdentityFields = bincode::deserialize(&plaintext).ok()?;
    if fields.protocol_version != PROTOCOL_VERSION {
        return None;
    }
    if fields.expires_at < now_secs {
        return None;
    }
    let mut ticket_id = fields.ticket_id;
    if ticket_id == [0u8; 16] {
        ticket_id.copy_from_slice(&fields.resumption_secret[..16]);
    }
    let mut client_id = fields.client_id;
    if client_id == [0u8; 16] {
        client_id = ticket_id;
    }

    Some(ValidatedTicket {
        ticket_id,
        client_id,
        resumption_secret: fields.resumption_secret,
        expires_at: fields.expires_at,
    })
}

/// Derives purpose- and direction-scoped keys from the shared secret and transcript.
pub fn derive_session_keys(
    shared_secret: [u8; 32],
    role: HandshakeRole,
    context: &HandshakeContext,
) -> Result<SessionKeys> {
    let payload_c2s = derive_key_material(shared_secret, context, 0xA1, b"/payload/c2s")?;
    let payload_s2c = derive_key_material(shared_secret, context, 0xA2, b"/payload/s2c")?;
    let header_c2s = derive_key_material(shared_secret, context, 0xB1, b"/header/c2s")?;
    let header_s2c = derive_key_material(shared_secret, context, 0xB2, b"/header/s2c")?;

    let keys = match role {
        HandshakeRole::Client => SessionKeys {
            payload_tx: payload_c2s,
            payload_rx: payload_s2c,
            header_tx: header_c2s,
            header_rx: header_s2c,
        },
        HandshakeRole::Server => SessionKeys {
            payload_tx: payload_s2c,
            payload_rx: payload_c2s,
            header_tx: header_s2c,
            header_rx: header_c2s,
        },
    };

    Ok(keys)
}

/// Derives purpose- and direction-scoped keys for 0-RTT ticket resumption.
pub fn derive_resumption_session_keys(
    resumption_secret: [u8; 32],
    role: HandshakeRole,
    session_id: u64,
    client_nonce: [u8; 24],
) -> Result<SessionKeys> {
    let payload_c2s = derive_resumption_key_material(
        resumption_secret,
        session_id,
        client_nonce,
        0xC1,
        b"/payload/c2s",
    )?;
    let payload_s2c = derive_resumption_key_material(
        resumption_secret,
        session_id,
        client_nonce,
        0xC2,
        b"/payload/s2c",
    )?;
    let header_c2s = derive_resumption_key_material(
        resumption_secret,
        session_id,
        client_nonce,
        0xD1,
        b"/header/c2s",
    )?;
    let header_s2c = derive_resumption_key_material(
        resumption_secret,
        session_id,
        client_nonce,
        0xD2,
        b"/header/s2c",
    )?;

    let keys = match role {
        HandshakeRole::Client => SessionKeys {
            payload_tx: payload_c2s,
            payload_rx: payload_s2c,
            header_tx: header_c2s,
            header_rx: header_s2c,
        },
        HandshakeRole::Server => SessionKeys {
            payload_tx: payload_s2c,
            payload_rx: payload_c2s,
            header_tx: header_s2c,
            header_rx: header_c2s,
        },
    };

    Ok(keys)
}

pub struct KeyExchange {
    secret: StaticSecret,
    pub public: PublicKey,
}

impl KeyExchange {
    /// Generate a fresh, random keypair.
    pub fn new() -> Self {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        Self { secret, public }
    }

    /// Combine our Secret with their Public to get the shared secret bytes.
    pub fn derive_shared_secret(self, peer_public_bytes: [u8; 32]) -> [u8; 32] {
        let peer_public = PublicKey::from(peer_public_bytes);
        let shared = self.secret.diffie_hellman(&peer_public);
        *shared.as_bytes()
    }
}

impl Default for KeyExchange {
    fn default() -> Self {
        Self::new()
    }
}

/// Swappable handshake implementation boundary.
///
/// Default deployments use `DefaultHandshakeEngine`, while standardized
/// alternatives (for example DTLS-backed engines) can implement this trait.
pub trait HandshakeEngine: Send + Sync {
    fn build_client_hello(&self, session_id: u64, client_public: [u8; 32]) -> HandshakePacket;

    fn verify_client_hello(&self, packet: &HandshakePacket) -> bool;

    fn build_server_hello(
        &self,
        session_id: u64,
        server_public: [u8; 32],
        client_public: [u8; 32],
        session_ticket: Option<SessionTicket>,
    ) -> HandshakePacket;

    fn verify_server_hello(&self, packet: &HandshakePacket, client_public: [u8; 32]) -> bool;

    fn derive_session_keys(
        &self,
        shared_secret: [u8; 32],
        role: HandshakeRole,
        context: &HandshakeContext,
    ) -> Result<SessionKeys>;

    fn build_resume_packet(&self, session_id: u64, ticket: &SessionTicket) -> ResumePacket;

    fn verify_resume_packet(
        &self,
        packet: &ResumePacket,
        resumption_secret: &[u8; 32],
        now_secs: u64,
    ) -> bool;

    fn derive_resumption_session_keys(
        &self,
        resumption_secret: [u8; 32],
        role: HandshakeRole,
        session_id: u64,
        client_nonce: [u8; 24],
    ) -> Result<SessionKeys>;

    fn issue_session_ticket(
        &self,
        ticket_key: &[u8; 32],
        lifetime_secs: u64,
    ) -> Result<SessionTicket>;

    fn validate_ticket_identity(
        &self,
        ticket_key: &[u8; 32],
        identity: &[u8],
        now_secs: u64,
    ) -> Option<ValidatedTicket>;
}

/// Default Kyu2 handshake engine using authenticated X25519 + ticket resumption.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultHandshakeEngine;

impl HandshakeEngine for DefaultHandshakeEngine {
    fn build_client_hello(&self, session_id: u64, client_public: [u8; 32]) -> HandshakePacket {
        HandshakePacket::new_client(session_id, client_public)
    }

    fn verify_client_hello(&self, packet: &HandshakePacket) -> bool {
        packet.verify_client()
    }

    fn build_server_hello(
        &self,
        session_id: u64,
        server_public: [u8; 32],
        client_public: [u8; 32],
        session_ticket: Option<SessionTicket>,
    ) -> HandshakePacket {
        HandshakePacket::new_server(session_id, server_public, client_public, session_ticket)
    }

    fn verify_server_hello(&self, packet: &HandshakePacket, client_public: [u8; 32]) -> bool {
        packet.verify_server(client_public)
    }

    fn derive_session_keys(
        &self,
        shared_secret: [u8; 32],
        role: HandshakeRole,
        context: &HandshakeContext,
    ) -> Result<SessionKeys> {
        derive_session_keys(shared_secret, role, context)
    }

    fn build_resume_packet(&self, session_id: u64, ticket: &SessionTicket) -> ResumePacket {
        ResumePacket::new_client(session_id, ticket)
    }

    fn verify_resume_packet(
        &self,
        packet: &ResumePacket,
        resumption_secret: &[u8; 32],
        now_secs: u64,
    ) -> bool {
        packet.verify(resumption_secret, now_secs)
    }

    fn derive_resumption_session_keys(
        &self,
        resumption_secret: [u8; 32],
        role: HandshakeRole,
        session_id: u64,
        client_nonce: [u8; 24],
    ) -> Result<SessionKeys> {
        derive_resumption_session_keys(resumption_secret, role, session_id, client_nonce)
    }

    fn issue_session_ticket(
        &self,
        ticket_key: &[u8; 32],
        lifetime_secs: u64,
    ) -> Result<SessionTicket> {
        issue_session_ticket(ticket_key, lifetime_secs)
    }

    fn validate_ticket_identity(
        &self,
        ticket_key: &[u8; 32],
        identity: &[u8],
        now_secs: u64,
    ) -> Option<ValidatedTicket> {
        validate_ticket_identity(ticket_key, identity, now_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CIPHER_SUITE_DEFAULT, HandshakeContext, HandshakePacket, HandshakeRole,
        PROTOCOL_BASELINE_CAPS, PROTOCOL_VERSION, ResumePacket, derive_resumption_session_keys,
        derive_session_keys, issue_session_ticket, validate_ticket_identity,
    };

    #[test]
    fn authenticated_tags_reject_tampering() {
        let client_pub = [0x11; 32];
        let mut packet = HandshakePacket::new_client(7, client_pub);
        assert!(packet.verify_client());

        packet.session_id = 8;
        assert!(!packet.verify_client());
    }

    #[test]
    fn directional_key_derivation_matches_opposite_roles() {
        let shared_secret = [0x44; 32];
        let context = HandshakeContext {
            protocol_version: PROTOCOL_VERSION,
            capabilities: PROTOCOL_BASELINE_CAPS,
            cipher_suite: CIPHER_SUITE_DEFAULT,
            session_id: 1234,
            client_public: [0x10; 32],
            server_public: [0x20; 32],
        };

        let client = derive_session_keys(shared_secret, HandshakeRole::Client, &context)
            .expect("client derivation should succeed");
        let server = derive_session_keys(shared_secret, HandshakeRole::Server, &context)
            .expect("server derivation should succeed");

        assert_eq!(client.payload_tx, server.payload_rx);
        assert_eq!(client.payload_rx, server.payload_tx);
        assert_eq!(client.header_tx, server.header_rx);
        assert_eq!(client.header_rx, server.header_tx);
        assert_ne!(client.payload_tx, client.header_tx);
    }

    #[test]
    fn server_tag_binds_client_and_server_keys() {
        let client_pub = [0xAA; 32];
        let server_pub = [0xCC; 32];
        let packet = HandshakePacket::new_server(9, server_pub, client_pub, None);

        assert!(packet.verify_server(client_pub));
        assert!(!packet.verify_server([0xDD; 32]));
    }

    #[test]
    fn ticket_round_trip_and_resumption_keys_match() {
        let ticket_key = [0x55; 32];
        let ticket = issue_session_ticket(&ticket_key, 60).expect("ticket should be issued");
        let validated = validate_ticket_identity(
            &ticket_key,
            &ticket.identity,
            ticket.expires_at.saturating_sub(1),
        )
        .expect("ticket identity should validate");

        assert_ne!(validated.ticket_id, [0u8; 16]);
        assert_ne!(validated.client_id, [0u8; 16]);
        assert_eq!(validated.expires_at, ticket.expires_at);
        assert_eq!(validated.resumption_secret, ticket.resumption_secret);

        let resume = ResumePacket::new_client(88, &ticket);
        assert!(resume.verify(&validated.resumption_secret, ticket.expires_at - 1));

        let client = derive_resumption_session_keys(
            ticket.resumption_secret,
            HandshakeRole::Client,
            resume.session_id,
            resume.client_nonce,
        )
        .expect("client resumption key derivation should succeed");
        let server = derive_resumption_session_keys(
            validated.resumption_secret,
            HandshakeRole::Server,
            resume.session_id,
            resume.client_nonce,
        )
        .expect("server resumption key derivation should succeed");

        assert_eq!(client.payload_tx, server.payload_rx);
        assert_eq!(client.payload_rx, server.payload_tx);
        assert_eq!(client.header_tx, server.header_rx);
        assert_eq!(client.header_rx, server.header_tx);
    }

    #[test]
    fn resumption_binder_rejects_tampering() {
        let ticket_key = [0x99; 32];
        let ticket = issue_session_ticket(&ticket_key, 60).expect("ticket should be issued");
        let validated = validate_ticket_identity(
            &ticket_key,
            &ticket.identity,
            ticket.expires_at.saturating_sub(1),
        )
        .expect("ticket identity should validate");

        let mut resume = ResumePacket::new_client(144, &ticket);
        assert!(resume.verify(&validated.resumption_secret, ticket.expires_at - 1));

        resume.session_id = 145;
        assert!(!resume.verify(&validated.resumption_secret, ticket.expires_at - 1));
    }

    #[test]
    fn ticket_identity_contains_stable_unique_ticket_and_client_ids() {
        let ticket_key = [0x77; 32];
        let ticket_a = issue_session_ticket(&ticket_key, 60).expect("ticket a should be issued");
        let ticket_b = issue_session_ticket(&ticket_key, 60).expect("ticket b should be issued");

        let validated_a =
            validate_ticket_identity(&ticket_key, &ticket_a.identity, ticket_a.expires_at - 1)
                .expect("ticket a should validate");
        let validated_b =
            validate_ticket_identity(&ticket_key, &ticket_b.identity, ticket_b.expires_at - 1)
                .expect("ticket b should validate");

        assert_eq!(
            validated_a.ticket_id,
            validate_ticket_identity(&ticket_key, &ticket_a.identity, ticket_a.expires_at - 1)
                .expect("ticket a should validate repeatedly")
                .ticket_id
        );
        assert_eq!(
            validated_a.client_id,
            validate_ticket_identity(&ticket_key, &ticket_a.identity, ticket_a.expires_at - 1)
                .expect("ticket a should validate repeatedly")
                .client_id
        );
        assert_ne!(validated_a.ticket_id, validated_b.ticket_id);
        assert_ne!(validated_a.client_id, validated_b.client_id);
    }
}
