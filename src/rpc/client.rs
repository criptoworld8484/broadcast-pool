use anyhow::{Context, Result};
use bitcoincore_rpc::{Auth, Client, RpcApi};
use crate::config::BitcoinRpcConfig;
use crate::db::models::NodeStatus;

pub struct BitcoinRpc {
    client: Client,
}

impl BitcoinRpc {
    pub fn new(config: &BitcoinRpcConfig) -> Result<Self> {
        let auth = Auth::UserPass(config.user.clone(), config.password.clone());
        let client = Client::new(&config.url, auth).context("Failed to create RPC client")?;
        Ok(Self { client })
    }

    pub fn get_node_status(&self) -> Result<NodeStatus> {
        let blockchain_info = self
            .client
            .get_blockchain_info()
            .context("Failed to get blockchain info")?;

        let mempool_info = self.client.get_mempool_info().ok();

        let connected = true;
        let blockchain_height = Some(blockchain_info.blocks);
        let mempool_size = mempool_info.as_ref().map(|info| info.size);
        let network = blockchain_info.chain.to_string();
        let sync_percentage = if blockchain_info.initial_block_download {
            let verification = blockchain_info.verification_progress;
            Some(verification * 100.0)
        } else {
            Some(100.0)
        };

        Ok(NodeStatus {
            connected,
            blockchain_height,
            mempool_size,
            network,
            sync_percentage,
        })
    }

    pub fn broadcast_transaction(&self, tx_hex: &str) -> Result<String> {
        let txid = self
            .client
            .send_raw_transaction(tx_hex)
            .context("Failed to broadcast transaction")?;
        Ok(txid.to_string())
    }

    pub fn get_transaction(&self, txid: &str) -> Result<bitcoincore_rpc::json::GetRawTransactionResult> {
        let txid = txid
            .parse()
            .context("Invalid transaction ID")?;
        self.client
            .get_raw_transaction_info(&txid, None)
            .context("Failed to get transaction")
    }

    pub fn get_mempool_info(&self) -> Result<bitcoincore_rpc::json::GetMempoolInfoResult> {
        self.client
            .get_mempool_info()
            .context("Failed to get mempool info")
    }

    pub fn get_block_count(&self) -> Result<u64> {
        self.client
            .get_block_count()
            .context("Failed to get block count")
    }

    pub fn estimate_smart_fee(&self, blocks: u64) -> Result<Option<f64>> {
        let result = self
            .client
            .estimate_smart_fee(blocks as u16, None)
            .context("Failed to estimate fee")?;

        // fee_rate is in BTC/kB, convert to sat/vB
        Ok(result.fee_rate.map(|rate| rate.to_btc() * 100_000.0))
    }

    /// What it currently costs, at minimum, to get a transaction relayed and into a block:
    /// the mempool's own minimum plus the relay floor, with the mempool's backlog size so
    /// callers can tell a congested mempool from an idle one.
    pub fn get_mempool_floor(&self) -> Result<MempoolFloor> {
        let info = self
            .client
            .get_mempool_info()
            .context("Failed to get mempool info")?;
        Ok(MempoolFloor {
            vbytes: info.bytes as u64,
            // Both are BTC/kvB like estimatesmartfee, so the same conversion applies.
            min_fee_sat_vb: info.mempool_min_fee.to_btc() * 100_000.0,
            relay_floor_sat_vb: info.min_relay_tx_fee.to_btc() * 100_000.0,
        })
    }

    pub fn test_connection(&self) -> Result<bool> {
        self.client
            .get_blockchain_info()
            .context("Failed to connect to Bitcoin Core")?;
        Ok(true)
    }

    /// Bitcoin Core chain name: `main`, `test`, `signet`, etc.
    pub fn get_bitcoin_chain(&self) -> Result<String> {
        let info = self
            .client
            .get_blockchain_info()
            .context("Failed to get blockchain info")?;
        Ok(info.chain.to_string())
    }

    /// Genesis block hash from the connected node (handles custom signets on Umbrel).
    pub fn get_genesis_block_hash(&self) -> Result<String> {
        let hash = self
            .client
            .get_block_hash(0)
            .context("Failed to get genesis block hash")?;
        Ok(hash.to_string().to_lowercase())
    }

    pub fn get_median_time(&self) -> Result<u64> {
        let info = self
            .client
            .get_blockchain_info()
            .context("Failed to get blockchain info")?;
        Ok(info.median_time)
    }

    /// Everything the chain clock needs, in a single `getblockchaininfo`:
    /// `(height, median_time_past, in_initial_block_download, sync_percentage)`.
    pub fn get_chain_status(&self) -> Result<(u64, u64, bool, f64)> {
        let info = self
            .client
            .get_blockchain_info()
            .context("Failed to get blockchain info")?;
        let sync_pct = if info.initial_block_download {
            info.verification_progress * 100.0
        } else {
            100.0
        };
        Ok((
            info.blocks,
            info.median_time,
            info.initial_block_download,
            sync_pct,
        ))
    }
}

/// The floor below which a transaction will not be relayed, plus the current backlog.
#[derive(Debug, Clone, Copy)]
pub struct MempoolFloor {
    /// Total virtual size of everything waiting. One block is ~1_000_000 vB.
    pub vbytes: u64,
    pub min_fee_sat_vb: f64,
    pub relay_floor_sat_vb: f64,
}
