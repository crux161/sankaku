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
    SANKAKU_STATUS_PANIC = -7
};

enum {
    SANKAKU_FRAME_FLAG_KEYFRAME = 0x00000001
};

enum {
    SANKAKU_VIDEO_CODEC_HEVC = 0x01,
    SANKAKU_VIDEO_CODEC_H264 = 0x02
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

/*
 * Thread-safety:
 * - `sankaku_stream_create` and `sankaku_stream_destroy` may be called from any thread.
 * - Operations on the same `SankakuStreamHandle` are internally serialized by the DLL.
 * - `sankaku_stream_destroy` must not race with any other function using the same handle.
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

SANKAKU_API void init(void);

#ifdef __cplusplus
}
#endif

#endif
