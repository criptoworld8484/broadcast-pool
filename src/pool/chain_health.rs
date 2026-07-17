//! Cached view of where the scheduler can read the chain (height + MTP) from.
//!
//! The scheduler needs only a block height and a median-time-past; none of its five operations
//! need the address index. So when electrs/Fulcrum is down, Bitcoin Core is a complete substitute
//! and the pool keeps honouring its schedules. This module holds the health snapshot that decides
//! which of the two is in use, refreshed by a single poller instead of a blocking probe per tick.

/// Where the chain clock (height + MTP) is currently being read from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainSource {
    Indexer,
    BitcoinCore,
    None,
}

#[derive(Clone, Debug)]
pub struct ChainHealth {
    pub indexer_up: bool,
    pub core_up: bool,
    pub core_ibd: bool,
    pub core_sync_pct: Option<f64>,
    pub height: Option<u64>,
    pub mtp: Option<u64>,
    /// Indexer software name from `server.version`, e.g. "electrs" or "Fulcrum".
    pub indexer_software: Option<String>,
    pub source: ChainSource,
    /// False until the first poll completes; callers treat that as "assume healthy".
    pub polled: bool,
}

impl Default for ChainHealth {
    fn default() -> Self {
        Self {
            indexer_up: false,
            core_up: false,
            core_ibd: false,
            core_sync_pct: None,
            height: None,
            mtp: None,
            indexer_software: None,
            source: ChainSource::None,
            polled: false,
        }
    }
}

impl ChainHealth {
    pub fn clock_available(&self) -> bool {
        self.source != ChainSource::None
    }

    /// Whether a data path should still try the indexer. Before the first poll we have no
    /// evidence, so we try it rather than assume it is dead.
    pub fn should_try_indexer(&self) -> bool {
        self.indexer_up || !self.polled
    }
}

/// A node still in initial block download reports a validated tip far behind the network, so a
/// height-locked tx could look due against a stale height. An IBD node is not a chain clock.
pub fn decide_chain_source(indexer_up: bool, core_up: bool, core_ibd: bool) -> ChainSource {
    if indexer_up {
        ChainSource::Indexer
    } else if core_up && !core_ibd {
        ChainSource::BitcoinCore
    } else {
        ChainSource::None
    }
}

/// Extract the software name from an Electrum `server.version` reply, which is either a bare
/// string or `[server_name, protocol_version]`. electrs answers "electrs/0.10.5", Fulcrum
/// answers "Fulcrum 1.9.8".
pub fn parse_indexer_software(server_version: &str) -> Option<String> {
    let name = server_version
        .trim()
        .split(['/', ' '])
        .next()
        .unwrap_or("")
        .trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexer_wins_when_up() {
        assert_eq!(decide_chain_source(true, true, false), ChainSource::Indexer);
        assert_eq!(decide_chain_source(true, false, false), ChainSource::Indexer);
    }

    #[test]
    fn core_takes_over_when_indexer_down() {
        assert_eq!(decide_chain_source(false, true, false), ChainSource::BitcoinCore);
    }

    #[test]
    fn core_in_ibd_is_not_a_clock() {
        assert_eq!(decide_chain_source(false, true, true), ChainSource::None);
    }

    #[test]
    fn nothing_up_means_no_source() {
        assert_eq!(decide_chain_source(false, false, false), ChainSource::None);
    }

    #[test]
    fn parses_indexer_software_names() {
        assert_eq!(parse_indexer_software("electrs/0.10.5").as_deref(), Some("electrs"));
        assert_eq!(parse_indexer_software("Fulcrum 1.9.8").as_deref(), Some("Fulcrum"));
        assert_eq!(parse_indexer_software("ElectrumX 1.16").as_deref(), Some("ElectrumX"));
        assert_eq!(parse_indexer_software("  electrs/0.9.1  ").as_deref(), Some("electrs"));
        assert_eq!(parse_indexer_software(""), None);
        assert_eq!(parse_indexer_software("   "), None);
        assert_eq!(parse_indexer_software("{\"bogus\":1}"), None);
    }

    #[test]
    fn unpolled_health_still_tries_the_indexer() {
        let fresh = ChainHealth::default();
        assert!(!fresh.polled);
        assert!(fresh.should_try_indexer());
        assert!(!fresh.clock_available());

        let dead = ChainHealth { polled: true, ..Default::default() };
        assert!(!dead.should_try_indexer());
    }
}
