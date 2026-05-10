# ADR-018: RustCrypto for Local IPC Bootstrap Authentication

**Status:** Accepted | **Date:** 2026-05-05 | **Supersedes:** None

## TL;DR

Use RustCrypto HMAC-SHA-256 plus OS randomness for opt-in Phase 3 local IPC bootstrap
authentication.

## Problem

IPC transfers CPU and GPU external-memory handles between peer processes. Existing platform access controls reduce
endpoint exposure, but they do not authenticate that the connected same-user or same-session process is the intended peer.

The hardening layer needs to:

- Authenticate local peers before any message envelope or handle transfer
- Work the same way on Windows and Linux local transports
- Keep the public API synchronous and builder-oriented
- Avoid copyleft dependencies
- Allow builds to opt out of the RustCrypto dependency set when shared-secret authentication is not
  needed

## Decision

Use a default-enabled Cargo feature, `rustcrypto-auth`, for shared-secret bootstrap authentication.

The feature enables the following new dependencies. All newly introduced cryptography crates come
from the RustCrypto project:

- RustCrypto `hmac` for HMAC construction and constant-time tag verification
- RustCrypto `sha2` for SHA-256
- RustCrypto `zeroize` for best-effort shared-secret memory clearing on drop
- `getrandom` for per-connection OS-random nonces

The public builder API remains available regardless of feature selection. If a caller configures a
shared secret while `rustcrypto-auth` is disabled, local endpoint construction fails explicitly with
an unsupported-authentication error.

## Rationale

- HMAC-SHA-256 is a conservative, widely reviewed primitive for shared-secret challenge/response.
- RustCrypto crates are small, focused, and widely used in Rust projects.
- `hmac::Mac::verify_slice` avoids ad hoc tag comparison.
- `getrandom` delegates nonce generation directly to the operating system without adding a
  user-space random-number-generator abstraction that this library does not otherwise need.
  Bootstrap authentication only needs fixed-size, unpredictable nonces, not a long-lived RNG object
  or distribution API.
- A feature gate keeps the dependency set configurable without changing the public builder API.

## Protocol Shape

The local bootstrap includes:

- connection header with local protocol version, metadata encoding, local access policy, and auth
  mode
- 32-byte sender and receiver nonces
- HMAC transcript covering:
  - local protocol domain separator
  - protocol version
  - metadata encoding
  - local access policy
  - auth mode
  - `ChannelId`
  - both nonces
  - authenticated endpoint role (`sender` response or `receiver` confirmation)

Authentication completes before any message envelope or CPU/GPU handle transfer.

## Alternatives Considered

### Hand-rolled hash/MAC construction

Rejected. It would increase security review burden and risk subtle construction or comparison bugs.

### Platform-only peer checks

Rejected as the primary mechanism. PID and same-user checks are useful pre-authentication filters,
but they do not authenticate application intent and are weaker than a shared-secret transcript.

### `rand`

Rejected for this layer. `rand` is a good general-purpose RNG facade, but it brings a broader API
surface than needed here. Local IPC authentication only needs OS-provided bytes for 32-byte nonces,
which is exactly the boundary exposed by `getrandom`.

### Platform-specific randomness calls

Rejected. Calling `BCryptGenRandom`, `getrandom(2)`, `/dev/urandom`, or equivalent APIs directly
would duplicate platform-specific fallback and error-handling logic already maintained by
`getrandom`. It would also expand unsafe/FFI surface in a security-sensitive path.

### Deriving nonces from timestamps, process ids, or counters

Rejected. These values are predictable or partially predictable and are not suitable nonces for
challenge/response authentication.

### TLS or Noise-style framework

Deferred. These are stronger and more general, but they add more design surface than Phase 3 local
IPC needs for a point-to-point same-host bootstrap.

## Security Review Notes

- Shared secrets are never written to logs or `Debug` output.
- Secret bytes are zeroized on drop when `rustcrypto-auth` is enabled.
- HMAC tags are verified through RustCrypto's constant-time verification API.
- Nonces are generated with OS randomness through `getrandom`.
- `cargo audit` run on 2026-05-06 scanned the lockfile with these dependencies and reported no
  vulnerabilities.

## License Review Notes

- `hmac`: `MIT OR Apache-2.0`
- `sha2`: `MIT OR Apache-2.0`
- `getrandom`: `MIT OR Apache-2.0`
- `zeroize`: `Apache-2.0 OR MIT`
- No copyleft dependency is introduced by this decision.

## Consequences

- Local IPC can authenticate peers with a shared secret before handle transfer.
- Builds that do not need shared-secret IPC authentication can opt out with `--no-default-features`.
- The local channel bootstrap protocol now has an explicit auth-mode field and connection outcome.
