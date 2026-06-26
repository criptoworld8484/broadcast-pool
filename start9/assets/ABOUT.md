# Broadcast Pool

Privacy-preserving Bitcoin broadcast scheduler for StartOS. Sits between your
wallet (Sparrow or Liana) and your node's Electrum index, holding signed
transactions in a virtual mempool until a chosen trigger fires (immediate,
scheduled time, fiat price, or block height). Network auto-detected from
Bitcoin Core (mainnet, testnet4, signet).

Upstream: https://github.com/criptoworld8484/broadcast-pool
