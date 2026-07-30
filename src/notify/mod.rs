//! Broadcast notifications — tells the user, over a private channel, that a waiting
//! transaction has just been redistributed to the network.
//!
//! The event is emitted from the synchronous broadcast hook (`PoolManager::broadcast_due_transactions`)
//! over a `tokio::sync::broadcast` channel and delivered by an async dispatcher task, so a slow or
//! unreachable relay can never delay or fail a broadcast.

pub mod nostr;

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::config::NotificationsConfig;

/// A transaction was broadcast to the network. Deliberately carries no destination address.
#[derive(Debug, Clone, Serialize)]
pub struct BroadcastNotification {
    pub txid: String,
    pub amount_sats: u64,
    /// `sparrow` / `liana` / other wallet label, as stored on the tx.
    pub source_label: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub broadcast_at: DateTime<Utc>,
    pub network: String,
}

/// Message language for the notification body. The dashboard is en/es, so notifications are too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    En,
    Es,
}

impl Language {
    /// Anything that isn't Spanish falls back to English.
    pub fn from_code(code: &str) -> Self {
        if code.trim().to_ascii_lowercase().starts_with("es") {
            Language::Es
        } else {
            Language::En
        }
    }
}

/// BTC as stored on the tx (`total_value_btc`) into whole sats.
pub fn btc_to_sats(btc: f64) -> u64 {
    if btc <= 0.0 {
        return 0;
    }
    (btc * 100_000_000.0).round() as u64
}

/// `abcd1234…9876wxyz` — enough to recognise the tx without pasting a full txid into a chat.
fn short_txid(txid: &str) -> String {
    if txid.len() <= 20 {
        return txid.to_string();
    }
    format!("{}…{}", &txid[..8], &txid[txid.len() - 8..])
}

/// `sparrow` -> `Sparrow`, `liana` -> `Liana`, missing -> a localized "unknown".
fn source_display(source: Option<&str>, lang: Language) -> String {
    match source.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => {
            let mut chars = s.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => s.to_string(),
            }
        }
        None => match lang {
            Language::En => "unknown wallet".to_string(),
            Language::Es => "cartera desconocida".to_string(),
        },
    }
}

fn format_ts(ts: &DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// Group digits so `80235` reads as `80,235` (en) / `80.235` (es).
fn group_digits(n: u64, sep: char) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

impl BroadcastNotification {
    /// Human-readable body for the Nostr DM.
    pub fn message(&self, lang: Language) -> String {
        let txid = short_txid(&self.txid);
        let source = source_display(self.source_label.as_deref(), lang);
        let broadcast_at = format_ts(&self.broadcast_at);

        match lang {
            Language::En => {
                let amount = group_digits(self.amount_sats, ',');
                let created = self
                    .created_at
                    .map(|t| format!("\nCreated: {}", format_ts(&t)))
                    .unwrap_or_default();
                format!(
                    "Broadcast Pool: your transaction from {source} has been redistributed to the network.\n\
                     Txid: {txid}\n\
                     Amount: {amount} sats\n\
                     Network: {network}{created}\n\
                     Broadcast: {broadcast_at}",
                    network = self.network,
                )
            }
            Language::Es => {
                let amount = group_digits(self.amount_sats, '.');
                let created = self
                    .created_at
                    .map(|t| format!("\nCreada: {}", format_ts(&t)))
                    .unwrap_or_default();
                format!(
                    "Broadcast Pool: tu transacción de {source} ha sido redistribuida a la red.\n\
                     Txid: {txid}\n\
                     Importe: {amount} sats\n\
                     Red: {network}{created}\n\
                     Difundida: {broadcast_at}",
                    network = self.network,
                )
            }
        }
    }
}

/// How many times each channel is attempted before the event is dropped (with a log line).
const MAX_ATTEMPTS: u32 = 3;
/// Backoff before retry N (index 0 = after the first failure).
const RETRY_BACKOFF: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(5)];

/// Retry wrapper shared by every channel. Never panics, never propagates — the caller only
/// learns success/failure so it can log.
async fn with_retries<F, Fut>(channel: &str, mut attempt: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    for n in 0..MAX_ATTEMPTS {
        match attempt().await {
            Ok(()) => return true,
            Err(e) => {
                tracing::warn!(
                    "Notification via {} failed (attempt {}/{}): {}",
                    channel,
                    n + 1,
                    MAX_ATTEMPTS,
                    e
                );
                if let Some(backoff) = RETRY_BACKOFF.get(n as usize) {
                    tokio::time::sleep(*backoff).await;
                }
            }
        }
    }
    false
}

