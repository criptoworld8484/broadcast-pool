// Electrum proxy port for Sparrow (plain TCP).
export const electrumPort = 50050
// Dedicated Electrum port for Liana — txs here are ingested as manual/pending so the
// user can schedule a date/time (Liana's block-height nLockTime would otherwise be
// categorized as "by_block").
export const lianaPort = 50051
// Web dashboard port.
export const webPort = 8080
