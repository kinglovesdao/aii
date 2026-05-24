//! Block body — transactions, ommers, withdrawals.

use crate::{header::Header, tx::Tx, withdrawal::Withdrawal};
use alloy_rlp::{Decodable, Encodable};

/// The mutable, non-header portion of a block.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BlockBody {
    /// Transactions, in inclusion order.
    pub transactions: Vec<Tx>,
    /// Ommer/uncle headers — empty post-merge.
    pub ommers: Vec<Header>,
    /// Beacon-chain withdrawals (EIP-4895).
    pub withdrawals: Vec<Withdrawal>,
}

impl BlockBody {
    fn payload_length(&self) -> usize {
        let txs_inner: usize = self.transactions.iter().map(Encodable::length).sum();
        let txs = alloy_rlp::length_of_length(txs_inner) + txs_inner;
        let omm_inner: usize = self.ommers.iter().map(Encodable::length).sum();
        let omm = alloy_rlp::length_of_length(omm_inner) + omm_inner;
        let w_inner: usize = self.withdrawals.iter().map(Encodable::length).sum();
        let w = alloy_rlp::length_of_length(w_inner) + w_inner;
        txs + omm + w
    }
}

impl Encodable for BlockBody {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let payload_length = self.payload_length();
        alloy_rlp::Header { list: true, payload_length }.encode(out);
        let txs_inner: usize = self.transactions.iter().map(Encodable::length).sum();
        alloy_rlp::Header { list: true, payload_length: txs_inner }.encode(out);
        for t in &self.transactions {
            t.encode(out);
        }
        let omm_inner: usize = self.ommers.iter().map(Encodable::length).sum();
        alloy_rlp::Header { list: true, payload_length: omm_inner }.encode(out);
        for o in &self.ommers {
            o.encode(out);
        }
        let w_inner: usize = self.withdrawals.iter().map(Encodable::length).sum();
        alloy_rlp::Header { list: true, payload_length: w_inner }.encode(out);
        for w in &self.withdrawals {
            w.encode(out);
        }
    }
    fn length(&self) -> usize {
        let p = self.payload_length();
        p + alloy_rlp::length_of_length(p)
    }
}

impl Decodable for BlockBody {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let h = alloy_rlp::Header::decode(buf)?;
        if !h.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }

        let txs_h = alloy_rlp::Header::decode(buf)?;
        let mut transactions = Vec::new();
        let txs_start = buf.len();
        while txs_start - buf.len() < txs_h.payload_length {
            transactions.push(Tx::decode(buf)?);
        }

        let omm_h = alloy_rlp::Header::decode(buf)?;
        let mut ommers = Vec::new();
        let omm_start = buf.len();
        while omm_start - buf.len() < omm_h.payload_length {
            ommers.push(Header::decode(buf)?);
        }

        let w_h = alloy_rlp::Header::decode(buf)?;
        let mut withdrawals = Vec::new();
        let w_start = buf.len();
        while w_start - buf.len() < w_h.payload_length {
            withdrawals.push(Withdrawal::decode(buf)?);
        }

        Ok(Self { transactions, ommers, withdrawals })
    }
}
