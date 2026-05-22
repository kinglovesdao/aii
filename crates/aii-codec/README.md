# aii-codec

Encoding and decoding of AII protocol types in three wire formats: RLP, SSZ,
and ETH JSON-RPC hex.

## Supported types

| Type | RLP | SSZ | JSON |
|---|---|---|---|
| `H256` | yes (32B string) | yes (32B bytes) | yes (`0x` + 64 hex) |
| `Address` | yes (20B string) | yes (20B bytes) | yes (`0x` + 40 hex) |
| `AlgoId` | yes (1B string) | yes (1B uint8) | via serde derive |
| `BlsPubKey` | use bytes | yes (48B bytes) | use `bytes_hex` |
| `BlsSignature` | use bytes | yes (96B bytes) | use `bytes_hex` |
| `U256` | use alloy-rlp | planned | yes (quantity hex) |
| `SignedTx` | yes (4-tuple list) | yes (container) | use `bytes_hex` per field |

## Quickstart

```rust
use aii_codec::{rlp, ssz, json};
use aii_types::{H256, SignedTx, AlgoId};

let h = H256::new([0x42; 32]);

// RLP
let rlp_bytes = rlp::encode_h256(&h);
let decoded = rlp::decode_h256(&rlp_bytes)?;

// SSZ
let ssz_bytes = ssz::encode_h256(&h);
let decoded = ssz::decode_h256(&ssz_bytes)?;

// JSON via serde
#[derive(serde::Serialize, serde::Deserialize)]
struct Resp {
    #[serde(with = "aii_codec::json::hex_h256")]
    hash: H256,
}
```

## Hex conventions

`aii-codec::hex` provides ETH JSON-RPC hex helpers:

- `encode_bytes` / `decode_bytes` — byte arrays, length preserved.
- `encode_quantity` / `decode_quantity` — `U256` integers, minimal hex.

## Testing

```bash
cargo test -p aii-codec                    # 52 unit tests
cargo test -p aii-codec --test proptest    # 12 property tests
cargo doc -p aii-codec --no-deps --open
```

## Stability

`0.0.x` — unstable; breaking changes allowed in any release until `0.1.0`.

## Roadmap

- v0.0.2 (this release): RLP / SSZ / JSON for the 7 `aii-types` primitives + `SignedTx`.
- v0.0.3: `cargo-fuzz` harness for `decode_signed_tx` (RLP + SSZ).
- v0.1.0: encoding for `Block`, `Header`, `Receipt`, `Log` once `aii-state` defines them.
