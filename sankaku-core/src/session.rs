use crate::handshake::{DefaultHandshakeEngine, HandshakeEngine, SessionTicket};
use crate::pipeline::{PipelineConfig, SankakuPipeline, VideoPayloadKind};
use crate::transport::{QuicTransport, SrtTransport};
use crate::{WirehairDecoder, WirehairEncoder};
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use quinn::{Connection, Endpoint};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::time::sleep;

const MAX_REDUNDANCY: f32 = 4.0;
const MAX_PROTECTED_BLOCK_SIZE: u32 = 512 * 1024;
const MAX_WIRE_PACKET_SIZE: usize = 65_507;
const TARGET_PACKET_SIZE: usize = 1150;
const ADAPTIVE_PADDING_ALIGN: usize = 32;
const GEOMETRY_HEADER_SIZE: usize = 24;
const DATA_PREFIX_SIZE: usize = 1 + 8 + GEOMETRY_HEADER_SIZE;
const AUDIO_PREFIX_SIZE: usize = 1 + 8 + 4 + 8 + 1 + 4 + 4;
const FEEDBACK_WINDOW: Duration = Duration::from_millis(500);
const MIN_VIDEO_BITRATE_BPS: u32 = 500_000;
const MAX_VIDEO_BITRATE_BPS: u32 = 8_000_000;
const AIMD_INCREASE_STEP_BPS: u32 = 50_000;
const AIMD_DECREASE_FACTOR: f32 = 0.80;
const DEFAULT_VIDEO_BITRATE_BPS: u32 = 2_000_000;
const TRANSPORT_PIPELINE_KEY: [u8; 32] = [0u8; 32];
const QUIC_DATAGRAM_FALLBACK_MAX: usize = 1_200;

/// Input handle for QUIC-native constructors.
pub enum QuicHandle {
    Endpoint(Endpoint),
    Connection(Connection),
}

impl From<Endpoint> for QuicHandle {
    fn from(endpoint: Endpoint) -> Self {
        Self::Endpoint(endpoint)
    }
}

impl From<Connection> for QuicHandle {
    fn from(connection: Connection) -> Self {
        Self::Connection(connection)
    }
}

pub const VIDEO_CODEC_HEVC: u8 = 0x01;
pub const VIDEO_CODEC_H264: u8 = 0x02;
pub const AUDIO_CODEC_OPUS: u8 = 0x03;
pub const AUDIO_CODEC_DEBUG_TEXT: u8 = 0x7E;

fn is_supported_video_codec(codec: u8) -> bool {
    matches!(codec, VIDEO_CODEC_HEVC | VIDEO_CODEC_H264)
}

fn is_supported_audio_codec(codec: u8) -> bool {
    matches!(codec, AUDIO_CODEC_OPUS | AUDIO_CODEC_DEBUG_TEXT)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StreamType {
    Audio = 0x01,
    Video = 0x02,
    ScreenShare = 0x03,
    Data = 0x04,
}

#[derive(Debug, Clone)]
pub struct StreamContext<T> {
    pub stream_type: StreamType,
    pub state: T,
}

#[derive(Debug, Clone)]
struct StreamRegistry<T> {
    entries: HashMap<u32, StreamContext<T>>,
}

impl<T> Default for StreamRegistry<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

impl<T> StreamRegistry<T> {
    fn get(&self, stream_id: &u32) -> Option<&T> {
        self.entries.get(stream_id).map(|ctx| &ctx.state)
    }

    fn get_mut(&mut self, stream_id: &u32) -> Option<&mut T> {
        self.entries.get_mut(stream_id).map(|ctx| &mut ctx.state)
    }

    fn insert(
        &mut self,
        stream_id: u32,
        stream_type: StreamType,
        state: T,
    ) -> Option<StreamContext<T>> {
        self.entries
            .insert(stream_id, StreamContext { stream_type, state })
    }

    fn remove(&mut self, stream_id: &u32) -> Option<StreamContext<T>> {
        self.entries.remove(stream_id)
    }

    fn ensure_with<F>(
        &mut self,
        stream_id: u32,
        stream_type: StreamType,
        build: F,
    ) -> Result<&mut T>
    where
        F: FnOnce() -> T,
    {
        match self.entries.entry(stream_id) {
            Entry::Occupied(occupied) => {
                let existing = occupied.into_mut();
                if existing.stream_type != stream_type {
                    bail!(
                        "stream id {stream_id} is registered as {:?}, not {:?}",
                        existing.stream_type,
                        stream_type
                    );
                }
                Ok(&mut existing.state)
            }
            Entry::Vacant(vacant) => {
                let inserted = vacant.insert(StreamContext {
                    stream_type,
                    state: build(),
                });
                Ok(&mut inserted.state)
            }
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PacketType {
    Data = b'D',
    Audio = 0x05,
    Ack = b'A',
    Ping = b'P',
    Pong = b'O',
    FecFeedback = b'F',
    StreamFin = b'E',
    Telemetry = b'T',
    Feedback = b'B',
}

impl PacketType {
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            b'D' => Some(Self::Data),
            0x05 => Some(Self::Audio),
            b'A' => Some(Self::Ack),
            b'P' => Some(Self::Ping),
            b'O' => Some(Self::Pong),
            b'F' => Some(Self::FecFeedback),
            b'E' => Some(Self::StreamFin),
            b'T' => Some(Self::Telemetry),
            b'B' => Some(Self::Feedback),
            _ => None,
        }
    }
}

const TYPE_DATA: u8 = PacketType::Data as u8;
const TYPE_AUDIO: u8 = PacketType::Audio as u8;
const TYPE_ACK: u8 = PacketType::Ack as u8;
const TYPE_PONG: u8 = PacketType::Pong as u8;
const TYPE_FEC_FEEDBACK: u8 = PacketType::FecFeedback as u8;
const TYPE_STREAM_FIN: u8 = PacketType::StreamFin as u8;
const TYPE_TELEMETRY: u8 = PacketType::Telemetry as u8;
const TYPE_FEEDBACK: u8 = PacketType::Feedback as u8;

/// Stable error taxonomy surfaced by stream APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KyuErrorCode {
    Config,
    Socket,
    HandshakeAuth,
    VersionMismatch,
    PacketMalformed,
    PacketRejected,
    Internal,
}

impl KyuErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Config => "CONFIG",
            Self::Socket => "SOCKET",
            Self::HandshakeAuth => "HANDSHAKE_AUTH",
            Self::VersionMismatch => "VERSION_MISMATCH",
            Self::PacketMalformed => "PACKET_MALFORMED",
            Self::PacketRejected => "PACKET_REJECTED",
            Self::Internal => "INTERNAL",
        }
    }

    pub fn from_quic_connection_error(error: &quinn::ConnectionError) -> Self {
        match error {
            quinn::ConnectionError::VersionMismatch => Self::VersionMismatch,
            quinn::ConnectionError::TransportError(_)
            | quinn::ConnectionError::ConnectionClosed(_)
            | quinn::ConnectionError::ApplicationClosed(_)
            | quinn::ConnectionError::Reset
            | quinn::ConnectionError::TimedOut
            | quinn::ConnectionError::LocallyClosed
            | quinn::ConnectionError::CidsExhausted => Self::Socket,
        }
    }

    pub fn from_quic_read_error(error: &quinn::ReadError) -> Self {
        match error {
            quinn::ReadError::ConnectionLost(connection) => {
                Self::from_quic_connection_error(connection)
            }
            quinn::ReadError::Reset(_)
            | quinn::ReadError::IllegalOrderedRead
            | quinn::ReadError::ClosedStream => Self::Socket,
            quinn::ReadError::ZeroRttRejected => Self::PacketRejected,
        }
    }

    pub fn from_quic_write_error(error: &quinn::WriteError) -> Self {
        match error {
            quinn::WriteError::ConnectionLost(connection) => {
                Self::from_quic_connection_error(connection)
            }
            quinn::WriteError::Stopped(_) | quinn::WriteError::ClosedStream => Self::Socket,
            quinn::WriteError::ZeroRttRejected => Self::PacketRejected,
        }
    }
}

