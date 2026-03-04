# REQUEST-SANKAKU-CALL-SIGNALING-KAGAMI-0001

Request ID: `kagami-sankaku-call-signaling-0001`
Audience: Sankaku team
Requester: Kagami
Status: Requested

## Summary

Kagami needs a supported Sankaku/RT call bootstrap and incoming call signaling API so either peer can place a call, the callee can explicitly accept or reject it, and invalid caller input can never crash the process.

## Current Source State Observed By Kagami

The current Sankaku source of truth available to Kagami is the source tree under [reference/sankaku](/Volumes/DevWorkspace/Vanilla/kagami/reference/sankaku), especially:

- [reference/sankaku/sankaku-core/src/ffi.rs](/Volumes/DevWorkspace/Vanilla/kagami/reference/sankaku/sankaku-core/src/ffi.rs)
- [reference/sankaku/sankaku-core/include/sankaku.h](/Volumes/DevWorkspace/Vanilla/kagami/reference/sankaku/sankaku-core/include/sankaku.h)

From that source, Kagami observes:

- `SankakuQuicHandle.handle` is currently a `void*` in the header and is consumed as an owned Rust allocation in FFI.
- `sankaku_stream_create()` currently expects ownership of a Rust `Box<quinn::Connection>` or `Box<quinn::Endpoint>`.
- Kagami does not have access to `quinn` objects or any documented Sankaku API that can create a valid endpoint/listener/connection handle from C-safe inputs.
- Kagami currently discovers peers as `SocketAddr` values over mDNS and cannot safely transform those values into the Sankaku handle type required by the current FFI.
- Kagami also needs explicit incoming call offer, accept, reject, cancel, and end semantics. Those are not present in the currently exported public FFI surface.

## Problem Statement

Without a supported bootstrap API, Kagami cannot safely originate or receive Sankaku calls. Any attempt to infer or fabricate a `SankakuQuicHandle` from a local port or network address is invalid. Kagami has already observed a macOS crash where `sankaku_stream_create()` dereferenced invalid caller input during connect.

Kagami therefore needs a new public FFI surface for call setup and call signaling that is safe for non-Rust consumers and does not require downstream applications to traffic in `quinn` types.

## Requested Capabilities

Kagami requests a public Sankaku FFI/API that provides all of the following:

1. A documented way to create or obtain a valid local Sankaku endpoint or listener handle from C-safe inputs on macOS and Windows arm64.
2. A documented way to initiate an outgoing call or session to a remote peer by address, endpoint identity, or another Sankaku-owned opaque handle that Kagami can obtain without linking `quinn`.
3. A documented way for a passive peer to observe pending inbound call offers.
4. A documented way to accept an inbound call offer.
5. A documented way to reject an inbound call offer.
6. A documented way to cancel an outgoing ringing call.
7. A documented way to end an established call.
8. A documented event or polling model for at least these states:
   - outgoing ringing
   - incoming offer
   - accepted
   - rejected
   - connected
   - ended
   - transport failure
9. Strict invalid-input behavior for every exported call setup and signaling function:
   - return null or a negative error code on invalid input
   - never segfault
   - never unwind or abort across the FFI boundary
10. Ownership, lifetime, and thread-safety rules for every new handle and event payload.
11. Updated packaged headers and link/runtime artifacts for:
   - macOS x86_64
   - macOS arm64 or universal
   - Windows arm64

## Required Contract Clarification

Kagami needs one specific contract issue clarified in the response:

- What is the canonical meaning of `SankakuQuicHandle.handle` in the public API going forward?

Kagami needs the response to state clearly whether this field is:

- a pointer to Sankaku-owned state,
- a pointer to caller-owned Rust `quinn` state,
- a numeric endpoint identifier,
- or an obsolete concept that should be replaced by a new bootstrap API.

Kagami cannot safely integrate the current call setup path until this is documented and aligned with implementation.

## Desired Response Shape

Please return a response document tied to request id `kagami-sankaku-call-signaling-0001` that includes:

- the exact supported public symbols,
- C header definitions for new structs and enums,
- status/error codes,
- memory ownership rules,
- threading rules,
- platform notes,
- and the intended Kagami integration flow for:
  - caller places call
  - callee receives incoming offer
  - callee accepts or rejects
  - either side ends the call

## Kagami Integration Constraint

Kagami will not implement guessed function names or inferred handle semantics. Kagami will bind only to the exact public API documented in the Sankaku response for this request id.
