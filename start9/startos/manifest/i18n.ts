export const short = {
  en_US: 'Schedule and delay Bitcoin broadcasts from Sparrow/Liana',
  es_ES: 'Programa y retrasa difusiones de Bitcoin desde Sparrow/Liana',
}

export const long = {
  en_US:
    'Broadcast Pool sits between your wallet (Sparrow or Liana) and your node\'s Electrum index. It intercepts transaction broadcasts and holds the signed transactions in a virtual mempool until a chosen criterion is met (immediate, scheduled date/time, fiat price, or block height). It connects automatically to Bitcoin Core and Electrs on StartOS and exposes a plain TCP Electrum interface plus a web dashboard. The network (mainnet, testnet4 or signet) is detected automatically from Bitcoin Core.',
  es_ES:
    'Broadcast Pool se sitúa entre tu billetera (Sparrow o Liana) y el índice Electrum de tu nodo. Intercepta las difusiones de transacciones y retiene las transacciones firmadas en una mempool virtual hasta que se cumple el criterio elegido (inmediato, fecha/hora programada, precio fiat o altura de bloque). Se conecta automáticamente a Bitcoin Core y Electrs en StartOS y expone una interfaz Electrum TCP y un panel web. La red (mainnet, testnet4 o signet) se detecta automáticamente desde Bitcoin Core.',
}

export const alertInstall = {
  en_US:
    'Broadcast Pool requires Bitcoin Core (pruning disabled) and Electrs. A transaction shown as pending is retained in the virtual mempool and is NOT yet broadcast to the network until its trigger fires.',
  es_ES:
    'Broadcast Pool requiere Bitcoin Core (sin poda) y Electrs. Una transacción marcada como pendiente se retiene en la mempool virtual y NO se difunde a la red hasta que se dispara su criterio.',
}

export const alertStart = {
  en_US:
    'Broadcast Pool will connect to Bitcoin Core and Electrs. Point Sparrow or Liana at the Electrum (TCP) interface shown below.',
  es_ES:
    'Broadcast Pool se conectará a Bitcoin Core y Electrs. Apunta Sparrow o Liana a la interfaz Electrum (TCP) que se muestra abajo.',
}

export const bitcoindDescription = {
  en_US: 'Provides blockchain data, RPC, and cookie authentication',
  es_ES: 'Proporciona datos de blockchain, RPC y autenticación por cookie',
}

export const electrsDescription = {
  en_US: 'Provides the Electrum index backend for address and history lookups',
  es_ES:
    'Proporciona el backend de índice Electrum para direcciones e historial',
}
