use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub network: NetworkConfig,
    pub bitcoin_rpc: Option<BitcoinRpcConfig>,
    pub indexer: Option<IndexerConfig>,
    pub pool: PoolConfig,
    #[serde(default)]
    pub schedule: ScheduleConfig,
    pub privacy: PrivacyConfig,
    #[serde(default)]
    pub web: WebConfig,
    #[serde(default)]
    pub electrum_server: ElectrumServerConfig,
    #[serde(default)]
    pub notifications: NotificationsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BroadcastMode {
    Immediate,
    Scheduled,
    ByBlock,
    /// Per-TX manual scheduling (Liana, or Sparrow with nLockTime disabled).
    Manual,
}

impl Default for BroadcastMode {
    fn default() -> Self {
        BroadcastMode::Immediate
    }
}

impl std::fmt::Display for BroadcastMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BroadcastMode::Immediate => write!(f, "immediate"),
            BroadcastMode::Scheduled => write!(f, "scheduled"),
            BroadcastMode::ByBlock => write!(f, "by_block"),
            BroadcastMode::Manual => write!(f, "manual"),
        }
    }
}

impl std::str::FromStr for BroadcastMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "immediate" => Ok(BroadcastMode::Immediate),
            "scheduled" => Ok(BroadcastMode::Scheduled),
            "by_block" => Ok(BroadcastMode::ByBlock),
            "manual" => Ok(BroadcastMode::Manual),
            _ => Err(format!("Unknown broadcast mode: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexerConfig {
    pub url: String,
    /// User-provided external indexer; when true, startup skips auto-discovery.
    #[serde(default)]
    pub manual_override: bool,
}

impl Config {
    pub fn load(config_path: Option<&Path>) -> Result<Self> {
        let path = match config_path {
            Some(p) => p.to_path_buf(),
            None => Self::default_config_path()?,
        };

        let mut config = if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read config file: {}", path.display()))?;
            toml::from_str(&content)
                .with_context(|| format!("Failed to parse config file: {}", path.display()))?
        } else {
            tracing::warn!("Config file not found at {}, using defaults", path.display());
            Self::default_config()
        };

        // Apply environment variable overrides (for Docker / Umbrel)
        if let Ok(url) = std::env::var("BROADCAST_POOL_RPC_URL") {
            if config.bitcoin_rpc.is_none() {
                config.bitcoin_rpc = Some(BitcoinRpcConfig {
                    url: String::new(),
                    user: String::new(),
                    password: String::new(),
                });
            }
            config.bitcoin_rpc.as_mut().unwrap().url = url;
        }
        if let Ok(user) = std::env::var("BROADCAST_POOL_RPC_USER") {
            if let Some(ref mut rpc) = config.bitcoin_rpc {
                rpc.user = user;
            }
        }
        if let Ok(pass) = std::env::var("BROADCAST_POOL_RPC_PASS") {
            if let Some(ref mut rpc) = config.bitcoin_rpc {
                rpc.password = pass;
            }
        }
        if let Ok(url) = std::env::var("BROADCAST_POOL_INDEXER_URL") {
            config.indexer = Some(IndexerConfig {
                url,
                manual_override: false,
            });
        }
        // Indexer host/port auto-discovery runs at startup (see discovery.rs).
        if let Ok(ip) = std::env::var("BROADCAST_POOL_LAN_IP") {
            let ip = ip.trim().to_string();
            if !ip.is_empty() {
                config.electrum_server.lan_connect_host = Some(ip);
            }
        }
        if let Ok(network) = std::env::var("BROADCAST_POOL_NETWORK") {
            config.network.network_type = match network.to_lowercase().as_str() {
                "mainnet" => NetworkType::Mainnet,
                "signet" => NetworkType::Signet,
                _ => NetworkType::Testnet4,
            };
        } else if let Ok(network) = std::env::var("APP_BITCOIN_NETWORK") {
            config.network.network_type = match network.to_lowercase().as_str() {
                "mainnet" => NetworkType::Mainnet,
                "signet" => NetworkType::Signet,
                "testnet" | "testnet3" | "testnet4" => NetworkType::Testnet4,
                _ => config.network.network_type.clone(),
            };
        }
        if let Ok(host) = std::env::var("BROADCAST_POOL_WEB_HOST") {
            config.web.host = host;
        }
        if let Ok(port) = std::env::var("BROADCAST_POOL_WEB_PORT") {
            if let Ok(p) = port.parse() {
                config.web.port = p;
            }
        }
        if let Ok(host) = std::env::var("BROADCAST_POOL_ELECTRUM_HOST") {
            config.electrum_server.host = host;
        }
        if let Ok(port) = std::env::var("BROADCAST_POOL_ELECTRUM_PORT") {
            if let Ok(p) = port.parse() {
                config.electrum_server.port = p;
            }
        }
        if let Ok(port) = std::env::var("BROADCAST_POOL_LIANA_ELECTRUM_PORT") {
            if let Ok(p) = port.parse() {
                config.electrum_server.liana_port = Some(p);
            }
        }

        Ok(config)
    }

    fn default_config_path() -> Result<PathBuf> {
        Ok(Self::resolved_config_path())
    }

    /// Prefer `BROADCAST_POOL_DATA_DIR/config.toml` on Umbrel so settings survive in the app volume.
    pub fn resolved_config_path() -> PathBuf {
        if let Ok(dir) = std::env::var("BROADCAST_POOL_DATA_DIR") {
            let dir = dir.trim();
            if !dir.is_empty() {
                return PathBuf::from(dir).join("config.toml");
            }
        }
        dirs::config_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("broadcast-pool")
            .join("config.toml")
    }

    pub fn default_config() -> Self {
        let network_type = std::env::var("BROADCAST_POOL_NETWORK")
            .or_else(|_| std::env::var("APP_BITCOIN_NETWORK"))
            .map(|n| match n.to_lowercase().as_str() {
                "mainnet" | "main" => NetworkType::Mainnet,
                "signet" => NetworkType::Signet,
                _ => NetworkType::Testnet4,
            })
            .unwrap_or(NetworkType::Testnet4);

        let indexer = std::env::var("BROADCAST_POOL_INDEXER_URL")
            .ok()
            .map(|url| IndexerConfig {
                url,
                manual_override: false,
            });

        Self {
            network: NetworkConfig {
                network_type,
                resolved_genesis: None,
            },
            bitcoin_rpc: std::env::var("BROADCAST_POOL_RPC_URL").ok().map(|url| {
                BitcoinRpcConfig {
                    url,
                    user: std::env::var("BROADCAST_POOL_RPC_USER").unwrap_or_default(),
                    password: std::env::var("BROADCAST_POOL_RPC_PASS").unwrap_or_default(),
                }
            }),
            indexer,
            pool: PoolConfig {
                max_size_kb: 300,
                rebroadcast_interval_minutes: 30,
                expiry_days: 30,
            },
            schedule: ScheduleConfig {
                broadcast_mode: BroadcastMode::Immediate,
                default_delay_hours: 24,
                scheduled_datetime: None,
                liana_virtual_block: LianaVirtualBlockConfig::default(),
                min_delay_hours: 2,
                max_delay_hours: 72,
                min_fee_rate: 1.0,
                max_fee_rate: 50.0,
            },
            privacy: PrivacyConfig {
                use_tor: false,
                tor_socks_port: 9050,
                rotate_identity_per_tx: true,
            },
            web: WebConfig {
                host: std::env::var("BROADCAST_POOL_WEB_HOST")
                    .unwrap_or_else(|_| "127.0.0.1".to_string()),
                port: std::env::var("BROADCAST_POOL_WEB_PORT")
                    .unwrap_or_else(|_| "8080".to_string())
                    .parse()
                    .unwrap_or(8080),
            },
            electrum_server: ElectrumServerConfig {
                host: std::env::var("BROADCAST_POOL_ELECTRUM_HOST")
                    .unwrap_or_else(|_| "0.0.0.0".to_string()),
                port: std::env::var("BROADCAST_POOL_ELECTRUM_PORT")
                    .unwrap_or_else(|_| "50050".to_string())
                    .parse()
                    .unwrap_or(50050),
                liana_port: std::env::var("BROADCAST_POOL_LIANA_ELECTRUM_PORT")
                    .ok()
                    .and_then(|p| p.parse().ok()),
                lan_connect_host: std::env::var("BROADCAST_POOL_LAN_IP")
                    .ok()
                    .filter(|s| !s.trim().is_empty()),
            },
            notifications: NotificationsConfig::default(),
        }
    }

    pub fn db_path(&self, data_dir: &Path) -> PathBuf {
        data_dir.join(format!("broadcast-pool-{}.db", self.network.network_type.data_dir_name()))
    }
}

