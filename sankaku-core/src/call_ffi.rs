use crate::ffi::{
    SANKAKU_STATUS_DISCONNECTED, SANKAKU_STATUS_INTERNAL, SANKAKU_STATUS_INVALID_ARGUMENT,
    SANKAKU_STATUS_INVALID_HANDLE, SANKAKU_STATUS_OK, SANKAKU_STATUS_PANIC,
    SANKAKU_STATUS_WOULD_BLOCK, SankakuQuicHandle, SankakuQuicHandleKind, SankakuStreamHandle,
    sankaku_stream_create,
};
use crate::init;
use anyhow::{Context, Result, anyhow, bail};
use quinn::crypto::rustls::QuicClientConfig;
use quinn::rustls;
use quinn::rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use quinn::{ClientConfig, Connection, Endpoint, RecvStream, SendStream, ServerConfig};
use rcgen::generate_simple_self_signed;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::any::Any;
use std::collections::VecDeque;
use std::ffi::c_char;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::slice;
use std::str;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use tokio::time::sleep;

pub const SANKAKU_STATUS_INVALID_STATE: i32 = -8;
pub const SANKAKU_STATUS_REJECTED: i32 = -9;
pub const SANKAKU_STATUS_BUFFER_TOO_SMALL: i32 = -10;
pub const SANKAKU_STATUS_UNSUPPORTED: i32 = -11;

pub const SANKAKU_CALL_DIAL_FLAG_ALLOW_INSECURE_ADDR_ONLY: u32 = 0x0000_0001;
pub const SANKAKU_CALL_IDENTITY_LEN: usize = 32;

const DEFAULT_BIND_ADDR: &str = "0.0.0.0:0";
const CALL_SERVER_NAME: &str = "sankaku.invalid";
const MAX_SIGNAL_MESSAGE_BYTES: usize = 4 * 1024;
const CLOSE_REASON_DESTROY: &[u8] = b"sankaku ffi call destroy";
const CLOSE_REASON_REJECTED: &[u8] = b"sankaku ffi call rejected";
const CLOSE_REASON_CANCELLED: &[u8] = b"sankaku ffi call cancelled";
const CLOSE_REASON_ENDED: &[u8] = b"sankaku ffi call ended";
const CLOSE_REASON_TRANSPORT_FAILURE: &[u8] = b"sankaku ffi call transport failure";

#[repr(C)]
pub struct SankakuCallEndpointHandle {
    _private: [u8; 0],
}

