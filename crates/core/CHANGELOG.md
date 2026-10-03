# Changelog

## [0.6.3](https://github.com/h1994st/rllvm/compare/rllvm-core-v0.6.2...rllvm-core-v0.6.3) (2026-10-03)


### Features

* answer queries piped on stdin from one load ([#277](https://github.com/h1994st/rllvm/issues/277)) ([59e1884](https://github.com/h1994st/rllvm/commit/59e18844f9c96d2f773ed7ef02ee4693b710e19c))
* cache extracted facts per module ([#281](https://github.com/h1994st/rllvm/issues/281)) ([db0441d](https://github.com/h1994st/rllvm/commit/db0441dab64faec1afcb8f9235bd9227723ac5fb))


### Bug Fixes

* exit quietly when a reader closes stdout early ([#269](https://github.com/h1994st/rllvm/issues/269)) ([9570486](https://github.com/h1994st/rllvm/commit/9570486ac6c72f65322cfa3d7c0fd4b17c7b9a36))
* read the catalog in one call ([#275](https://github.com/h1994st/rllvm/issues/275)) ([1a4e910](https://github.com/h1994st/rllvm/commit/1a4e910ec542c71c50d7b54d6e42ba164ab7fbcc))

## [0.6.2](https://github.com/h1994st/rllvm/compare/rllvm-core-v0.6.1...rllvm-core-v0.6.2) (2026-09-22)


### Bug Fixes

* link a positional archive after the objects ([#241](https://github.com/h1994st/rllvm/issues/241)) ([dd39459](https://github.com/h1994st/rllvm/commit/dd39459bd2623a318fee5a914b5da05acdaf77e5))

## [0.6.1](https://github.com/h1994st/rllvm/compare/rllvm-core-v0.6.0...rllvm-core-v0.6.1) (2026-09-20)


### Bug Fixes

* carry --target into the relink ([#233](https://github.com/h1994st/rllvm/issues/233)) ([dca92b7](https://github.com/h1994st/rllvm/commit/dca92b76c66ddf5efbc5a5c015738358127912e6))

## [0.6.0](https://github.com/h1994st/rllvm/compare/rllvm-core-v0.5.1...rllvm-core-v0.6.0) (2026-09-19)


### ⚠ BREAKING CHANGES

* split the library into rllvm-core and the wrapper crate ([#217](https://github.com/h1994st/rllvm/issues/217))

### Code Refactoring

* split the library into rllvm-core and the wrapper crate ([#217](https://github.com/h1994st/rllvm/issues/217)) ([0768345](https://github.com/h1994st/rllvm/commit/076834520064e3be2c2fec07a48bb2143902bcb2))