/// Tell the user, over a private channel, when a waiting tx is redistributed to the network.
/// Off by default; delivery is fire-and-forget and never affects broadcasting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NotificationsConfig {
    /// Master switch. When false, no channel is contacted regardless of its own `enabled`.
    #[serde(default)]
    pub enabled: bool,
    /// Message language: `en` (default) or `es`.
    #[serde(default = "default_notification_language")]
    pub language: String,
    #[serde(default)]
    pub nostr: NostrNotificationConfig,
}

fn default_notification_language() -> String {
    "en".to_string()
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            language: default_notification_language(),
            nostr: NostrNotificationConfig::default(),
        }
    }
}

impl NotificationsConfig {
    pub fn nostr_active(&self) -> bool {
        self.enabled
            && self.nostr.enabled
            && !self.nostr.recipient_npub.trim().is_empty()
            && self.nostr.relays.iter().any(|r| !r.trim().is_empty())
    }

    pub fn any_active(&self) -> bool {
        self.nostr_active()
    }
}

/// Encrypted DM (NIP-17 gift wrap) to the user's own npub, via the user's own relay(s).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NostrNotificationConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Where the DM is sent — the user's npub.
    #[serde(default)]
    pub recipient_npub: String,
    /// Relay websocket URLs, e.g. `wss://relay.example.com`.
    #[serde(default)]
    pub relays: Vec<String>,
    /// The app's own sending identity. Generated on first activation and persisted
    /// encrypted (`enc:v1:`) with the keyfile — never exposed through the API.
    #[serde(default)]
    pub app_nsec: String,
    /// SOCKS5 proxy (`host:port`) used to reach `.onion` relays. Only `.onion` URLs go
    /// through it; clearnet and LAN relays stay direct. Empty = resolve a platform default
    /// (Umbrel ships a Tor proxy); `"off"` disables Tor routing entirely.
    #[serde(default)]
    pub tor_socks: String,
}

