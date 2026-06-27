import { LangDict } from './default'

const translations: Record<string, LangDict> = {
  es_ES: {
    0: '¡Iniciando Broadcast Pool!',
    1: 'Electrum (TCP)',
    2: 'La interfaz Electrum está lista',
    3: 'La interfaz Electrum no está lista',
    4: 'Panel web',
    5: 'Interfaz Electrum principal para Sparrow (TCP plano, sin SSL)',
    6: 'Panel web para monitorizar y programar las difusiones pendientes',
    7: 'Proporciona datos de blockchain, RPC y autenticación por cookie',
    8: 'Proporciona el backend de índice Electrum para direcciones e historial',
    9: 'La poda debe estar deshabilitada para que Electrs y las búsquedas de transacciones funcionen.',
    10: 'Electrum — Liana (TCP)',
    11: 'Interfaz Electrum dedicada para Liana — las transacciones entran como pendientes para que programes la fecha/hora de difusión',
  },
}

export default translations
