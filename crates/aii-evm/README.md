# aii-evm

Execution layer for the AII protocol.

**v0.0.9 scope (this PR):** native value-transfer execution against
`aii-state::StateDb`. Validates nonce + balance, debits sender, credits
recipient, increments nonce, returns a `Receipt`. Contract-call /
`CREATE` paths return `ExecError::ContractCallsNotYetSupported` until
the `revm` integration lands.

**Later:** wrap `revm` for EVM bytecode execution, gas metering,
precompile dispatch.
