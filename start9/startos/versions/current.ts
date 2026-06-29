import { IMPOSSIBLE, VersionInfo } from '@start9labs/start-sdk'

export const current = VersionInfo.of({
  version: '0.3.17:1',
  releaseNotes: {
    en_US: 'Wait for the Bitcoin Core dependency to be reachable before starting, so the network is detected correctly (fixes the dashboard showing testnet4 on a mainnet node when the dependency network was not yet ready at startup).',
    es_ES: 'Espera a que la dependencia Bitcoin Core sea alcanzable antes de arrancar, para detectar bien la red (corrige que el panel mostrara testnet4 en un nodo mainnet cuando la red de dependencias aún no estaba lista al arrancar).',
  },
  migrations: {
    up: async () => {},
    down: IMPOSSIBLE,
  },
})
