# Broadcast Pool

Broadcast Pool sits between your wallet (Sparrow or Liana) and your node's
Electrum index. It intercepts transaction broadcasts and holds the signed
transactions in a **virtual mempool** until a chosen criterion is met
(immediate, scheduled date/time, fiat price, or block height).

## Setup

1. Make sure **Bitcoin Core** (pruning disabled) and **Electrs** are installed,
   running, and synced.
2. Start Broadcast Pool. The network (mainnet, testnet4 or signet) is detected
   automatically from Bitcoin Core.
3. Open the **Interfaces** section and copy the **Electrum (TCP)** LAN address.

## Connecting a wallet

- In **Sparrow** or **Liana**, set the Electrum server to the **Electrum (TCP)**
  address shown in the Interfaces section (plain TCP, no SSL). Make sure the
  wallet's network matches your node's network.
- Use the **Web Dashboard** interface to monitor pending transactions and to
  schedule or trigger broadcasts.

## Important

A transaction shown as *pending* is **retained in the virtual mempool and has
NOT been broadcast to the network** until its trigger fires. Keep this service
and its data volume running so scheduled broadcasts are not lost. The signed
transaction hex is retained and can be re-broadcast manually if needed.
