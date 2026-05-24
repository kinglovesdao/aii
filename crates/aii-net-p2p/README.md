# aii-net-p2p

Peer-to-peer transport scaffold for the AII protocol.

**v0.0.9 scope:** TCP listener + dial; length-prefixed RLP frame codec;
`Hello` / `Ping` / `Pong` / `Disconnect` message types; one-roundtrip
handshake. Two test peers can dial each other and exchange a Hello.

**Later:** UDP discovery (devp2p Kademlia), ECIES encryption, RLPx framing,
peer reputation, NAT traversal.
