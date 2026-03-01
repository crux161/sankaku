use crate::{InboundVideoFrame, QuicHandle, SankakuStream, VideoFrame, VideoPayloadKind, init};
use quinn::{Connection, Endpoint};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::slice;
use std::sync::Mutex;
use tokio::runtime::{Builder, Runtime};

pub const SANKAKU_STATUS_OK: i32 = 0;
pub const SANKAKU_STATUS_INVALID_ARGUMENT: i32 = -1;
pub const SANKAKU_STATUS_INVALID_HANDLE: i32 = -2;
pub const SANKAKU_STATUS_DISCONNECTED: i32 = -3;
pub const SANKAKU_STATUS_WOULD_BLOCK: i32 = -4;
pub const SANKAKU_STATUS_BUFFER_OVERFLOW: i32 = -5;
pub const SANKAKU_STATUS_INTERNAL: i32 = -6;
pub const SANKAKU_STATUS_PANIC: i32 = -7;

pub const SANKAKU_FRAME_FLAG_KEYFRAME: u32 = 0x0000_0001;

#[repr(C)]
pub struct SankakuStreamHandle {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SankakuQuicHandleKind {
    Invalid = 0,
    Connection = 1,
    Endpoint = 2,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SankakuQuicHandle {
    pub kind: SankakuQuicHandleKind,
    pub handle: *mut c_void,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SankakuFrameKind {
    NalUnit = 0,
    SaoParameters = 1,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SankakuVideoFrame {
    pub data: *const u8,
    pub len: usize,
    pub pts_us: u64,
    pub dts_us: u64,
    pub codec: u8,
    pub kind: SankakuFrameKind,
    pub flags: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SankakuInboundFrame {
    pub data: *const u8,
    pub len: usize,
    pub session_id: u64,
    pub stream_id: u32,
    pub frame_index: u64,
    pub pts_us: u64,
    pub dts_us: u64,
    pub codec: u8,
    pub kind: SankakuFrameKind,
    pub flags: u32,
    pub packet_loss_ratio: f32,
}

struct FfiStreamState {
    runtime: Runtime,
    stream: SankakuStream,
}

struct FfiStreamHandle {
    state: Mutex<FfiStreamState>,
}

#[repr(C)]
struct OwnedInboundFrame {
    frame: SankakuInboundFrame,
    payload: Box<[u8]>,
}

impl OwnedInboundFrame {
    fn new(frame: InboundVideoFrame) -> Box<Self> {
        let payload = frame.payload.into_boxed_slice();
        let data = payload.as_ptr();
        let flags = if frame.keyframe {
            SANKAKU_FRAME_FLAG_KEYFRAME
        } else {
            0
        };
        Box::new(Self {
            frame: SankakuInboundFrame {
                data,
                len: payload.len(),
                session_id: frame.session_id,
                stream_id: frame.stream_id,
                frame_index: frame.frame_index,
                pts_us: frame.timestamp_us,
                dts_us: frame.timestamp_us,
                codec: frame.codec,
                kind: frame_kind_from_rust(frame.kind),
                flags,
                packet_loss_ratio: frame.packet_loss_ratio,
            },
            payload,
        })
    }
}

fn frame_kind_from_rust(kind: VideoPayloadKind) -> SankakuFrameKind {
    match kind {
        VideoPayloadKind::NalUnit => SankakuFrameKind::NalUnit,
        VideoPayloadKind::SaoParameters => SankakuFrameKind::SaoParameters,
    }
}

fn frame_kind_to_rust(kind: SankakuFrameKind) -> Result<VideoPayloadKind, i32> {
    match kind {
        SankakuFrameKind::NalUnit => Ok(VideoPayloadKind::NalUnit),
        SankakuFrameKind::SaoParameters => Ok(VideoPayloadKind::SaoParameters),
    }
}

fn map_runtime_error(message: &str) -> i32 {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("disconnected")
        || lowered.contains("connection")
        || lowered.contains("timed out")
        || lowered.contains("[socket]")
    {
        return SANKAKU_STATUS_DISCONNECTED;
    }
    if lowered.contains("too large")
        || lowered.contains("overflow")
        || lowered.contains("exceeded")
    {
        return SANKAKU_STATUS_BUFFER_OVERFLOW;
    }
    SANKAKU_STATUS_INTERNAL
}

unsafe fn take_quic_handle(handle: SankakuQuicHandle) -> Result<QuicHandle, i32> {
    if handle.handle.is_null() {
        return Err(SANKAKU_STATUS_INVALID_ARGUMENT);
    }

    match handle.kind {
        SankakuQuicHandleKind::Connection => {
            let connection = *unsafe { Box::from_raw(handle.handle.cast::<Connection>()) };
            Ok(QuicHandle::Connection(connection))
        }
        SankakuQuicHandleKind::Endpoint => {
            let endpoint = *unsafe { Box::from_raw(handle.handle.cast::<Endpoint>()) };
            Ok(QuicHandle::Endpoint(endpoint))
        }
        SankakuQuicHandleKind::Invalid => Err(SANKAKU_STATUS_INVALID_ARGUMENT),
    }
}

fn with_stream_state<T>(
    handle: *mut SankakuStreamHandle,
    on_invalid_handle: T,
    on_panic: T,
    f: impl FnOnce(&mut FfiStreamState) -> T,
) -> T {
    catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() {
            return on_invalid_handle;
        }
        let raw = unsafe { &*(handle.cast::<FfiStreamHandle>()) };
        let mut guard = match raw.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&mut guard)
    }))
    .unwrap_or(on_panic)
}

/// Creates an opaque Sankaku/RT stream from an owned QUIC connection or endpoint pointer.
///
/// The passed `SankakuQuicHandle` transfers ownership of the underlying Rust allocation
/// (`Box<quinn::Connection>` or `Box<quinn::Endpoint>`) into the Sankaku DLL.
#[unsafe(no_mangle)]
pub extern "C" fn sankaku_stream_create(quic_handle: SankakuQuicHandle) -> *mut SankakuStreamHandle {
    catch_unwind(AssertUnwindSafe(|| {
        init();
        let runtime = match Builder::new_multi_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(_) => return ptr::null_mut(),
        };
        let quic_handle = match unsafe { take_quic_handle(quic_handle) } {
            Ok(handle) => handle,
            Err(_) => return ptr::null_mut(),
        };
        let stream = match runtime.block_on(SankakuStream::connect(quic_handle)) {
            Ok(stream) => stream,
            Err(_) => return ptr::null_mut(),
        };
        let handle = Box::new(FfiStreamHandle {
            state: Mutex::new(FfiStreamState { runtime, stream }),
        });
        Box::into_raw(handle).cast::<SankakuStreamHandle>()
    }))
    .unwrap_or(ptr::null_mut())
}