/// Lightweight event stream for embeddings that need observability.
#[derive(Debug, Clone)]
pub enum KyuEvent {
    Log(String),
    HandshakeInitiated,
    HandshakeComplete,
    Progress {
        stream_id: u32,
        frame_index: u64,
        bytes: u64,
        frames: u64,
    },
    Fault {
        code: KyuErrorCode,
        message: String,
        session_id: Option<u64>,
        stream_id: Option<u32>,
    },
}

fn kyu_error_code_from_transport(error: &anyhow::Error) -> KyuErrorCode {
    if let Some(connection) = error.downcast_ref::<quinn::ConnectionError>() {
        return KyuErrorCode::from_quic_connection_error(connection);
    }
    if let Some(read) = error.downcast_ref::<quinn::ReadError>() {
        return KyuErrorCode::from_quic_read_error(read);
    }
    if let Some(write) = error.downcast_ref::<quinn::WriteError>() {
        return KyuErrorCode::from_quic_write_error(write);
    }
    KyuErrorCode::Socket
}

fn annotate_transport_error(context: &'static str, error: anyhow::Error) -> anyhow::Error {
    let code = kyu_error_code_from_transport(&error);
    error.context(format!("{context} [{}]", code.as_str()))
}

async fn resolve_quic_connection(
    handle: impl Into<QuicHandle>,
) -> Result<(Connection, Option<Endpoint>)> {
    match handle.into() {
        QuicHandle::Connection(connection) => Ok((connection, None)),
        QuicHandle::Endpoint(endpoint) => {
            let incoming = endpoint
                .accept()
                .await
                .context("endpoint closed before receiving a QUIC connection")?;
            let connection = incoming.await.map_err(|error| {
                annotate_transport_error("failed to accept QUIC connection", error.into())
            })?;
            Ok((connection, Some(endpoint)))
        }
    }
}

/// Indicates how the current sender session keys were bootstrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionBootstrapMode {
    Unknown,
    FullHandshake,
    ZeroRttResume,
}

/// Packet shaping policy for optional obfuscation.
#[derive(Debug, Clone, Copy)]
pub enum PaddingMode {
    Disabled,
    Fixed(usize),
    Adaptive { min: usize, max: usize },
}

impl Default for PaddingMode {
    fn default() -> Self {
        Self::Disabled
    }
}

/// Runtime FEC adaptation policy.
#[derive(Debug, Clone, Copy)]
pub enum FecPolicy {
    Fixed,
    Adaptive {
        min: f32,
        max: f32,
        increase_step: f32,
        decrease_step: f32,
        high_watermark: f32,
        low_watermark: f32,
    },
}

impl Default for FecPolicy {
    fn default() -> Self {
        Self::Adaptive {
            min: 1.0,
            max: MAX_REDUNDANCY,
            increase_step: 0.15,
            decrease_step: 0.05,
            high_watermark: 1.20,
            low_watermark: 1.05,
        }
    }
}

/// Sender/receiver transport behavior knobs.
#[derive(Debug, Clone, Copy)]
pub struct TransportConfig {
    pub pipeline: PipelineConfig,
    pub padding: PaddingMode,
    pub fec: FecPolicy,
    pub initial_redundancy: f32,
    pub max_bytes_per_sec: u64,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            pipeline: PipelineConfig::default(),
            padding: PaddingMode::Disabled,
            fec: FecPolicy::default(),
            initial_redundancy: 1.1,
            max_bytes_per_sec: 20_000_000,
        }
    }
}

/// Outbound frame data accepted by the sender.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub timestamp_us: u64,
    pub keyframe: bool,
    pub codec: u8,
    pub kind: VideoPayloadKind,
    pub payload: Vec<u8>,
}

impl VideoFrame {
    pub fn nal(payload: Vec<u8>, timestamp_us: u64, keyframe: bool) -> Self {
        Self::nal_with_codec(payload, timestamp_us, keyframe, VIDEO_CODEC_HEVC)
    }

    pub fn nal_with_codec(payload: Vec<u8>, timestamp_us: u64, keyframe: bool, codec: u8) -> Self {
        Self {
            timestamp_us,
            keyframe,
            codec,
            kind: VideoPayloadKind::NalUnit,
            payload,
        }
    }

    pub fn sao(payload: Vec<u8>, timestamp_us: u64) -> Self {
        Self {
            timestamp_us,
            keyframe: false,
            codec: VIDEO_CODEC_HEVC,
            kind: VideoPayloadKind::SaoParameters,
            payload,
        }
    }
}

/// Inbound frame data emitted by the receiver.
#[derive(Debug, Clone)]
pub struct InboundVideoFrame {
    pub session_id: u64,
    pub stream_id: u32,
    pub frame_index: u64,
    pub timestamp_us: u64,
    pub keyframe: bool,
    pub codec: u8,
    pub packet_loss_ratio: f32,
    pub kind: VideoPayloadKind,
    pub payload: Vec<u8>,
}

/// Inbound audio data emitted by the receiver.
#[derive(Debug, Clone)]
pub struct InboundAudioFrame {
    pub session_id: u64,
    pub stream_id: u32,
    pub timestamp_us: u64,
    pub codec: u8,
    pub frames_per_packet: u32,
    pub payload: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct FrameEnvelope {
    timestamp_us: u64,
    keyframe: bool,
    payload: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
struct FecFeedbackPacket {
    session_id: u64,
    stream_id: u32,
    frame_index: u64,
    ideal_packets: u32,
    used_packets: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
struct TelemetryPacket {
    session_id: u64,
    stream_id: u32,
    frame_index: u64,
    packet_loss_ppm: u32,
    jitter_us: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
struct FeedbackPacket {
    session_id: u64,
    stream_id: u32,
    loss_ratio: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
struct StreamFinPacket {
    session_id: u64,
    stream_id: u32,
    final_bytes: u64,
    final_frames: u64,
}

/// Converts a 64-char hex string into a 32-byte key.
pub fn parse_psk_hex(input: &str) -> Result<[u8; 32]> {
    let trimmed = input.trim();
    if trimmed.len() != 64 {
        bail!("PSK must be exactly 64 hex characters (32 bytes)");
    }

    let mut out = [0u8; 32];
    for (index, chunk) in trimmed.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(chunk).context("PSK contains non-UTF8 bytes")?;
        out[index] = u8::from_str_radix(pair, 16)
            .with_context(|| format!("PSK has invalid hex at byte index {index}"))?;
    }
    Ok(out)
}

fn random_ticket_key() -> [u8; 32] {
    let mut key = rand::random::<[u8; 32]>();
    if key == [0u8; 32] {
        key[0] = 1;
    }
    key
}

fn parse_u64_le(bytes: &[u8]) -> Option<u64> {
    let array: [u8; 8] = bytes.try_into().ok()?;
    Some(u64::from_le_bytes(array))
}

fn parse_u16_le(bytes: &[u8]) -> Option<u16> {
    let array: [u8; 2] = bytes.try_into().ok()?;
    Some(u16::from_le_bytes(array))
}

fn parse_u32_le(bytes: &[u8]) -> Option<u32> {
    let array: [u8; 4] = bytes.try_into().ok()?;
    Some(u32::from_le_bytes(array))
}

fn parse_header_u32(header: &[u8; GEOMETRY_HEADER_SIZE], start: usize) -> u32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&header[start..start + 4]);
    u32::from_le_bytes(bytes)
}

fn parse_header_u64(header: &[u8; GEOMETRY_HEADER_SIZE], start: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&header[start..start + 8]);
    u64::from_le_bytes(bytes)
}

fn clamp_padding_target(target: usize, min_len: usize) -> usize {
    target.max(min_len).min(MAX_WIRE_PACKET_SIZE)
}

fn target_packet_len(mode: PaddingMode, raw_len: usize) -> usize {
    match mode {
        PaddingMode::Disabled => raw_len,
        PaddingMode::Fixed(target) => clamp_padding_target(target, raw_len),
        PaddingMode::Adaptive { min, max } => {
            let aligned = raw_len.div_ceil(ADAPTIVE_PADDING_ALIGN) * ADAPTIVE_PADDING_ALIGN;
            clamp_padding_target(aligned.clamp(min.max(raw_len), max.max(min)), raw_len)
        }
    }
}

fn adjust_redundancy(current: f32, feedback: &FecFeedbackPacket, policy: FecPolicy) -> f32 {
    let observed = if feedback.ideal_packets == 0 {
        1.0
    } else {
        feedback.used_packets as f32 / feedback.ideal_packets as f32
    };

    match policy {
        FecPolicy::Fixed => current,
        FecPolicy::Adaptive {
            min,
            max,
            increase_step,
            decrease_step,
            high_watermark,
            low_watermark,
        } => {
            if observed > high_watermark {
                (current + increase_step).clamp(min, max)
            } else if observed < low_watermark {
                (current - decrease_step).clamp(min, max)
            } else {
                current.clamp(min, max)
            }
        }
    }
}

fn adjust_redundancy_with_telemetry(
    current: f32,
    telemetry: &TelemetryPacket,
    policy: FecPolicy,
) -> f32 {
    let synthetic = if telemetry.packet_loss_ppm >= 150_000 || telemetry.jitter_us >= 20_000 {
        FecFeedbackPacket {
            session_id: telemetry.session_id,
            stream_id: telemetry.stream_id,
            frame_index: telemetry.frame_index,
            ideal_packets: 100,
            used_packets: 150,
        }
    } else if telemetry.packet_loss_ppm <= 20_000 && telemetry.jitter_us <= 5_000 {
        FecFeedbackPacket {
            session_id: telemetry.session_id,
            stream_id: telemetry.stream_id,
            frame_index: telemetry.frame_index,
            ideal_packets: 100,
            used_packets: 100,
        }
    } else {
        FecFeedbackPacket {
            session_id: telemetry.session_id,
            stream_id: telemetry.stream_id,
            frame_index: telemetry.frame_index,
            ideal_packets: 100,
            used_packets: 110,
        }
    };
    adjust_redundancy(current, &synthetic, policy)
}

fn clamp_video_bitrate(bitrate_bps: u32) -> u32 {
    bitrate_bps.clamp(MIN_VIDEO_BITRATE_BPS, MAX_VIDEO_BITRATE_BPS)
}

fn apply_aimd_bitrate(current_bitrate_bps: u32, loss_ratio: f32) -> u32 {
    if loss_ratio > 0.02 {
        let decreased = ((current_bitrate_bps as f32) * AIMD_DECREASE_FACTOR).round() as u32;
        return clamp_video_bitrate(decreased);
    }

    if loss_ratio <= f32::EPSILON {
        return clamp_video_bitrate(current_bitrate_bps.saturating_add(AIMD_INCREASE_STEP_BPS));
    }

    clamp_video_bitrate(current_bitrate_bps)
}

struct Pacer {
    target_bytes_per_sec: u64,
    max_burst_bytes: f64,
    tokens: f64,
    last_refill: Instant,
}

impl Pacer {
    fn new(target_bytes_per_sec: u64) -> Self {
        let max_burst_bytes = ((target_bytes_per_sec as f64) * 0.25).max(16_384.0);
        Self {
            target_bytes_per_sec,
            max_burst_bytes,
            tokens: max_burst_bytes,
            last_refill: Instant::now(),
        }
    }

