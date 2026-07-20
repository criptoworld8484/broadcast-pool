import { IMPOSSIBLE, VersionInfo } from '@start9labs/start-sdk'

export const current = VersionInfo.of({
  version: '0.3.22:0',
  releaseNotes: {
    en_US: 'Fixes a spurious "Error saving" message that appeared when saving settings; the configuration was being saved correctly — it was only a mistaken UI warning (leftover dead code from the indexer cleanup).',
    es_ES: 'Corrige un mensaje de error falso ("Error saving") que aparecía al guardar los ajustes; la configuración se guardaba correctamente, solo era un aviso erróneo de la interfaz (código muerto de la limpieza del indexador).',
  },
  migrations: {
    up: async () => {},
    down: IMPOSSIBLE,
  },
})
