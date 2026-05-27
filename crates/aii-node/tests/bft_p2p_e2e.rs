//! End-to-end: two `BftEngine` + `BftGossip` validators talk over real
//! TCP and agree on a finalised block at height 1.
//!
//! This is the integration test for v0.0.34. It does NOT spawn aiid
//! subprocesses — it boots the same modules aiid does, in-process, on
//! two tokio tasks, with sockets bound to `127.0.0.1:0`. The asserted
//! invariant is the same as the production target: both nodes' chain
//! head reaches block 1 within a reasonable wall-clock window.

use std::sync::Arc;
use std::time::Duration;

use aii_block::{Block, BlockBody, Bloom, Header, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
use aii_consensus_bft::{
    bft::{Validator, ValidatorSet},
    BftConfig, BftEngine, BftGossip,
};
use aii_crypto::bls::SecretKey as BlsSecretKey;
use aii_crypto::vrf::SecretKey as VrfSecretKey;
use aii_node::bft_p2p::TcpBftTransport;
use aii_types::{Address, H256, U256};

fn bls_sk(seed: u8) -> BlsSecretKey {
    BlsSecretKey::from_ikm(&[seed; 32], b"AII-BFT-E2E").unwrap()
}

fn genesis_block() -> Block {
    Block {
        header: Header {
            parent_hash: H256::ZERO,
            ommers_hash: EMPTY_LIST_HASH,
            beneficiary: Address::ZERO,
            state_root: EMPTY_TRIE_HASH,
            transactions_root: EMPTY_TRIE_HASH,
            receipts_root: EMPTY_TRIE_HASH,
            logs_bloom: Bloom::ZERO,
            difficulty: U256::ZERO,
            number: 0,
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp: 1_700_000_000,
            extra_data: vec![],
            mix_hash: H256::ZERO,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_validators_finalise_block_over_tcp() {
    // Shared validator set. Same BLS/VRF keys on both nodes — they
    // know who they are by `my_index`.
    let bls = [bls_sk(1), bls_sk(2)];
    let vrf = [VrfSecretKey::generate(), VrfSecretKey::generate()];
    let mut vs_list = Vec::new();
    for i in 0..2 {
        vs_list.push(Validator {
            bls_pubkey: bls[i].public_key(),
            vrf_pubkey: vrf[i].public_key(),
            stake: 100,
        });
    }
    let vs = ValidatorSet::new(vs_list).unwrap();
    let g = genesis_block();

    let mk_engine = |idx: u32| -> Arc<BftEngine> {
        let cfg = BftConfig {
            validator_set: vs.clone(),
            my_index: idx,
            my_bls_sk: bls[idx as usize].clone(),
            my_vrf_sk: vrf[idx as usize].clone(),
            initial_seed: [0xee; 32],
            coinbase: Address::new([0xab; 20]),
            gas_limit: 30_000_000,
            base_fee_per_gas: U256::from(1_000_000_000u64),
            slot_seconds: 3,
            executor: None,
        };
        Arc::new(BftEngine::new(cfg, &g))
    };
    let e_a = mk_engine(0);
    let e_b = mk_engine(1);

    // Bind node A first so we know its port to feed node B.
    let t_a = Arc::new(
        TcpBftTransport::new("127.0.0.1:0".parse().unwrap(), vec![])
            .await
            .unwrap(),
    );
    let a_addr = t_a.local_addr();
    let t_b = Arc::new(
        TcpBftTransport::new("127.0.0.1:0".parse().unwrap(), vec![a_addr])
            .await
            .unwrap(),
    );

    let gossip_a = BftGossip::new(e_a.clone(), t_a);
    let gossip_b = BftGossip::new(e_b.clone(), t_b);

    // Drive both gossips on the same thread (round-robin) until both
    // engines commit height 1 or 5 seconds elapse.
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        gossip_a.tick();
        gossip_b.tick();
        let _ = e_a.try_harvest_committed();
        let _ = e_b.try_harvest_committed();
        if e_a.head().1 >= 1 && e_b.head().1 >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(e_a.head().1, 1, "node A should reach height 1");
    assert_eq!(e_b.head().1, 1, "node B should reach height 1");
    assert_eq!(e_a.head().0, e_b.head().0, "both nodes agree on block hash");
}