#[repr(C)]
pub struct SankakuCallHandle {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SankakuCallEndpointConfig {
    pub bind_addr_utf8: *const c_char,
    pub bind_addr_len: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SankakuCallDialParams {
    pub remote_addr_utf8: *const c_char,
    pub remote_addr_len: usize,
    pub remote_identity: *const u8,
    pub remote_identity_len: usize,
    pub flags: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SankakuCallEventKind {
    Invalid = 0,
    OutgoingRinging = 1,
    IncomingOffer = 2,
    Accepted = 3,
    Rejected = 4,
    Connected = 5,
    Cancelled = 6,
    Ended = 7,
    TransportFailure = 8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SankakuCallEvent {
    pub kind: SankakuCallEventKind,
    pub call: *mut SankakuCallHandle,
    pub call_id: u64,
    pub status: i32,
    pub remote_addr_utf8: *const c_char,
    pub remote_addr_len: usize,
    pub remote_identity: *const u8,
    pub remote_identity_len: usize,
    pub message_utf8: *const c_char,
    pub message_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallRole {
    Caller,
    Callee,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallLifecycleState {
    OutgoingRinging,
    IncomingOffer,
    Connected,
    Rejected,
    Cancelled,
    Ended,
    TransportFailure,
}

impl CallLifecycleState {
    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Rejected | Self::Cancelled | Self::Ended | Self::TransportFailure
        )
    }
}

#[derive(Serialize, Deserialize, Debug)]
enum CallSignalMessage {
    Offer(CallOfferSignal),
    Accept { wire_call_id: u64 },
    Reject { wire_call_id: u64, reason: String },
    Cancel { wire_call_id: u64 },
    End { wire_call_id: u64 },
}

#[derive(Serialize, Deserialize, Debug)]
struct CallOfferSignal {
    wire_call_id: u64,
    caller_identity: [u8; SANKAKU_CALL_IDENTITY_LEN],
}

struct OwnedCallEvent {
    event: SankakuCallEvent,
    _remote_addr: Box<[u8]>,
    _remote_identity: Box<[u8]>,
    _message: Box<[u8]>,
}

unsafe impl Send for OwnedCallEvent {}

struct ManagedRuntime(Option<Runtime>);

impl ManagedRuntime {
    fn new(runtime: Runtime) -> Self {
        Self(Some(runtime))
    }

    fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.0.as_ref().expect("runtime unavailable").spawn(future)
    }

    fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.0
            .as_ref()
            .expect("runtime unavailable")
            .block_on(future)
    }
}

impl Drop for ManagedRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

impl OwnedCallEvent {
    fn new(
        kind: SankakuCallEventKind,
        call: *mut SankakuCallHandle,
        call_id: u64,
        status: i32,
        remote_addr: &str,
        remote_identity: Option<[u8; SANKAKU_CALL_IDENTITY_LEN]>,
        message: Option<&str>,
    ) -> Box<Self> {
        let remote_addr = remote_addr.as_bytes().to_vec().into_boxed_slice();
        let remote_identity = remote_identity
            .map(|identity| identity.to_vec().into_boxed_slice())
            .unwrap_or_default();
        let message = message
            .unwrap_or_default()
            .as_bytes()
            .to_vec()
            .into_boxed_slice();

        let remote_addr_ptr = if remote_addr.is_empty() {
            ptr::null()
        } else {
            remote_addr.as_ptr().cast::<c_char>()
        };
        let remote_identity_ptr = if remote_identity.is_empty() {
            ptr::null()
        } else {
            remote_identity.as_ptr()
        };
        let message_ptr = if message.is_empty() {
            ptr::null()
        } else {
            message.as_ptr().cast::<c_char>()
        };

        Box::new(Self {
            event: SankakuCallEvent {
                kind,
                call,
                call_id,
                status,
                remote_addr_utf8: remote_addr_ptr,
                remote_addr_len: remote_addr.len(),
                remote_identity: remote_identity_ptr,
                remote_identity_len: remote_identity.len(),
                message_utf8: message_ptr,
                message_len: message.len(),
            },
            _remote_addr: remote_addr,
            _remote_identity: remote_identity,
            _message: message,
        })
    }
}

struct FfiCallEndpointState {
    runtime: ManagedRuntime,
    endpoint: Endpoint,
    local_addr: String,
    identity: [u8; SANKAKU_CALL_IDENTITY_LEN],
    events: Mutex<VecDeque<Box<OwnedCallEvent>>>,
    next_call_id: AtomicU64,
}

struct FfiCallEndpointBox {
    state: Arc<FfiCallEndpointState>,
}

struct FfiCallState {
    endpoint: Arc<FfiCallEndpointState>,
    local_call_id: u64,
    wire_call_id: u64,
    role: CallRole,
    connection: Connection,
    remote_addr: String,
    remote_identity: Mutex<Option<[u8; SANKAKU_CALL_IDENTITY_LEN]>>,
    lifecycle: Mutex<CallLifecycleState>,
    signal_send: AsyncMutex<Option<SendStream>>,
    stream_taken: Mutex<bool>,
}

#[derive(Debug)]
struct EndpointIdentityVerifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
    expected_identity: Option<[u8; SANKAKU_CALL_IDENTITY_LEN]>,
}

impl EndpointIdentityVerifier {
    fn new(expected_identity: Option<[u8; SANKAKU_CALL_IDENTITY_LEN]>) -> Arc<Self> {
        Arc::new(Self {
            provider: Arc::new(rustls::crypto::ring::default_provider()),
            expected_identity,
        })
    }
}

impl rustls::client::danger::ServerCertVerifier for EndpointIdentityVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if let Some(expected_identity) = self.expected_identity {
            let actual_identity = hash_certificate_der(end_entity.as_ref());
            if actual_identity != expected_identity {
                return Err(rustls::Error::General(
                    "remote endpoint identity mismatch".to_string(),
                ));
            }
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl FfiCallEndpointState {
    fn push_event(
        &self,
        kind: SankakuCallEventKind,
        call: *mut SankakuCallHandle,
        call_id: u64,
        status: i32,
        remote_addr: &str,
        remote_identity: Option<[u8; SANKAKU_CALL_IDENTITY_LEN]>,
        message: Option<&str>,
    ) {
        let event = OwnedCallEvent::new(
            kind,
            call,
            call_id,
            status,
            remote_addr,
            remote_identity,
            message,
        );
        let mut queue = match self.events.lock() {
            Ok(queue) => queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        queue.push_back(event);
    }

    fn next_local_call_id(&self) -> u64 {
        self.next_call_id.fetch_add(1, Ordering::Relaxed)
    }
}

impl FfiCallState {
    fn handle_ptr(this: &Arc<Self>) -> *mut SankakuCallHandle {
        Arc::as_ptr(this).cast_mut().cast::<SankakuCallHandle>()
    }

    fn export_handle(this: &Arc<Self>) -> *mut SankakuCallHandle {
        let ptr = Arc::as_ptr(this);
        unsafe {
            Arc::increment_strong_count(ptr);
        }
        ptr.cast_mut().cast::<SankakuCallHandle>()
    }

    fn remote_identity(&self) -> Option<[u8; SANKAKU_CALL_IDENTITY_LEN]> {
        match self.remote_identity.lock() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    fn lifecycle(&self) -> CallLifecycleState {
        match self.lifecycle.lock() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    fn transition(&self, expected: &[CallLifecycleState], next: CallLifecycleState) -> bool {
        let mut guard = match self.lifecycle.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !expected.contains(&*guard) {
            return false;
        }
        *guard = next;
        true
    }

    fn transition_nonterminal(&self, next: CallLifecycleState) -> bool {
        let mut guard = match self.lifecycle.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.is_terminal() {
            return false;
        }
        *guard = next;
        true
    }

    fn schedule_close(&self, reason: &'static [u8]) {
        let connection = self.connection.clone();
        self.endpoint.runtime.spawn(async move {
            sleep(Duration::from_millis(150)).await;
            connection.close(0u32.into(), reason);
        });
    }
}

fn build_runtime() -> Result<Runtime, i32> {
    Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|_| SANKAKU_STATUS_INTERNAL)
}

fn copy_bytes_to_caller(
    data: &[u8],
    buffer: *mut u8,
    buffer_len: usize,
    out_len: *mut usize,
) -> i32 {
    if out_len.is_null() {
        return SANKAKU_STATUS_INVALID_ARGUMENT;
    }
    unsafe {
        *out_len = data.len();
    }

    if data.is_empty() {
        return SANKAKU_STATUS_OK;
    }

    if buffer.is_null() {
        if buffer_len == 0 {
            return SANKAKU_STATUS_OK;
        }
        return SANKAKU_STATUS_INVALID_ARGUMENT;
    }
    if buffer_len < data.len() {
        return SANKAKU_STATUS_BUFFER_TOO_SMALL;
    }

    unsafe {
        ptr::copy_nonoverlapping(data.as_ptr(), buffer, data.len());
    }
    SANKAKU_STATUS_OK
}

fn map_call_error(message: &str) -> i32 {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("disconnected")
        || lowered.contains("connection")
        || lowered.contains("closed")
        || lowered.contains("timed out")
        || lowered.contains("stopped")
    {
        return SANKAKU_STATUS_DISCONNECTED;
    }
    SANKAKU_STATUS_INTERNAL
}

fn parse_socket_addr_or_default(
    data: *const c_char,
    len: usize,
    default: &str,
) -> Result<std::net::SocketAddr, i32> {
    if len == 0 {
        return default.parse().map_err(|_| SANKAKU_STATUS_INVALID_ARGUMENT);
    }
    if data.is_null() {
        return Err(SANKAKU_STATUS_INVALID_ARGUMENT);
    }

    let raw = unsafe { slice::from_raw_parts(data.cast::<u8>(), len) };
    let text = str::from_utf8(raw).map_err(|_| SANKAKU_STATUS_INVALID_ARGUMENT)?;
    text.parse().map_err(|_| SANKAKU_STATUS_INVALID_ARGUMENT)
}

fn parse_required_socket_addr(
    data: *const c_char,
    len: usize,
) -> Result<std::net::SocketAddr, i32> {
    if len == 0 || data.is_null() {
        return Err(SANKAKU_STATUS_INVALID_ARGUMENT);
    }
    let raw = unsafe { slice::from_raw_parts(data.cast::<u8>(), len) };
    let text = str::from_utf8(raw).map_err(|_| SANKAKU_STATUS_INVALID_ARGUMENT)?;
    text.parse().map_err(|_| SANKAKU_STATUS_INVALID_ARGUMENT)
}

fn parse_remote_identity(
    data: *const u8,
    len: usize,
    flags: u32,
) -> Result<Option<[u8; SANKAKU_CALL_IDENTITY_LEN]>, i32> {
    if len == 0 {
        if flags & SANKAKU_CALL_DIAL_FLAG_ALLOW_INSECURE_ADDR_ONLY != 0 {
            return Ok(None);
        }
        return Err(SANKAKU_STATUS_INVALID_ARGUMENT);
    }
    if data.is_null() || len != SANKAKU_CALL_IDENTITY_LEN {
        return Err(SANKAKU_STATUS_INVALID_ARGUMENT);
    }
    let mut identity = [0u8; SANKAKU_CALL_IDENTITY_LEN];
    let raw = unsafe { slice::from_raw_parts(data, len) };
    identity.copy_from_slice(raw);
    Ok(Some(identity))
}

fn hash_certificate_der(data: &[u8]) -> [u8; SANKAKU_CALL_IDENTITY_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut identity = [0u8; SANKAKU_CALL_IDENTITY_LEN];
    identity.copy_from_slice(&digest[..SANKAKU_CALL_IDENTITY_LEN]);
    identity
}

fn connection_peer_identity(connection: &Connection) -> Option<[u8; SANKAKU_CALL_IDENTITY_LEN]> {
    let peer_identity = connection.peer_identity()?;
    downcast_cert_chain(peer_identity.as_ref())
        .and_then(|certs| certs.first())
        .map(|cert| hash_certificate_der(cert.as_ref()))
}

fn downcast_cert_chain(any: &dyn Any) -> Option<&Vec<CertificateDer<'static>>> {
    any.downcast_ref::<Vec<CertificateDer<'static>>>()
}

fn make_server_endpoint(
    bind_addr: std::net::SocketAddr,
) -> Result<(Endpoint, [u8; SANKAKU_CALL_IDENTITY_LEN]), anyhow::Error> {
    let certified = generate_simple_self_signed(vec![CALL_SERVER_NAME.to_string()])?;
    let cert_der = certified.cert.der().clone();
    let identity = hash_certificate_der(cert_der.as_ref());
    let private_key = PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der());
    let mut server_config = ServerConfig::with_single_cert(vec![cert_der], private_key.into())?;
    let transport = Arc::get_mut(&mut server_config.transport)
        .ok_or_else(|| anyhow!("failed to mutate QUIC transport config"))?;
    transport.max_concurrent_uni_streams(0u8.into());
    let endpoint = Endpoint::server(server_config, bind_addr)?;
    Ok((endpoint, identity))
}

fn make_client_config(
    expected_identity: Option<[u8; SANKAKU_CALL_IDENTITY_LEN]>,
) -> Result<ClientConfig> {
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(EndpointIdentityVerifier::new(expected_identity))
        .with_no_client_auth();
    Ok(ClientConfig::new(Arc::new(QuicClientConfig::try_from(
        config,
    )?)))
}

async fn send_signal_message(send: &mut SendStream, message: &CallSignalMessage) -> Result<()> {
    let payload = bincode::serialize(message).context("failed to serialize call signal")?;
    if payload.len() > MAX_SIGNAL_MESSAGE_BYTES {
        bail!(
            "call signal payload exceeded {} bytes",
            MAX_SIGNAL_MESSAGE_BYTES
        );
    }
    let len = u32::try_from(payload.len()).context("call signal payload too large")?;
    send.write_all(&len.to_le_bytes())
        .await
        .context("failed to write call signal length")?;
    send.write_all(&payload)
        .await
        .context("failed to write call signal payload")?;
    Ok(())
}

async fn recv_signal_message(recv: &mut RecvStream) -> Result<CallSignalMessage> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .context("failed to read call signal length")?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > MAX_SIGNAL_MESSAGE_BYTES {
        bail!("invalid call signal length: {len}");
    }
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload)
        .await
        .context("failed to read call signal payload")?;
    bincode::deserialize(&payload).context("failed to decode call signal payload")
}

