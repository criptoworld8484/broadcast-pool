import { sdk } from './sdk'

// Broadcast Pool needs Bitcoin Core RPC (genesis / network detection / block height,
// via the .cookie) and a working Electrum indexer. This node ships Fulcrum, which is
// Electrum-protocol compatible (the binary works with electrs or Fulcrum). Health-check
// ids come from the installed packages: bitcoind -> [bitcoind, sync-progress];
// fulcrum -> [primary, sync-progress].
export const setDependencies = sdk.setupDependencies(async () => {
  return {
    bitcoind: {
      kind: 'running',
      versionRange: '>=28.0:0',
      healthChecks: ['bitcoind', 'sync-progress'],
    },
    fulcrum: {
      kind: 'running',
      versionRange: '>=1.9:0',
      healthChecks: ['primary', 'sync-progress'],
    },
  }
})
