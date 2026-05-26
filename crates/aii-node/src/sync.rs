//! Cold-join block sync against a peer RPC endpoint.
//!
//! A node started against an empty (or simply lagging) data dir
//! catches up to a peer's head by walking blocks from
//! `local_head + 1 ..= peer_head`, fetching each as RLP bytes via
//! `aii_getRawBlock`, decoding to a full `Block`, and committing into
//! the local `NodeState`. The same code path that handles a freshly
//! produced block runs here, so state mutations, receipt indexing,
//! gas-fee credits, and block-subsidy minting all replay deterministically.
//!
//! Verification today is "trust the bootnode" — the peer is assumed
//! honest. Cryptographic verification (BFT certificate per finalised
//! block + leader VRF proof check) is a follow-up release; this is
//! intentionally an MVP cold-join, not a Light Client.

use crate::NodeState;
use aii_block::Block;
use alloy_rlp::Decodable;
use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::HttpClientBuilder;
use jsonrpsee::rpc_params;

/// Catch up `local` to `peer_url`'s head. Returns the number of
/// blocks committed during this call (zero if already at or ahead of
/// the peer).
///
/// # Errors
/// Returns the underlying RPC / decode / commit error verbatim.
pub async fn bootstrap_sync_from_peer(
    local: &NodeState,
    peer_url: &str,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let client = HttpClientBuilder::default().build(peer_url)?;
    let peer_head: u64 = {
        let s: String = client.request("eth_blockNumber", rpc_params![]).await?;
        u64::from_str_radix(s.strip_prefix("0x").unwrap_or(&s), 16)?
    };
    let local_head = local.head_block_number_sync();
    if peer_head <= local_head {
        return Ok(0);
    }
    let mut synced: u64 = 0;
    for n in (local_head + 1)..=peer_head {
        let raw: Option<String> = client
            .request("aii_getRawBlock", rpc_params![n.to_string()])
            .await?;
        let Some(raw_hex) = raw else {
            return Err(format!("peer returned null for block {n} — bootnode is corrupt").into());
        };
        let bytes = hex::decode(raw_hex.strip_prefix("0x").unwrap_or(&raw_hex))?;
        let mut s: &[u8] = &bytes;
        let block = Block::decode(&mut s)?;
        local.commit_block(&block);
        local.set_head(block.header.number);
        synced += 1;
    }
    Ok(synced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeState;
    use aii_block::{Block, BlockBody, Bloom, Hashable, Header, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
    use aii_config::ChainSpec;
    use aii_types::{Address, H256, U256};

    fn fake_block(number: u64, parent_hash: H256) -> Block {
        Block {
            header: Header {
                parent_hash,
                ommers_hash: EMPTY_LIST_HASH,
                beneficiary: Address::new([0xcc; 20]),
                state_root: EMPTY_TRIE_HASH,
                transactions_root: EMPTY_TRIE_HASH,
                receipts_root: EMPTY_TRIE_HASH,
                logs_bloom: Bloom::ZERO,
                difficulty: U256::ZERO,
                number,
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: 1_700_000_000 + number,
                extra_data: b"sync-test".to_vec(),
                mix_hash: H256::new([0xab; 32]),
                nonce: [0u8; 8],
                base_fee_per_gas: U256::from(1_000_000_000u64),
                withdrawals_root: EMPTY_TRIE_HASH,
                blob_gas_used: None,
                excess_blob_gas: None,
                parent_beacon_block_root: None,
            },
            body: BlockBody::default(),
        }
    }

    #[tokio::test]
    async fn cold_join_replays_full_chain_from_peer() {
        // Producer node: persist 5 blocks, serve RPC.
        let producer = NodeState::new_for_tests(ChainSpec::mainnet());
        let mut parent = H256::ZERO;
        let mut hashes = Vec::new();
        for n in 1..=5 {
            let b = fake_block(n, parent);
            parent = b.hash();
            hashes.push(parent);
            producer.commit_block(&b);
        }
        producer.set_head(5);

        let (addr, handle) = aii_rpc::serve("127.0.0.1:0".parse().unwrap(), producer.clone())
            .await
            .unwrap();
        let peer_url = format!("http://{addr}");

        // Cold node: empty data dir, run bootstrap_sync.
        let cold = NodeState::new_for_tests(ChainSpec::mainnet());
        assert_eq!(cold.head_block_number_sync(), 0);
        let added = bootstrap_sync_from_peer(&cold, &peer_url).await.unwrap();
        assert_eq!(added, 5, "must sync exactly 5 blocks");
        assert_eq!(cold.head_block_number_sync(), 5);
        // Each hash must match — proves byte-identical reconstruction.
        for (i, h) in hashes.iter().enumerate() {
            let n = (i + 1) as u64;
            let by_n = cold.blocks_read_test_hash_by_number(n);
            assert_eq!(by_n, Some(*h), "cold node block {n} hash must match peer");
        }
        handle.stop().unwrap();
    }
}