    async fn pace(&mut self, packet_size: usize, keyframe: bool, jitter_us: u32) {
        if self.target_bytes_per_sec == 0 {
            return;
        }

        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens =
            (self.tokens + elapsed * self.target_bytes_per_sec as f64).min(self.max_burst_bytes);

        let keyframe_budget = if keyframe { 2.5 } else { 1.0 };
        let jitter_penalty = 1.0 + (jitter_us as f64 / 200_000.0);
        let budget = (self.max_burst_bytes * keyframe_budget / jitter_penalty).max(1500.0);
        if self.tokens > budget {
            self.tokens = budget;
        }

        let needed = packet_size as f64;
        if self.tokens < needed {
            let deficit = needed - self.tokens;
            let sleep_secs = deficit / self.target_bytes_per_sec as f64;
            sleep(Duration::from_secs_f64(sleep_secs)).await;

            let now_after = Instant::now();
            let elapsed_after = now_after.duration_since(self.last_refill).as_secs_f64();
            self.last_refill = now_after;
            self.tokens = (self.tokens + elapsed_after * self.target_bytes_per_sec as f64)
                .min(self.max_burst_bytes);
        }

        self.tokens = (self.tokens - needed).max(0.0);
    }
}

#[derive(Debug, Clone, Copy)]
struct TxPacketContext {
    session_id: u64,
    stream_id: u32,
}

#[derive(Debug, Clone)]
struct SenderStreamState {
    next_frame_index: u64,
    redundancy: f32,
    jitter_us: u32,
    bytes_sent: u64,
    frames_sent: u64,
}

pub struct SankakuSender {
    socket: Arc<dyn SrtTransport>,
    endpoint_guard: Option<Endpoint>,
    transport: TransportConfig,
    resumption_ticket: Option<SessionTicket>,
    session_id: Option<u64>,
    pipeline: Option<SankakuPipeline>,
    compression_graph: Vec<u8>,
    bootstrap_mode: SessionBootstrapMode,
    next_stream_id: u32,
    streams: StreamRegistry<SenderStreamState>,
    pacer: Pacer,
    target_bitrate_bps: u32,
    pending_bitrate_update_bps: Option<u32>,
    control_out_tx: Option<mpsc::UnboundedSender<Bytes>>,
    control_in_rx: Option<mpsc::UnboundedReceiver<Bytes>>,
}

impl SankakuSender {
    pub async fn new(handle: impl Into<QuicHandle>) -> Result<Self> {
        Self::new_with_ticket(handle, None).await
    }

    pub async fn new_with_ticket(
        handle: impl Into<QuicHandle>,
        ticket: Option<SessionTicket>,
    ) -> Result<Self> {
        Self::new_with_ticket_config_and_engine(
            handle,
            ticket,
            TransportConfig::default(),
            Arc::new(DefaultHandshakeEngine),
        )
        .await
    }

    pub async fn new_with_config(
        handle: impl Into<QuicHandle>,
        config: TransportConfig,
    ) -> Result<Self> {
        Self::new_with_ticket_config_and_engine(
            handle,
            None,
            config,
            Arc::new(DefaultHandshakeEngine),
        )
        .await
    }

    pub async fn new_with_ticket_config_and_engine(
        handle: impl Into<QuicHandle>,
        ticket: Option<SessionTicket>,
        config: TransportConfig,
        handshake_engine: Arc<dyn HandshakeEngine>,
    ) -> Result<Self> {
        crate::init();
        let _ = handshake_engine;
        let (connection, endpoint_guard) = resolve_quic_connection(handle).await?;
        let socket: Box<dyn SrtTransport> = Box::new(QuicTransport::new(connection));
        let mut sender = Self::new_with_connected_transport_config_and_engine(
            socket,
            ticket,
            config,
            Arc::new(DefaultHandshakeEngine),
        )?;
        sender.endpoint_guard = endpoint_guard;
        Ok(sender)
    }

    pub fn new_with_connected_transport_config_and_engine(
        socket: Box<dyn SrtTransport>,
        ticket: Option<SessionTicket>,
        config: TransportConfig,
        _handshake_engine: Arc<dyn HandshakeEngine>,
    ) -> Result<Self> {
        crate::init();
        let socket: Arc<dyn SrtTransport> = socket.into();
        let seed = rand::random::<u32>().max(1);

        Ok(Self {
            socket,
            endpoint_guard: None,
            transport: config,
            // Handshake is now provided by QUIC/TLS; keep constructor shape for compatibility.
            resumption_ticket: ticket,
            session_id: None,
            pipeline: None,
            compression_graph: Vec::new(),
            bootstrap_mode: SessionBootstrapMode::Unknown,
            next_stream_id: seed,
            streams: StreamRegistry::default(),
            pacer: Pacer::new(config.max_bytes_per_sec),
            target_bitrate_bps: clamp_video_bitrate(DEFAULT_VIDEO_BITRATE_BPS),
            pending_bitrate_update_bps: None,
            control_out_tx: None,
            control_in_rx: None,
        })
    }

    // Compatibility constructor while callers migrate away from PSK wiring.
    pub async fn new_with_psk(_dest: &str, _psk: [u8; 32]) -> Result<Self> {
        bail!(
            "PSK/socket-address constructors were removed; pass a configured QUIC Connection or Endpoint"
        )
    }

