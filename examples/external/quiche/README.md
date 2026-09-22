# quiche + rllvm

[quiche](https://github.com/cloudflare/quiche) is Cloudflare's QUIC and HTTP/3
library: a Cargo workspace that also builds BoringSSL through a build script.

No `check.sh` — see [external/](../README.md).

## Build and extract

```bash
git clone --recursive https://github.com/cloudflare/quiche && cd quiche

RUSTC_WRAPPER=rllvm-rustc cargo build --release -p quiche

rllvm-get-bc target/release/libquiche.rlib -o quiche.bc
rllvm-info quiche.bc
```

## What ends up captured

`RUSTC_WRAPPER` captures what Cargo compiles. BoringSSL is not that: a build
script compiles it through a C compiler, so add `CC=rllvm-cc` to capture it
too.

| Build | Extract from | Functions |
|---|---|---|
| `RUSTC_WRAPPER=rllvm-rustc` | `target/release/libquiche.rlib` | 557 |
| `RUSTC_WRAPPER=rllvm-rustc` | `target/release/libquiche.a` | 1697 |
| plus `CC=rllvm-cc` | `target/release/libquiche.a` | 7376 |

The `.rlib` is the `quiche` crate alone; the `.a` adds the dependency crates
linked into it. Without `CC`, the crypto names in there belong to the `boring`
bindings crate — BoringSSL's own `X509`, `BIO` and `bssl::` code only appears
in the third row.

## Queries across the FFI boundary

With `CC=rllvm-cc` the C side is captured too, so a call path can be followed
out of Rust and into BoringSSL:

```bash
rllvm-get-bc target/release/libquiche.a --output-dir cat
rllvm-query --catalog cat/catalog.json callers SSL_do_handshake
```

```text
quiche::tls::Handshake::do_handshake
    quiche/src/tls/mod.rs:556  direct    SSL_do_handshake
SSL_accept
    boringssl/ssl/ssl_lib.cc:770  direct    SSL_do_handshake
```

The Rust call site and the C++ definition are both reported from source.
`reach` follows it further, through the boundary and into BoringSSL's
internals:

```bash
rllvm-query --catalog cat/catalog.json reach \
  _RNvMs2_NtCs8f0ESrtUyjS_6quiche3tlsNtB5_9Handshake12do_handshake ssl_send_alert
```

```text
<quiche::tls::Handshake>::do_handshake     Rust
SSL_do_handshake                           C ABI
bssl::ssl_run_handshake(...)               C++
bssl::ssl_handle_open_record(...)          C++
bssl::ssl_send_alert(ssl_st*, int, int)    C++
```

## Auditing the C API against a real advisory

quiche's C API is behind the `ffi` feature, off by default. CVE-2026-11941
(GHSA-mh64-ph39-mrc9, fixed in 0.29.2) is a use-after-free in two of its
functions, and the advisory limits it to applications that call them. That is a
whole-program question spanning both languages.

quiche's own C examples never call the iterator, so this directory ships
[`cid_logger.c`](cid_logger.c), which walks a connection's source IDs through
`quiche_conn_source_ids` and `quiche_connection_id_iter_next`. Build the
vulnerable version with the feature on, link against it, and catalog the
result:

```bash
git checkout 0.29.1
RUSTC_WRAPPER=rllvm-rustc CC=rllvm-cc cargo build -p quiche --features ffi

MACOSX_DEPLOYMENT_TARGET=$(sw_vers -productVersion) \
  rllvm-cc -g -Iquiche/include cid_logger.c \
           target/debug/libquiche.a -o cid_logger

rllvm-get-bc cid_logger --output-dir cat
```

The deployment target keeps the link quiet: the build script compiles
BoringSSL against the host SDK, so without it the link reports a few hundred
"built for newer macOS version" warnings. Plain `clang` does the same.

Answers are JSON, so `jq` does the formatting. `callees` on the entry point
shows the defect: the value is cloned, a pointer into it is taken, and it is
dropped before returning to C.

```bash
rllvm-query --catalog cat/catalog.json callees \
    quiche_connection_id_iter_next | jq -r '
  .symbols as $s | .results[]
  | select(.target.kind == "direct")
  | ($s[.target.callee.symbol] // .target.callee.symbol) as $n
  | select($n | test("panic") | not)
  | .location as $l
  | "\($l.file|split("/")|last):\($l.line)  \($n)"'
```

```text
ffi.rs:1157  <quiche::ffi::ConnectionIdIter as ...Iterator>::next
ffi.rs:1158  <quiche::packet::ConnectionId as ...AsRef<[u8]>>::as_ref
ffi.rs:1162  core::ptr::drop_glue::<quiche::packet::ConnectionId>
```

The same query on 0.29.2 has no `drop_glue` line, because the fix indexes the
iterator instead of cloning out of it. `defs` and `callers` answer whether the
code is present and whether anything reaches it, which `cargo audit` cannot
distinguish:

| Build | `defs` | `callers` |
|---|---|---|
| default features | 0 | n/a |
| `--features ffi`, quiche's own `client.c` | `ffi.rs:1154` | 0 |
| `--features ffi`, a C caller that iterates CIDs | `ffi.rs:1154` | `cid_logger.c:19` |

Row two is linked but unreachable: `nm` shows
`_quiche_connection_id_iter_next` in the binary as a defined symbol while
`callers` returns zero.

## Scanning the rest of the C API

The defect is a shape, not a one-off: an entry point takes a pointer into a
value and drops that value before returning. Both halves appear in `callees`,
so the surface can be swept.

```bash
llvm-nm target/debug/libquiche.a 2>/dev/null \
  | awk '$2=="T" && $3 ~ /^_quiche_/ { print substr($3, 2) }' \
  | sort -u > surface.txt          # 169 names, without the Mach-O underscore

for fn in $(cat surface.txt); do
  rllvm-query --catalog cat/catalog.json callees "$fn" \
    | jq -r --arg fn "$fn" '
      .symbols as $s
      | [ .results[] | (.target.callee.symbol // "") as $y
                     | ($s[$y] // $y) ] as $n
      | select(($n | any(test("drop_glue")))
           and ($n | any(test("::(as_ref|as_ptr|as_slice)$"))))
      | $fn'
done
```

Keeping the entry points whose callees hold both `drop_glue::<T>` and something
taking a pointer into `T` leaves 7 of 169, including both functions the
advisory names.

Use LLVM's `llvm-nm`, not the system one: rustc ships `std` and `core` into the
archive with embedded bitcode, and a reader built on an older LLVM fails with
`Unknown attribute kind`. Its stderr carries harmless "no symbols" notes for
empty members. The `substr` drops Mach-O's leading underscore, which the queries
do not want; an ELF build matches `/^quiche_/` and keeps the whole name. Issue #243 tracks reporting this surface from the catalog instead of
shelling out at all.

The other five are safe, and show where call-graph answers stop.
`quiche_conn_source_id` drops a `ConnectionId`, but `ConnectionId` is Cow-like
(`enum { Vec(Vec<u8>), Ref(&'a [u8]) }`) and the dropped value is the borrowed
variant, so nothing is freed. `quiche_h3_take_last_priority_update` hands its
pointer to a C callback before freeing, so its safety depends on code the Rust
side cannot see. Both need data-flow analysis (#149) rather than a call graph.
Two more are noise from matching the method name `as_ref`, which also catches
`Option::as_ref`.

[rllvm: From Capture to Query](https://shengtuo.me/blog/from-capture-to-query/)
walks through this triage at length.

## What the answers do not cover

`EVP_AEAD_CTX_seal` and `SHA256_Update` resolve to nothing: BoringSSL
implements them in assembly, so there is no bitcode to capture and `reach`
reports the absence rather than inventing a path.

The modules also disagree about the target triple — 282 from clang say
`arm64-apple-macosx26.5.0`, 16 from rustc say `arm64-apple-macosx11.0.0`,
being rustc's deployment target. They merge and analyse anyway; all 298 carry
debug info and none failed to parse.

## Validated against

quiche `4d23d859` for the capture sections, and `0.29.1` / `0.29.2` for the
advisory triage, on arm64 macOS, built with rustc 1.98.0 (LLVM 22.1.8) and
extracted with LLVM 23.1.1. A reader handles its own major and older, so the
newer tools read what rustc emitted.