/// Destroys a previously created stream handle.
///
/// This function is safe to call with `NULL`. It must not race with any other operation
/// on the same handle.
#[unsafe(no_mangle)]
pub extern "C" fn sankaku_stream_destroy(handle: *mut SankakuStreamHandle) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() {
            return;
        }
        let handle = unsafe { Box::from_raw(handle.cast::<FfiStreamHandle>()) };
        if let Ok(guard) = handle.state.lock() {
            guard.stream.close();
        }
    }));
}

/// Sends one outbound frame over the QUIC-backed Sankaku/RT session.
#[unsafe(no_mangle)]
pub extern "C" fn sankaku_stream_send_frame(
    handle: *mut SankakuStreamHandle,
    frame: *const SankakuVideoFrame,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if frame.is_null() {
            return SANKAKU_STATUS_INVALID_ARGUMENT;
        }

        let frame = unsafe { &*frame };
        if frame.len > 0 && frame.data.is_null() {
            return SANKAKU_STATUS_INVALID_ARGUMENT;
        }

        let kind = match frame_kind_to_rust(frame.kind) {
            Ok(kind) => kind,
            Err(code) => return code,
        };

        let payload = if frame.len == 0 {
            Vec::new()
        } else {
            unsafe { slice::from_raw_parts(frame.data, frame.len) }.to_vec()
        };
        let timestamp_us = if frame.pts_us != 0 {
            frame.pts_us
        } else {
            frame.dts_us
        };
        let rust_frame = VideoFrame {
            timestamp_us,
            keyframe: (frame.flags & SANKAKU_FRAME_FLAG_KEYFRAME) != 0,
            codec: frame.codec,
            kind,
            payload,
        };

        with_stream_state(
            handle,
            SANKAKU_STATUS_INVALID_HANDLE,
            SANKAKU_STATUS_PANIC,
            |state| match state.runtime.block_on(state.stream.send(rust_frame)) {
                Ok(_) => SANKAKU_STATUS_OK,
                Err(error) => map_runtime_error(&error.to_string()),
            },
        )
    }))
    .unwrap_or(SANKAKU_STATUS_PANIC)
}

/// Attempts to pop one inbound frame without blocking.
#[unsafe(no_mangle)]
pub extern "C" fn sankaku_stream_poll_frame(
    handle: *mut SankakuStreamHandle,
    out_frame: *mut *mut SankakuInboundFrame,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_frame.is_null() {
            return SANKAKU_STATUS_INVALID_ARGUMENT;
        }
        unsafe {
            *out_frame = ptr::null_mut();
        }

        with_stream_state(
            handle,
            SANKAKU_STATUS_INVALID_HANDLE,
            SANKAKU_STATUS_PANIC,
            |state| match state.stream.try_recv() {
                Ok(Some(frame)) => {
                    let owned = OwnedInboundFrame::new(frame);
                    let raw = Box::into_raw(owned);
                    unsafe {
                        *out_frame = &mut (*raw).frame;
                    }
                    SANKAKU_STATUS_OK
                }
                Ok(None) => SANKAKU_STATUS_WOULD_BLOCK,
                Err(error) => map_runtime_error(&error.to_string()),
            },
        )
    }))
    .unwrap_or(SANKAKU_STATUS_PANIC)
}

/// Releases an inbound frame previously returned by `sankaku_stream_poll_frame`.
#[unsafe(no_mangle)]
pub extern "C" fn sankaku_frame_free(frame: *mut SankakuInboundFrame) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if frame.is_null() {
            return;
        }
        unsafe {
            drop(Box::from_raw(frame.cast::<OwnedInboundFrame>()));
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbound_frame_wrapper_preserves_payload_pointer_and_length() {
        let payload = b"ffi-frame".to_vec();
        let owned = OwnedInboundFrame::new(InboundVideoFrame {
            session_id: 1,
            stream_id: 2,
            frame_index: 3,
            timestamp_us: 4,
            keyframe: true,
            codec: 5,
            packet_loss_ratio: 0.25,
            kind: VideoPayloadKind::NalUnit,
            payload: payload.clone(),
        });

        assert_eq!(owned.frame.len, payload.len());
        assert!(!owned.frame.data.is_null());
        let round_trip = unsafe { slice::from_raw_parts(owned.frame.data, owned.frame.len) };
        assert_eq!(round_trip, payload.as_slice());
    }
}
