# RESPONSE-SANKAKU-CALL-SIGNALING-KAGAMI-0001

Request ID: `kagami-sankaku-call-signaling-0001`
Audience: Kagami developers
Requester: Kagami
Status: Implemented Pending Packaging
Subject: Sankaku call bootstrap, incoming offer signaling, and safe FFI contract

## Summary

Sankaku now exposes a supported C ABI for call bootstrap and call signaling that does not require Kagami to fabricate or own `quinn` types.

This update adds:

- An opaque call endpoint handle that creates and owns the local QUIC listener/bootstrap state.
- An opaque per-call handle for outgoing calls, incoming offers, and established calls.
- A polling event model for outgoing ringing, incoming offer, accepted, rejected, connected, cancelled, ended, and transport failure.
- Explicit accept, reject, cancel, end, and stream-take functions.
- Strict invalid-input handling for the new exported functions.
- Updated public header definitions in `sankaku-core/include/sankaku.h`.

## Contract Clarification

The canonical meaning of `SankakuQuicHandle.handle` going forward is:

- a legacy compatibility hook for Rust/ABI-compatible embedders only.
- specifically, a pointer to a Rust-allocated `Box<quinn::Connection>` or `Box<quinn::Endpoint>` that transfers ownership into `sankaku_stream_create()`.
- not the supported public bootstrap contract for Kagami or other non-Rust consumers.

The supported public bootstrap contract for Kagami is now:

- `sankaku_call_endpoint_create()`
- `sankaku_call_place()`
- `sankaku_call_poll_event()`
- `sankaku_call_accept()`
- `sankaku_call_reject()`
- `sankaku_call_cancel()`
- `sankaku_call_end()`
- `sankaku_call_take_stream()`

## Implemented Surface

Exact new public symbols:

```c
SankakuCallEndpointHandle* sankaku_call_endpoint_create(
    const SankakuCallEndpointConfig* config
);

void sankaku_call_endpoint_destroy(SankakuCallEndpointHandle* handle);

int32_t sankaku_call_endpoint_copy_local_addr(
    SankakuCallEndpointHandle* handle,
    char* buffer,
    size_t buffer_len,
    size_t* out_len
);

int32_t sankaku_call_endpoint_copy_identity(
    SankakuCallEndpointHandle* handle,
    uint8_t* buffer,
    size_t buffer_len,
    size_t* out_len
);

int32_t sankaku_call_place(
    SankakuCallEndpointHandle* handle,
    const SankakuCallDialParams* params,
    SankakuCallHandle** out_call
);

int32_t sankaku_call_poll_event(
    SankakuCallEndpointHandle* handle,
    SankakuCallEvent** out_event
);

void sankaku_call_event_free(SankakuCallEvent* event);

int32_t sankaku_call_accept(SankakuCallHandle* handle);
int32_t sankaku_call_reject(
    SankakuCallHandle* handle,
    const char* reason_utf8,
    size_t reason_len
);
int32_t sankaku_call_cancel(SankakuCallHandle* handle);
int32_t sankaku_call_end(SankakuCallHandle* handle);

int32_t sankaku_call_copy_remote_addr(
    SankakuCallHandle* handle,
    char* buffer,
    size_t buffer_len,
    size_t* out_len
);

int32_t sankaku_call_copy_remote_identity(
    SankakuCallHandle* handle,
    uint8_t* buffer,
    size_t buffer_len,
    size_t* out_len
);

int32_t sankaku_call_take_stream(
    SankakuCallHandle* handle,
    SankakuStreamHandle** out_stream
);

void sankaku_call_destroy(SankakuCallHandle* handle);
```

Legacy compatibility symbols retained:

```c
SankakuStreamHandle* sankaku_stream_create(SankakuQuicHandle quic_handle);
void sankaku_stream_destroy(SankakuStreamHandle* handle);
int32_t sankaku_stream_send_frame(
    SankakuStreamHandle* handle,
    const SankakuVideoFrame* frame
);
int32_t sankaku_stream_poll_frame(
    SankakuStreamHandle* handle,
    SankakuInboundFrame** out_frame
);
void sankaku_frame_free(SankakuInboundFrame* frame);
```

## C Header Definitions

New opaque handles and configuration types:

```c
typedef struct SankakuCallEndpointHandle SankakuCallEndpointHandle;
typedef struct SankakuCallHandle SankakuCallHandle;

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
```

New event enum and payload:

```c
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
```

New related constants:

```c
enum {
    SANKAKU_CALL_DIAL_FLAG_ALLOW_INSECURE_ADDR_ONLY = 0x00000001
};

enum {
    SANKAKU_CALL_IDENTITY_LEN = 32
};
```

## Status and Error Codes

Existing status codes remain valid. New call-related codes are:

```c
enum {
    SANKAKU_STATUS_INVALID_STATE = -8,
    SANKAKU_STATUS_REJECTED = -9,
    SANKAKU_STATUS_BUFFER_TOO_SMALL = -10,
    SANKAKU_STATUS_UNSUPPORTED = -11
};
```

Contract:

- Handle-creating functions return `NULL` on failure.
- Non-creating functions return `0` on success or a negative status code on failure.
- `sankaku_call_poll_event()` returns `SANKAKU_STATUS_WOULD_BLOCK` when no event is queued.
- `copy_*` functions support size probing with `buffer = NULL` and `buffer_len = 0`.
- New functions validate null pointers and pointer/length pairs before dereferencing caller memory.
- FFI entrypoints catch Rust panics and convert them to `SANKAKU_STATUS_PANIC` or `NULL`.

## Ownership and Threading

Ownership:

