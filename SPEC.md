# Sankaku/RT Wire Format (QUIC Transport)

Sankaku/RT runs as an application-layer protocol over QUIC:

- Media frames use QUIC Datagrams (unordered, unreliable, MTU-limited).
- Control and lifecycle signaling use a reliable QUIC bidirectional stream.
- QUIC/TLS 1.3 provides transport authentication and encryption.

## 1. Session and Security

There is no Sankaku-specific `H`/`R` handshake packet exchange on the wire.
Session authentication and channel security are provided by the underlying QUIC connection.

## 2. Media Datagram Packet (`D`)

Media packets are sent on QUIC datagrams and are variable sized.

| Offset | Size | Field | Notes |
| :--- | :--- | :--- | :--- |
| 0 | 1 | Type | `0x44` (`D`) |
| 1 | 8 | Session ID | Plaintext lookup key |
| 9 | 24 | Geometry Header | Plaintext |
| 33 | `PktSize` | Wirehair droplet bytes | Actual encoded droplet |
| var | optional | Padding | Optional policy-driven padding |

`TARGET_PACKET_SIZE` is bounded by `connection.max_datagram_size() - DATA_PREFIX_SIZE`
with a fallback ceiling for paths that do not advertise datagram size.

## 3. Geometry Header (24 bytes, Plaintext)

`frame_index` is the monotonic per-stream frame counter.

| Offset | Type | Field |
| :--- | :--- | :--- |
| 0 | `u32` | Stream ID |
| 4 | `u64` | Frame Index |
| 12 | `u32` | FEC Sequence ID |
| 16 | `u32` | Protected Size |
| 20 | `u16` | Packet Size |
| 22 | `u8` | Data Kind Flag (`0` = NAL, `1` = SAO) |
| 23 | `u8` | Codec ID |

Header bytes are written and read directly (no XOR masking step).

## 4. Frame Payload Pipeline

Per frame:

1. Serialize frame envelope (`timestamp_us`, `keyframe`, raw payload bytes).
2. If kind is SAO and compression enabled, apply OpenZL.
3. Wrap with pipeline envelope mode byte.
4. Apply Wirehair FEC and emit droplets.

Receiver reverses this path and emits recovered frames.

## 5. Control Stream Packets (Reliable QUIC Stream)

Control packets are sent over a reliable QUIC bidirectional stream and are not multiplexed into media datagrams.

- `F` (`0x46`): FEC feedback (`ideal_packets`, `used_packets`)
- `T` (`0x54`): telemetry (`packet_loss_ppm`, `jitter_us`)
- `E` (`0x45`): stream finish marker (`final_bytes`, `final_frames`)
- `A` (`0x41`): stream ACK
- `B` (`0x42`): loss-ratio feedback

Each control message payload is encoded exactly as before (`type byte + bincode struct payload`);
transporting them over a QUIC stream provides ordered, reliable delivery.

## 6. Async API Surface

`SankakuStream` exposes async send/receive of `VideoFrame`:

- outbound: `send(VideoFrame)`
- inbound: `recv() -> InboundVideoFrame`
- optional ticket import/export remains API-compatible for callers migrating transport wiring
