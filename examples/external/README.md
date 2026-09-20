# External Examples

Flows whose subject is somebody else's project: how to point rllvm at a
third-party build and hand the result to the tool that consumes it.

These ship a `README.md` and no `check.sh`. Verifying one would mean building
the third-party project from source, which is minutes of work and a toolchain
this repository does not control, so `cargo test --test examples` skips this
directory entirely rather than reporting a prerequisite missing on every
machine.

That also means nothing here is checked automatically. Each README records the
versions it was validated against; re-run it by hand when those move.