/// Umbrel's bundled Tor daemon. Reachable from any app container on the umbrel network.
pub const UMBREL_TOR_SOCKS: &str = "10.21.21.11:9050";
/// A Tor daemon running on the same host (non-containerised installs).
pub const LOCAL_TOR_SOCKS: &str = "127.0.0.1:9050";

impl NostrNotificationConfig {
    /// Resolve the SOCKS proxy to use for `.onion` relays.
    ///
    /// `None` means "connect directly" — either the user turned Tor off, or there is no
    /// sensible default on this platform. An explicit value always wins over detection so a
    /// user with a proxy elsewhere is never overridden.
    pub fn resolved_tor_socks(&self, umbrel: bool) -> Option<String> {
        let configured = self.tor_socks.trim();
        if configured.eq_ignore_ascii_case("off") {
            return None;
        }
        if !configured.is_empty() {
            return Some(configured.to_string());
        }
        // Only auto-detect where we know a proxy exists. Guessing 127.0.0.1:9050 on a
        // platform without Tor would turn every .onion send into a confusing timeout.
        if umbrel {
            return Some(UMBREL_TOR_SOCKS.to_string());
        }
        None
    }

    /// True when any configured relay is a Tor hidden service, i.e. a proxy is required.
    pub fn has_onion_relay(&self) -> bool {
        self.relays
            .iter()
            .any(|r| r.trim().trim_end_matches('/').to_ascii_lowercase().ends_with(".onion"))
    }
}

