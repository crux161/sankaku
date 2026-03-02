# SANKAKU-FFI-RESPONSE

Audience: Kagami developers  
Subject: Sankaku/RT FFI availability and integration guidance

## Summary

The Sankaku Streaming FFI is now implemented in `sankaku-core` and exported through a stable C ABI for downstream integration.

This update provides:

- An opaque stream handle for managing Sankaku/RT session state across the ABI boundary.
- C-compatible frame structs for outbound and inbound video payload exchange.
- Exported lifecycle, send, polling, and frame-free functions.
- A strict ownership model to prevent cross-allocator faults.

No Rust-specific async, Tokio, Quinn, or standard-library container types are exposed in the public C ABI surface.

## Data Structures and Handles

### Opaque Stream Handle

Sankaku now exposes an opaque stream handle:

```c
typedef struct SankakuStreamHandle SankakuStreamHandle;
```

This handle owns the internal Rust session state, including the sender/receiver pipeline and the runtime tasks required to service QUIC-backed Sankaku/RT traffic.

Kagami must treat this type as opaque and only pass it back to exported Sankaku functions.

### QUIC Session Wrapper

Stream creation accepts an FFI-safe QUIC handle wrapper:

```c
typedef enum SankakuQuicHandleKind {
    SANKAKU_QUIC_HANDLE_INVALID = 0,
    SANKAKU_QUIC_HANDLE_CONNECTION = 1,
    SANKAKU_QUIC_HANDLE_ENDPOINT = 2
} SankakuQuicHandleKind;

typedef struct SankakuQuicHandle {
    SankakuQuicHandleKind kind;
    void* handle;
} SankakuQuicHandle;
```

This is the ABI-safe representation used to transfer ownership of an already-configured QUIC connection or endpoint into Sankaku.

### Outbound Frame Structure

Outbound video submission uses:

```c
typedef struct SankakuVideoFrame {
    const uint8_t* data;
    size_t len;
    uint64_t pts_us;
    uint64_t dts_us;
    uint8_t codec;
    SankakuFrameKind kind;
    uint32_t flags;
} SankakuVideoFrame;
```

This structure is C-compatible and contains only raw pointers, sizes, timestamps, codec identifiers, and frame flags.

### Inbound Frame Structure

Inbound frame delivery uses:

```c
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
```

This structure is heap-allocated by Sankaku when a frame is returned from polling.

## Exported FFI Functions

### Lifecycle

```c
SankakuStreamHandle* sankaku_stream_create(SankakuQuicHandle quic_handle);
void sankaku_stream_destroy(SankakuStreamHandle* handle);
```

`sankaku_stream_create`

- Creates a Sankaku/RT stream instance from an owned QUIC connection or endpoint wrapper.
- Initializes the internal runtime and streaming state required by the sender/receiver loop.
- Returns `NULL` on failure.

`sankaku_stream_destroy`

- Shuts down the stream and releases all internal Rust-owned resources.
- Safe to call with `NULL`.
- Must not race with any other operation using the same handle.

### Transmission

```c
int32_t sankaku_stream_send_frame(
    SankakuStreamHandle* handle,
    const SankakuVideoFrame* frame
);
```

- Submits one outbound frame into the Sankaku sender pipeline.
- Copies the payload from the caller-provided frame buffer into internal Rust-owned storage before transmission.
- Returns `0` on success.
- Returns negative error codes for invalid arguments, invalid handles, disconnected streams, internal failures, or overflow conditions.
- Internal Rust panics are trapped and converted into an error code rather than crossing the C ABI boundary.

### Polling and Ownership

```c
int32_t sankaku_stream_poll_frame(
    SankakuStreamHandle* handle,
    SankakuInboundFrame** out_frame
);

void sankaku_frame_free(SankakuInboundFrame* frame);
```

`sankaku_stream_poll_frame`

- Performs a non-blocking poll against the internal inbound frame queue.
- If a frame is available, allocates a `SankakuInboundFrame` and returns it through `out_frame`.
- Returns a specific negative status when no frame is currently available.
- Does not block the calling thread waiting for network input.

`sankaku_frame_free`

- Releases a frame previously returned by `sankaku_stream_poll_frame`.
- Must be called by Kagami once it has finished processing an inbound frame.

## Compiled Artifacts

Sankaku-core produces a dynamic library (`cdylib`) and a Rust-linkable static lib (`rlib`) for each supported target.

### Build Command

```bash
cargo build -p sankaku-core --lib --release
```

### Per-Platform Outputs

| Platform | Cargo Target | Dynamic Library | Import/Link Library |
|---|---|---|---|
| Windows x86-64 | `x86_64-pc-windows-msvc` | `sankaku.dll` | `sankaku.dll.lib` |
| Windows ARM64 | `aarch64-pc-windows-msvc` | `sankaku.dll` | `sankaku.dll.lib` |
| macOS (Universal) | `aarch64-apple-darwin` / `x86_64-apple-darwin` | `libsankaku.dylib` | (embedded) |
| Linux x86-64 | `x86_64-unknown-linux-gnu` | `libsankaku.so` | (embedded) |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `libsankaku.so` | (embedded) |
| iOS (device) | `aarch64-apple-ios` | `libsankaku.dylib` | (embedded) |

For cross-compilation, supply `--target <triple>`:

```bash
cargo build -p sankaku-core --lib --release --target aarch64-pc-windows-msvc
```

A macOS universal binary can be produced with `lipo` after building both `aarch64-apple-darwin` and `x86_64-apple-darwin`.

### C Header

The public C header is located at `sankaku-core/include/sankaku.h`. On Windows, the `SANKAKU_API` macro resolves to `__declspec(dllexport)` when building the DLL and `__declspec(dllimport)` when consuming it. Consumers must **not** define `SANKAKU_BUILD_DLL`.

### Linking on Windows

Link against the generated import library (`sankaku.dll.lib`) and ensure `sankaku.dll` is available at runtime. For Kagami, the expected layout remains:

- `core/sankaku/sankaku.dll`
- `core/sankaku/sankaku.dll.lib`
- `core/nezumi/nezumi.dll`
- `core/nezumi/nezumi.dll.lib`

### Linking on macOS / Linux / iOS

Link against `libsankaku.dylib` (macOS/iOS) or `libsankaku.so` (Linux) using standard `-lsankaku` / `-L<path>` linker flags.

## Memory Contract

The allocator boundary is strict.

Rules:

- Kagami owns the memory behind any outbound `SankakuVideoFrame` input buffer it provides.
- Sankaku copies outbound frame payloads into internal Rust-managed storage before transmission.
- Sankaku owns any `SankakuInboundFrame` returned by `sankaku_stream_poll_frame` until Kagami explicitly releases it.
- Kagami must return every inbound frame to:

```c
void sankaku_frame_free(SankakuInboundFrame* frame);
```

Failure to do so will cause memory leaks. Freeing these frames with a foreign allocator instead of `sankaku_frame_free` risks cross-allocator corruption or process crashes.

## Integration Notes

- The public ABI intentionally avoids exposing Rust futures, Tokio runtimes, `Vec`, `String`, `quinn::Connection`, or other Rust-native implementation types.
- Handle operations are internally serialized inside the Sankaku DLL.
- `sankaku_stream_destroy` must not run concurrently with send or poll operations on the same handle.

## Recommended Next Step for Kagami

Kagami can now bind directly against `sankaku.h`, link against `sankaku.dll.lib`, and begin integration using the lifecycle, send, poll, and free functions described above.
