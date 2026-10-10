# libxml2 + rllvm

[libxml2](https://gitlab.gnome.org/GNOME/libxml2) is a C library built with
autotools and libtool. It ships its own libFuzzer harnesses in `fuzz/`; this
builds the `xml` one through rllvm and asks where its indirect calls lead.

No `check.sh` — see [external/](../README.md).
[`reproduce.sh`](reproduce.sh) runs every flow below on a macOS host and prints
the numbers under [Validated against](#validated-against).

## Build and extract

```bash
brew install autoconf automake libtool

git clone --branch v2.15.4 --depth 1 https://github.com/GNOME/libxml2 && cd libxml2

./autogen.sh CC=rllvm-cc CXX=rllvm-cxx --disable-shared --without-python
make
make -C fuzz xml

rllvm-get-bc fuzz/xml -o xml.bc
rllvm-info xml.bc
```

The GitHub repository is GNOME's read-only mirror. `autogen.sh` generates
`configure` and runs it with the arguments given. `--disable-shared` links the
fuzzer against the static archive, so the extracted module holds the library
as well as the harness: 33 of the archive's 37 members, plus `xml.c` and
`fuzz.c`.

This is a build for analysis. The library is not compiled with
`-fsanitize=fuzzer-no-link`, so the fuzzer runs but sees no coverage inside
libxml2. Adding it to `CFLAGS`, as `fuzz/README.md` does, makes a working
fuzzer, but rllvm captures that instrumentation too: the bitcode then calls a
`__sanitizer_cov_*` hook at every edge.

## Where the indirect calls go

The fuzzer makes 3023 indirect calls, and LLVM bounds the targets of 3.
`resolution-candidates` groups the rest by the record field each call loads its
target from, or by signature when there is no field, and lists the functions
stored where that call reads:

```bash
rllvm-get-bc fuzz/xml --output-dir cat
rllvm-query --catalog cat/catalog.json resolution-candidates > candidates.txt
grep -E '^no field, (ptr \(i64\)|ptr \(ptr, i64\)|void \(ptr\)) ' candidates.txt
```

```text
no field, ptr (i64) — 539 sites, 2 candidates
no field, ptr (ptr, i64) — 178 sites, 2 candidates
no field, void (ptr) — 1993 sites, 12 candidates
```

Three groups hold 2710 of the 3020 unresolved calls. They are `xmlMalloc`,
`xmlRealloc` and `xmlFree`: libxml2 allocates through global function pointers
rather than calling `malloc` directly, so there is no field to group by. The
candidates for `xmlRealloc` show why:

```bash
grep -E '^  candidate (xmlFuzzRealloc|realloc) ' candidates.txt
```

```text
  candidate xmlFuzzRealloc assigned in xmlFuzzMemSetup fuzz.c:152
  candidate realloc assigned in xmlRealloc <no location>
```

`realloc` is the global's initializer, libxml2's default. `xmlFuzzRealloc` is
the fuzzer's replacement, installed through `xmlMemSetup` so it can inject
allocation failures. Both are candidates, not edges: which one a call reaches
depends on whether `xmlFuzzMemSetup` ran first, and a call graph cannot say.
The `void (ptr)` group is looser still, since it is matched by signature alone:
its 12 candidates include `free` and SAX callbacks such as
`xmlSAX2StartDocument`.

## Validated against

libxml2 `v2.15.4` (`9649899`) on arm64 macOS with Homebrew Clang 23.1.2,
autoconf 2.73, automake 1.18.1 and libtool 2.6.2. `xml.bc` holds 35 modules
and 2147 functions; `resolution-candidates` reports 3020 unresolved calls in 77
groups.
