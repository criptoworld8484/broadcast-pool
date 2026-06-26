import { i18n } from './i18n'
import { sdk } from './sdk'
import { electrumPort } from './utils'

export const main = sdk.setupMain(async ({ effects }) => {
  console.info('Starting Broadcast Pool!')

  const container = await sdk.SubContainer.of(
    effects,
    { imageId: 'broadcast-pool' },
    sdk.Mounts.of()
      .mountVolume({
        volumeId: 'main',
        subpath: null,
        mountpoint: '/data',
        readonly: false,
      })
      // Bitcoin Core data dir, read-only, for the RPC .cookie (auth) — the
      // entrypoint reads it and derives RPC user/pass. Network is auto-detected
      // from Bitcoin Core, so the same image works on mainnet/testnet4/signet.
      .mountDependency({
        dependencyId: 'bitcoind',
        volumeId: 'main',
        subpath: null,
        mountpoint: '/mnt/bitcoind',
        readonly: true,
      }),
    'broadcast-pool',
  )

  return sdk.Daemons.of(effects)
    .addDaemon('broadcast-pool', {
      subcontainer: container,
      exec: { command: ['/entrypoint.sh'] },
      ready: {
        display: i18n('Electrum (TCP)'),
        fn: async () => {
          const result = await sdk.healthCheck.checkPortListening(
            effects,
            electrumPort,
            {
              successMessage: i18n('The Electrum interface is ready'),
              errorMessage: i18n('The Electrum interface is not ready'),
            },
          )

          if (result.result === 'success') return result

          return {
            result: 'starting',
            message: i18n('The Electrum interface is not ready'),
          }
        },
      },
      requires: [],
    })
})
