import { i18n } from './i18n'
import { sdk } from './sdk'
import { electrumPort, lianaPort, webPort } from './utils'

export const setInterfaces = sdk.setupInterfaces(async ({ effects }) => {
  // --- Electrum proxy (plain TCP) for Sparrow ---
  const electrumHost = sdk.MultiHost.of(effects, 'electrum')
  const electrumOrigin = await electrumHost.bindPort(electrumPort, {
    protocol: null,
    preferredExternalPort: electrumPort,
    addSsl: null,
    secure: { ssl: false },
  })
  const electrum = sdk.createInterface(effects, {
    id: 'electrum',
    name: i18n('Electrum (TCP)'),
    description: i18n('The main Electrum interface for Sparrow (plain TCP, no SSL)'),
    type: 'api',
    masked: false,
    schemeOverride: null,
    username: null,
    path: '',
    query: {},
  })

  // --- Dedicated Electrum proxy for Liana (plain TCP) ---
  // Liana's anti-fee-sniping block-height nLockTime would be categorized "by_block"
  // on the Sparrow port; connecting here tags the source as Liana → manual/pending.
  const lianaHost = sdk.MultiHost.of(effects, 'liana')
  const lianaOrigin = await lianaHost.bindPort(lianaPort, {
    protocol: null,
    preferredExternalPort: lianaPort,
    addSsl: null,
    secure: { ssl: false },
  })
  const liana = sdk.createInterface(effects, {
    id: 'liana',
    name: i18n('Electrum — Liana (TCP)'),
    description: i18n(
      'Dedicated Electrum interface for Liana — transactions arrive as pending so you can schedule a broadcast time',
    ),
    type: 'api',
    masked: false,
    schemeOverride: null,
    username: null,
    path: '',
    query: {},
  })

  // --- Web dashboard (HTTP UI) ---
  const webHost = sdk.MultiHost.of(effects, 'web')
  const webOrigin = await webHost.bindPort(webPort, {
    protocol: 'http',
    preferredExternalPort: webPort,
  })
  const web = sdk.createInterface(effects, {
    id: 'web',
    name: i18n('Web Dashboard'),
    description: i18n(
      'Web dashboard to monitor and schedule pending broadcasts',
    ),
    type: 'ui',
    masked: false,
    schemeOverride: null,
    username: null,
    path: '',
    query: {},
  })

  return [
    await electrumOrigin.export([electrum]),
    await lianaOrigin.export([liana]),
    await webOrigin.export([web]),
  ]
})
