import { IMPOSSIBLE, VersionInfo } from '@start9labs/start-sdk'

export const current = VersionInfo.of({
  version: '0.3.24:0',
  releaseNotes: {
    en_US: 'Fixes how the block height reaches your wallet. The tip served to Sparrow could fall hours behind and new blocks were never pushed, so nLockTime was stamped with a stale height and mined transactions kept showing as unconfirmed until you reconnected. Adds a "Broadcast now" button on every transaction, with the reason when it cannot be used — it overrides the app\'s own scheduling but never an nLockTime signed into the transaction, which the network enforces. Importing an unsigned transaction is now refused instead of failing later, and a node\'s rejection reason reaches the dashboard instead of a generic error. The mempool fee indicator no longer reports absurd values on test networks. Copying a txid works over plain HTTP, and broadcast and confirmed no longer share nearly the same colour.',
    es_ES: 'Corrige cómo llega la altura de bloque a tu cartera. La que se servía a Sparrow podía quedarse horas desfasada y los bloques nuevos no se notificaban, así que el nLockTime se sellaba con una altura vieja y las transacciones minadas seguían apareciendo sin confirmar hasta reconectar. Añade un botón «Difundir ahora» en cada transacción, con el motivo cuando no puede usarse: se salta la programación de la app, pero nunca un nLockTime firmado en la transacción, que impone la red. Importar una transacción sin firmar ahora se rechaza en vez de fallar después, y el motivo del rechazo del nodo llega al panel en lugar de un error genérico. El indicador de comisiones ya no muestra valores absurdos en redes de prueba. Copiar un txid funciona por HTTP, y «transmitida» y «confirmada» ya no comparten casi el mismo color.',
  },
  migrations: {
    up: async () => {},
    down: IMPOSSIBLE,
  },
})