- `SankakuCallEndpointHandle*` is owned by the caller and must be released with `sankaku_call_endpoint_destroy()`.
- `SankakuCallHandle*` is owned by the caller and must be released with `sankaku_call_destroy()`.
- `SankakuCallEvent*` returned from `sankaku_call_poll_event()` is Sankaku-owned until released with `sankaku_call_event_free()`.
- `SankakuStreamHandle*` returned from `sankaku_call_take_stream()` is caller-owned and must be released with `sankaku_stream_destroy()`.
- `sankaku_call_take_stream()` may succeed only once per call handle.

Threading:

- Endpoint and call functions may be called from any thread.
- Operations on the same endpoint or call handle are internally synchronized.
- Destroy functions must not race with other operations on the same handle.

String and byte rules:

- UTF-8 text values use pointer + length and are not NUL-terminated by Sankaku.
- Identity values are raw 32-byte blobs, not hex strings.

Identity notes:

- For outgoing calls with `remote_identity` supplied, Sankaku pins the remote server certificate to that 32-byte identity.
- If Kagami sets `SANKAKU_CALL_DIAL_FLAG_ALLOW_INSECURE_ADDR_ONLY`, Sankaku permits address-only dialing and derives the remote server identity after the QUIC handshake.
- Incoming offer `remote_identity` currently reflects the caller-declared endpoint identity carried in the offer message. It is useful for discovery correlation, but it is not yet mutual-certificate-authenticated.

## Integration Flow

### Caller places call

1. Create one local endpoint:

   ```c
   SankakuCallEndpointHandle* endpoint =
       sankaku_call_endpoint_create(&(SankakuCallEndpointConfig){
           .bind_addr_utf8 = "0.0.0.0:0",
           .bind_addr_len = 9,
       });
   ```

2. Read the local bind address and 32-byte identity:

   - `sankaku_call_endpoint_copy_local_addr()`
   - `sankaku_call_endpoint_copy_identity()`

3. Publish both through Kagami discovery. Recommended discovery payload:

   - `SocketAddr`
   - Sankaku 32-byte endpoint identity

4. Place a call:

   ```c
   SankakuCallHandle* call = NULL;
   int32_t status = sankaku_call_place(
       endpoint,
       &(SankakuCallDialParams){
           .remote_addr_utf8 = remote_addr,
           .remote_addr_len = remote_addr_len,
           .remote_identity = remote_identity,
           .remote_identity_len = 32,
           .flags = 0,
       },
       &call
   );
   ```

5. Poll endpoint events until:

   - `SANKAKU_CALL_EVENT_OUTGOING_RINGING`
   - then `SANKAKU_CALL_EVENT_ACCEPTED`
   - then `SANKAKU_CALL_EVENT_CONNECTED`
   - or `SANKAKU_CALL_EVENT_REJECTED`
   - or `SANKAKU_CALL_EVENT_CANCELLED`
   - or `SANKAKU_CALL_EVENT_TRANSPORT_FAILURE`

6. After `CONNECTED`, call:

   - `sankaku_call_take_stream()`

7. Use the returned `SankakuStreamHandle*` with existing:

   - `sankaku_stream_send_frame()`
   - `sankaku_stream_poll_frame()`
   - `sankaku_frame_free()`

### Callee receives incoming offer

1. Poll `sankaku_call_poll_event(endpoint, &event)`.
2. Wait for `SANKAKU_CALL_EVENT_INCOMING_OFFER`.
3. Read `event->call`.
4. Optionally inspect:

   - `event->remote_addr_utf8`
   - `event->remote_identity`
   - `sankaku_call_copy_remote_addr()`
   - `sankaku_call_copy_remote_identity()`

### Callee accepts

1. Call `sankaku_call_accept(event->call)`.
2. Poll for `ACCEPTED` and `CONNECTED`.
3. Call `sankaku_call_take_stream()`.
4. Begin media send/poll with the returned `SankakuStreamHandle*`.

### Callee rejects

1. Call `sankaku_call_reject(event->call, reason, reason_len)`.
2. Caller will receive `SANKAKU_CALL_EVENT_REJECTED`.
3. Both sides release the call handle with `sankaku_call_destroy()` when finished.

### Caller cancels ringing

1. Call `sankaku_call_cancel(call)`.
2. Callee will receive `SANKAKU_CALL_EVENT_CANCELLED`.

### Either side ends an established call

1. Call `sankaku_call_end(call)`.
2. Peer receives `SANKAKU_CALL_EVENT_ENDED`.
3. Release stream handle, call handle, and endpoint handle when done.

## Build and Platform Notes

Public header:

- `sankaku-core/include/sankaku.h`

Core library build:

```bash
cargo build -p sankaku-core --lib --release
```

Expected downstream artifacts remain:

- macOS x86_64: `libsankaku.dylib`
- macOS arm64: `libsankaku.dylib`
- macOS universal: combine both macOS builds with `lipo`
- Windows arm64: `sankaku.dll` plus `sankaku.dll.lib`

Notes:

- The ABI additions landed in code and header form in this turn.
- Cross-target packaging for macOS x86_64, macOS arm64/universal, and Windows arm64 was not built in this turn.

## Verification and Remaining Notes

Implemented and verified locally:

- `cargo check -p sankaku-core`
- `cargo test -p sankaku-core --lib call_ffi::tests -- --nocapture`

Verified behaviors:

- endpoint creation on loopback
- outgoing offer creation
- inbound offer polling
- accept flow
- stream export after connected
- end flow
- invalid remote identity input rejection

Remaining notes:

- The broader workspace test suite currently contains unrelated stale tests that do not compile against the current `SankakuStream` API. Those failures are outside this request and were not changed here.
- Incoming caller identity is currently offer-declared rather than mutual-certificate-authenticated. If Kagami needs authenticated caller identity at transport level, that should be tracked as a separate follow-up.