/// Encrypt the app's Nostr secret key for persistence. Idempotent, same contract as
/// `encrypt_rpc_password` but with its own AAD so the two fields aren't interchangeable.
pub fn encrypt_nostr_nsec(key: &[u8; 32], nsec: &str) -> String {
    if nsec.is_empty() || crate::crypto::is_encoded(nsec) {
        return nsec.to_string();
    }
    crate::crypto::encode_field(key, nsec, b"notifications.nostr.nsec")
}

/// Decrypt the app's Nostr secret key loaded from disk. A value that fails to decrypt
/// (e.g. written under a different keyfile) is returned as-is.
pub fn decrypt_nostr_nsec(key: &[u8; 32], stored: &str) -> String {
    crate::crypto::decode_field(key, stored, b"notifications.nostr.nsec")
        .unwrap_or_else(|_| stored.to_string())
}

/// Encrypt `bitcoin_rpc.password` for persistence to disk. Idempotent: an empty password or
/// one already in `enc:v1:` form passes through unchanged.
pub fn encrypt_rpc_password(key: &[u8; 32], pw: &str) -> String {
    if pw.is_empty() || crate::crypto::is_encoded(pw) {
        return pw.to_string();
    }
    crate::crypto::encode_field(key, pw, b"bitcoin_rpc.password")
}

