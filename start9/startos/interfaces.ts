import { i18n } from './i18n'
import { sdk } from './sdk'
import { electrumPort, webPort } from './utils'

export const setInterfaces = sdk.setupInterfaces(async ({ effects }) => {
  // --- Electrum proxy (plain TCP) for Sparrow / Liana ---
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
    description: i18n(
      'The main Electrum interface for Sparrow and Liana (plain TCP, no SSL)',
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
    await webOrigin.export([web]),
  ]
})