    // Compatibility constructor while callers migrate away from PSK wiring.
    pub async fn new_with_psk_and_ticket(
        _dest: &str,
        _psk: [u8; 32],
        ticket: Option<SessionTicket>,
    ) -> Result<Self> {
        let _ = ticket;
        bail!(
            "PSK/socket-address constructors were removed; pass a configured QUIC Connection or Endpoint"
        )
    }

    // Compatibility constructor while callers migrate away from PSK wiring.
    pub async fn new_with_psk_and_config(
        _dest: &str,
        _psk: [u8; 32],
        config: TransportConfig,
    ) -> Result<Self> {
        let _ = config;
        bail!(
            "PSK/socket-address constructors were removed; pass a configured QUIC Connection or Endpoint"
        )
    }

    // Compatibility constructor while callers migrate away from PSK wiring.
    pub async fn new_with_psk_ticket_config_and_engine(
        _dest: &str,
        _psk: [u8; 32],
        ticket: Option<SessionTicket>,
        config: TransportConfig,
        handshake_engine: Arc<dyn HandshakeEngine>,
    ) -> Result<Self> {
        let _ = (ticket, config, handshake_engine);
        bail!(
            "PSK/socket-address constructors were removed; pass a configured QUIC Connection or Endpoint"
        )
    }

    pub fn set_transport_config(&mut self, config: TransportConfig) {
        self.transport = config;
        self.pacer = Pacer::new(config.max_bytes_per_sec);
    }

    pub fn import_resumption_ticket(&mut self, blob: &[u8]) -> Result<()> {
        let ticket = bincode::deserialize::<SessionTicket>(blob)
            .context("Failed to deserialize resumption ticket blob")?;
        self.resumption_ticket = Some(ticket);
        Ok(())
    }

