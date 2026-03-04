#ifndef SANKAKU_H
#define SANKAKU_H

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32) && !defined(SANKAKU_STATIC)
#  if defined(SANKAKU_BUILD_DLL)
#    define SANKAKU_API __declspec(dllexport)
#  else
#    define SANKAKU_API __declspec(dllimport)
#  endif
#else
#  define SANKAKU_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef struct SankakuStreamHandle SankakuStreamHandle;
typedef struct SankakuCallEndpointHandle SankakuCallEndpointHandle;
typedef struct SankakuCallHandle SankakuCallHandle;

typedef enum SankakuQuicHandleKind {
    SANKAKU_QUIC_HANDLE_INVALID = 0,
    SANKAKU_QUIC_HANDLE_CONNECTION = 1,
    SANKAKU_QUIC_HANDLE_ENDPOINT = 2
} SankakuQuicHandleKind;

typedef enum SankakuFrameKind {
    SANKAKU_FRAME_KIND_NAL_UNIT = 0,
    SANKAKU_FRAME_KIND_SAO_PARAMETERS = 1
} SankakuFrameKind;

enum {
    SANKAKU_STATUS_OK = 0,
    SANKAKU_STATUS_INVALID_ARGUMENT = -1,
    SANKAKU_STATUS_INVALID_HANDLE = -2,
    SANKAKU_STATUS_DISCONNECTED = -3,
    SANKAKU_STATUS_WOULD_BLOCK = -4,
    SANKAKU_STATUS_BUFFER_OVERFLOW = -5,
    SANKAKU_STATUS_INTERNAL = -6,
    SANKAKU_STATUS_PANIC = -7,
    SANKAKU_STATUS_INVALID_STATE = -8,
    SANKAKU_STATUS_REJECTED = -9,
    SANKAKU_STATUS_BUFFER_TOO_SMALL = -10,
    SANKAKU_STATUS_UNSUPPORTED = -11
};

enum {
    SANKAKU_FRAME_FLAG_KEYFRAME = 0x00000001
};

enum {
    SANKAKU_VIDEO_CODEC_HEVC = 0x01,
    SANKAKU_VIDEO_CODEC_H264 = 0x02
};

enum {
    SANKAKU_CALL_DIAL_FLAG_ALLOW_INSECURE_ADDR_ONLY = 0x00000001
};

enum {
    SANKAKU_CALL_IDENTITY_LEN = 32
};

typedef struct SankakuQuicHandle {
    SankakuQuicHandleKind kind;
    void* handle;
} SankakuQuicHandle;

typedef struct SankakuVideoFrame {
    const uint8_t* data;
    size_t len;
    uint64_t pts_us;
    uint64_t dts_us;
    uint8_t codec;
    SankakuFrameKind kind;
    uint32_t flags;
} SankakuVideoFrame;

typedef struct SankakuInboundFrame {
    const uint8_t* data;
    size_t len;
    uint64_t session_id;
    uint32_t stream_id;
    uint64_t frame_index;
    uint64_t pts_us;
    uint64_t dts_us;
    uint8_t codec;
    SankakuFrameKind kind;
    uint32_t flags;
    float packet_loss_ratio;
} SankakuInboundFrame;

typedef struct SankakuCallEndpointConfig {
    const char* bind_addr_utf8;
    size_t bind_addr_len;
} SankakuCallEndpointConfig;

typedef struct SankakuCallDialParams {
    const char* remote_addr_utf8;
    size_t remote_addr_len;
    const uint8_t* remote_identity;
    size_t remote_identity_len;
    uint32_t flags;
} SankakuCallDialParams;

typedef enum SankakuCallEventKind {
    SANKAKU_CALL_EVENT_INVALID = 0,
    SANKAKU_CALL_EVENT_OUTGOING_RINGING = 1,
    SANKAKU_CALL_EVENT_INCOMING_OFFER = 2,
    SANKAKU_CALL_EVENT_ACCEPTED = 3,
    SANKAKU_CALL_EVENT_REJECTED = 4,
    SANKAKU_CALL_EVENT_CONNECTED = 5,
    SANKAKU_CALL_EVENT_CANCELLED = 6,
    SANKAKU_CALL_EVENT_ENDED = 7,
    SANKAKU_CALL_EVENT_TRANSPORT_FAILURE = 8
} SankakuCallEventKind;

