import { IMPOSSIBLE, VersionInfo } from '@start9labs/start-sdk'

export const current = VersionInfo.of({
  version: '0.3.14:2',
  releaseNotes: {
    en_US: 'Add a dedicated Electrum interface for Liana (port 50051) so its transactions are held as pending for date/time scheduling instead of being categorized by block height.',
    es_ES: 'Añade una interfaz Electrum dedicada para Liana (puerto 50051) para que sus transacciones entren como pendientes y se puedan programar por fecha/hora, en vez de categorizarse por altura de bloque.',
  },
  migrations: {
    up: async () => {},
    down: IMPOSSIBLE,
  },
})
