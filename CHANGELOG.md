# Changelog

## [0.4.6](https://github.com/h1994st/rllvm/compare/v0.4.5...v0.4.6) (2026-09-09)


### Bug Fixes

* run the tap bump as part of the release build ([#131](https://github.com/h1994st/rllvm/issues/131)) ([2efac89](https://github.com/h1994st/rllvm/commit/2efac89b07f986298e7c08dc07e18371d2a83523))

## [0.4.5](https://github.com/h1994st/rllvm/compare/v0.4.4...v0.4.5) (2026-09-09)


### Bug Fixes

* bump the tap only for an actual release build ([#130](https://github.com/h1994st/rllvm/issues/130)) ([81a3e0f](https://github.com/h1994st/rllvm/commit/81a3e0f097ed2468ebb036adb5131024436382cf))
* trigger the tap bump from the release build, not the release event ([#127](https://github.com/h1994st/rllvm/issues/127)) ([8dfa53e](https://github.com/h1994st/rllvm/commit/8dfa53e6d5a2f4f443e70769a0360d2a135bc122))

## [0.4.4](https://github.com/h1994st/rllvm/compare/v0.4.3...v0.4.4) (2026-09-09)


### Bug Fixes

* keep the prefix rllvm-init was given instead of resolving it ([#125](https://github.com/h1994st/rllvm/issues/125)) ([8a5aaf6](https://github.com/h1994st/rllvm/commit/8a5aaf6c5ba5f8a48f2190468f908919bb53d923))

## [0.4.3](https://github.com/h1994st/rllvm/compare/v0.4.2...v0.4.3) (2026-09-09)


### Bug Fixes

* generate completions from the CLI the binaries parse with ([#122](https://github.com/h1994st/rllvm/issues/122)) ([886e17e](https://github.com/h1994st/rllvm/commit/886e17e273e234545fa928f9f2a8782b14f963dd))

## [0.4.2](https://github.com/h1994st/rllvm/compare/v0.4.1...v0.4.2) (2026-09-09)


### Features

* name the cause when a universal or mixed-mode build cannot record bitcode ([#120](https://github.com/h1994st/rllvm/issues/120)) ([6ce7d03](https://github.com/h1994st/rllvm/commit/6ce7d032eae402a34fb374180e77c3a664919770))


### Bug Fixes

* record the bitcode path in both halves of a fat LTO object ([#117](https://github.com/h1994st/rllvm/issues/117)) ([704938c](https://github.com/h1994st/rllvm/commit/704938ca4e96ef12c1f42e9a7f4d9040ccf231b0))

## [0.4.1](https://github.com/h1994st/rllvm/compare/v0.4.0...v0.4.1) (2026-09-08)


### Bug Fixes

* pass oversized llvm-link and llvm-ar invocations through a response file ([#113](https://github.com/h1994st/rllvm/issues/113)) ([5027fc4](https://github.com/h1994st/rllvm/commit/5027fc419191a57604e9385cb65ff0c2c2b11cf7)), closes [#112](https://github.com/h1994st/rllvm/issues/112)

## [0.4.0](https://github.com/h1994st/rllvm/compare/v0.3.0...v0.4.0) (2026-09-06)


### ⚠ BREAKING CHANGES

* `rllvm::cache::compute_cache_key` is replaced by `manifest_key` and `content_key`. Library API only -- wrapper behaviour is unchanged for anyone not opting into the cache, which is off by default.

### Bug Fixes

* keep dependency files describing the user's object ([#108](https://github.com/h1994st/rllvm/issues/108)) ([230c2b5](https://github.com/h1994st/rllvm/commit/230c2b518e61d0900acd4ebb5fd0fbf78f1334ed))
* key the bitcode cache on the compilation, not the source file ([#111](https://github.com/h1994st/rllvm/issues/111)) ([171d1e9](https://github.com/h1994st/rllvm/commit/171d1e9fec95e058fa1196dfd5eebcf232da192d))

## [0.3.0](https://github.com/h1994st/rllvm/compare/v0.2.0...v0.3.0) (2026-09-05)


### ⚠ BREAKING CHANGES

* `lto_mode` defaults to `marker`, so `-flto` builds now generate bitcode instead of skipping it, and `-flto` on COFF or WASM targets is now an error directing to `lto_mode = "skip"` where it previously linked with a warning.

### Features

* extract whole-program bitcode from LTO builds ([#99](https://github.com/h1994st/rllvm/issues/99)) ([31d1dfc](https://github.com/h1994st/rllvm/commit/31d1dfc41a91d99769041ed7c889d9abbb09dc87))


### Bug Fixes

* report the LTO bitcode skip where a build can see it ([#97](https://github.com/h1994st/rllvm/issues/97)) ([7a64931](https://github.com/h1994st/rllvm/commit/7a64931b29c65c0da6305ef1ecca1ca9c53c6cec))

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
