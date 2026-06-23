# Broadcast Pool

Privacy-preserving Bitcoin transaction scheduling and broadcasting tool. It sits
between a wallet (Sparrow / Liana) and an Electrum indexer (electrs / Fulcrum),
exposing an Electrum server that intercepts `blockchain.transaction.broadcast`
and holds signed transactions in a **virtual mempool** until they meet a chosen
broadcast criterion (immediate, scheduled date/time, fiat price, or block height).

## Architecture

```
Sparrow / Liana  ──tcp:50050──▶  Broadcast Pool (Electrum server + virtual mempool)
                                        │
                                        ├──▶ electrs / Fulcrum   (history, balances, UTXOs)
                                        └──▶ Bitcoin Core RPC     (final broadcast)
```

- Wallet broadcasts are intercepted and **retained** instead of being relayed.
- The wallet still sees the pending tx (the pool injects it into history/mempool
  responses) so balances update immediately.
- A scheduler broadcasts the tx to the network when its trigger fires.

## Build

```bash
cargo build --release
./target/release/broadcast-pool start
```

## Configuration

Configured via `config/default.toml` and `BROADCAST_POOL_*` environment variables
(network, indexer, Bitcoin Core RPC, Electrum host/port). **Never commit real RPC
credentials** — inject them via environment variables.

## Umbrel

Packaged as an Umbrel Community Store app under `umbrel-app/sparrow-broadcast-pool/`.
The Docker image is published to
`ghcr.io/criptoworld8484/broadcast-pool-umbrel` by the
`.github/workflows/umbrel-docker.yml` workflow when a `v*` tag is pushed.

See [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md) and [`UMBREL.md`](UMBREL.md) for
deployment and debugging notes.

## License

[MIT](LICENSE)