/// Decrypt `bitcoin_rpc.password` loaded from disk. Legacy plaintext (or a value that fails
/// to decrypt, e.g. under a different key) is returned as-is.
pub fn decrypt_rpc_password(key: &[u8; 32], stored: &str) -> String {
    crate::crypto::decode_field(key, stored, b"bitcoin_rpc.password")
        .unwrap_or_else(|_| stored.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    #[serde(rename = "type")]
    pub network_type: NetworkType,
    /// Real genesis hash of the connected node, resolved at startup (handles custom
    /// signets and corrects the built-in fallback constants). Not persisted.
    #[serde(skip)]
    pub resolved_genesis: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lowercase")]
pub enum NetworkType {
    Mainnet,
    #[serde(alias = "testnet", alias = "testnet3")]
    Testnet4,
    Signet,
}

impl NetworkType {
    pub fn default_port(&self) -> u16 {
        match self {
            NetworkType::Mainnet => 8332,
            NetworkType::Testnet4 => 48332,
            NetworkType::Signet => 38332,
        }
    }

    pub fn data_dir_name(&self) -> &str {
        match self {
            NetworkType::Mainnet => "mainnet",
            NetworkType::Testnet4 => "testnet4",
            NetworkType::Signet => "signet",
        }
    }

    pub fn supported_networks() -> &'static [&'static str] {
        &["mainnet", "testnet4", "signet"]
    }

    pub fn genesis_hash(&self) -> &'static str {
        match self {
            NetworkType::Mainnet => {
                "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
            }
            NetworkType::Testnet4 => {
                "00000000da84f2bafbbc53dee25a72ae507ff4914b867c565be350b0da8bf043"
            }
            NetworkType::Signet => {
                "00000008819873e925422c1ff0f99f7cc9bbb232af63a077a480a3633bee1ef6"
            }
        }
    }

    /// Reverse of `genesis_hash`: map a node's genesis hash to its network. Used to
    /// detect the network from the genesis (resolved via a fresh RPC client), which is
    /// more reliable than `getblockchaininfo.chain` on some setups.
    pub fn from_genesis_hash(hash: &str) -> Option<NetworkType> {
        let h = hash.trim().to_lowercase();
        for net in [NetworkType::Mainnet, NetworkType::Testnet4, NetworkType::Signet] {
            if net.genesis_hash() == h {
                return Some(net);
            }
        }
        None
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            NetworkType::Mainnet => "Mainnet",
            NetworkType::Testnet4 => "Testnet 4",
            NetworkType::Signet => "Signet",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitcoinRpcConfig {
    pub url: String,
    pub user: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolConfig {
    pub max_size_kb: u64,
    pub rebroadcast_interval_minutes: u64,
    pub expiry_days: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleConfig {
    #[serde(default)]
    pub broadcast_mode: BroadcastMode,
    #[serde(default = "default_delay_hours")]
    pub default_delay_hours: u64,
    #[serde(default)]
    pub scheduled_datetime: Option<String>,
    #[serde(default)]
    pub liana_virtual_block: LianaVirtualBlockConfig,
    pub min_delay_hours: u64,
    pub max_delay_hours: u64,
    pub min_fee_rate: f64,
    pub max_fee_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LianaVirtualBlockConfig {
    /// Tick armed: while true, Liana sessions are served a virtual (future) tip.
    #[serde(default)]
    pub enabled: bool,
    /// Absolute virtual block height to serve to the next Liana tx. Advances +2 per capture.
    #[serde(default)]
    pub target_height: u64,
    /// Real chain height captured when the tick was armed. Auto-disarm at armed_at_height + 10.
    #[serde(default)]
    pub armed_at_height: u64,
}

fn default_delay_hours() -> u64 {
    24
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            broadcast_mode: BroadcastMode::Immediate,
            default_delay_hours: 24,
            scheduled_datetime: None,
            liana_virtual_block: LianaVirtualBlockConfig::default(),
            min_delay_hours: 2,
            max_delay_hours: 72,
            min_fee_rate: 1.0,
            max_fee_rate: 50.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElectrumServerConfig {
    pub host: String,
    pub port: u16,
    /// Optional dedicated Electrum port for Liana (manual scheduling ingest).
    #[serde(default)]
    pub liana_port: Option<u16>,
    /// LAN IP shown to users for wallet connections (Sparrow/Liana). Bind may still use 0.0.0.0.
    #[serde(default)]
    pub lan_connect_host: Option<String>,
}

impl Default for ElectrumServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 50050,
            liana_port: std::env::var("BROADCAST_POOL_LIANA_ELECTRUM_PORT")
                .ok()
                .and_then(|p| p.parse().ok()),
            lan_connect_host: std::env::var("BROADCAST_POOL_LAN_IP")
                .ok()
                .filter(|s| !s.trim().is_empty()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyConfig {
    pub use_tor: bool,
    pub tor_socks_port: u16,
    pub rotate_identity_per_tx: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    pub host: String,
    pub port: u16,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::NetworkType;

    #[test]
    fn rpc_password_encrypt_decrypt_roundtrip_and_legacy() {
        let key = crate::crypto::generate_key();
        let enc = crate::config::encrypt_rpc_password(&key, "s3cret");
        assert!(enc.starts_with("enc:v1:"));
        assert_eq!(crate::config::decrypt_rpc_password(&key, &enc), "s3cret");
        // Legacy plaintext passes through.
        assert_eq!(crate::config::decrypt_rpc_password(&key, "plain"), "plain");
    }

    #[test]
    fn nostr_nsec_encrypt_decrypt_roundtrip_and_idempotence() {
        let key = crate::crypto::generate_key();
        // Generated, never a literal: a hardcoded nsec would trip secret scanners.
        let nsec = crate::notify::nostr::generate_nsec().unwrap();
        let enc = crate::config::encrypt_nostr_nsec(&key, &nsec);
        assert!(enc.starts_with("enc:v1:"));
        assert_eq!(crate::config::decrypt_nostr_nsec(&key, &enc), nsec);
        // Re-encrypting an already-encrypted value must not double-wrap it (save_config_to_disk
        // runs on every save, including saves of an unchanged config).
        assert_eq!(crate::config::encrypt_nostr_nsec(&key, &enc), enc);
        // Empty stays empty — no key is stored until Nostr is enabled.
        assert_eq!(crate::config::encrypt_nostr_nsec(&key, ""), "");
    }

    #[test]
    fn nsec_and_rpc_password_ciphertexts_are_not_interchangeable() {
        // Distinct AAD: a value encrypted as one field must not decrypt as the other.
        let key = crate::crypto::generate_key();
        let enc_nsec = crate::config::encrypt_nostr_nsec(&key, "a-secret-value");
        // Failed decryption falls back to returning the stored value untouched.
        assert_eq!(crate::config::decrypt_rpc_password(&key, &enc_nsec), enc_nsec);
    }

    #[test]
    fn notifications_are_inert_until_fully_configured() {
        let mut cfg = crate::config::NotificationsConfig::default();
        assert!(!cfg.any_active());

        // Channel enabled but master switch off -> still inert.
        cfg.nostr.enabled = true;
        cfg.nostr.recipient_npub = "npub1test".to_string();
        cfg.nostr.relays = vec!["ws://192.168.50.68:7777".to_string()];
        assert!(!cfg.any_active());

        cfg.enabled = true;
        assert!(cfg.any_active());

        // Nostr needs a recipient AND at least one non-blank relay.
        cfg.nostr.recipient_npub = String::new();
        cfg.nostr.relays = Vec::new();
        assert!(!cfg.nostr_active());
        cfg.nostr.recipient_npub = "npub1test".to_string();
        assert!(!cfg.nostr_active());
        cfg.nostr.relays = vec!["   ".to_string()];
        assert!(!cfg.nostr_active());
        cfg.nostr.relays = vec!["ws://192.168.50.68:7777".to_string()];
        assert!(cfg.nostr_active());
    }

    #[test]
    fn tor_socks_defaults_to_umbrel_proxy_only_on_umbrel() {
        let cfg = crate::config::NostrNotificationConfig::default();
        assert_eq!(
            cfg.resolved_tor_socks(true).as_deref(),
            Some(crate::config::UMBREL_TOR_SOCKS)
        );
        // Off Umbrel we don't guess: a wrong guess turns every .onion send into a timeout.
        assert_eq!(cfg.resolved_tor_socks(false), None);
    }

    #[test]
    fn explicit_tor_socks_overrides_detection() {
        let mut cfg = crate::config::NostrNotificationConfig::default();
        cfg.tor_socks = " 192.168.1.5:9150 ".to_string();
        assert_eq!(cfg.resolved_tor_socks(true).as_deref(), Some("192.168.1.5:9150"));
        assert_eq!(cfg.resolved_tor_socks(false).as_deref(), Some("192.168.1.5:9150"));
    }

    #[test]
    fn tor_can_be_turned_off_explicitly_even_on_umbrel() {
        let mut cfg = crate::config::NostrNotificationConfig::default();
        cfg.tor_socks = "OFF".to_string();
        assert_eq!(cfg.resolved_tor_socks(true), None);
    }

    #[test]
    fn onion_relays_are_recognised() {
        let mut cfg = crate::config::NostrNotificationConfig::default();
        assert!(!cfg.has_onion_relay());
        cfg.relays = vec!["wss://relay.damus.io".to_string()];
        assert!(!cfg.has_onion_relay());
        cfg.relays = vec!["ws://192.168.50.68:7777".to_string()];
        assert!(!cfg.has_onion_relay());
        // Trailing slash and case must not defeat the check.
        cfg.relays = vec!["ws://ABCDEF234567.ONION/".to_string()];
        assert!(cfg.has_onion_relay());
        // Mixed list: one onion is enough to need the proxy.
        cfg.relays = vec![
            "wss://relay.damus.io".to_string(),
            "ws://abcdef234567.onion".to_string(),
        ];
        assert!(cfg.has_onion_relay());
    }

    #[test]
    fn notifications_default_to_english_and_disabled() {
        let cfg = crate::config::NotificationsConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.language, "en");
    }

    #[test]
    fn config_without_a_notifications_section_still_parses() {
        // Existing installs have no [notifications] block; loading must not fail.
        let toml_str = r#"
[network]
type = "testnet4"

[pool]
max_size_kb = 300
rebroadcast_interval_minutes = 30
expiry_days = 30

[privacy]
use_tor = false
tor_socks_port = 9050
rotate_identity_per_tx = true
"#;
        let cfg: crate::config::Config = toml::from_str(toml_str).unwrap();
        assert!(!cfg.notifications.enabled);
        assert_eq!(cfg.notifications.language, "en");
    }

    #[test]
    fn notifications_section_survives_a_toml_roundtrip() {
        let mut cfg = crate::config::Config::default_config();
        cfg.notifications.enabled = true;
        cfg.notifications.language = "es".to_string();
        cfg.notifications.nostr.enabled = true;
        cfg.notifications.nostr.recipient_npub = "npub1abc".to_string();
        cfg.notifications.nostr.relays = vec!["ws://192.168.50.68:7777".to_string()];

        let s = toml::to_string_pretty(&cfg).unwrap();
        let back: crate::config::Config = toml::from_str(&s).unwrap();
        assert_eq!(back.notifications, cfg.notifications);
    }

    // The signet/testnet4 constants were previously wrong (signet had 65 chars; testnet4
    // held testnet3's hash), which broke Liana's genesis check. Guard all networks.
    #[test]
    fn from_genesis_hash_maps_back() {
        for nt in [NetworkType::Mainnet, NetworkType::Testnet4, NetworkType::Signet] {
            assert_eq!(NetworkType::from_genesis_hash(nt.genesis_hash()), Some(nt.clone()));
        }
        // case-insensitive + trimmed
        assert_eq!(
            NetworkType::from_genesis_hash("  000000000019D6689C085AE165831E934FF763AE46A2A6C172B3F1B60A8CE26F  "),
            Some(NetworkType::Mainnet)
        );
        assert_eq!(NetworkType::from_genesis_hash("deadbeef"), None);
    }

    #[test]
    fn genesis_hashes_are_valid_and_correct() {
        for nt in [NetworkType::Mainnet, NetworkType::Testnet4, NetworkType::Signet] {
            let g = nt.genesis_hash();
            assert_eq!(g.len(), 64, "{:?} genesis must be 64 hex chars", nt);
            assert!(hex::decode(g).is_ok(), "{:?} genesis must be valid hex", nt);
        }
        assert_eq!(
            NetworkType::Mainnet.genesis_hash(),
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
        );
        assert_eq!(
            NetworkType::Signet.genesis_hash(),
            "00000008819873e925422c1ff0f99f7cc9bbb232af63a077a480a3633bee1ef6"
        );
        assert_eq!(
            NetworkType::Testnet4.genesis_hash(),
            "00000000da84f2bafbbc53dee25a72ae507ff4914b867c565be350b0da8bf043"
        );
    }

    #[cfg(test)]
    mod liana_vb_tests {
        use super::super::*;

        #[test]
        fn liana_virtual_block_defaults_disarmed() {
            let c = LianaVirtualBlockConfig::default();
            assert!(!c.enabled);
            assert_eq!(c.target_height, 0);
            assert_eq!(c.armed_at_height, 0);
        }

        #[test]
        fn schedule_config_deserializes_without_liana_block() {
            // Old configs on disk have no liana_virtual_block key; it must default in.
            let toml = r#"
                broadcast_mode = "Immediate"
                default_delay_hours = 24
                min_delay_hours = 2
                max_delay_hours = 72
                min_fee_rate = 1.0
                max_fee_rate = 50.0
            "#;
            let sc: ScheduleConfig = toml::from_str(toml).expect("parse");
            assert!(!sc.liana_virtual_block.enabled);
        }
    }
}
