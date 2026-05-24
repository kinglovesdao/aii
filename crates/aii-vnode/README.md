# aii-vnode

V-node (validator) stake records, active-set management, and reward
splitting for the AII protocol.

- `VNode` — `(address, bls_pubkey, stake_wei, online)`
- `VSet` — ordered set of V-nodes; supports `apply_stake`, `apply_unstake`, `active`
- `MIN_STAKE_WEI = 100_000 AII` (per project memo)
- `split_reward(total) -> (validator_share, treasury_share)` 80/20
