import { setupManifest } from '@start9labs/start-sdk'
import {
  alertInstall,
  alertStart,
  bitcoindDescription,
  electrsDescription,
  long,
  short,
} from './i18n'

export const manifest = setupManifest({
  id: 'broadcast-pool',
  title: 'Broadcast Pool',
  license: 'MIT',
  packageRepo: 'https://github.com/criptoworld8484/broadcast-pool',
  upstreamRepo: 'https://github.com/criptoworld8484/broadcast-pool',
  marketingUrl: 'https://github.com/criptoworld8484/broadcast-pool',
  donationUrl: null,
  description: { short, long },
  volumes: ['main'],
  images: {
    'broadcast-pool': {
      source: {
        dockerBuild: {
          // The Rust binary is built from the repository root (Cargo.toml, src/),
          // which lives one level above this StartOS package dir (start9/). So the
          // build context is the repo root and the Dockerfile path is relative to it.
          dockerfile: 'start9/Dockerfile',
          workdir: '..',
        },
      },
      arch: ['x86_64', 'aarch64'],
    },
  },
  alerts: {
    install: alertInstall,
    update: null,
    uninstall: null,
    restore: null,
    start: alertStart,
    stop: null,
  },
  dependencies: {
    bitcoind: {
      description: bitcoindDescription,
      optional: false,
      metadata: {
        title: 'Bitcoin Core',
        icon: 'https://raw.githubusercontent.com/Start9Labs/bitcoin-core-startos/refs/heads/30.x/dep-icon.svg',
      },
    },
    electrs: {
      description: electrsDescription,
      optional: false,
      metadata: {
        title: 'Electrs',
        icon: 'https://raw.githubusercontent.com/Start9Labs/electrs-startos/refs/heads/master/icon.svg',
      },
    },
  },
})
