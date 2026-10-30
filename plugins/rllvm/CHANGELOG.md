# Changelog

## [0.5.0](https://github.com/h1994st/rllvm/compare/rllvm-plugin-v0.4.0...rllvm-plugin-v0.5.0) (2026-10-30)


### ⚠ BREAKING CHANGES

* pick a shadowed local by its declaration line ([#361](https://github.com/h1994st/rllvm/issues/361))

### Features

* pick a shadowed local by its declaration line ([#361](https://github.com/h1994st/rllvm/issues/361)) ([a52784f](https://github.com/h1994st/rllvm/commit/a52784f588819e0785a021e728f6f681718b92fe))
* place an inlined read at its caller's line ([#366](https://github.com/h1994st/rllvm/issues/366)) ([ec311cc](https://github.com/h1994st/rllvm/commit/ec311ccbd67f291499e507aeffd1bb16150a58f1))
* read a trivial min or max helper as its clamp ([#367](https://github.com/h1994st/rllvm/issues/367)) ([9ba837f](https://github.com/h1994st/rllvm/commit/9ba837fe77d5bd3594537a8ee648870e063f366b))

## [0.4.0](https://github.com/h1994st/rllvm/compare/rllvm-plugin-v0.3.0...rllvm-plugin-v0.4.0) (2026-10-25)


### ⚠ BREAKING CHANGES

* report the predicate a compare holds at the use ([#357](https://github.com/h1994st/rllvm/issues/357))
* disambiguate a reused local by use-site ([#352](https://github.com/h1994st/rllvm/issues/352))
* carry verified, caveat and affine on guard sites ([#350](https://github.com/h1994st/rllvm/issues/350))

### Features

* carry verified, caveat and affine on guard sites ([#350](https://github.com/h1994st/rllvm/issues/350)) ([4b0f8d2](https://github.com/h1994st/rllvm/commit/4b0f8d26f4e37a6bcd715434467606d400086969))
* disambiguate a reused local by use-site ([#352](https://github.com/h1994st/rllvm/issues/352)) ([e042325](https://github.com/h1994st/rllvm/commit/e042325e6ba4cec91b2c74561d49a67169142206))
* explain which capture levels guards can trace ([#347](https://github.com/h1994st/rllvm/issues/347)) ([6688fd1](https://github.com/h1994st/rllvm/commit/6688fd152a1ee1d0bea2333db6b9c143264a2ee5))
* follow a guard through a single-store field load ([#351](https://github.com/h1994st/rllvm/issues/351)) ([0aedda4](https://github.com/h1994st/rllvm/commit/0aedda42fd37201e3c5e6259f9a5b9ec02b17ad3))
* prove a field load from its nearest store ([#354](https://github.com/h1994st/rllvm/issues/354)) ([3bac09b](https://github.com/h1994st/rllvm/commit/3bac09b8feea6183fef571774bd8da0da7cd2017))
* surface a downstream clamp's compare as a guard ([#355](https://github.com/h1994st/rllvm/issues/355)) ([c765cc3](https://github.com/h1994st/rllvm/commit/c765cc3caeb817a53d8a2bde490297e637a4ea6e))


### Bug Fixes

* report the predicate a compare holds at the use ([#357](https://github.com/h1994st/rllvm/issues/357)) ([8bce15f](https://github.com/h1994st/rllvm/commit/8bce15f82eaceb9c00d41fdb8c16ee426a7fb8d4))

## [0.3.0](https://github.com/h1994st/rllvm/compare/rllvm-plugin-v0.2.0...rllvm-plugin-v0.3.0) (2026-10-19)


### ⚠ BREAKING CHANGES

* refuse get-bc output over a module it reads ([#332](https://github.com/h1994st/rllvm/issues/332))
* surface annotated flows on reach and slice paths ([#329](https://github.com/h1994st/rllvm/issues/329))
* record annotate verdicts in a version-2 overlay ([#328](https://github.com/h1994st/rllvm/issues/328))
* add the guards query for value guards and clamps ([#327](https://github.com/h1994st/rllvm/issues/327))

### Features

* add the guards query for value guards and clamps ([#327](https://github.com/h1994st/rllvm/issues/327)) ([f2a41de](https://github.com/h1994st/rllvm/commit/f2a41deb40611453928eb72c15df1f4bc035f2b7))
* distinguish field and signature-only candidate groups ([#316](https://github.com/h1994st/rllvm/issues/316)) ([ddb6c84](https://github.com/h1994st/rllvm/commit/ddb6c84606850d10d06cb1693715454c788fde13))
* group and record indirect calls by slot ([#322](https://github.com/h1994st/rllvm/issues/322)) ([c57dd05](https://github.com/h1994st/rllvm/commit/c57dd05253a182ed275d5c122241f28a3648a813))
* record annotate verdicts in a version-2 overlay ([#328](https://github.com/h1994st/rllvm/issues/328)) ([0aef9e3](https://github.com/h1994st/rllvm/commit/0aef9e3d42d1cde602ccde2d52952d900d7c057c))
* surface annotated flows on reach and slice paths ([#329](https://github.com/h1994st/rllvm/issues/329)) ([93c0586](https://github.com/h1994st/rllvm/commit/93c0586050f44025e2347de13fa72c9c15cbc2e2))


### Bug Fixes

* match file:line sites by file, not spelling ([#337](https://github.com/h1994st/rllvm/issues/337)) ([dc2b825](https://github.com/h1994st/rllvm/commit/dc2b82547c8ddf6bdaa420563bd97f684eb64c2a))
* refuse get-bc output over a module it reads ([b11ad9e](https://github.com/h1994st/rllvm/commit/b11ad9e3cdf72319b9ca07953b6bf0fb8b27db9d))
* refuse get-bc output over a module it reads ([#332](https://github.com/h1994st/rllvm/issues/332)) ([b11ad9e](https://github.com/h1994st/rllvm/commit/b11ad9e3cdf72319b9ca07953b6bf0fb8b27db9d))

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
