# Changelog

## [0.9.1](https://github.com/h1994st/rllvm/compare/rllvm-query-v0.9.0...rllvm-query-v0.9.1) (2026-10-20)


### Bug Fixes

* explain an unmatched or inlined call-site line ([#348](https://github.com/h1994st/rllvm/issues/348)) ([cd172ad](https://github.com/h1994st/rllvm/commit/cd172ad35e8f318895471e6fae211eaa1f70291f))

## [0.9.0](https://github.com/h1994st/rllvm/compare/rllvm-query-v0.8.0...rllvm-query-v0.9.0) (2026-10-19)


### ⚠ BREAKING CHANGES

* surface annotated flows on reach and slice paths ([#329](https://github.com/h1994st/rllvm/issues/329))
* record annotate verdicts in a version-2 overlay ([#328](https://github.com/h1994st/rllvm/issues/328))
* add the guards query for value guards and clamps ([#327](https://github.com/h1994st/rllvm/issues/327))
* an MCP query answer no longer carries `analysis.modules`; the aggregate counts stay, and `load_catalog` and `list_catalogs` still report the roster.

### Features

* add the guards query for value guards and clamps ([#327](https://github.com/h1994st/rllvm/issues/327)) ([f2a41de](https://github.com/h1994st/rllvm/commit/f2a41deb40611453928eb72c15df1f4bc035f2b7))
* group and record indirect calls by slot ([#322](https://github.com/h1994st/rllvm/issues/322)) ([c57dd05](https://github.com/h1994st/rllvm/commit/c57dd05253a182ed275d5c122241f28a3648a813))
* omit the per-module roster from MCP query answers ([#317](https://github.com/h1994st/rllvm/issues/317)) ([fc35b0f](https://github.com/h1994st/rllvm/commit/fc35b0fe033e2f16370ca65adb9b5654c7c04f7b))
* record annotate verdicts in a version-2 overlay ([#328](https://github.com/h1994st/rllvm/issues/328)) ([0aef9e3](https://github.com/h1994st/rllvm/commit/0aef9e3d42d1cde602ccde2d52952d900d7c057c))
* surface annotated flows on reach and slice paths ([#329](https://github.com/h1994st/rllvm/issues/329)) ([93c0586](https://github.com/h1994st/rllvm/commit/93c0586050f44025e2347de13fa72c9c15cbc2e2))


### Bug Fixes

* match file:line sites by file, not spelling ([#337](https://github.com/h1994st/rllvm/issues/337)) ([dc2b825](https://github.com/h1994st/rllvm/commit/dc2b82547c8ddf6bdaa420563bd97f684eb64c2a))
* name where each reach call step leads ([#335](https://github.com/h1994st/rllvm/issues/335)) ([ba0f812](https://github.com/h1994st/rllvm/commit/ba0f812a5967c4c5b39c726d2ef2c3b2bf5770f1))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * rllvm-core bumped from 0.7.0 to 0.7.1

## [0.8.0](https://github.com/h1994st/rllvm/compare/rllvm-query-v0.7.2...rllvm-query-v0.8.0) (2026-10-08)


### ⚠ BREAKING CHANGES

* add llvm_bindir for LLVM tools the config omits ([#315](https://github.com/h1994st/rllvm/issues/315))

### Features

* add call-graph overlay store ([#302](https://github.com/h1994st/rllvm/issues/302)) ([70af290](https://github.com/h1994st/rllvm/commit/70af29028d995b8cda55f928e287a33d3aecb363))
* add llvm_bindir for LLVM tools the config omits ([#315](https://github.com/h1994st/rllvm/issues/315)) ([97261e6](https://github.com/h1994st/rllvm/commit/97261e6bd04c9a4ebead6d16578a16ca1dd335f2))
* add resolution-candidates query ([#301](https://github.com/h1994st/rllvm/issues/301)) ([e76a877](https://github.com/h1994st/rllvm/commit/e76a877cdb6be851c5cecdaee01cd992c17dff5f))
* add slice query with module extraction ([#305](https://github.com/h1994st/rllvm/issues/305)) ([27ae0ba](https://github.com/h1994st/rllvm/commit/27ae0ba63f1117d35e22a40e42f17289d8db83c4))
* record the fields indirect calls dispatch through ([#300](https://github.com/h1994st/rllvm/issues/300)) ([c0d62a2](https://github.com/h1994st/rllvm/commit/c0d62a2ff71d953091f44b2fcaa2bc293f5dd713))
* serve call-graph overlay tools over MCP ([#304](https://github.com/h1994st/rllvm/issues/304)) ([1f5cb22](https://github.com/h1994st/rllvm/commit/1f5cb22dac7fcc2ad1ac064d0aac6fd7b9470b33))
* walk overlay edges in reach and closure ([#303](https://github.com/h1994st/rllvm/issues/303)) ([4b16837](https://github.com/h1994st/rllvm/commit/4b16837c405ec64c183ec06bb296ada9658c08bb))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * rllvm-core bumped from 0.6.3 to 0.7.0

## [0.7.2](https://github.com/h1994st/rllvm/compare/rllvm-query-v0.7.1...rllvm-query-v0.7.2) (2026-10-05)


### Bug Fixes

* use spec-valid cacheScope in modern MCP results ([#294](https://github.com/h1994st/rllvm/issues/294)) ([e6a7aec](https://github.com/h1994st/rllvm/commit/e6a7aec3f0500d8e9ded7b2330986b69bf5bfb90))

## [0.7.1](https://github.com/h1994st/rllvm/compare/rllvm-query-v0.7.0...rllvm-query-v0.7.1) (2026-10-04)


### Features

* add rllvm-query cache to inspect and prune ([#282](https://github.com/h1994st/rllvm/issues/282)) ([f636480](https://github.com/h1994st/rllvm/commit/f63648016ab729b72ab3263f3b89efb95b4448f9))
* add the ffi-exports query ([#262](https://github.com/h1994st/rllvm/issues/262)) ([4aff714](https://github.com/h1994st/rllvm/commit/4aff7148d747c501afaaf520bc5b22fef72a2e54))
* answer queries piped on stdin from one load ([#277](https://github.com/h1994st/rllvm/issues/277)) ([59e1884](https://github.com/h1994st/rllvm/commit/59e18844f9c96d2f773ed7ef02ee4693b710e19c))
* cache extracted facts per module ([#281](https://github.com/h1994st/rllvm/issues/281)) ([db0441d](https://github.com/h1994st/rllvm/commit/db0441dab64faec1afcb8f9235bd9227723ac5fb))


### Bug Fixes

* blame no function for a global's missing location ([#270](https://github.com/h1994st/rllvm/issues/270)) ([ed1f5e5](https://github.com/h1994st/rllvm/commit/ed1f5e5acce5f03ccfea593bc49e9e775c83f01a))
* count only signature matches as heuristic candidates ([#267](https://github.com/h1994st/rllvm/issues/267)) ([b70cf5f](https://github.com/h1994st/rllvm/commit/b70cf5f456fb241a11e88914c00794d8f79d2daf))
* exit quietly when a reader closes stdout early ([#269](https://github.com/h1994st/rllvm/issues/269)) ([9570486](https://github.com/h1994st/rllvm/commit/9570486ac6c72f65322cfa3d7c0fd4b17c7b9a36))
* name the global whose initializer takes an address ([#268](https://github.com/h1994st/rllvm/issues/268)) ([56cfc2e](https://github.com/h1994st/rllvm/commit/56cfc2ea5bb2026ffeea24a0f03b3a7298a9ba0f))
* remember facts cache usage between loads ([#286](https://github.com/h1994st/rllvm/issues/286)) ([b34e564](https://github.com/h1994st/rllvm/commit/b34e56403faefe245a08314b7c15b365f5f3da02))
* tidy the facts cache's loose ends ([#285](https://github.com/h1994st/rllvm/issues/285)) ([ab41946](https://github.com/h1994st/rllvm/commit/ab41946e3ca673fe8987aa31ab175f4715675304))
* treat LLVM aliases as the functions they alias ([#288](https://github.com/h1994st/rllvm/issues/288)) ([8e80ec4](https://github.com/h1994st/rllvm/commit/8e80ec4c29dac9f8e26ae346ad0c9afab1a48827))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * rllvm-core bumped from 0.6.2 to 0.6.3

## [0.7.0](https://github.com/h1994st/rllvm/compare/rllvm-query-v0.6.2...rllvm-query-v0.7.0) (2026-09-24)


### ⚠ BREAKING CHANGES

* print rllvm-query answers as text by default ([#248](https://github.com/h1994st/rllvm/issues/248))

### Features

* add --version to rllvm-query ([#251](https://github.com/h1994st/rllvm/issues/251)) ([6e4daa7](https://github.com/h1994st/rllvm/commit/6e4daa70b07d3485290a717e68c4b5fd5246e242))
* print rllvm-query answers as text by default ([#248](https://github.com/h1994st/rllvm/issues/248)) ([3cbd678](https://github.com/h1994st/rllvm/commit/3cbd678bdad724dbb6c556bd06bef18331a23204))


### Bug Fixes

* use US spelling of analyze throughout ([#257](https://github.com/h1994st/rllvm/issues/257)) ([88883c4](https://github.com/h1994st/rllvm/commit/88883c432cf8b3a5df6f7e1e8ac697677418f521))

## [0.6.2](https://github.com/h1994st/rllvm/compare/rllvm-query-v0.6.1...rllvm-query-v0.6.2) (2026-09-22)


### Bug Fixes

* demangle Rust v0 symbols in query answers ([#238](https://github.com/h1994st/rllvm/issues/238)) ([0d7cda0](https://github.com/h1994st/rllvm/commit/0d7cda0b30d816689d64b7ee515f7f037d326fda))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * rllvm-core bumped from 0.6.1 to 0.6.2

## [0.6.1](https://github.com/h1994st/rllvm/compare/rllvm-query-v0.6.0...rllvm-query-v0.6.1) (2026-09-20)


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * rllvm-core bumped from 0.6.0 to 0.6.1

## [0.6.0](https://github.com/h1994st/rllvm/compare/rllvm-query-v0.5.1...rllvm-query-v0.6.0) (2026-09-19)


### ⚠ BREAKING CHANGES

* give rllvm-query its own crate ([#224](https://github.com/h1994st/rllvm/issues/224))

### Code Refactoring

* give rllvm-query its own crate ([#224](https://github.com/h1994st/rllvm/issues/224)) ([dc9ba2b](https://github.com/h1994st/rllvm/commit/dc9ba2ba2a5cf140aaa0cefbb3a9386875bebfde))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * rllvm-core bumped from 0.5.1 to 0.6.0
