# aii-net-sync

Block-sync state machine for AII.

States: `Idle → Headers → Bodies → Done`.

This crate owns no network or storage — it just transitions state given
`Event`s and emits `Action`s. The node binary wires the actions to
`aii-net-p2p` peers + writes blocks to `aii-storage`.

**v0.0.9 scope:** state machine + tests. Live wiring to real peers lands
once the consensus loop is up.
