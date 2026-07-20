import { IMPOSSIBLE, VersionInfo } from '@start9labs/start-sdk'

export const current = VersionInfo.of({
  version: '0.3.21:0',
  releaseNotes: {
    en_US: 'At-rest encryption of sensitive pool data (signed txs, addresses) with tamper detection: altering a database row triggers a "safe mode" that halts broadcasting until you review it. New password-encrypted history archive: finished transactions can be flushed manually ("Archive now") or automatically after 30 days, and are only viewable by entering the password. The Bitcoin Core RPC password is now stored encrypted too. Pool value is now shown in sats.',
    es_ES: 'Cifrado en reposo de los datos sensibles del pool (tx firmadas, direcciones) con detección de manipulación: alterar una fila de la base de datos activa un "modo seguro" que detiene la difusión hasta que lo revises. Nuevo archivo histórico cifrado con contraseña: las transacciones terminadas se pueden volcar manualmente ("Archivar ahora") o automáticamente a los 30 días, y solo se consultan introduciendo la contraseña. La contraseña RPC de Bitcoin Core también se guarda cifrada. El valor del pool se muestra ahora en sats.',
  },
  migrations: {
    up: async () => {},
    down: IMPOSSIBLE,
  },
})
