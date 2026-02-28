pub mod fec;
pub mod handshake;
pub mod hevc;
pub mod metadata;
pub mod openzl;
pub mod pipeline;
pub mod session;
pub mod transport;
#[cfg(feature = "webrtc")]
pub mod webrtc;

pub use fec::{FecError, WirehairDecoder, WirehairEncoder};
pub use handshake::{
    CIPHER_SUITE_DEFAULT, DefaultHandshakeEngine, HandshakeContext, HandshakeEngine,
    HandshakePacket, HandshakeRole, KeyExchange, PROTOCOL_BASELINE_CAPS, PROTOCOL_CAP_RESUMPTION,
    PROTOCOL_VERSION, ResumePacket, SessionKeys, SessionTicket, ValidatedTicket,
    derive_resumption_session_keys, issue_session_ticket, validate_ticket_identity,
};
pub use hevc::{
    AnnexBNalIter, SaoParameters, annex_b_nal_units, extract_sao_parameters, nal_unit_type,
    split_annex_b,
};
pub use metadata::{SessionManifest, StreamSemantics};
pub use pipeline::{
    CompressionMode, KyuPipeline, PipelineConfig, SankakuPipeline, VideoPayloadKind,
};
pub use session::{
    AUDIO_CODEC_DEBUG_TEXT, AUDIO_CODEC_OPUS, FecPolicy, InboundAudioFrame, InboundFrame,
    InboundVideoFrame, KyuErrorCode, KyuEvent, KyuReceiver, KyuSender, MediaFrame, PaddingMode,
    QuicHandle, SankakuReceiver, SankakuSender, SankakuStream, SessionBootstrapMode, StreamContext,
    StreamType, TransportConfig, VIDEO_CODEC_H264, VIDEO_CODEC_HEVC, VideoFrame, parse_psk_hex,
};
pub use transport::{QuicTransport, SrtTransport};
#[cfg(feature = "webrtc")]
pub use webrtc::{
    DEFAULT_STUN_SERVER, IceServerConfig, InboundDataChannelMessage, InboundRtpFrame, WebRtcConfig,
    WebRtcPeer,
};

use std::sync::OnceLock;

/// Initialize global library state (Wirehair tables).
#[unsafe(no_mangle)]
pub extern "C" fn init() {
    static WIREHAIR_INIT: OnceLock<()> = OnceLock::new();
    WIREHAIR_INIT.get_or_init(|| unsafe {
        let _ = sankaku_wirehair_sys::wirehair_init_(2);
    });
}
