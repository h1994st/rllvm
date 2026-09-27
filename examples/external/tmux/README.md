# tmux + rllvm

[tmux](https://github.com/tmux/tmux) is a C program built with autotools: one
executable, event-driven, with commands and terminal escape sequences dispatched
through function-pointer tables.

No `check.sh` — see [external/](../README.md).
[`reproduce.sh`](reproduce.sh) runs every flow below on a macOS host and prints
the numbers under [Validated against](#validated-against).

## Build and extract

```bash
brew install autoconf automake pkg-config libevent ncurses utf8proc

git clone https://github.com/tmux/tmux && cd tmux
git checkout 94796f6b

sh autogen.sh
./configure CC=rllvm-cc --disable-jemalloc
make

rllvm-get-bc tmux -o tmux.bc
rllvm-info tmux.bc
```

On macOS, `configure` refuses to pick an allocator: `--disable-jemalloc` keeps
the system one, and `--enable-jemalloc` links a prebuilt jemalloc that
contributes no bitcode. It enables utf8proc when it finds the headers, and
otherwise asks for `--enable-utf8proc` or `--disable-utf8proc` too.

A build from a Git checkout compiles with `-O2 -g3`, so query answers carry
source lines without extra flags.

## Where a client's command goes

A client sends its command line to the server over a socket. `reach` follows
it to the parser:

```bash
rllvm-get-bc tmux --output-dir cat
rllvm-query --catalog cat/catalog.json reach server_client_dispatch cmd_parse_from_arguments
```

```text
call              server_client_dispatch
call              server_client_dispatch_command
binding           cmd_parse_from_arguments  (unique, 1 candidate(s))

note: 103 indirect call site(s), 2 with an LLVM target bound
```

Past the parser the trail ends. Each command is a `cmd_entry` whose `.exec`
names its implementation, and `cmdq_next` calls it through the pointer, so
`kill-server` has no caller:

```bash
rllvm-query --catalog cat/catalog.json callers cmd_kill_server_exec
rllvm-query --catalog cat/catalog.json at cmd-queue.c 625
```

```text
note: no call to the target was found in the selected scope
note: 103 indirect call site(s), 2 with an LLVM target bound
cmdq_next
    cmd-queue.c:625  indirect  i32 (ptr, ptr)  unresolved

note: 103 indirect call site(s), 2 with an LLVM target bound
```

`uses` finds where the address went instead, and `--heuristics` lists what
that call could reach by signature alone:

```bash
rllvm-query --catalog cat/catalog.json uses cmd_kill_server_exec
rllvm-query --catalog cat/catalog.json indirect-targets cmd-queue.c:625 --heuristics
```

```text
global_initializer  in cmd_start_server_entry  at <no location>
global_initializer  in cmd_kill_server_entry  at <no location>
cmd-queue.c:625  i32 (ptr, ptr)
    unresolved
    address-taken candidates: 93 of 668 address-taken function(s) match the signature

note: 103 indirect call site(s), 2 with an LLVM target bound
note: address-taken candidates are signature-matched, never call edges
```

`kill-server` and `start-server` share one implementation. Of the 93
candidates, 61 are `cmd_*_exec` functions; the rest are comparators and
callbacks such as `sort_session_cmp` and `cfg_done`, which have the same shape
once pointers are opaque. The candidates are opt-in and never become edges, so
`reach` still stops at this call.

## Where a pane's output goes

What a program in a pane writes reaches `input_parse_buffer`, which runs it
through a state machine. Each transition names its handler, so the parser for
`ESC [` sequences is reached only through a table:

```bash
rllvm-query --catalog cat/catalog.json reach input_parse_buffer input_csi_dispatch
rllvm-query --catalog cat/catalog.json at input.c 1013
```

```text
note: no path over resolved edges; not proof of unreachability
note: 103 indirect call site(s), 2 with an LLVM target bound
input_parse
    input.c:1013  indirect  i32 (ptr)  unresolved

note: 103 indirect call site(s), 2 with an LLVM target bound
```

Every escape sequence a pane prints is handled through that call, yet `reach`
finds no path: an empty answer is not proof, and the note says so. `uses`
shows where the handler is registered:

```bash
rllvm-query --catalog cat/catalog.json uses input_csi_dispatch
```

```text
global_initializer  in input_state_csi_parameter_table  at <no location>
global_initializer  in input_state_csi_intermediate_table  at <no location>
global_initializer  in input_state_csi_enter_table  at <no location>
```

## Validated against

tmux `94796f6b` on arm64 macOS with Homebrew Clang 23.1.2, libevent 2.1.12,
ncurses 6.5 and utf8proc 2.11.0. `tmux.bc` holds 159 modules and 2361
functions.
