#[cfg(not(target_arch = "wasm32"))]
pub mod call_ffi;
pub mod fec;
#[cfg(not(target_arch = "wasm32"))]
pub mod ffi;
pub mod handshake;
#[cfg(not(target_arch = "wasm32"))]
pub mod hevc;
pub mod metadata;
pub mod openzl;
#[cfg(not(target_arch = "wasm32"))]
pub mod pipeline;
#[cfg(not(target_arch = "wasm32"))]
pub mod session;
#[cfg(not(target_arch = "wasm32"))]
pub mod transport;
#[cfg(target_arch = "wasm32")]
pub mod wasm_bridge;
#[cfg(feature = "webrtc")]
pub mod webrtc;

#[cfg(not(target_arch = "wasm32"))]
pub use call_ffi::{
    SANKAKU_CALL_DIAL_FLAG_ALLOW_INSECURE_ADDR_ONLY, SANKAKU_CALL_IDENTITY_LEN,
    SANKAKU_STATUS_BUFFER_TOO_SMALL, SANKAKU_STATUS_INVALID_STATE, SANKAKU_STATUS_REJECTED,
    SANKAKU_STATUS_UNSUPPORTED, SankakuCallDialParams, SankakuCallEndpointConfig,
    SankakuCallEndpointHandle, SankakuCallEvent, SankakuCallEventKind, SankakuCallHandle,
};
pub use fec::{FecError, WirehairDecoder, WirehairEncoder};
#[cfg(not(target_arch = "wasm32"))]
pub use ffi::{
    SANKAKU_FRAME_FLAG_KEYFRAME, SANKAKU_STATUS_BUFFER_OVERFLOW, SANKAKU_STATUS_DISCONNECTED,
    SANKAKU_STATUS_INTERNAL, SANKAKU_STATUS_INVALID_ARGUMENT, SANKAKU_STATUS_INVALID_HANDLE,
    SANKAKU_STATUS_OK, SANKAKU_STATUS_PANIC, SANKAKU_STATUS_WOULD_BLOCK, SankakuFrameKind,
    SankakuInboundFrame, SankakuQuicHandle, SankakuQuicHandleKind, SankakuStreamHandle,
    SankakuVideoFrame,
};
pub use handshake::{
    CIPHER_SUITE_DEFAULT, DefaultHandshakeEngine, HandshakeContext, HandshakeEngine,
    HandshakePacket, HandshakeRole, KeyExchange, PROTOCOL_BASELINE_CAPS, PROTOCOL_CAP_RESUMPTION,
    PROTOCOL_VERSION, ResumePacket, SessionKeys, SessionTicket, ValidatedTicket,
    derive_resumption_session_keys, issue_session_ticket, validate_ticket_identity,
};
#[cfg(not(target_arch = "wasm32"))]
pub use hevc::{
    AnnexBNalIter, SaoParameters, annex_b_nal_units, extract_sao_parameters, nal_unit_type,
    split_annex_b,
};
pub use metadata::{SessionManifest, StreamSemantics};
#[cfg(not(target_arch = "wasm32"))]
pub use pipeline::{
    CompressionMode, KyuPipeline, PipelineConfig, SankakuPipeline, VideoPayloadKind,
};
#[cfg(not(target_arch = "wasm32"))]
pub use session::{
    AUDIO_CODEC_DEBUG_TEXT, AUDIO_CODEC_OPUS, FecPolicy, InboundAudioFrame, InboundFrame,
    InboundVideoFrame, KyuErrorCode, KyuEvent, KyuReceiver, KyuSender, MediaFrame, PaddingMode,
    QuicHandle, SankakuReceiver, SankakuSender, SankakuStream, SessionBootstrapMode, StreamContext,
    StreamType, TransportConfig, VIDEO_CODEC_H264, VIDEO_CODEC_HEVC, VideoFrame, parse_psk_hex,
};
#[cfg(not(target_arch = "wasm32"))]
pub use transport::{QuicTransport, SrtTransport};
#[cfg(feature = "webrtc")]
pub use webrtc::{
    DEFAULT_STUN_SERVER, IceServerConfig, InboundDataChannelMessage, InboundRtpFrame, WebRtcConfig,
    WebRtcPeer,
};

use std::sync::OnceLock;

/// Initialize global library state (Wirehair tables).
#[cfg(not(target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn init() {
    static WIREHAIR_INIT: OnceLock<()> = OnceLock::new();
    WIREHAIR_INIT.get_or_init(|| unsafe {
        let _ = sankaku_wirehair_sys::wirehair_init_(2);
    });
}
