import { IMPOSSIBLE, VersionInfo } from '@start9labs/start-sdk'

export const current = VersionInfo.of({
  version: '0.3.20:0',
  releaseNotes: {
    en_US: 'Imported transactions now show as a distinct "Importada" type in the dashboard and can be scheduled by date/time or price, just like manual ones. Removed the external/secondary indexer from the UI and config; the primary indexer and Bitcoin Core backup remain.',
    es_ES: 'Las transacciones importadas ahora se muestran como un tipo "Importada" diferenciado en el dashboard y pueden programarse por fecha/hora o por precio, igual que las manuales. Se elimina el indexador externo/secundario de la interfaz y la configuración; el indexador principal y la copia de seguridad de Bitcoin Core se mantienen.',
  },
  migrations: {
    up: async () => {},
    down: IMPOSSIBLE,
  },
})
