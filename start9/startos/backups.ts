import { sdk } from './sdk'

// Back up the whole main volume. The pending/scheduled transactions in the
// virtual mempool are the recovery-critical state, so nothing is excluded.
export const { createBackup, restoreInit } = sdk.setupBackups(async () =>
  sdk.Backups.ofVolumes('main').setOptions({
    exclude: [],
  }),
)
