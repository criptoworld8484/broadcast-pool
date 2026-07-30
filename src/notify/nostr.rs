//! Nostr delivery — a NIP-17 gift-wrapped direct message from the app's own identity to the
//! user's npub, published to the relay(s) the user configures.
//!
//! NIP-17 (via NIP-59 gift wrap) hides sender, recipient and timestamp from the relay, so even
//! a relay operator learns nothing beyond "an encrypted blob arrived".

use std::time::Duration;

use anyhow::{Context, Result};
use nostr_sdk::prelude::*;

use super::{BroadcastNotification, Language};
use crate::config::NostrNotificationConfig;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SEND_TIMEOUT: Duration = Duration::from_secs(20);

/// Tor needs far more headroom than clearnet. Building a circuit to a cold hidden service
/// (descriptor fetch + rendezvous) was measured at ~9s against a real relay — right on top of
/// the clearnet limit — while a warm circuit answers in under 2s. Since we disconnect after
/// every send, most notifications pay the cold cost, so the clearnet budget must not apply.
/// Delivery is fire-and-forget on a background task, so a generous ceiling costs nothing.
const TOR_CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
const TOR_SEND_TIMEOUT: Duration = Duration::from_secs(90);

/// A client plus the timeouts appropriate for how it reaches the network.
#[cfg_attr(test, derive(Debug))]
struct Transport {
    client: Client,
    connect_timeout: Duration,
    send_timeout: Duration,
}

/// Generate a fresh app identity, returned as a bech32 `nsec`. Called when the user enables
/// Nostr notifications and no key has been stored yet.
pub fn generate_nsec() -> Result<String> {
    Keys::generate()
        .secret_key()
        .to_bech32()
        .context("Failed to encode generated Nostr secret key")
}

/// The public identity matching `nsec`, as a bech32 `npub`. This is what the dashboard shows so
/// the user can recognise (or whitelist) the sender. The nsec itself never leaves the process.
pub fn npub_from_nsec(nsec: &str) -> Result<String> {
    let keys = parse_keys(nsec)?;
    keys.public_key()
        .to_bech32()
        .context("Failed to encode Nostr public key")
}

fn parse_keys(nsec: &str) -> Result<Keys> {
    Keys::parse(nsec.trim()).context("Invalid Nostr secret key (expected nsec… or hex)")
}

fn parse_recipient(npub: &str) -> Result<PublicKey> {
    PublicKey::parse(npub.trim()).context("Invalid recipient npub")
}

/// Reject a recipient key at save time instead of at broadcast time.
pub fn validate_npub(npub: &str) -> Result<()> {
    parse_recipient(npub).map(|_| ())
}

/// Build the client, routing `.onion` relays through the configured SOCKS proxy.
///
/// `ConnectionTarget::Onion` means clearnet and LAN relays still connect directly, so a
/// user can mix a hidden-service relay with a local one and each takes the right path.
/// We use an external Tor daemon (Umbrel ships one) rather than nostr-sdk's embedded
/// `tor` feature, which would pull in arti and bloat the binary.
fn build_client(keys: Keys, cfg: &NostrNotificationConfig) -> Result<Transport> {
    let socks = cfg.resolved_tor_socks(crate::discovery::is_umbrel_mode());
    let onion = cfg.has_onion_relay();

    let Some(socks) = socks else {
        if onion {
            // Failing here beats a 20s timeout with no explanation.
            anyhow::bail!(
                "A .onion relay is configured but no Tor SOCKS proxy is available. \
                 Set the Tor proxy address in Settings (e.g. 127.0.0.1:9050)."
            );
        }
        return Ok(Transport {
            client: Client::new(keys),
            connect_timeout: CONNECT_TIMEOUT,
            send_timeout: SEND_TIMEOUT,
        });
    };

    let addr = resolve_socks_addr(&socks)?;
    let opts = ClientOptions::new()
        .connection(Connection::new().proxy(addr).target(ConnectionTarget::Onion));

    // Only stretch the budget when an onion relay is actually in the list: a proxy configured
    // alongside clearnet-only relays changes nothing about how fast they answer.
    let (connect_timeout, send_timeout) = if onion {
        (TOR_CONNECT_TIMEOUT, TOR_SEND_TIMEOUT)
    } else {
        (CONNECT_TIMEOUT, SEND_TIMEOUT)
    };

    Ok(Transport {
        client: Client::builder().signer(keys).opts(opts).build(),
        connect_timeout,
        send_timeout,
    })
}