fn clone_call_state(handle: *mut SankakuCallHandle) -> Result<Arc<FfiCallState>, i32> {
    if handle.is_null() {
        return Err(SANKAKU_STATUS_INVALID_HANDLE);
    }
    let ptr = handle.cast::<FfiCallState>();
    unsafe {
        Arc::increment_strong_count(ptr);
        Ok(Arc::from_raw(ptr))
    }
}

fn with_endpoint_state<T>(
    handle: *mut SankakuCallEndpointHandle,
    on_invalid_handle: T,
    on_panic: T,
    f: impl FnOnce(&Arc<FfiCallEndpointState>) -> T,
) -> T {
    catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() {
            return on_invalid_handle;
        }
        let raw = unsafe { &*(handle.cast::<FfiCallEndpointBox>()) };
        f(&raw.state)
    }))
    .unwrap_or(on_panic)
}

fn with_call_state<T>(
    handle: *mut SankakuCallHandle,
    on_invalid_handle: T,
    on_panic: T,
    f: impl FnOnce(Arc<FfiCallState>) -> T,
) -> T {
    catch_unwind(AssertUnwindSafe(|| match clone_call_state(handle) {
        Ok(state) => f(state),
        Err(_) => on_invalid_handle,
    }))
    .unwrap_or(on_panic)
}

