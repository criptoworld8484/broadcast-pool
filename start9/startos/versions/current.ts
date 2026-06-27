import { IMPOSSIBLE, VersionInfo } from '@start9labs/start-sdk'

export const current = VersionInfo.of({
  version: '0.3.15:1',
  releaseNotes: {
    en_US: 'Initial StartOS package: Electrum broadcast pool for Sparrow/Liana with virtual mempool and scheduled broadcasting. Network auto-detected from Bitcoin Core.',
    es_ES: 'Paquete inicial para StartOS: pool de difusión Electrum para Sparrow/Liana con mempool virtual y difusión programada. Red autodetectada desde Bitcoin Core.',
  },
  migrations: {
    up: async () => {},
    down: IMPOSSIBLE,
  },
})
