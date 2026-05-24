# aii-crypto

Cryptographic primitives for the AII protocol stack.

## Modules

| Module    | Purpose                                                            |
|-----------|--------------------------------------------------------------------|
| `keccak`  | Keccak-256 hashing (Ethereum-compatible, **not** FIPS-202 SHA3).   |
| `secp`    | secp256k1 ECDSA sign / verify / public-key recovery (ETH-style).   |
| `bls`     | BLS12-381 sign / verify / aggregate (Eth2 `min-pk`).               |
| `vrf`     | Schnorrkel VRF (Ristretto-25519) for V-node leader election.       |
| `error`   | `CryptoError` umbrella.                                            |

## Quickstart

```rust
use aii_crypto::{keccak256, secp, bls, vrf};

// Keccak-256
let h = keccak256(b"hello");

// secp256k1 — ETH-compatible
let sk = secp::SecretKey::from_bytes(&[/*…*/; 32]).unwrap();
let sig = secp::sign(&sk, &h).unwrap();
let pk = secp::recover(&sig, &h).unwrap();
let addr = pk.address();

// BLS12-381 (Eth2 min-pk)
let bls_sk = bls::SecretKey::from_ikm(&[/*…*/; 32], b"my-domain").unwrap();
let bls_sig = bls_sk.sign(b"PRE-COMMIT (round, hash)");
bls_sig.verify(b"PRE-COMMIT (round, hash)", &bls_sk.public_key()).unwrap();

// Schnorrkel VRF — leader election
let vrf_sk = vrf::SecretKey::generate();
let (proof, randomness) = vrf::prove(&vrf_sk, b"parent-seed");
let derived = vrf::verify(&vrf_sk.public_key(), b"parent-seed", &proof).unwrap();
assert_eq!(derived, randomness);
```

## Wire formats

| Type            | Bytes | Format                                                       |
|-----------------|-------|--------------------------------------------------------------|
| `H256`          | 32    | raw Keccak-256 output                                        |
| secp PublicKey  | 33    | SEC1-compressed (`0x02`/`0x03` prefix + X)                   |
| secp Signature  | 65    | `r ‖ s ‖ v`, `v ∈ {0,1}` (ETH layout, EIP-155 mix elsewhere) |
| BLS PublicKey   | 48    | G1 compressed                                                |
| BLS Signature   | 96    | G2 compressed                                                |
| VRF PublicKey   | 32    | Ristretto compressed                                         |
| VRF Proof       | 96    | pre-output (32) ‖ proof (64)                                 |

## Testing

```bash
cargo test -p aii-crypto                  # 31 unit tests
cargo test -p aii-crypto --test proptest  # 5 property tests
cargo doc -p aii-crypto --no-deps --open
```

## Stability

`0.0.x` — unstable; breaking changes allowed in any release until `0.1.0`.

## Roadmap

- v0.0.3 (this release): keccak / secp / bls / vrf modules + CryptoError.
- v0.0.4: PQ algorithm slots (`MlDsa65`, `SlhDsa128s`, `Falcon512`) wired
  up behind feature flags so `aii-registry` can dispatch via `AlgoId`.
- v0.1.0: KAT vector corpus for every primitive; `cargo-fuzz` harness on
  signature decoding.

## External dependencies

| crate         | version  | role                                          |
|---------------|----------|-----------------------------------------------|
| `tiny-keccak` | 2.0      | Keccak-256 implementation                     |
| `k256`        | 0.13     | secp256k1 (RustCrypto, pure Rust)             |
| `blst`        | 0.3      | BLS12-381 (Supranational, asm-accelerated)    |
| `schnorrkel`  | 0.11     | Ristretto VRF                                 |
| `merlin`      | 3        | transcript framework for VRF                  |
