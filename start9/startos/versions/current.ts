import { IMPOSSIBLE, VersionInfo } from '@start9labs/start-sdk'

export const current = VersionInfo.of({
  version: '0.3.25:0',
  releaseNotes: {
    en_US: 'Wallets are now told when the indexer sees a change. Until now every call to the indexer opened a fresh connection and closed it, so the app held no subscription and had no way to learn that a payment had arrived or a transaction confirmed — a wallet only ever saw what it thought to ask for, and reconnecting was the one reliable way to resync. A single persistent subscription now relays those changes as they happen.',
    es_ES: 'Ahora se avisa a la cartera cuando el indexador ve un cambio. Hasta ahora cada llamada al indexador abría una conexión nueva y la cerraba, así que la app no mantenía ninguna suscripción y no tenía forma de enterarse de que había llegado un pago o de que una transacción se había confirmado: la cartera solo veía lo que se le ocurría preguntar, y reconectar era el único modo fiable de resincronizar. Una única suscripción persistente retransmite ahora esos cambios en cuanto ocurren.',
  },
  migrations: {
    up: async () => {},
    down: IMPOSSIBLE,
  },
})