    pub fn export_resumption_ticket(&self) -> Result<Option<Vec<u8>>> {
        let Some(ticket) = &self.resumption_ticket else {
            return Ok(None);
        };
        Ok(Some(
            bincode::serialize(ticket).context("Failed to serialize resumption ticket")?,
        ))
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    pub fn session_id(&self) -> Option<u64> {
        self.session_id
    }

    pub fn bootstrap_mode(&self) -> SessionBootstrapMode {
        self.bootstrap_mode
    }

    pub fn stream_redundancy(&self, stream_id: u32) -> Option<f32> {
        self.streams.get(&stream_id).map(|state| state.redundancy)
    }

    pub fn target_bitrate_bps(&self) -> u32 {
        self.target_bitrate_bps
    }

    pub fn take_bitrate_update_bps(&mut self) -> Option<u32> {
        self.pending_bitrate_update_bps.take()
    }

    pub fn update_compression_graph(&mut self, serialized_graph: &[u8]) -> Result<()> {
        self.compression_graph.clear();
        self.compression_graph.extend_from_slice(serialized_graph);
        if let Some(pipeline) = self.pipeline.as_mut() {
            pipeline.update_compression_graph(serialized_graph)?;
        }
        Ok(())
    }

    async fn start_control_tasks(&mut self) {
        if self.control_out_tx.is_some() && self.control_in_rx.is_some() {
            return;
        }

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Bytes>();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<Bytes>();

        let send_socket = Arc::clone(&self.socket);
        tokio::spawn(async move {
            while let Some(packet) = out_rx.recv().await {
                if send_socket.send_control(packet).await.is_err() {
                    break;
                }
            }
        });

        let recv_socket = Arc::clone(&self.socket);
        tokio::spawn(async move {
            loop {
                match recv_socket.recv_control().await {
                    Ok(packet) => {
                        if in_tx.send(packet).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        self.control_out_tx = Some(out_tx);
        self.control_in_rx = Some(in_rx);
    }

    fn enqueue_control_packet(&self, packet: Vec<u8>) -> Result<()> {
        let tx = self
            .control_out_tx
            .as_ref()
            .context("control stream sender is not initialized")?;
        tx.send(Bytes::from(packet))
            .map_err(|_| anyhow::anyhow!("control stream sender task is not running"))?;
        Ok(())
    }

    fn drain_control_channel(
        &mut self,
        session_id: u64,
        stream_id: u32,
        frame_index: u64,
        redundancy: &mut f32,
        jitter_us: &mut u32,
    ) -> Result<()> {
        let Some(rx) = self.control_in_rx.as_mut() else {
            return Ok(());
        };

        loop {
            match rx.try_recv() {
                Ok(packet) if !packet.is_empty() => match PacketType::from_byte(packet[0]) {
                    Some(PacketType::Feedback) => {
                        if let Ok(feedback) = bincode::deserialize::<FeedbackPacket>(&packet[1..])
                            && feedback.session_id == session_id
                            && feedback.stream_id == stream_id
                        {
                            let next_bitrate =
                                apply_aimd_bitrate(self.target_bitrate_bps, feedback.loss_ratio);
                            if next_bitrate != self.target_bitrate_bps {
                                self.target_bitrate_bps = next_bitrate;
                                self.pending_bitrate_update_bps = Some(next_bitrate);
                            }
                        }
                    }
                    Some(PacketType::FecFeedback) => {
                        if let Ok(feedback) =
                            bincode::deserialize::<FecFeedbackPacket>(&packet[1..])
                            && feedback.session_id == session_id
                            && feedback.stream_id == stream_id
                            && feedback.frame_index <= frame_index
                        {
                            *redundancy =
                                adjust_redundancy(*redundancy, &feedback, self.transport.fec);
                        }
                    }
                    Some(PacketType::Telemetry) => {
                        if let Ok(telemetry) = bincode::deserialize::<TelemetryPacket>(&packet[1..])
                            && telemetry.session_id == session_id
                            && telemetry.stream_id == stream_id
                            && telemetry.frame_index <= frame_index
                        {
                            *redundancy = adjust_redundancy_with_telemetry(
                                *redundancy,
                                &telemetry,
                                self.transport.fec,
                            );
                            *jitter_us = telemetry.jitter_us;
                        }
                    }
                    _ => {}
                },
                Ok(_) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        Ok(())
    }

    fn target_packet_size(&self) -> usize {
        let max_datagram = self
            .socket
            .max_datagram_size()
            .unwrap_or(QUIC_DATAGRAM_FALLBACK_MAX);
        max_datagram
            .saturating_sub(DATA_PREFIX_SIZE)
            .clamp(1, TARGET_PACKET_SIZE)
    }

    /// Applies external RTCP-style telemetry (for example from app-managed feedback channels).
    pub fn apply_network_telemetry(
        &mut self,
        stream_id: u32,
        packet_loss_ppm: u32,
        jitter_us: u32,
    ) {
        let Some(state) = self.streams.get_mut(&stream_id) else {
            return;
        };
        let telemetry = TelemetryPacket {
            session_id: self.session_id.unwrap_or(0),
            stream_id,
            frame_index: state.next_frame_index,
            packet_loss_ppm,
            jitter_us,
        };
        state.redundancy =
            adjust_redundancy_with_telemetry(state.redundancy, &telemetry, self.transport.fec);
        state.jitter_us = jitter_us;
    }

    pub fn open_stream(&mut self) -> Result<u32> {
        self.open_stream_with_type(StreamType::Video)
    }

    pub fn open_stream_with_type(&mut self, stream_type: StreamType) -> Result<u32> {
        let stream_id = self.next_stream_id;
        self.next_stream_id = self
            .next_stream_id
            .checked_add(1)
            .context("Stream id space exhausted for this sender session")?;
        self.streams.insert(
            stream_id,
            stream_type,
            SenderStreamState {
                next_frame_index: 0,
                redundancy: self.transport.initial_redundancy.clamp(1.0, MAX_REDUNDANCY),
                jitter_us: 0,
                bytes_sent: 0,
                frames_sent: 0,
            },
        );
        Ok(stream_id)
    }

    pub async fn send_frame(&mut self, stream_id: u32, frame: VideoFrame) -> Result<u64> {
        self.ensure_handshake().await?;
        if !is_supported_video_codec(frame.codec) {
            bail!("unsupported video codec id {}", frame.codec);
        }
        let _ = self
            .streams
            .ensure_with(stream_id, StreamType::Video, || SenderStreamState {
                next_frame_index: 0,
                redundancy: self.transport.initial_redundancy.clamp(1.0, MAX_REDUNDANCY),
                jitter_us: 0,
                bytes_sent: 0,
                frames_sent: 0,
            })?;

        let frame_index = self
            .streams
            .get(&stream_id)
            .map(|state| state.next_frame_index)
            .unwrap_or(0);

        let envelope = FrameEnvelope {
            timestamp_us: frame.timestamp_us,
            keyframe: frame.keyframe,
            payload: frame.payload,
        };
        let raw = bincode::serialize(&envelope).context("Failed to serialize frame envelope")?;

        let session_id = self
            .session_id
            .context("Sender missing session id after handshake")?;
        let tx_context = TxPacketContext {
            session_id,
            stream_id,
        };

        let (mut redundancy, mut jitter_us) = {
            let state = self
                .streams
                .get(&stream_id)
                .expect("sender stream state should exist");
            (state.redundancy, state.jitter_us)
        };

        self.send_chunk(
            tx_context,
            &raw,
            frame.codec,
            frame.kind,
            frame_index,
            frame.keyframe,
            &mut redundancy,
            &mut jitter_us,
        )
        .await?;

        if let Some(state) = self.streams.get_mut(&stream_id) {
            state.redundancy = redundancy.clamp(1.0, MAX_REDUNDANCY);
            state.jitter_us = jitter_us;
            state.bytes_sent = state.bytes_sent.saturating_add(raw.len() as u64);
            state.frames_sent = state.frames_sent.saturating_add(1);
            state.next_frame_index = state.next_frame_index.saturating_add(1);
        }

        Ok(frame_index)
    }

    pub async fn send_audio_frame(
        &mut self,
        stream_id: u32,
        timestamp_us: u64,
        codec: u8,
        frames_per_packet: u32,
        payload: Vec<u8>,
    ) -> Result<()> {
        if payload.is_empty() {
            return Ok(());
        }
        if !is_supported_audio_codec(codec) {
            bail!("unsupported audio codec 0x{codec:02X}");
        }

        self.ensure_handshake().await?;
        let _ = self
            .streams
            .ensure_with(stream_id, StreamType::Audio, || SenderStreamState {
                next_frame_index: 0,
                redundancy: self.transport.initial_redundancy.clamp(1.0, MAX_REDUNDANCY),
                jitter_us: 0,
                bytes_sent: 0,
                frames_sent: 0,
            })?;
        let session_id = self
            .session_id
            .context("Sender missing session id after handshake")?;
        let payload_len = u32::try_from(payload.len()).context("audio payload too large")?;

        let mut packet = Vec::with_capacity(AUDIO_PREFIX_SIZE + payload.len());
        packet.push(TYPE_AUDIO);
        packet.extend_from_slice(&session_id.to_le_bytes());
        packet.extend_from_slice(&stream_id.to_le_bytes());
        packet.extend_from_slice(&timestamp_us.to_le_bytes());
        packet.push(codec);
        packet.extend_from_slice(&frames_per_packet.to_le_bytes());
        packet.extend_from_slice(&payload_len.to_le_bytes());
        packet.extend_from_slice(&payload);
        self.socket
            .send_datagram(Bytes::from(packet))
            .await
            .map_err(|error| annotate_transport_error("failed to send audio datagram", error))?;
        Ok(())
    }

    pub async fn send_stream_fin(
        &mut self,
        stream_id: u32,
        final_bytes: u64,
        final_frames: u64,
    ) -> Result<()> {
        self.ensure_handshake().await?;
        let session_id = self
            .session_id
            .context("Sender missing session id after handshake")?;
        let fin = StreamFinPacket {
            session_id,
            stream_id,
            final_bytes,
            final_frames,
        };
        let mut packet = vec![TYPE_STREAM_FIN];
        packet.extend(bincode::serialize(&fin)?);
        self.enqueue_control_packet(packet)?;
        if let Some(stats) = self.socket.quic_stats() {
            let dropped = stats.path.lost_packets;
            println!(
                "QUIC_TELEMETRY: path.rtt={:?} udp_tx.dropped={}",
                stats.path.rtt, dropped
            );
        }
        Ok(())
    }

    async fn ensure_handshake(&mut self) -> Result<()> {
        if self.session_id.is_some() && self.pipeline.is_some() {
            return Ok(());
        }
        self.start_control_tasks().await;
        self.bootstrap_mode = SessionBootstrapMode::FullHandshake;
        let session_id = self.session_id.unwrap_or_else(rand::random::<u64>);
        let mut pipeline =
            SankakuPipeline::new_with_config(&TRANSPORT_PIPELINE_KEY, self.transport.pipeline);
        if !self.compression_graph.is_empty() {
            pipeline.update_compression_graph(&self.compression_graph)?;
        }
        self.pipeline = Some(pipeline);
        self.session_id = Some(session_id);
        Ok(())
    }

    fn prepare_protected_frame(
        &mut self,
        data: &[u8],
        kind: VideoPayloadKind,
        stream_id: u32,
        frame_index: u64,
    ) -> Result<Vec<u8>> {
        // Pipeline transforms (OpenZL + payload framing) must execute before FEC chunking.
        self.pipeline
            .as_mut()
            .context("Sender pipeline not initialized")?
            .protect_frame(data, kind, stream_id, frame_index)
    }

    async fn send_chunk(
        &mut self,
        context: TxPacketContext,
        data: &[u8],
        codec: u8,
        kind: VideoPayloadKind,
        frame_index: u64,
        keyframe: bool,
        redundancy: &mut f32,
        jitter_us: &mut u32,
    ) -> Result<()> {
        let protected = self.prepare_protected_frame(data, kind, context.stream_id, frame_index)?;

        let total_size =
            u32::try_from(protected.len()).context("Protected block exceeded u32 length")?;
        if total_size == 0 || total_size > MAX_PROTECTED_BLOCK_SIZE {
            bail!(
                "Protected block size {total_size} outside allowed range (1..={MAX_PROTECTED_BLOCK_SIZE})"
            );
        }

        let mut pkt_size = self.target_packet_size() as u32;
        if total_size <= pkt_size {
            pkt_size = total_size.div_ceil(2).max(1);
        }

        let encoder = WirehairEncoder::new(&protected, pkt_size)?;
        let needed_packets = total_size.div_ceil(pkt_size);
        let bounded_redundancy = redundancy.clamp(1.0, MAX_REDUNDANCY);
        let total_packets = ((needed_packets as f32) * bounded_redundancy).ceil() as u32;

        for seq_id in 0..total_packets {
            self.drain_control_channel(
                context.session_id,
                context.stream_id,
                frame_index,
                redundancy,
                jitter_us,
            )?;

            let packet_data = encoder
                .encode(seq_id)
                .map_err(|error| anyhow::anyhow!("{error:?}"))?;

            let mut plain_header = [0u8; GEOMETRY_HEADER_SIZE];
            plain_header[0..4].copy_from_slice(&context.stream_id.to_le_bytes());
            plain_header[4..12].copy_from_slice(&frame_index.to_le_bytes());
            plain_header[12..16].copy_from_slice(&seq_id.to_le_bytes());
            plain_header[16..20].copy_from_slice(&total_size.to_le_bytes());
            plain_header[20..22].copy_from_slice(&(pkt_size as u16).to_le_bytes());
            plain_header[22] = kind.as_header_flag();
            plain_header[23] = codec;

            let mut wire_packet = Vec::with_capacity(DATA_PREFIX_SIZE + packet_data.len() + 64);
            wire_packet.push(TYPE_DATA);
            wire_packet.extend_from_slice(&context.session_id.to_le_bytes());
            wire_packet.extend_from_slice(&plain_header);
            wire_packet.extend_from_slice(&packet_data);

            let max_datagram = self
                .socket
                .max_datagram_size()
                .unwrap_or(QUIC_DATAGRAM_FALLBACK_MAX);
            let target_len =
                target_packet_len(self.transport.padding, wire_packet.len()).min(max_datagram);
            if wire_packet.len() < target_len {
                wire_packet.resize(target_len, 0u8);
            }

            let wire_len = wire_packet.len();
            self.socket
                .send_datagram(Bytes::from(wire_packet))
                .await
                .map_err(|error| {
                    annotate_transport_error("failed to send media datagram", error)
                })?;
            self.pacer.pace(wire_len, keyframe, *jitter_us).await;
        }

        Ok(())
    }
}

struct DecoderState {
    frame_index: u64,
    decoder: WirehairDecoder,
    ideal_packets: u32,
    used_packets: u32,
}

#[derive(Default)]
struct FrameSequenceWindow {
    max_seq_id: u32,
    seen_seq_ids: HashSet<u32>,
}

#[derive(Default)]
struct LossFeedbackWindow {
    started_at: Option<Instant>,
    frames: HashMap<u64, FrameSequenceWindow>,
}

impl LossFeedbackWindow {
    fn observe_packet(&mut self, at: Instant, frame_index: u64, seq_id: u32) {
        self.started_at.get_or_insert(at);
        let frame = self.frames.entry(frame_index).or_default();
        frame.max_seq_id = frame.max_seq_id.max(seq_id);
        frame.seen_seq_ids.insert(seq_id);
    }

    fn maybe_flush_loss_ratio(&mut self, at: Instant) -> Option<f32> {
        let started_at = self.started_at?;
        if at.duration_since(started_at) < FEEDBACK_WINDOW {
            return None;
        }

        let mut expected_packets = 0u64;
        let mut received_packets = 0u64;
        for frame in self.frames.values() {
            expected_packets = expected_packets.saturating_add(frame.max_seq_id as u64 + 1);
            received_packets =
                received_packets.saturating_add(u64::try_from(frame.seen_seq_ids.len()).ok()?);
        }

        self.started_at = Some(at);
        self.frames.clear();

        if expected_packets == 0 {
            return Some(0.0);
        }

        let lost_packets = expected_packets.saturating_sub(received_packets);
        Some((lost_packets as f32 / expected_packets as f32).clamp(0.0, 1.0))
    }
}

#[derive(Default)]
struct StreamState {
    next_frame_index: u64,
    decoder_state: Option<DecoderState>,
    bytes_received: u64,
    frames_received: u64,
    last_arrival: Option<Instant>,
    last_timestamp_us: Option<u64>,
    jitter_us: u32,
    loss_window: LossFeedbackWindow,
    packet_loss_ratio: f32,
}

struct SessionState {
    pipeline: SankakuPipeline,
    streams: StreamRegistry<StreamState>,
}

#[derive(Default)]
struct ReceiverRuntime {
    sessions: HashMap<u64, SessionState>,
}

pub struct SankakuReceiver {
    socket: Arc<dyn SrtTransport>,
    endpoint_guard: Option<Endpoint>,
    transport: TransportConfig,
    compression_graph: Vec<u8>,
    control_out_tx: Option<mpsc::UnboundedSender<Bytes>>,
    control_in_rx: Option<mpsc::UnboundedReceiver<Bytes>>,
}

impl SankakuReceiver {
    pub async fn new(handle: impl Into<QuicHandle>) -> Result<Self> {
        Self::new_with_ticket_key(handle, random_ticket_key()).await
    }

    pub async fn new_with_ticket_key(
        handle: impl Into<QuicHandle>,
        ticket_key: [u8; 32],
    ) -> Result<Self> {
        Self::new_with_ticket_key_config_and_engine(
            handle,
            ticket_key,
            TransportConfig::default(),
            Arc::new(DefaultHandshakeEngine),
        )
        .await
    }

    pub async fn new_with_ticket_key_and_config(
        handle: impl Into<QuicHandle>,
        ticket_key: [u8; 32],
        config: TransportConfig,
    ) -> Result<Self> {
        Self::new_with_ticket_key_config_and_engine(
            handle,
            ticket_key,
            config,
            Arc::new(DefaultHandshakeEngine),
        )
        .await
    }

    pub async fn new_with_ticket_key_config_and_engine(
        handle: impl Into<QuicHandle>,
        ticket_key: [u8; 32],
        config: TransportConfig,
        handshake_engine: Arc<dyn HandshakeEngine>,
    ) -> Result<Self> {
        crate::init();
        let _ = (ticket_key, handshake_engine);
        let (connection, endpoint_guard) = resolve_quic_connection(handle).await?;
        let socket: Box<dyn SrtTransport> = Box::new(QuicTransport::new(connection));
        let mut receiver = Self::new_with_transport_ticket_key_config_and_engine(
            socket,
            [0u8; 32],
            config,
            Arc::new(DefaultHandshakeEngine),
        )?;
        receiver.endpoint_guard = endpoint_guard;
        Ok(receiver)
    }

    pub fn new_with_transport_ticket_key_config_and_engine(
        socket: Box<dyn SrtTransport>,
        _ticket_key: [u8; 32],
        config: TransportConfig,
        _handshake_engine: Arc<dyn HandshakeEngine>,
    ) -> Result<Self> {
        crate::init();
        let socket: Arc<dyn SrtTransport> = socket.into();
        Ok(Self {
            socket,
            endpoint_guard: None,
            transport: config,
            compression_graph: Vec::new(),
            control_out_tx: None,
            control_in_rx: None,
        })
    }

    // Compatibility constructor while callers migrate away from PSK wiring.
    pub async fn new_with_psk(_bind_addr: &str, _psk: [u8; 32]) -> Result<Self> {
        bail!(
            "PSK/socket-address constructors were removed; pass a configured QUIC Connection or Endpoint"
        )
    }

    // Compatibility constructor while callers migrate away from PSK wiring.
    pub async fn new_with_psk_and_ticket_key(
        _bind_addr: &str,
        _psk: [u8; 32],
        ticket_key: [u8; 32],
    ) -> Result<Self> {
        let _ = ticket_key;
        bail!(
            "PSK/socket-address constructors were removed; pass a configured QUIC Connection or Endpoint"
        )
    }

    // Compatibility constructor while callers migrate away from PSK wiring.
    pub async fn new_with_psk_ticket_key_and_config(
        _bind_addr: &str,
        _psk: [u8; 32],
        ticket_key: [u8; 32],
        config: TransportConfig,
    ) -> Result<Self> {
        let _ = (ticket_key, config);
        bail!(
            "PSK/socket-address constructors were removed; pass a configured QUIC Connection or Endpoint"
        )
    }

    // Compatibility constructor while callers migrate away from PSK wiring.
    pub async fn new_with_psk_ticket_key_config_and_engine(
        _bind_addr: &str,
        _psk: [u8; 32],
        ticket_key: [u8; 32],
        config: TransportConfig,
        handshake_engine: Arc<dyn HandshakeEngine>,
    ) -> Result<Self> {
        let _ = (ticket_key, config, handshake_engine);
        bail!(
            "PSK/socket-address constructors were removed; pass a configured QUIC Connection or Endpoint"
        )
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    pub fn update_compression_graph(&mut self, serialized_graph: &[u8]) -> Result<()> {
        self.compression_graph.clear();
        self.compression_graph.extend_from_slice(serialized_graph);
        Ok(())
    }

    async fn start_control_tasks(&mut self) {
        if self.control_out_tx.is_some() && self.control_in_rx.is_some() {
            return;
        }

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Bytes>();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<Bytes>();

        let send_socket = Arc::clone(&self.socket);
        tokio::spawn(async move {
            while let Some(packet) = out_rx.recv().await {
                if send_socket.send_control(packet).await.is_err() {
                    break;
                }
            }
        });

        let recv_socket = Arc::clone(&self.socket);
        tokio::spawn(async move {
            loop {
                match recv_socket.recv_control().await {
                    Ok(packet) => {
                        if in_tx.send(packet).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        self.control_out_tx = Some(out_tx);
        self.control_in_rx = Some(in_rx);
    }

    fn enqueue_control_packet(&self, packet: Vec<u8>) -> Result<()> {
        let tx = self
            .control_out_tx
            .as_ref()
            .context("receiver control stream sender is not initialized")?;
        tx.send(Bytes::from(packet))
            .map_err(|_| anyhow::anyhow!("receiver control stream sender task is not running"))?;
        Ok(())
    }

    fn build_receiver_pipeline(&self) -> Result<SankakuPipeline> {
        let mut pipeline =
            SankakuPipeline::new_with_config(&TRANSPORT_PIPELINE_KEY, self.transport.pipeline);
        if !self.compression_graph.is_empty() {
            pipeline.update_compression_graph(&self.compression_graph)?;
        }
        Ok(pipeline)
    }

    pub fn spawn_frame_channel(self) -> mpsc::Receiver<InboundVideoFrame> {
        let (frame_tx, frame_rx) = mpsc::channel(2048);
        thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let _ = self.run_media_loop(frame_tx, None).await;
            });
        });
        frame_rx
    }

    pub fn spawn_media_channels(
        self,
    ) -> (
        mpsc::Receiver<InboundVideoFrame>,
        mpsc::Receiver<InboundAudioFrame>,
    ) {
        let (frame_tx, frame_rx) = mpsc::channel(2048);
        let (audio_tx, audio_rx) = mpsc::channel(2048);

        thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let _ = self.run_media_loop(frame_tx, Some(audio_tx)).await;
            });
        });

        (frame_rx, audio_rx)
    }

    pub async fn run_frame_loop(self, frame_tx: mpsc::Sender<InboundVideoFrame>) -> Result<()> {
        self.run_media_loop(frame_tx, None).await
    }

    fn max_datagram_payload(&self) -> usize {
        self.socket
            .max_datagram_size()
            .unwrap_or(QUIC_DATAGRAM_FALLBACK_MAX)
            .saturating_sub(DATA_PREFIX_SIZE)
            .max(1)
    }

    pub async fn run_media_loop(
        mut self,
        frame_tx: mpsc::Sender<InboundVideoFrame>,
        audio_tx: Option<mpsc::Sender<InboundAudioFrame>>,
    ) -> Result<()> {
        self.start_control_tasks().await;
        let mut runtime = ReceiverRuntime::default();
        let mut control_rx = self
            .control_in_rx
            .take()
            .context("receiver control stream was not initialized")?;

        loop {
            tokio::select! {
                datagram = self.socket.recv_datagram() => {
                    let packet = datagram
                        .map_err(|error| annotate_transport_error("failed to receive media datagram", error))?;
                    if packet.is_empty() {
                        continue;
                    }
                    match PacketType::from_byte(packet[0]) {
                        Some(PacketType::Ping) => {
                            self.handle_ping_packet(&mut runtime, packet.as_ref()).await?;
                        }
                        Some(PacketType::Data) => {
                            self.handle_data_packet(&mut runtime, packet.as_ref(), &frame_tx)
                                .await?;
                        }
                        Some(PacketType::Audio) => {
                            self.handle_audio_packet(&mut runtime, packet.as_ref(), audio_tx.as_ref())
                                .await?;
                        }
                        _ => {}
                    }
                }
                control = control_rx.recv() => {
                    let Some(packet) = control else {
                        continue;
                    };
                    if packet.is_empty() {
                        continue;
                    }
                    if matches!(PacketType::from_byte(packet[0]), Some(PacketType::StreamFin)) {
                        self.handle_stream_fin_packet(&mut runtime, packet.as_ref()).await?;
                    }
                }
            }
        }
    }

    async fn handle_handshake_packet(
        &self,
        runtime: &mut ReceiverRuntime,
        session_id: u64,
    ) -> Result<()> {
        runtime.sessions.entry(session_id).or_insert(SessionState {
            pipeline: self.build_receiver_pipeline()?,
            streams: StreamRegistry::default(),
        });
        Ok(())
    }

    async fn handle_ping_packet(&self, runtime: &mut ReceiverRuntime, packet: &[u8]) -> Result<()> {
        let Some(session_id) = packet.get(1..9).and_then(parse_u64_le) else {
            return Ok(());
        };
        self.handle_handshake_packet(runtime, session_id).await?;

        let mut pong = vec![TYPE_PONG];
        pong.extend_from_slice(&session_id.to_le_bytes());
        self.socket
            .send_datagram(Bytes::from(pong))
            .await
            .map_err(|error| annotate_transport_error("failed to send pong datagram", error))?;
        Ok(())
    }

    async fn handle_stream_fin_packet(
        &self,
        runtime: &mut ReceiverRuntime,
        packet: &[u8],
    ) -> Result<()> {
        let Ok(fin) = bincode::deserialize::<StreamFinPacket>(&packet[1..]) else {
            return Ok(());
        };
        self.handle_handshake_packet(runtime, fin.session_id)
            .await?;
        let Some(session) = runtime.sessions.get_mut(&fin.session_id) else {
            return Ok(());
        };

        session.streams.remove(&fin.stream_id);

        let mut ack = vec![TYPE_ACK];
        ack.extend_from_slice(&fin.session_id.to_le_bytes());
        ack.extend_from_slice(&fin.stream_id.to_le_bytes());
        self.enqueue_control_packet(ack)?;
        Ok(())
    }

    async fn handle_audio_packet(
        &self,
        runtime: &mut ReceiverRuntime,
        packet: &[u8],
        audio_tx: Option<&mpsc::Sender<InboundAudioFrame>>,
    ) -> Result<()> {
        if packet.len() < AUDIO_PREFIX_SIZE {
            return Ok(());
        }

        let Some(session_id) = packet.get(1..9).and_then(parse_u64_le) else {
            return Ok(());
        };
        self.handle_handshake_packet(runtime, session_id).await?;
        let Some(stream_id) = packet.get(9..13).and_then(parse_u32_le) else {
            return Ok(());
        };
        let Some(timestamp_us) = packet.get(13..21).and_then(parse_u64_le) else {
            return Ok(());
        };
        let Some(codec) = packet.get(21).copied() else {
            return Ok(());
        };
        if !is_supported_audio_codec(codec) {
            return Ok(());
        }
        let Some(frames_per_packet) = packet.get(22..26).and_then(parse_u32_le) else {
            return Ok(());
        };
        let Some(payload_len_u32) = packet.get(26..30).and_then(parse_u32_le) else {
            return Ok(());
        };
        let payload_len = usize::try_from(payload_len_u32).unwrap_or(0);
        if payload_len == 0 {
            return Ok(());
        }

        let payload_start = AUDIO_PREFIX_SIZE;
        let payload_end = payload_start.saturating_add(payload_len);
        if payload_end > packet.len() {
            return Ok(());
        }

        let Some(session) = runtime.sessions.get_mut(&session_id) else {
            return Ok(());
        };

        let _ = session
            .streams
            .ensure_with(stream_id, StreamType::Audio, StreamState::default);

        if let Some(audio_tx) = audio_tx {
            let frame = InboundAudioFrame {
                session_id,
                stream_id,
                timestamp_us,
                codec,
                frames_per_packet,
                payload: packet[payload_start..payload_end].to_vec(),
            };
            let _ = audio_tx.send(frame).await;
        }

        Ok(())
    }

    async fn handle_data_packet(
        &self,
        runtime: &mut ReceiverRuntime,
        packet: &[u8],
        frame_tx: &mpsc::Sender<InboundVideoFrame>,
    ) -> Result<()> {
        if packet.len() < DATA_PREFIX_SIZE {
            return Ok(());
        }

        let Some(session_id) = packet.get(1..9).and_then(parse_u64_le) else {
            return Ok(());
        };
        self.handle_handshake_packet(runtime, session_id).await?;
        let Some(session) = runtime.sessions.get_mut(&session_id) else {
            return Ok(());
        };

        let Some(wire_header) = packet.get(9..DATA_PREFIX_SIZE) else {
            return Ok(());
        };
        let Some(payload_with_padding) = packet.get(DATA_PREFIX_SIZE..) else {
            return Ok(());
        };
        if payload_with_padding.is_empty() {
            return Ok(());
        }

        let mut plain_header = [0u8; GEOMETRY_HEADER_SIZE];
        plain_header.copy_from_slice(wire_header);

        let stream_id = parse_header_u32(&plain_header, 0);
        let frame_index = parse_header_u64(&plain_header, 4);
        let seq_id = parse_header_u32(&plain_header, 12);
        let total_size = parse_header_u32(&plain_header, 16);
        let Some(pkt_size) = parse_u16_le(&plain_header[20..22]) else {
            return Ok(());
        };
        let kind_flag = plain_header[22];
        let codec = plain_header[23];

        if total_size == 0 || total_size > MAX_PROTECTED_BLOCK_SIZE {
            return Ok(());
        }
        if pkt_size == 0 || usize::from(pkt_size) > self.max_datagram_payload() {
            return Ok(());
        }
        if VideoPayloadKind::from_header_flag(kind_flag).is_none() {
            return Ok(());
        }
        if !is_supported_video_codec(codec) {
            return Ok(());
        }

        let payload_len = usize::from(pkt_size).min(payload_with_padding.len());
        if payload_len == 0 {
            return Ok(());
        }
        let payload = &payload_with_padding[..payload_len];

        let stream =
            session
                .streams
                .ensure_with(stream_id, StreamType::Video, StreamState::default)?;
        let arrival_now = Instant::now();

        stream
            .loss_window
            .observe_packet(arrival_now, frame_index, seq_id);
        if let Some(loss_ratio) = stream.loss_window.maybe_flush_loss_ratio(arrival_now) {
            stream.packet_loss_ratio = loss_ratio;
            let feedback = FeedbackPacket {
                session_id,
                stream_id,
                loss_ratio,
            };
            let mut feedback_wire = vec![TYPE_FEEDBACK];
            feedback_wire.extend(bincode::serialize(&feedback)?);
            self.enqueue_control_packet(feedback_wire)?;
        }

        if frame_index != stream.next_frame_index {
            return Ok(());
        }

        if stream
            .decoder_state
            .as_ref()
            .is_some_and(|state| state.frame_index != frame_index)
        {
            stream.decoder_state = None;
        }

        if stream.decoder_state.is_none() {
            let decoder = WirehairDecoder::new(total_size as u64, u32::from(pkt_size))?;
            stream.decoder_state = Some(DecoderState {
                frame_index,
                decoder,
                ideal_packets: total_size.div_ceil(u32::from(pkt_size)),
                used_packets: 0,
            });
        }

        let mut recovered: Option<Vec<u8>> = None;
        let mut ideal_packets = 0u32;
        let mut used_packets = 0u32;
        if let Some(decoder_state) = stream.decoder_state.as_mut() {
            match decoder_state.decoder.decode(seq_id, payload) {
                Ok(true) => {
                    recovered = Some(decoder_state.decoder.recover()?);
                    decoder_state.used_packets = seq_id.saturating_add(1);
                    ideal_packets = decoder_state.ideal_packets;
                    used_packets = decoder_state.used_packets;
                }
                Ok(false) => return Ok(()),
                Err(_) => {
                    stream.decoder_state = None;
                    return Ok(());
                }
            }
        }
        stream.decoder_state = None;

        let Some(protected) = recovered else {
            return Ok(());
        };
        let (restored_kind, raw) =
            session
                .pipeline
                .restore_frame(&protected, stream_id, frame_index)?;
        if restored_kind.as_header_flag() != kind_flag {
            return Ok(());
        }

        let envelope: FrameEnvelope =
            bincode::deserialize(&raw).context("Failed to decode frame envelope")?;

        if let (Some(last_arrival), Some(last_timestamp)) =
            (stream.last_arrival, stream.last_timestamp_us)
        {
            let arrival_delta = arrival_now.duration_since(last_arrival).as_micros() as i128;
            let sender_delta = envelope.timestamp_us.saturating_sub(last_timestamp) as i128;
            let sample = (arrival_delta - sender_delta).unsigned_abs() as u64;
            stream.jitter_us = ((stream.jitter_us as u64 * 7 + sample) / 8) as u32;
        }
        stream.last_arrival = Some(arrival_now);
        stream.last_timestamp_us = Some(envelope.timestamp_us);

        stream.bytes_received = stream
            .bytes_received
            .saturating_add(envelope.payload.len() as u64);
        stream.frames_received = stream.frames_received.saturating_add(1);
        stream.next_frame_index = stream.next_frame_index.saturating_add(1);

        let inbound = InboundVideoFrame {
            session_id,
            stream_id,
            frame_index,
            timestamp_us: envelope.timestamp_us,
            keyframe: envelope.keyframe,
            codec,
            packet_loss_ratio: stream.packet_loss_ratio,
            kind: restored_kind,
            payload: envelope.payload,
        };
        if frame_tx.send(inbound).await.is_err() {
            return Ok(());
        }

        let feedback = FecFeedbackPacket {
            session_id,
            stream_id,
            frame_index,
            ideal_packets,
            used_packets,
        };
        let mut feedback_wire = vec![TYPE_FEC_FEEDBACK];
        feedback_wire.extend(bincode::serialize(&feedback)?);
        self.enqueue_control_packet(feedback_wire)?;

        let packet_loss_ppm = if ideal_packets == 0 || used_packets <= ideal_packets {
            0
        } else {
            let excess = used_packets.saturating_sub(ideal_packets) as u64;
            ((excess.saturating_mul(1_000_000)) / ideal_packets as u64).min(u32::MAX as u64) as u32
        };
        let telemetry = TelemetryPacket {
            session_id,
            stream_id,
            frame_index,
            packet_loss_ppm,
            jitter_us: stream.jitter_us,
        };
        let mut telemetry_wire = vec![TYPE_TELEMETRY];
        telemetry_wire.extend(bincode::serialize(&telemetry)?);
        self.enqueue_control_packet(telemetry_wire)?;

        Ok(())
    }
}

/// Public high-level API for frontend clients.
///
/// `SankakuStream` accepts a configured QUIC connection and provides async
/// send/receive methods over in-memory `VideoFrame` channels.
pub struct SankakuStream {
    connection: Connection,
    sender: SankakuSender,
    stream_id: u32,
    inbound: mpsc::Receiver<InboundVideoFrame>,
}

impl SankakuStream {
    pub async fn connect(handle: impl Into<QuicHandle>) -> Result<Self> {
        let (connection, endpoint_guard) = resolve_quic_connection(handle).await?;
        let stream_connection = connection.clone();

        let sender_socket: Box<dyn SrtTransport> = Box::new(QuicTransport::new(connection.clone()));
        let mut sender = SankakuSender::new_with_connected_transport_config_and_engine(
            sender_socket,
            None,
            TransportConfig::default(),
            Arc::new(DefaultHandshakeEngine),
        )?;
        sender.endpoint_guard = endpoint_guard.clone();

        let receiver_socket: Box<dyn SrtTransport> = Box::new(QuicTransport::new(connection));
        let mut receiver = SankakuReceiver::new_with_transport_ticket_key_config_and_engine(
            receiver_socket,
            [0u8; 32],
            TransportConfig::default(),
            Arc::new(DefaultHandshakeEngine),
        )?;
        receiver.endpoint_guard = endpoint_guard;

        let inbound = receiver.spawn_frame_channel();
        let stream_id = sender.open_stream()?;
        Ok(Self {
            connection: stream_connection,
            sender,
            stream_id,
            inbound,
        })
    }

    pub async fn connect_with_endpoint(endpoint: Endpoint) -> Result<Self> {
        Self::connect(endpoint).await
    }

    pub async fn connect_with_env(connection: Connection) -> Result<Self> {
        Self::connect(connection).await
    }

    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    pub fn import_resumption_ticket(&mut self, blob: &[u8]) -> Result<()> {
        self.sender.import_resumption_ticket(blob)
    }

    pub fn export_resumption_ticket(&self) -> Result<Option<Vec<u8>>> {
        self.sender.export_resumption_ticket()
    }

    pub fn session_id(&self) -> Option<u64> {
        self.sender.session_id()
    }

    pub fn bootstrap_mode(&self) -> SessionBootstrapMode {
        self.sender.bootstrap_mode()
    }

    pub fn update_compression_graph(&mut self, serialized_graph: &[u8]) -> Result<()> {
        self.sender.update_compression_graph(serialized_graph)
    }

    pub async fn send(&mut self, frame: VideoFrame) -> Result<u64> {
        self.sender.send_frame(self.stream_id, frame).await
    }

    pub async fn recv(&mut self) -> Option<InboundVideoFrame> {
        self.inbound.recv().await
    }

    pub fn try_recv(&mut self) -> Result<Option<InboundVideoFrame>> {
        match self.inbound.try_recv() {
            Ok(frame) => Ok(Some(frame)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => bail!("inbound receiver disconnected"),
        }
    }

    pub fn close(&self) {
        self.connection.close(0u32.into(), b"sankaku ffi shutdown");
    }
}

pub type KyuSender = SankakuSender;
pub type KyuReceiver = SankakuReceiver;
pub type MediaFrame = VideoFrame;
pub type InboundFrame = InboundVideoFrame;
