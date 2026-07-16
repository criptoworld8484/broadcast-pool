//! Fabricates synthetic block headers above the real chain tip, for the Liana virtual-block
//! feature. Liana validates chain continuity (prev_blockhash) and the genesis hash, but NOT
//! proof-of-work, so a chained header with zeroed merkle root and nonce 0 is accepted.

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::consensus::encode;
    use bitcoin::block::Header;

    // A real Bitcoin mainnet header hex (80 bytes) — the genesis block header.
    const TIP_HEX: &str = "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c";

    fn decode(hex: &str) -> Header {
        encode::deserialize(&hex::decode(hex).unwrap()).unwrap()
    }

    #[test]
    fn empty_when_target_not_above_tip() {
        let out = fabricate_headers(100, TIP_HEX, 100).unwrap();
        assert!(out.is_empty());
        let out = fabricate_headers(100, TIP_HEX, 99).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn fabricates_chained_headers() {
        let tip = decode(TIP_HEX);
        let out = fabricate_headers(100, TIP_HEX, 103).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].0, 101);
        assert_eq!(out[2].0, 103);

        // First synthetic header chains from the real tip.
        let h101 = decode(&out[0].1);
        assert_eq!(h101.prev_blockhash, tip.block_hash());
        // Each subsequent header chains from the previous synthetic one.
        let h102 = decode(&out[1].1);
        assert_eq!(h102.prev_blockhash, h101.block_hash());
        let h103 = decode(&out[2].1);
        assert_eq!(h103.prev_blockhash, h102.block_hash());

        // Time advances 600s/block from the tip; merkle zeroed; nonce 0; bits/version copied.
        assert_eq!(h101.time, tip.time + 600);
        assert_eq!(h103.time, tip.time + 1800);
        assert_eq!(h101.merkle_root, bitcoin::TxMerkleNode::all_zeros());
        assert_eq!(h101.nonce, 0);
        assert_eq!(h101.bits, tip.bits);
        assert_eq!(h101.version, tip.version);
        // 80-byte header → 160 hex chars.
        assert_eq!(out[0].1.len(), 160);
    }

    #[test]
    fn header_hex_at_passes_through_below_tip() {
        assert_eq!(header_hex_at(100, TIP_HEX, 100).unwrap(), None);
        assert_eq!(header_hex_at(100, TIP_HEX, 50).unwrap(), None);
        let one = header_hex_at(100, TIP_HEX, 101).unwrap();
        assert!(one.is_some());
    }
}

use anyhow::{Context, Result};
use bitcoin::block::Header;
use bitcoin::consensus::encode;
use bitcoin::hashes::Hash;
use bitcoin::TxMerkleNode;

const SECS_PER_BLOCK: u32 = 600;

fn decode_header(hex: &str) -> Result<Header> {
    let bytes = hex::decode(hex.trim()).context("virtual_headers: bad hex")?;
    encode::deserialize::<Header>(&bytes).context("virtual_headers: not an 80-byte header")
}

/// Fabricate `(height, header_hex)` for every height in `real_tip_height+1..=up_to_height`,
/// each chained from the previous. Empty when `up_to_height <= real_tip_height`.
pub fn fabricate_headers(
    real_tip_height: u64,
    real_tip_header_hex: &str,
    up_to_height: u64,
) -> Result<Vec<(u64, String)>> {
    if up_to_height <= real_tip_height {
        return Ok(Vec::new());
    }
    let tip = decode_header(real_tip_header_hex)?;
    let mut out = Vec::with_capacity((up_to_height - real_tip_height) as usize);
    let mut prev_hash = tip.block_hash();
    for h in (real_tip_height + 1)..=up_to_height {
        let steps = (h - real_tip_height) as u32;
        let header = Header {
            version: tip.version,
            prev_blockhash: prev_hash,
            merkle_root: TxMerkleNode::all_zeros(),
            time: tip.time.saturating_add(SECS_PER_BLOCK.saturating_mul(steps)),
            bits: tip.bits,
            nonce: 0,
        };
        prev_hash = header.block_hash();
        out.push((h, encode::serialize_hex(&header)));
    }
    Ok(out)
}

/// The fabricated header for a single height above the tip. `Ok(None)` for heights at/below the
/// real tip — the caller forwards those to electrs unchanged.
pub fn header_hex_at(
    real_tip_height: u64,
    real_tip_header_hex: &str,
    height: u64,
) -> Result<Option<String>> {
    if height <= real_tip_height {
        return Ok(None);
    }
    let headers = fabricate_headers(real_tip_height, real_tip_header_hex, height)?;
    Ok(headers.into_iter().last().map(|(_, hex)| hex))
}
