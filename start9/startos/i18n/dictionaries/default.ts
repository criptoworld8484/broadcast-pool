export const DEFAULT_LANG = 'en_US'

const dict = {
  'Starting Broadcast Pool!': 0,
  'Electrum (TCP)': 1,
  'The Electrum interface is ready': 2,
  'The Electrum interface is not ready': 3,
  'Web Dashboard': 4,
  'The main Electrum interface for Sparrow and Liana (plain TCP, no SSL)': 5,
  'Web dashboard to monitor and schedule pending broadcasts': 6,
  'Provides blockchain data, RPC, and cookie authentication': 7,
  'Provides the Electrum index backend for address and history lookups': 8,
  'Pruning must be disabled for Electrs and transaction lookups to work.': 9,
} as const

export type LangDict = Record<number, string>

export default dict
