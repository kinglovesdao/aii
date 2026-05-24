# aii-crypto

Cryptographic primitives for the AII protocol stack.

## Modules

| Module    | Purpose                                                            |
|-----------|--------------------------------------------------------------------|
| `keccak`  | Keccak-256 hashing (Ethereum-compatible, **not** FIPS-202 SHA3).   |
| `secp`    | secp256k1 ECDSA sign / verify / public-key recovery (planned).     |
| `bls`     | BLS12-381 single + aggregate sign / verify (planned).              |
| `vrf`     | Schnorrkel VRF for V-node leader election (planned).               |
| `error`   | `CryptoError` umbrella.                                            |

## Quickstart

```rust
use aii_crypto::keccak256;

let h = keccak256(b"");
assert_eq!(
    hex::encode(h.as_bytes()),
    "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
);
```

## Stability

`0.0.x` — unstable. Breaking changes allowed in any release until `0.1.0`.

## Roadmap

- v0.0.3 (this release, in progress):
  - `keccak::keccak256`
  - secp256k1 sign / verify / recover (+ ETH address derivation)
  - BLS12-381 sign / verify / aggregate (via `blst`)
  - Schnorrkel VRF prove / verify
  - Property-test integration suite
- v0.1.0: KAT vector corpus for every primitive; `cargo-fuzz` harness on
  signature decoding.
