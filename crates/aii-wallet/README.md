# aii-wallet

Local wallet primitives for the AII protocol.

- `LocalWallet` — owns a secp256k1 secret key and the derived `Address`
- `LocalWallet::sign_message_hash` — sign a 32-byte hash
- `LocalWallet::sign_tx_hash` — convenience wrapper for `Tx::hash()` input

Day-0 scope: in-memory keys only. Encrypted keystore (PBKDF2/scrypt) + BIP-39
mnemonic recovery land in v0.0.7 alongside the `aii-cli` `account` commands.
