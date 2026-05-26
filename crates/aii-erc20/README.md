# aii-erc20

Ethereum-compatible ERC-20 ABI helpers for the AII chain.

This crate is **ABI / selectors only** — it does not embed a reference
ERC-20 contract bytecode. Pair with any solc-compiled token (e.g.
OpenZeppelin's `ERC20Mock`) and use these helpers to encode calldata
for and decode results from `eth_sendRawTransaction` / `eth_call`.

## Function selectors

| Function | Selector |
|----------|----------|
| `totalSupply()` | `0x18160ddd` |
| `balanceOf(address)` | `0x70a08231` |
| `transfer(address,uint256)` | `0xa9059cbb` |
| `approve(address,uint256)` | `0x095ea7b3` |
| `allowance(address,address)` | `0xdd62ed3e` |
| `transferFrom(address,address,uint256)` | `0x23b872dd` |

All selectors are `keccak256(canonical_signature)[..4]`. The constants
`SELECTOR_TRANSFER`, etc., expose them at compile time.

## Encode / decode helpers

```rust
use aii_erc20::{encode_balance_of, decode_uint256_result};
use aii_types::{Address, U256};

let alice = Address::new([0xa1; 20]);
let calldata = encode_balance_of(&alice); // 36 bytes
// hand calldata to eth_call / execute_with_revm against the token contract
// then:
let balance: U256 = decode_uint256_result(&return_data);
```
