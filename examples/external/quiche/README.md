# quiche + rllvm

[quiche](https://github.com/cloudflare/quiche) is Cloudflare's QUIC and HTTP/3
library: a Cargo workspace that also builds BoringSSL through a build script.

No `check.sh` — see [external/](../README.md).
[`reproduce.sh`](reproduce.sh) runs every flow below on a macOS host and prints
the numbers each section records.

## Build and extract

```bash
git clone --recursive https://github.com/cloudflare/quiche && cd quiche
git checkout 4d23d859
git submodule update --init --recursive

RUSTC_WRAPPER=rllvm-rustc cargo build --release -p quiche

rllvm-get-bc target/release/libquiche.rlib -o quiche.bc
rllvm-info quiche.bc
```

## What ends up captured

`RUSTC_WRAPPER` captures what Cargo compiles. BoringSSL is not that: a build
script compiles it through CMake, so add `CC=rllvm-cc CXX=rllvm-cxx` to capture
it too. Run `cargo clean` first: Cargo does not rebuild BoringSSL when only the
compilers change, and the earlier build's objects carry no bitcode.

```bash
cargo clean
CC=rllvm-cc CXX=rllvm-cxx RUSTC_WRAPPER=rllvm-rustc cargo build --release -p quiche
```

| Build | Extract from | Functions |
|---|---|---|
| `RUSTC_WRAPPER=rllvm-rustc` | `target/release/libquiche.rlib` | 557 |
| `RUSTC_WRAPPER=rllvm-rustc` | `target/release/libquiche.a` | 1697 |
| plus `CC` and `CXX` | `target/release/libquiche.a` | 7376 |

The `.rlib` is the `quiche` crate alone; the `.a` adds the dependency crates
linked into it. Without `CC` and `CXX`, the crypto names in there belong to the
`boring` bindings crate — BoringSSL's own `X509`, `BIO` and `bssl::` code only
appears in the third row.

## Queries across the FFI boundary

With BoringSSL captured, a call path can be followed out of Rust and into it:

```bash
rllvm-get-bc target/release/libquiche.a --output-dir release-cat
rllvm-query --catalog release-cat/catalog.json callers SSL_do_handshake
```

```text
SSL_accept
    boringssl/ssl/ssl_lib.cc:770  direct    SSL_do_handshake
SSL_connect
    boringssl/ssl/ssl_lib.cc:761  direct    SSL_do_handshake
SSL_write
    boringssl/ssl/ssl_lib.cc:968  direct    SSL_do_handshake
ssl_read_impl(ssl_st*)
    boringssl/ssl/ssl_lib.cc:868  direct    SSL_do_handshake
<quiche::tls::Handshake>::do_handshake
    quiche/src/tls/mod.rs:556  direct    SSL_do_handshake

note: 1230 indirect call site(s), 15 with an LLVM target bound
```

The Rust caller and the C++ callers are reported alike, from source. `reach`
follows the path further, through the boundary and into BoringSSL's internals:

```bash
rllvm-query --catalog release-cat/catalog.json reach \
  '<quiche::tls::Handshake>::do_handshake' ssl_send_alert
```

```text
call              <quiche::tls::Handshake>::do_handshake
binding           SSL_do_handshake  (unique, 1 candidate(s))
call              SSL_do_handshake
binding           bssl::ssl_run_handshake(bssl::SSL_HANDSHAKE*, bool*)  (unique, 1 candidate(s))
call              bssl::ssl_run_handshake(bssl::SSL_HANDSHAKE*, bool*)
binding           bssl::ssl_handle_open_record(ssl_st*, bool*, bssl::ssl_open_record_t, unsigned long, unsigned char)  (unique, 1 candidate(s))
call              bssl::ssl_handle_open_record(ssl_st*, bool*, bssl::ssl_open_record_t, unsigned long, unsigned char)
binding           bssl::ssl_send_alert(ssl_st*, int, int)  (unique, 1 candidate(s))

note: 'ssl_send_alert' matched loosely, gathering 1 symbol(s)
note: 1230 indirect call site(s), 15 with an LLVM target bound
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
git submodule update --init --recursive
curl -fsSLO https://raw.githubusercontent.com/h1994st/rllvm/main/examples/external/quiche/cid_logger.c

RUSTC_WRAPPER=rllvm-rustc CC=rllvm-cc cargo build -p quiche --features ffi

MACOSX_DEPLOYMENT_TARGET=$(sw_vers -productVersion) \
  rllvm-cc -g -Iquiche/include cid_logger.c \
           target/debug/libquiche.a -o cid_logger

rllvm-get-bc cid_logger --output-dir cat
```

The deployment target keeps the link quiet: the build script compiles
BoringSSL against the host SDK, so without it the link reports a few hundred
"built for newer macOS version" warnings. Plain `clang` does the same.

`callees` on the entry point shows the defect: the value is cloned, a pointer
into it is taken, and it is dropped before returning to C.

```bash
rllvm-query --catalog cat/catalog.json callees quiche_connection_id_iter_next
```

```text
quiche/src/ffi.rs:1157  direct    <quiche::ffi::ConnectionIdIter as core::iter::traits::iterator::Iterator>::next
quiche/src/ffi.rs:1154  direct    core::panicking::panic_cannot_unwind
quiche/src/ffi.rs:1157  intrinsic llvm.memcpy.p0.p0.i64
quiche/src/ffi.rs:1158  direct    <quiche::packet::ConnectionId as core::convert::AsRef<[u8]>>::as_ref
quiche/src/ffi.rs:1162  direct    core::ptr::drop_glue::<quiche::packet::ConnectionId>
quiche/src/ffi.rs:1162  direct    core::ptr::drop_glue::<quiche::packet::ConnectionId>
quiche/src/ffi.rs:1154  direct    core::panicking::panic_cannot_unwind
quiche/src/ffi.rs:1154  direct    core::panicking::panic_in_cleanup
```

The same query on 0.29.2 has no `drop_glue` line, because the fix indexes the
iterator instead of cloning out of it. `defs` and `callers` answer whether the
code is present and whether anything reaches it, which `cargo audit` cannot
distinguish:

| Build | `defs` | `callers` |
|---|---|---|
| default features | 0 | n/a |
| `--features ffi`, quiche's own `client.c` | `ffi.rs:1154` | 0 |
| `--features ffi`, a C caller that iterates CIDs | `ffi.rs:1154` | `cid_logger.c:20` |

Row two is linked but unreachable: `nm` shows
`_quiche_connection_id_iter_next` in the binary as a defined symbol while
`callers` returns zero.

## Scanning the rest of the C API

The defect is a shape, not a one-off: an entry point takes a pointer into a
value and drops that value before returning. Both halves appear in `callees`,
so the surface can be swept.

```bash
rllvm-query --catalog cat/catalog.json ffi-exports \
  | awk 'NF && $1 != "note:" { print $NF }' > surface.txt   # 169 names

for fn in $(cat surface.txt); do
  out=$(rllvm-query --catalog cat/catalog.json callees "$fn")
  if grep -q drop_glue <<<"$out" &&
     grep -Eq '::(as_ref|as_ptr|as_slice)$' <<<"$out"; then
    echo "$fn"
  fi
done
```

Keeping the entry points whose callees hold both `drop_glue::<T>` and something
taking a pointer into `T` leaves 7 of 169, including both functions the
advisory names.

`ffi-exports` lists the definitions quiche's Rust modules export under an
unmangled name, which is what `#[no_mangle]` produces: no `nm`, no `quiche_`
prefix, and no Mach-O underscore to strip.

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

`externals` lists `aes_hw_encrypt` and `sha256_block_data_order_hw` as
unbound: BoringSSL implements its hardware AES and SHA-256 in assembly, so there
is no bitcode to capture, and a path through them ends at the boundary rather
than being invented.

The modules also disagree about the target triple — 282 from clang say
`arm64-apple-macosx26.5.0`, 16 from rustc say `arm64-apple-macosx11.0.0`,
being rustc's deployment target. They merge and analyze anyway; all 298 carry
debug info and none failed to parse.

## Validated against

quiche `4d23d859` for the capture sections, then `0.29.1` and `0.29.2` for the
advisory triage, in that order in one checkout, on arm64 macOS. Built with rustc
1.98.0 (LLVM 22.1.8) and extracted with LLVM 23.1.1: a reader handles its own
major and older, so the newer tools read what rustc emitted.

quiche commits no `Cargo.lock`, so dependency versions resolve when you build,
and a later `boring` release moves BoringSSL's counts and line numbers.
