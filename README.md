# Sankaku

Sankaku is a frame-native UDP transport focused on realtime video payload delivery.
It preserves authenticated X25519/ChaCha20-Poly1305 session setup and Wirehair FEC, and replaces file-centric flow with async in-memory frame I/O.

## Workspace Crates

- `sankaku-core`: async transport library (`SankakuSender`, `SankakuReceiver`, `SankakuStream`)
- `sankaku-cli`: minimal CLI for sending/receiving generated frame traffic
- `sankaku-wirehair-sys`: Wirehair FEC bindings
- `sankaku-openzl-sys`: OpenZL FFI bindings (wired for SAO payload path)

## Build

```bash
cargo check --workspace
```

## It's ALIVE 🧟‍♂️
```bash
../../../../openzl/zli train --profile sddl --profile-arg sao_compression.ozl ../sao_training.bin -o ../sao_graph.bin --force
Picked 1 samples out of 1 samples with total size 4170
Benchmarking untrained compressor...
1 files: 4170 -> 1827 (2.28),  20.13 MB/s  58.46 MB/s
Selected greedy trainer by default since no trainer was specified
[==================================================] Calculating improvement by clustering tag 11/11
[==================================================] Training ACE graph 1 / 4: ACE progress
[==================================================] Training ACE graph 3 / 4: ACE progress
[==================================================] Training ACE graph 4 / 4: ACE progress
Benchmarking trained compressor...
1 files: 4170 -> 1485 (2.81),  81.06 MB/s  283.59 MB/s
Training improved compression ratio by 23.03%

😆 yeah!
```

## Quick Start

Set a shared key:

```bash
export SANKAKU_PSK=00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff
```

Receiver:

```bash
cargo run -p sankaku-cli -- recv --bind 0.0.0.0:8080
```

Sender:

```bash
cargo run -p sankaku-cli -- send --dest 127.0.0.1:8080 --frames 120 --fps 30 --payload-bytes 1200
```

Send SAO-class payloads through the OpenZL path:

```bash
cargo run -p sankaku-cli -- send --dest 127.0.0.1:8080 --sao --frames 120 --fps 30
```

## Protocol

See `SPEC.md` for the v3 format:

- variable-size UDP payloads (no fixed 1200-byte enforcement)
- 23-byte masked geometry header with data-kind multiplex flag
- OpenZL stage for SAO payloads before ChaCha20-Poly1305
- RTCP-style telemetry (`loss`, `jitter`) + adaptive FEC tuning
- monotonic frame-index mapping (`block_id == frame_index`)

## License

MIT (`LICENSE`)
