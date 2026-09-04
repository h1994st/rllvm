# Changelog

## [0.2.0](https://github.com/h1994st/rllvm/compare/v0.1.9...v0.2.0) (2026-09-04)


### ⚠ BREAKING CHANGES

* the bitcode section names changed on every format. Objects built by an earlier rllvm are not readable and must be rebuilt.

### Features

* rename bitcode sections so wasm-ld keeps them ([#83](https://github.com/h1994st/rllvm/issues/83)) ([130e2f0](https://github.com/h1994st/rllvm/commit/130e2f0c9dc9b4be7862337bcb218f78e39f2256))


### Bug Fixes

* mark the bitcode section no_dead_strip instead of dropping -dead_strip ([#87](https://github.com/h1994st/rllvm/issues/87)) ([19683f7](https://github.com/h1994st/rllvm/commit/19683f7a2238ce9cb295130a908c828dedeee1f2))
* produce bitcode under cargo for bin and lib crates ([#88](https://github.com/h1994st/rllvm/issues/88)) ([3b72ca3](https://github.com/h1994st/rllvm/commit/3b72ca3e3e4d60ac97901b1d43b52c3b62732147))

## [0.1.9](https://github.com/h1994st/rllvm/compare/v0.1.8...v0.1.9) (2026-09-04)


### Bug Fixes

* default the link output to a.out when -o is absent ([#73](https://github.com/h1994st/rllvm/issues/73)) ([06d4531](https://github.com/h1994st/rllvm/commit/06d45313b483685128aba8edac631b043c51a360))
* unblock release PRs by reconciling autorelease labels ([#74](https://github.com/h1994st/rllvm/issues/74)) ([566e2df](https://github.com/h1994st/rllvm/commit/566e2dfaf1f500ced6495d3e423f695d7638ce07))

## [0.1.8](https://github.com/h1994st/rllvm/compare/v0.1.7...v0.1.8) (2026-09-03)


### Bug Fixes

* report the correct wrapper name in version and help ([#71](https://github.com/h1994st/rllvm/issues/71)) ([4e6f373](https://github.com/h1994st/rllvm/commit/4e6f373fff668c39de25af15d7bb20dd8c384bf0))