fn emit_event(
    call: &Arc<FfiCallState>,
    kind: SankakuCallEventKind,
    status: i32,
    message: Option<&str>,
) {
    call.endpoint.push_event(
        kind,
        FfiCallState::handle_ptr(call),
        call.local_call_id,
        status,
        &call.remote_addr,
        call.remote_identity(),
        message,
    );
}

async fn send_control_signal(call: &Arc<FfiCallState>, message: &CallSignalMessage) -> Result<()> {
    let mut guard = call.signal_send.lock().await;
    let send = guard
        .as_mut()
        .ok_or_else(|| anyhow!("call signaling stream is unavailable"))?;
    send_signal_message(send, message).await
}

async fn call_reader_loop(call: Arc<FfiCallState>, mut recv: RecvStream) {
    loop {
        match recv_signal_message(&mut recv).await {
            Ok(CallSignalMessage::Accept { wire_call_id }) => {
                if wire_call_id != call.wire_call_id {
                    if call.transition_nonterminal(CallLifecycleState::TransportFailure) {
                        emit_event(
                            &call,
                            SankakuCallEventKind::TransportFailure,
                            SANKAKU_STATUS_INTERNAL,
                            Some("received accept for unexpected call id"),
                        );
                    }
                    call.connection
                        .close(0u32.into(), CLOSE_REASON_TRANSPORT_FAILURE);
                    break;
                }
                if call.transition(
                    &[CallLifecycleState::OutgoingRinging],
                    CallLifecycleState::Connected,
                ) {
                    emit_event(
                        &call,
                        SankakuCallEventKind::Accepted,
                        SANKAKU_STATUS_OK,
                        None,
                    );
                    emit_event(
                        &call,
                        SankakuCallEventKind::Connected,
                        SANKAKU_STATUS_OK,
                        None,
                    );
                }
            }
            Ok(CallSignalMessage::Reject {
                wire_call_id,
                reason,
            }) => {
                if wire_call_id == call.wire_call_id
                    && call.transition_nonterminal(CallLifecycleState::Rejected)
                {
                    emit_event(
                        &call,
                        SankakuCallEventKind::Rejected,
                        SANKAKU_STATUS_REJECTED,
                        Some(&reason),
                    );
                }
                call.connection.close(0u32.into(), CLOSE_REASON_REJECTED);
                break;
            }
            Ok(CallSignalMessage::Cancel { wire_call_id }) => {
                if wire_call_id == call.wire_call_id
                    && call.transition_nonterminal(CallLifecycleState::Cancelled)
                {
                    emit_event(
                        &call,
                        SankakuCallEventKind::Cancelled,
                        SANKAKU_STATUS_OK,
                        None,
                    );
                }
                call.connection.close(0u32.into(), CLOSE_REASON_CANCELLED);
                break;
            }
            Ok(CallSignalMessage::End { wire_call_id }) => {
                if wire_call_id == call.wire_call_id
                    && call.transition_nonterminal(CallLifecycleState::Ended)
                {
                    emit_event(&call, SankakuCallEventKind::Ended, SANKAKU_STATUS_OK, None);
                }
                call.connection.close(0u32.into(), CLOSE_REASON_ENDED);
                break;
            }
            Ok(CallSignalMessage::Offer(_)) => {
                if call.transition_nonterminal(CallLifecycleState::TransportFailure) {
                    emit_event(
                        &call,
                        SankakuCallEventKind::TransportFailure,
                        SANKAKU_STATUS_INTERNAL,
                        Some("received duplicate offer on call signaling stream"),
                    );
                }
                call.connection
                    .close(0u32.into(), CLOSE_REASON_TRANSPORT_FAILURE);
                break;
            }
            Err(error) => {
                if !call.lifecycle().is_terminal() {
                    let message = error.to_string();
                    if call.transition_nonterminal(CallLifecycleState::TransportFailure) {
                        emit_event(
                            &call,
                            SankakuCallEventKind::TransportFailure,
                            map_call_error(&message),
                            Some(&message),
                        );
                    }
                    call.connection
                        .close(0u32.into(), CLOSE_REASON_TRANSPORT_FAILURE);
                }
                break;
            }
        }
    }
}

