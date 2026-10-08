# Changelog

## [0.2.0](https://github.com/h1994st/rllvm/compare/rllvm-plugin-v0.1.1...rllvm-plugin-v0.2.0) (2026-10-08)


### ⚠ BREAKING CHANGES

* add llvm_bindir for LLVM tools the config omits ([#315](https://github.com/h1994st/rllvm/issues/315))

### Features

* add call-graph completion skill and command ([#306](https://github.com/h1994st/rllvm/issues/306)) ([9cb37da](https://github.com/h1994st/rllvm/commit/9cb37da05c5542b04ff7879506bc3c1f0e01fa7a))
* add call-graph overlay store ([#302](https://github.com/h1994st/rllvm/issues/302)) ([70af290](https://github.com/h1994st/rllvm/commit/70af29028d995b8cda55f928e287a33d3aecb363))
* add llvm_bindir for LLVM tools the config omits ([#315](https://github.com/h1994st/rllvm/issues/315)) ([97261e6](https://github.com/h1994st/rllvm/commit/97261e6bd04c9a4ebead6d16578a16ca1dd335f2))
* add overlay view to the rllvm plugin ([#307](https://github.com/h1994st/rllvm/issues/307)) ([be8b282](https://github.com/h1994st/rllvm/commit/be8b28271ddca4cdf368f5e1046da352d8c5de87))
* add resolution-candidates query ([#301](https://github.com/h1994st/rllvm/issues/301)) ([e76a877](https://github.com/h1994st/rllvm/commit/e76a877cdb6be851c5cecdaee01cd992c17dff5f))
* add slice query with module extraction ([#305](https://github.com/h1994st/rllvm/issues/305)) ([27ae0ba](https://github.com/h1994st/rllvm/commit/27ae0ba63f1117d35e22a40e42f17289d8db83c4))
* record the fields indirect calls dispatch through ([#300](https://github.com/h1994st/rllvm/issues/300)) ([c0d62a2](https://github.com/h1994st/rllvm/commit/c0d62a2ff71d953091f44b2fcaa2bc293f5dd713))
* serve call-graph overlay tools over MCP ([#304](https://github.com/h1994st/rllvm/issues/304)) ([1f5cb22](https://github.com/h1994st/rllvm/commit/1f5cb22dac7fcc2ad1ac064d0aac6fd7b9470b33))
* walk overlay edges in reach and closure ([#303](https://github.com/h1994st/rllvm/issues/303)) ([4b16837](https://github.com/h1994st/rllvm/commit/4b16837c405ec64c183ec06bb296ada9658c08bb))


### Bug Fixes

* patch only members compiled from the crate bitcode ([#311](https://github.com/h1994st/rllvm/issues/311)) ([ef0ab37](https://github.com/h1994st/rllvm/commit/ef0ab375ea04a0e3e4094ed23bc41044181e9821))

## [0.1.1](https://github.com/h1994st/rllvm/compare/rllvm-plugin-v0.1.0...rllvm-plugin-v0.1.1) (2026-10-04)


### Features

* add rllvm-query cache to inspect and prune ([#282](https://github.com/h1994st/rllvm/issues/282)) ([f636480](https://github.com/h1994st/rllvm/commit/f63648016ab729b72ab3263f3b89efb95b4448f9))
* add the ffi-exports query ([#262](https://github.com/h1994st/rllvm/issues/262)) ([4aff714](https://github.com/h1994st/rllvm/commit/4aff7148d747c501afaaf520bc5b22fef72a2e54))
* answer queries piped on stdin from one load ([#277](https://github.com/h1994st/rllvm/issues/277)) ([59e1884](https://github.com/h1994st/rllvm/commit/59e18844f9c96d2f773ed7ef02ee4693b710e19c))
* cache extracted facts per module ([#281](https://github.com/h1994st/rllvm/issues/281)) ([db0441d](https://github.com/h1994st/rllvm/commit/db0441dab64faec1afcb8f9235bd9227723ac5fb))
* warn in Claude Code when the facts cache is large ([#284](https://github.com/h1994st/rllvm/issues/284)) ([d250f31](https://github.com/h1994st/rllvm/commit/d250f310421cf6da3391591111c44376fa930f3b))


### Bug Fixes

* extract static archives of LTO objects ([#287](https://github.com/h1994st/rllvm/issues/287)) ([9521891](https://github.com/h1994st/rllvm/commit/9521891b3d2884ff7b689e138da6b63144252bad))
* tidy the facts cache's loose ends ([#285](https://github.com/h1994st/rllvm/issues/285)) ([ab41946](https://github.com/h1994st/rllvm/commit/ab41946e3ca673fe8987aa31ab175f4715675304))

## 0.1.0 (2026-09-24)


### Features

* ship rllvm as a Claude Code plugin ([#252](https://github.com/h1994st/rllvm/issues/252)) ([99d651a](https://github.com/h1994st/rllvm/commit/99d651aaa6e6b506bb6771ce6ff01afe415d3c9a))


### Bug Fixes

* use US spelling of analyze throughout ([#257](https://github.com/h1994st/rllvm/issues/257)) ([88883c4](https://github.com/h1994st/rllvm/commit/88883c432cf8b3a5df6f7e1e8ac697677418f521))
