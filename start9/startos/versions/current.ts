import { IMPOSSIBLE, VersionInfo } from '@start9labs/start-sdk'

export const current = VersionInfo.of({
  version: '0.3.19:0',
  releaseNotes: {
    en_US: 'Scheduled broadcasts survive an indexer outage: the chain clock falls back to Bitcoin Core, and optionally to a secondary indexer, so schedules keep firing. Liana UTXO-cycling via a configurable virtual block height. Dashboard: source-coloured tx table, merged TXID column, and UI refinements.',
    es_ES: 'Las difusiones programadas sobreviven a una caída del indexador: el reloj de la cadena cae a Bitcoin Core y, opcionalmente, a un indexador secundario, para que las programaciones sigan cumpliéndose. Ciclado de UTXO de Liana mediante una altura de bloque virtual configurable. Dashboard: tabla de tx con origen coloreado, columna TXID unificada y mejoras de UI.',
  },
  migrations: {
    up: async () => {},
    down: IMPOSSIBLE,
  },
})