async fn accept_loop(endpoint_state: Arc<FfiCallEndpointState>) {
    while let Some(incoming) = endpoint_state.endpoint.accept().await {
        let connection = match incoming.await {
            Ok(connection) => connection,
            Err(_) => continue,
        };

        let (send, mut recv) = match connection.accept_bi().await {
            Ok(streams) => streams,
            Err(_) => {
                connection.close(0u32.into(), CLOSE_REASON_TRANSPORT_FAILURE);
                continue;
            }
        };

        let offer = match recv_signal_message(&mut recv).await {
            Ok(CallSignalMessage::Offer(offer)) => offer,
            _ => {
                connection.close(0u32.into(), CLOSE_REASON_TRANSPORT_FAILURE);
                continue;
            }
        };

        let local_call_id = endpoint_state.next_local_call_id();
        let remote_addr = connection.remote_address().to_string();
        let call = Arc::new(FfiCallState {
            endpoint: endpoint_state.clone(),
            local_call_id,
            wire_call_id: offer.wire_call_id,
            role: CallRole::Callee,
            connection,
            remote_addr,
            remote_identity: Mutex::new(Some(offer.caller_identity)),
            lifecycle: Mutex::new(CallLifecycleState::IncomingOffer),
            signal_send: AsyncMutex::new(Some(send)),
            stream_taken: Mutex::new(false),
        });

        let handle = FfiCallState::export_handle(&call);
        endpoint_state.push_event(
            SankakuCallEventKind::IncomingOffer,
            handle,
            local_call_id,
            SANKAKU_STATUS_OK,
            &call.remote_addr,
            call.remote_identity(),
            None,
        );

        endpoint_state.runtime.spawn(call_reader_loop(call, recv));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_endpoint_create(
    config: *const SankakuCallEndpointConfig,
) -> *mut SankakuCallEndpointHandle {
    catch_unwind(AssertUnwindSafe(|| {
        init();
        let runtime = match build_runtime() {
            Ok(runtime) => runtime,
            Err(_) => return ptr::null_mut(),
        };

        let bind_addr = if config.is_null() {
            match DEFAULT_BIND_ADDR.parse() {
                Ok(addr) => addr,
                Err(_) => return ptr::null_mut(),
            }
        } else {
            let config = unsafe { &*config };
            match parse_socket_addr_or_default(
                config.bind_addr_utf8,
                config.bind_addr_len,
                DEFAULT_BIND_ADDR,
            ) {
                Ok(addr) => addr,
                Err(_) => return ptr::null_mut(),
            }
        };

        let endpoint_and_identity = {
            let _guard = runtime.enter();
            make_server_endpoint(bind_addr)
        };

        let (endpoint, identity) = match endpoint_and_identity {
            Ok(value) => value,
            Err(_) => return ptr::null_mut(),
        };

        let local_addr = match endpoint.local_addr() {
            Ok(addr) => addr.to_string(),
            Err(_) => return ptr::null_mut(),
        };

        let state = Arc::new(FfiCallEndpointState {
            runtime: ManagedRuntime::new(runtime),
            endpoint,
            local_addr,
            identity,
            events: Mutex::new(VecDeque::new()),
            next_call_id: AtomicU64::new(1),
        });
        state.runtime.spawn(accept_loop(state.clone()));

        let handle = Box::new(FfiCallEndpointBox { state });
        Box::into_raw(handle).cast::<SankakuCallEndpointHandle>()
    }))
    .unwrap_or(ptr::null_mut())
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_endpoint_destroy(handle: *mut SankakuCallEndpointHandle) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() {
            return;
        }
        let handle = unsafe { Box::from_raw(handle.cast::<FfiCallEndpointBox>()) };
        handle
            .state
            .endpoint
            .close(0u32.into(), b"sankaku ffi call endpoint destroy");
    }));
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_endpoint_copy_local_addr(
    handle: *mut SankakuCallEndpointHandle,
    buffer: *mut c_char,
    buffer_len: usize,
    out_len: *mut usize,
) -> i32 {
    with_endpoint_state(
        handle,
        SANKAKU_STATUS_INVALID_HANDLE,
        SANKAKU_STATUS_PANIC,
        |state| {
            copy_bytes_to_caller(
                state.local_addr.as_bytes(),
                buffer.cast::<u8>(),
                buffer_len,
                out_len,
            )
        },
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_endpoint_copy_identity(
    handle: *mut SankakuCallEndpointHandle,
    buffer: *mut u8,
    buffer_len: usize,
    out_len: *mut usize,
) -> i32 {
    with_endpoint_state(
        handle,
        SANKAKU_STATUS_INVALID_HANDLE,
        SANKAKU_STATUS_PANIC,
        |state| copy_bytes_to_caller(&state.identity, buffer, buffer_len, out_len),
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_place(
    handle: *mut SankakuCallEndpointHandle,
    params: *const SankakuCallDialParams,
    out_call: *mut *mut SankakuCallHandle,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if params.is_null() || out_call.is_null() {
            return SANKAKU_STATUS_INVALID_ARGUMENT;
        }
        unsafe {
            *out_call = ptr::null_mut();
        }

        let params = unsafe { &*params };
        let remote_addr =
            match parse_required_socket_addr(params.remote_addr_utf8, params.remote_addr_len) {
                Ok(addr) => addr,
                Err(code) => return code,
            };
        let expected_identity = match parse_remote_identity(
            params.remote_identity,
            params.remote_identity_len,
            params.flags,
        ) {
            Ok(identity) => identity,
            Err(code) => return code,
        };

        with_endpoint_state(
            handle,
            SANKAKU_STATUS_INVALID_HANDLE,
            SANKAKU_STATUS_PANIC,
            |endpoint_state| {
                let client_config = match make_client_config(expected_identity) {
                    Ok(config) => config,
                    Err(_) => return SANKAKU_STATUS_INTERNAL,
                };
                let wire_call_id = endpoint_state.next_local_call_id();

                let call = match endpoint_state.runtime.block_on(async {
                    let connecting = endpoint_state
                        .endpoint
                        .connect_with(client_config, remote_addr, CALL_SERVER_NAME)
                        .map_err(|error| anyhow!(error.to_string()))?;
                    let connection = connecting
                        .await
                        .context("failed to establish QUIC call connection")?;
                    let (mut send, recv) = connection
                        .open_bi()
                        .await
                        .context("failed to open call signaling stream")?;
                    send_signal_message(
                        &mut send,
                        &CallSignalMessage::Offer(CallOfferSignal {
                            wire_call_id,
                            caller_identity: endpoint_state.identity,
                        }),
                    )
                    .await?;

                    let remote_identity =
                        connection_peer_identity(&connection).or(expected_identity);
                    let call = Arc::new(FfiCallState {
                        endpoint: endpoint_state.clone(),
                        local_call_id: wire_call_id,
                        wire_call_id,
                        role: CallRole::Caller,
                        connection,
                        remote_addr: remote_addr.to_string(),
                        remote_identity: Mutex::new(remote_identity),
                        lifecycle: Mutex::new(CallLifecycleState::OutgoingRinging),
                        signal_send: AsyncMutex::new(Some(send)),
                        stream_taken: Mutex::new(false),
                    });
                    endpoint_state
                        .runtime
                        .spawn(call_reader_loop(call.clone(), recv));
                    Ok::<Arc<FfiCallState>, anyhow::Error>(call)
                }) {
                    Ok(call) => call,
                    Err(error) => return map_call_error(&error.to_string()),
                };

                let raw = FfiCallState::export_handle(&call);
                unsafe {
                    *out_call = raw;
                }
                emit_event(
                    &call,
                    SankakuCallEventKind::OutgoingRinging,
                    SANKAKU_STATUS_OK,
                    None,
                );
                SANKAKU_STATUS_OK
            },
        )
    }))
    .unwrap_or(SANKAKU_STATUS_PANIC)
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_poll_event(
    handle: *mut SankakuCallEndpointHandle,
    out_event: *mut *mut SankakuCallEvent,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_event.is_null() {
            return SANKAKU_STATUS_INVALID_ARGUMENT;
        }
        unsafe {
            *out_event = ptr::null_mut();
        }

        with_endpoint_state(
            handle,
            SANKAKU_STATUS_INVALID_HANDLE,
            SANKAKU_STATUS_PANIC,
            |state| {
                let mut queue = match state.events.lock() {
                    Ok(queue) => queue,
                    Err(poisoned) => poisoned.into_inner(),
                };
                let Some(raw) = queue.pop_front() else {
                    return SANKAKU_STATUS_WOULD_BLOCK;
                };
                let raw = Box::into_raw(raw);
                unsafe {
                    *out_event = &mut (*raw).event;
                }
                SANKAKU_STATUS_OK
            },
        )
    }))
    .unwrap_or(SANKAKU_STATUS_PANIC)
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_event_free(event: *mut SankakuCallEvent) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if event.is_null() {
            return;
        }
        unsafe {
            drop(Box::from_raw(event.cast::<OwnedCallEvent>()));
        }
    }));
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_accept(handle: *mut SankakuCallHandle) -> i32 {
    with_call_state(
        handle,
        SANKAKU_STATUS_INVALID_HANDLE,
        SANKAKU_STATUS_PANIC,
        |call| {
            if !matches!(call.role, CallRole::Callee) {
                return SANKAKU_STATUS_INVALID_STATE;
            }
            if !call.transition(
                &[CallLifecycleState::IncomingOffer],
                CallLifecycleState::Connected,
            ) {
                return SANKAKU_STATUS_INVALID_STATE;
            }

            let result = call.endpoint.runtime.block_on(async {
                send_control_signal(
                    &call,
                    &CallSignalMessage::Accept {
                        wire_call_id: call.wire_call_id,
                    },
                )
                .await
            });
            match result {
                Ok(_) => {
                    emit_event(
                        &call,
                        SankakuCallEventKind::Accepted,
                        SANKAKU_STATUS_OK,
                        None,
                    );
                    emit_event(
                        &call,
                        SankakuCallEventKind::Connected,
                        SANKAKU_STATUS_OK,
                        None,
                    );
                    SANKAKU_STATUS_OK
                }
                Err(error) => {
                    let _ = call.transition_nonterminal(CallLifecycleState::TransportFailure);
                    emit_event(
                        &call,
                        SankakuCallEventKind::TransportFailure,
                        map_call_error(&error.to_string()),
                        Some(&error.to_string()),
                    );
                    SANKAKU_STATUS_DISCONNECTED
                }
            }
        },
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_reject(
    handle: *mut SankakuCallHandle,
    reason_utf8: *const c_char,
    reason_len: usize,
) -> i32 {
    with_call_state(
        handle,
        SANKAKU_STATUS_INVALID_HANDLE,
        SANKAKU_STATUS_PANIC,
        |call| {
            if !matches!(call.role, CallRole::Callee) {
                return SANKAKU_STATUS_INVALID_STATE;
            }
            if !call.transition(
                &[CallLifecycleState::IncomingOffer],
                CallLifecycleState::Rejected,
            ) {
                return SANKAKU_STATUS_INVALID_STATE;
            }

            let reason = if reason_len == 0 {
                "callee rejected call".to_string()
            } else {
                if reason_utf8.is_null() {
                    return SANKAKU_STATUS_INVALID_ARGUMENT;
                }
                let raw = unsafe { slice::from_raw_parts(reason_utf8.cast::<u8>(), reason_len) };
                match str::from_utf8(raw) {
                    Ok(text) => text.to_string(),
                    Err(_) => return SANKAKU_STATUS_INVALID_ARGUMENT,
                }
            };

            let result = call.endpoint.runtime.block_on(async {
                send_control_signal(
                    &call,
                    &CallSignalMessage::Reject {
                        wire_call_id: call.wire_call_id,
                        reason: reason.clone(),
                    },
                )
                .await
            });

            emit_event(
                &call,
                SankakuCallEventKind::Rejected,
                SANKAKU_STATUS_REJECTED,
                Some(&reason),
            );
            call.schedule_close(CLOSE_REASON_REJECTED);

            match result {
                Ok(_) => SANKAKU_STATUS_OK,
                Err(_) => SANKAKU_STATUS_DISCONNECTED,
            }
        },
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_cancel(handle: *mut SankakuCallHandle) -> i32 {
    with_call_state(
        handle,
        SANKAKU_STATUS_INVALID_HANDLE,
        SANKAKU_STATUS_PANIC,
        |call| {
            if !matches!(call.role, CallRole::Caller) {
                return SANKAKU_STATUS_INVALID_STATE;
            }
            if !call.transition(
                &[CallLifecycleState::OutgoingRinging],
                CallLifecycleState::Cancelled,
            ) {
                return SANKAKU_STATUS_INVALID_STATE;
            }

            let result = call.endpoint.runtime.block_on(async {
                send_control_signal(
                    &call,
                    &CallSignalMessage::Cancel {
                        wire_call_id: call.wire_call_id,
                    },
                )
                .await
            });

            emit_event(
                &call,
                SankakuCallEventKind::Cancelled,
                SANKAKU_STATUS_OK,
                None,
            );
            call.schedule_close(CLOSE_REASON_CANCELLED);

            match result {
                Ok(_) => SANKAKU_STATUS_OK,
                Err(_) => SANKAKU_STATUS_DISCONNECTED,
            }
        },
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_end(handle: *mut SankakuCallHandle) -> i32 {
    with_call_state(
        handle,
        SANKAKU_STATUS_INVALID_HANDLE,
        SANKAKU_STATUS_PANIC,
        |call| {
            if !call.transition(&[CallLifecycleState::Connected], CallLifecycleState::Ended) {
                return SANKAKU_STATUS_INVALID_STATE;
            }

            let result = call.endpoint.runtime.block_on(async {
                send_control_signal(
                    &call,
                    &CallSignalMessage::End {
                        wire_call_id: call.wire_call_id,
                    },
                )
                .await
            });

            emit_event(&call, SankakuCallEventKind::Ended, SANKAKU_STATUS_OK, None);
            call.schedule_close(CLOSE_REASON_ENDED);

            match result {
                Ok(_) => SANKAKU_STATUS_OK,
                Err(_) => SANKAKU_STATUS_DISCONNECTED,
            }
        },
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_copy_remote_addr(
    handle: *mut SankakuCallHandle,
    buffer: *mut c_char,
    buffer_len: usize,
    out_len: *mut usize,
) -> i32 {
    with_call_state(
        handle,
        SANKAKU_STATUS_INVALID_HANDLE,
        SANKAKU_STATUS_PANIC,
        |call| {
            copy_bytes_to_caller(
                call.remote_addr.as_bytes(),
                buffer.cast::<u8>(),
                buffer_len,
                out_len,
            )
        },
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_copy_remote_identity(
    handle: *mut SankakuCallHandle,
    buffer: *mut u8,
    buffer_len: usize,
    out_len: *mut usize,
) -> i32 {
    with_call_state(
        handle,
        SANKAKU_STATUS_INVALID_HANDLE,
        SANKAKU_STATUS_PANIC,
        |call| match call.remote_identity() {
            Some(identity) => copy_bytes_to_caller(&identity, buffer, buffer_len, out_len),
            None => copy_bytes_to_caller(&[], buffer, buffer_len, out_len),
        },
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_take_stream(
    handle: *mut SankakuCallHandle,
    out_stream: *mut *mut SankakuStreamHandle,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_stream.is_null() {
            return SANKAKU_STATUS_INVALID_ARGUMENT;
        }
        unsafe {
            *out_stream = ptr::null_mut();
        }

        with_call_state(
            handle,
            SANKAKU_STATUS_INVALID_HANDLE,
            SANKAKU_STATUS_PANIC,
            |call| {
                if call.lifecycle() != CallLifecycleState::Connected {
                    return SANKAKU_STATUS_INVALID_STATE;
                }

                let mut taken = match call.stream_taken.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if *taken {
                    return SANKAKU_STATUS_INVALID_STATE;
                }

                let connection = call.connection.clone();
                let stream = sankaku_stream_create(SankakuQuicHandle {
                    kind: SankakuQuicHandleKind::Connection,
                    handle: Box::into_raw(Box::new(connection)).cast(),
                });
                if stream.is_null() {
                    return SANKAKU_STATUS_INTERNAL;
                }

                *taken = true;
                unsafe {
                    *out_stream = stream;
                }
                SANKAKU_STATUS_OK
            },
        )
    }))
    .unwrap_or(SANKAKU_STATUS_PANIC)
}

#[unsafe(no_mangle)]
pub extern "C" fn sankaku_call_destroy(handle: *mut SankakuCallHandle) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() {
            return;
        }
        let call = unsafe { Arc::from_raw(handle.cast::<FfiCallState>()) };
        call.connection.close(0u32.into(), CLOSE_REASON_DESTROY);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn make_endpoint_config(bind: &str) -> SankakuCallEndpointConfig {
        SankakuCallEndpointConfig {
            bind_addr_utf8: bind.as_ptr().cast::<c_char>(),
            bind_addr_len: bind.len(),
        }
    }

    fn wait_for_event(
        endpoint: *mut SankakuCallEndpointHandle,
        expected: SankakuCallEventKind,
    ) -> Box<OwnedCallEvent> {
        for _ in 0..200 {
            let mut event = ptr::null_mut();
            let status = sankaku_call_poll_event(endpoint, &mut event);
            if status == SANKAKU_STATUS_OK {
                let owned = unsafe { Box::from_raw(event.cast::<OwnedCallEvent>()) };
                if owned.event.kind == expected {
                    return owned;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {:?}", expected);
    }

    #[test]
    fn invalid_identity_without_flag_is_rejected() {
        let params = SankakuCallDialParams {
            remote_addr_utf8: "127.0.0.1:4444".as_ptr().cast::<c_char>(),
            remote_addr_len: "127.0.0.1:4444".len(),
            remote_identity: ptr::null(),
            remote_identity_len: 0,
            flags: 0,
        };
        assert_eq!(
            parse_remote_identity(
                params.remote_identity,
                params.remote_identity_len,
                params.flags
            ),
            Err(SANKAKU_STATUS_INVALID_ARGUMENT)
        );
    }

    #[test]
    fn server_endpoint_factory_supports_loopback() {
        let runtime = build_runtime().unwrap();
        let _guard = runtime.enter();
        let (endpoint, identity) = make_server_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        assert_eq!(identity.len(), SANKAKU_CALL_IDENTITY_LEN);
        endpoint.close(0u32.into(), b"test");
    }

    #[test]
    fn call_offer_accept_and_end_flow_exports_streams() {
        let callee_endpoint = sankaku_call_endpoint_create(&make_endpoint_config("127.0.0.1:0"));
        assert!(!callee_endpoint.is_null());

        let mut addr_len = 0usize;
        assert_eq!(
            sankaku_call_endpoint_copy_local_addr(
                callee_endpoint,
                ptr::null_mut(),
                0,
                &mut addr_len
            ),
            SANKAKU_STATUS_OK
        );
        let mut addr = vec![0u8; addr_len];
        assert_eq!(
            sankaku_call_endpoint_copy_local_addr(
                callee_endpoint,
                addr.as_mut_ptr().cast::<c_char>(),
                addr.len(),
                &mut addr_len,
            ),
            SANKAKU_STATUS_OK
        );
        let callee_addr = String::from_utf8(addr).unwrap();

        let mut identity_len = 0usize;
        assert_eq!(
            sankaku_call_endpoint_copy_identity(
                callee_endpoint,
                ptr::null_mut(),
                0,
                &mut identity_len
            ),
            SANKAKU_STATUS_OK
        );
        assert_eq!(identity_len, SANKAKU_CALL_IDENTITY_LEN);
        let mut identity = vec![0u8; identity_len];
        assert_eq!(
            sankaku_call_endpoint_copy_identity(
                callee_endpoint,
                identity.as_mut_ptr(),
                identity.len(),
                &mut identity_len,
            ),
            SANKAKU_STATUS_OK
        );

        let caller_endpoint = sankaku_call_endpoint_create(&make_endpoint_config("127.0.0.1:0"));
        assert!(!caller_endpoint.is_null());

        let mut caller_call = ptr::null_mut();
        let dial = SankakuCallDialParams {
            remote_addr_utf8: callee_addr.as_ptr().cast::<c_char>(),
            remote_addr_len: callee_addr.len(),
            remote_identity: identity.as_ptr(),
            remote_identity_len: identity.len(),
            flags: 0,
        };
        assert_eq!(
            sankaku_call_place(caller_endpoint, &dial, &mut caller_call),
            SANKAKU_STATUS_OK
        );
        assert!(!caller_call.is_null());

        let outgoing = wait_for_event(caller_endpoint, SankakuCallEventKind::OutgoingRinging);
        assert_eq!(outgoing.event.call, caller_call);

        let incoming = wait_for_event(callee_endpoint, SankakuCallEventKind::IncomingOffer);
        let callee_call = incoming.event.call;
        assert!(!callee_call.is_null());
        assert_eq!(sankaku_call_accept(callee_call), SANKAKU_STATUS_OK);

        let accepted = wait_for_event(caller_endpoint, SankakuCallEventKind::Accepted);
        assert_eq!(accepted.event.call, caller_call);
        let connected = wait_for_event(caller_endpoint, SankakuCallEventKind::Connected);
        assert_eq!(connected.event.call, caller_call);

        let mut caller_stream = ptr::null_mut();
        let mut callee_stream = ptr::null_mut();
        assert_eq!(
            sankaku_call_take_stream(caller_call, &mut caller_stream),
            SANKAKU_STATUS_OK
        );
        assert!(!caller_stream.is_null());
        assert_eq!(
            sankaku_call_take_stream(callee_call, &mut callee_stream),
            SANKAKU_STATUS_OK
        );
        assert!(!callee_stream.is_null());

        assert_eq!(sankaku_call_end(caller_call), SANKAKU_STATUS_OK);
        let ended = wait_for_event(callee_endpoint, SankakuCallEventKind::Ended);
        assert_eq!(ended.event.call, callee_call);

        crate::ffi::sankaku_stream_destroy(caller_stream);
        crate::ffi::sankaku_stream_destroy(callee_stream);
        sankaku_call_destroy(caller_call);
        sankaku_call_destroy(callee_call);
        sankaku_call_endpoint_destroy(caller_endpoint);
        sankaku_call_endpoint_destroy(callee_endpoint);
    }
}