/// `host:port` to a SocketAddr. A hostname (Umbrel's `tor_proxy`) needs DNS resolution.
fn resolve_socks_addr(socks: &str) -> Result<std::net::SocketAddr> {
    use std::net::ToSocketAddrs;
    socks
        .trim()
        .to_socket_addrs()
        .with_context(|| format!("Could not resolve Tor SOCKS address '{socks}'"))?
        .next()
        .with_context(|| format!("Tor SOCKS address '{socks}' resolved to nothing"))
}

/// Flatten per-relay connect/publish failures into one readable line. Connect errors come
/// first: an unreachable relay explains any publish failure that follows.
fn describe_failures<K: std::fmt::Display, V: std::fmt::Display>(
    connect_failed: &std::collections::HashMap<K, V>,
    publish_failed: &std::collections::HashMap<K, V>,
) -> String {
    let mut parts: Vec<String> = connect_failed
        .iter()
        .chain(publish_failed.iter())
        .map(|(url, err)| format!("{url}: {err}"))
        .collect();
    parts.sort();
    parts.dedup();
    parts.join("; ")
}

/// Send the notification as a NIP-17 DM. Returns Err so the dispatcher can retry; the caller
/// must never let this reach the broadcast path.
pub async fn send(
    cfg: &NostrNotificationConfig,
    notification: &BroadcastNotification,
    lang: Language,
) -> Result<()> {
    let keys = parse_keys(&cfg.app_nsec)?;
    let recipient = parse_recipient(&cfg.recipient_npub)?;

    let relays: Vec<&str> = cfg
        .relays
        .iter()
        .map(|r| r.trim())
        .filter(|r| !r.is_empty())
        .collect();
    if relays.is_empty() {
        anyhow::bail!("No Nostr relays configured");
    }

    let Transport {
        client,
        connect_timeout,
        send_timeout,
    } = build_client(keys, cfg)?;
    for relay in &relays {
        client
            .add_relay(*relay)
            .await
            .with_context(|| format!("Invalid relay URL: {relay}"))?;
    }

    // `connect()` is fire-and-forget and hides an unreachable relay; `try_connect` waits and
    // reports per-relay failures, which is what the user needs to see in the test button.
    let connect = client.try_connect(connect_timeout).await;

    let result = tokio::time::timeout(
        send_timeout,
        client.send_private_msg(recipient, notification.message(lang), []),
    )
    .await
    .context("Timed out sending Nostr DM")?;

    // Best-effort teardown; a failure here doesn't change whether the DM was accepted.
    let _ = client.disconnect().await;

    let output = result.context("Failed to send Nostr DM")?;
    if output.success.is_empty() {
        // Surface the underlying reason ("No route to host", "connection refused", …) rather
        // than a generic failure the user can't act on.
        let reasons = describe_failures(&connect.failed, &output.failed);
        if reasons.is_empty() {
            anyhow::bail!("No relay accepted the Nostr DM");
        }
        anyhow::bail!("No relay accepted the Nostr DM: {reasons}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

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
    fn generated_nsec_is_bech32_and_yields_an_npub() {
        let nsec = generate_nsec().unwrap();
        assert!(nsec.starts_with("nsec1"), "{nsec}");
        let npub = npub_from_nsec(&nsec).unwrap();
        assert!(npub.starts_with("npub1"), "{npub}");
    }

    #[test]
    fn npub_derivation_is_deterministic() {
        let nsec = generate_nsec().unwrap();
        assert_eq!(npub_from_nsec(&nsec).unwrap(), npub_from_nsec(&nsec).unwrap());
        // Surrounding whitespace from a copy/paste must not change the identity.
        assert_eq!(
            npub_from_nsec(&nsec).unwrap(),
            npub_from_nsec(&format!("  {nsec}  ")).unwrap()
        );
    }

    #[test]
    fn distinct_keys_give_distinct_npubs() {
        assert_ne!(
            npub_from_nsec(&generate_nsec().unwrap()).unwrap(),
            npub_from_nsec(&generate_nsec().unwrap()).unwrap()
        );
    }

    fn cfg_with_relays(relays: &[&str], tor_socks: &str) -> NostrNotificationConfig {
        NostrNotificationConfig {
            enabled: true,
            recipient_npub: npub_from_nsec(&generate_nsec().unwrap()).unwrap(),
            relays: relays.iter().map(|s| s.to_string()).collect(),
            app_nsec: generate_nsec().unwrap(),
            tor_socks: tor_socks.to_string(),
        }
    }

    #[test]
    fn onion_relay_without_a_proxy_fails_fast_with_an_actionable_message() {
        let cfg = cfg_with_relays(&["ws://abcdef234567.onion"], "off");
        let keys = parse_keys(&cfg.app_nsec).unwrap();
        let err = build_client(keys, &cfg).unwrap_err().to_string();
        assert!(err.contains("Tor SOCKS proxy"), "{err}");
    }

    #[test]
    fn clearnet_relay_without_a_proxy_connects_directly_on_the_clearnet_budget() {
        // No proxy configured and no .onion relay: this must not error.
        let cfg = cfg_with_relays(&["ws://192.168.50.68:7777"], "off");
        let keys = parse_keys(&cfg.app_nsec).unwrap();
        let t = build_client(keys, &cfg).unwrap();
        assert_eq!(t.connect_timeout, CONNECT_TIMEOUT);
        assert_eq!(t.send_timeout, SEND_TIMEOUT);
    }

    #[test]
    fn an_onion_relay_gets_the_longer_tor_budget() {
        // A cold Tor circuit was measured at ~9s, which the clearnet budget cannot absorb.
        let cfg = cfg_with_relays(&["ws://abcdef234567.onion"], "127.0.0.1:9050");
        let keys = parse_keys(&cfg.app_nsec).unwrap();
        let t = build_client(keys, &cfg).unwrap();
        assert_eq!(t.connect_timeout, TOR_CONNECT_TIMEOUT);
        assert_eq!(t.send_timeout, TOR_SEND_TIMEOUT);
    }

    #[test]
    fn a_proxy_with_only_clearnet_relays_keeps_the_short_budget() {
        // The proxy is irrelevant to relays that don't go through it.
        let cfg = cfg_with_relays(&["wss://relay.damus.io"], "127.0.0.1:9050");
        let keys = parse_keys(&cfg.app_nsec).unwrap();
        let t = build_client(keys, &cfg).unwrap();
        assert_eq!(t.connect_timeout, CONNECT_TIMEOUT);
    }

    #[test]
    fn an_unresolvable_proxy_address_is_reported() {
        let cfg = cfg_with_relays(&["ws://abcdef234567.onion"], "no-such-host.invalid:9050");
        let keys = parse_keys(&cfg.app_nsec).unwrap();
        let err = build_client(keys, &cfg).unwrap_err().to_string();
        assert!(err.contains("Tor SOCKS address"), "{err}");
    }

    #[test]
    fn garbage_keys_are_rejected() {
        assert!(npub_from_nsec("not-a-key").is_err());
        assert!(npub_from_nsec("").is_err());
    }

    #[tokio::test]
    async fn missing_relays_fail_before_any_network_use() {
        let cfg = NostrNotificationConfig {
            enabled: true,
            recipient_npub: npub_from_nsec(&generate_nsec().unwrap()).unwrap(),
            relays: vec!["   ".to_string()],
            app_nsec: generate_nsec().unwrap(),
            tor_socks: String::new(),
        };
        let err = send(&cfg, &sample(), Language::En).await.unwrap_err();
        assert!(err.to_string().contains("No Nostr relays"), "{err}");
    }

    #[tokio::test]
    async fn invalid_recipient_is_rejected() {
        let cfg = NostrNotificationConfig {
            enabled: true,
            recipient_npub: "npub-nonsense".to_string(),
            relays: vec!["ws://127.0.0.1:7777".to_string()],
            app_nsec: generate_nsec().unwrap(),
            tor_socks: String::new(),
        };
        assert!(send(&cfg, &sample(), Language::En).await.is_err());
    }
}
