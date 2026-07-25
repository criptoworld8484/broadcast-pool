import { IMPOSSIBLE, VersionInfo } from '@start9labs/start-sdk'

export const current = VersionInfo.of({
  version: '0.3.23:0',
  releaseNotes: {
    en_US: 'Immediate transactions are now correctly labeled "immediate" instead of "by block": an already-final anti-fee-sniping nLockTime (at/below the current tip) is treated as immediate, while genuine future block locks stay by-block. Rewritten Liana slide in the Quick Guide as a step-by-step UTXO-cycling walkthrough. Added a discreet footer with author, year and app version.',
    es_ES: 'Las transacciones inmediatas ahora se etiquetan correctamente como "inmediata" en lugar de "por bloque": un nLockTime de anti-fee-sniping ya cumplido (≤ el bloque actual) se trata como inmediata, mientras que los bloqueos de altura futura reales siguen como por bloque. Reescrita la diapositiva de Liana en la Guía Rápida como un tutorial paso a paso del ciclado de UTXOs. Añadido un pie de página discreto con autor, año y versión de la app.',
  },
  migrations: {
    up: async () => {},
    down: IMPOSSIBLE,
  },
})