typedef struct SankakuCallEvent {
    SankakuCallEventKind kind;
    SankakuCallHandle* call;
    uint64_t call_id;
    int32_t status;
    const char* remote_addr_utf8;
    size_t remote_addr_len;
    const uint8_t* remote_identity;
    size_t remote_identity_len;
    const char* message_utf8;
    size_t message_len;
} SankakuCallEvent;

/*
 * Thread-safety:
 * - `sankaku_stream_create` and `sankaku_stream_destroy` may be called from any thread.
 * - Operations on the same `SankakuStreamHandle` are internally serialized by the DLL.
 * - `sankaku_stream_destroy` must not race with any other function using the same handle.
 * - `SankakuQuicHandle.handle` is a legacy compatibility hook for Rust/ABI-compatible embedders.
 *   It is not the supported external bootstrap contract for C/C++ consumers.
 * - Ownership of `SankakuQuicHandle.handle` transfers to `sankaku_stream_create`; it must point
 *   to a Rust-allocated `Box<quinn::Connection>` or `Box<quinn::Endpoint>` compatible with this DLL.
 */
SANKAKU_API SankakuStreamHandle* sankaku_stream_create(SankakuQuicHandle quic_handle);

SANKAKU_API void sankaku_stream_destroy(SankakuStreamHandle* handle);

SANKAKU_API int32_t sankaku_stream_send_frame(
    SankakuStreamHandle* handle,
    const SankakuVideoFrame* frame
);

SANKAKU_API int32_t sankaku_stream_poll_frame(
    SankakuStreamHandle* handle,
    SankakuInboundFrame** out_frame
);

SANKAKU_API void sankaku_frame_free(SankakuInboundFrame* frame);

/*
 * Call bootstrap and signaling API:
 * - `SankakuCallEndpointHandle` owns the local QUIC listener/bootstrap state.
 * - `SankakuCallHandle` is an opaque per-call reference that remains valid until
 *   `sankaku_call_destroy`.
 * - Endpoint and call functions may be called from any thread.
 * - Operations on the same endpoint or call handle are internally synchronized.
 * - Destroy functions must not race with any other operation on the same handle.
 * - UTF-8 text values use pointer+length pairs and are not NUL-terminated by Sankaku.
 * - `copy_*` functions support size probing by passing `buffer = NULL`, `buffer_len = 0`.
 */
SANKAKU_API SankakuCallEndpointHandle* sankaku_call_endpoint_create(
    const SankakuCallEndpointConfig* config
);

SANKAKU_API void sankaku_call_endpoint_destroy(SankakuCallEndpointHandle* handle);

SANKAKU_API int32_t sankaku_call_endpoint_copy_local_addr(
    SankakuCallEndpointHandle* handle,
    char* buffer,
    size_t buffer_len,
    size_t* out_len
);

SANKAKU_API int32_t sankaku_call_endpoint_copy_identity(
    SankakuCallEndpointHandle* handle,
    uint8_t* buffer,
    size_t buffer_len,
    size_t* out_len
);

SANKAKU_API int32_t sankaku_call_place(
    SankakuCallEndpointHandle* handle,
    const SankakuCallDialParams* params,
    SankakuCallHandle** out_call
);

SANKAKU_API int32_t sankaku_call_poll_event(
    SankakuCallEndpointHandle* handle,
    SankakuCallEvent** out_event
);

SANKAKU_API void sankaku_call_event_free(SankakuCallEvent* event);

SANKAKU_API int32_t sankaku_call_accept(SankakuCallHandle* handle);

SANKAKU_API int32_t sankaku_call_reject(
    SankakuCallHandle* handle,
    const char* reason_utf8,
    size_t reason_len
);

SANKAKU_API int32_t sankaku_call_cancel(SankakuCallHandle* handle);

SANKAKU_API int32_t sankaku_call_end(SankakuCallHandle* handle);

SANKAKU_API int32_t sankaku_call_copy_remote_addr(
    SankakuCallHandle* handle,
    char* buffer,
    size_t buffer_len,
    size_t* out_len
);

SANKAKU_API int32_t sankaku_call_copy_remote_identity(
    SankakuCallHandle* handle,
    uint8_t* buffer,
    size_t buffer_len,
    size_t* out_len
);

SANKAKU_API int32_t sankaku_call_take_stream(
    SankakuCallHandle* handle,
    SankakuStreamHandle** out_stream
);

SANKAKU_API void sankaku_call_destroy(SankakuCallHandle* handle);

SANKAKU_API void init(void);

#ifdef __cplusplus
}
#endif

#endif
