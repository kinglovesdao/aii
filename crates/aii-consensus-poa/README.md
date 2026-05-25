# aii-consensus-poa

Proof-of-Authority consensus engine for AII sub-chains.

PoA is a low-overhead consensus where a fixed list of authority
addresses takes turns signing blocks in round-robin order. No
voting, no quorum, no slashing. Suitable for permissioned
sub-chains where the operator set is known and trusted.

Implements `aii_consensus_iface::Engine`.