/// Deliver one event to every channel enabled in `cfg`. Errors are logged, never returned:
/// a notification failure must never surface anywhere near the broadcast path.
pub async fn deliver(cfg: &NotificationsConfig, notification: &BroadcastNotification) {
    if !cfg.any_active() {
        return;
    }
    let lang = Language::from_code(&cfg.language);

    if cfg.nostr_active() {
        let ok = with_retries("nostr", || nostr::send(&cfg.nostr, notification, lang)).await;
        if ok {
            tracing::info!("Broadcast notification delivered via Nostr");
        } else {
            tracing::error!("Broadcast notification via Nostr gave up after {MAX_ATTEMPTS} attempts");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample() -> BroadcastNotification {
        BroadcastNotification {
            txid: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".to_string(),
            amount_sats: 80_235,
            source_label: Some("sparrow".to_string()),
            created_at: Some(Utc.with_ymd_and_hms(2026, 7, 20, 9, 5, 0).unwrap()),
            broadcast_at: Utc.with_ymd_and_hms(2026, 7, 27, 15, 30, 0).unwrap(),
            network: "testnet4".to_string(),
        }
    }

    #[test]
    fn btc_converts_to_whole_sats() {
        assert_eq!(btc_to_sats(0.00080235), 80_235);
        assert_eq!(btc_to_sats(1.0), 100_000_000);
        assert_eq!(btc_to_sats(0.0), 0);
        assert_eq!(btc_to_sats(-1.0), 0);
    }

    #[test]
    fn btc_to_sats_rounds_instead_of_truncating() {
        // 0.1 + 0.2 style float error must not cost a sat.
        assert_eq!(btc_to_sats(0.1 + 0.2), 30_000_000);
        assert_eq!(btc_to_sats(0.000_000_01), 1);
    }

    #[test]
    fn txid_is_shortened_head_and_tail() {
        assert_eq!(
            short_txid("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"),
            "abcdef01…23456789"
        );
        // Short strings pass through untouched.
        assert_eq!(short_txid("abc123"), "abc123");
    }

    #[test]
    fn english_message_has_the_useful_fields() {
        let msg = sample().message(Language::En);
        assert!(msg.contains("Sparrow"), "{msg}");
        assert!(msg.contains("abcdef01…23456789"), "{msg}");
        assert!(msg.contains("80,235 sats"), "{msg}");
        assert!(msg.contains("testnet4"), "{msg}");
        assert!(msg.contains("Created: 2026-07-20 09:05 UTC"), "{msg}");
        assert!(msg.contains("Broadcast: 2026-07-27 15:30 UTC"), "{msg}");
    }

    #[test]
    fn spanish_message_has_the_useful_fields() {
        let msg = sample().message(Language::Es);
        assert!(msg.contains("Sparrow"), "{msg}");
        assert!(msg.contains("80.235 sats"), "{msg}");
        assert!(msg.contains("redistribuida a la red"), "{msg}");
        assert!(msg.contains("Difundida: 2026-07-27 15:30 UTC"), "{msg}");
    }

    #[test]
    fn message_never_leaks_a_destination_address() {
        // The struct has no address field; guard against one being added and formatted by accident.
        let msg = sample().message(Language::En);
        assert!(!msg.contains("bc1"), "{msg}");
        assert!(!msg.contains("tb1"), "{msg}");
    }

    #[test]
    fn missing_created_at_and_source_degrade_gracefully() {
        let mut n = sample();
        n.created_at = None;
        n.source_label = None;
        let en = n.message(Language::En);
        assert!(!en.contains("Created:"), "{en}");
        assert!(en.contains("unknown wallet"), "{en}");
        let es = n.message(Language::Es);
        assert!(es.contains("cartera desconocida"), "{es}");
    }

    #[test]
    fn language_falls_back_to_english() {
        assert_eq!(Language::from_code("es"), Language::Es);
        assert_eq!(Language::from_code("ES-es"), Language::Es);
        assert_eq!(Language::from_code("en"), Language::En);
        assert_eq!(Language::from_code(""), Language::En);
        assert_eq!(Language::from_code("fr"), Language::En);
    }

    #[tokio::test]
    async fn deliver_is_silent_when_no_channel_is_configured() {
        // Nostr enabled but no relay/recipient: `any_active()` is false, so `deliver` must
        // return without touching the network. Completing at all proves that.
        let mut cfg = NotificationsConfig::default();
        cfg.enabled = true;
        cfg.nostr.enabled = true;
        deliver(&cfg, &sample()).await;

        // Master switch off overrides a fully configured channel.
        let mut cfg = NotificationsConfig::default();
        cfg.nostr.enabled = true;
        cfg.nostr.recipient_npub = "npub1test".to_string();
        cfg.nostr.relays = vec!["ws://127.0.0.1:1".to_string()];
        deliver(&cfg, &sample()).await;
    }

    #[test]
    fn digit_grouping_handles_short_and_long_amounts() {
        assert_eq!(group_digits(0, ','), "0");
        assert_eq!(group_digits(999, ','), "999");
        assert_eq!(group_digits(1_000, ','), "1,000");
        assert_eq!(group_digits(100_000_000, '.'), "100.000.000");
    }
}
