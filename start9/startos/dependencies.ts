import { autoconfig } from 'bitcoin-core-startos/startos/actions/config/autoconfig'
import { i18n } from './i18n'
import { sdk } from './sdk'

export const setDependencies = sdk.setupDependencies(async ({ effects }) => {
  // Broadcast Pool only needs RPC (genesis/network/height) + a working Electrs,
  // so the single hard requirement on bitcoind is that pruning is disabled
  // (Electrs and prev-tx lookups need full block data).
  await sdk.action.createTask(effects, 'bitcoind', autoconfig, 'critical', {
    input: {
      kind: 'partial',
      value: {
        prune: 0,
      },
    },
    reason: i18n(
      'Pruning must be disabled for Electrs and transaction lookups to work.',
    ),
    when: { condition: 'input-not-matches', once: false },
  })

  return {
    bitcoind: {
      kind: 'running',
      versionRange: '>=28.0:0',
      healthChecks: ['bitcoind', 'sync-progress'],
    },
    electrs: {
      kind: 'running',
      versionRange: '>=0.10:0',
      healthChecks: ['electrs', 'sync'],
    },
  }
})
