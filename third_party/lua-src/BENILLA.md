# benilla's `lua-src` fork — what differs from upstream, and how to check

Upstream: [`mlua-rs/lua-src-rs`](https://github.com/mlua-rs/lua-src-rs), MIT, version `550.0.0` —
the version mlua 0.11 resolves to. Wired in through `[patch.crates-io]` in the workspace root.

## Why a fork exists at all

The 1.12 addon corpus is Lua **5.0** code. It uses the iterator-less generic-for:

```lua
for k, v in someTable do ... end        -- no pairs(), no iterator function
```

**183 of 218 corpus addons are reached by it** (118 carry it, 65 inherit it through a declared
dependency), across 1,163 sites. On stock Lua 5.1 it raises `attempt to call a table value`, and it
is the **first session-start error for 60 of 218 addons — three quarters of everything that breaks
once the client is actually running**, eleven times the next cause.

Lua 5.1 removed it at the **opcode** level, which is why no layer above the VM can reach it. Every
other dialect gap this project has closed was reachable from inside Lua — `table.setn` is a library
function, `bit` is a library, `print` is a global (decisions 1194, 1196). This one is not:

- a `__call` metamethod would work, but the tables are the addon's own and Lua 5.1 has no
  per-*type* default metatable for tables;
- rewriting the chunk source means parsing Lua with a regex to tell `in t do` from `in pairs(t) do`,
  and getting that wrong changes behaviour silently;
- there is no `lua50` backend — `lua-src` ships 5.1 through 5.5 and every one of them removed it.

## What is NOT being done

**benilla is not adopting Lua 5.0.** We stay on 5.1.5 and restore the behaviour of a single opcode
that Lua itself shipped and labelled `/* for compatibility only */`. Every other 5.1 fix, and the
whole of mlua's surface, stay exactly as upstream. That is a far smaller commitment than a version
downgrade, and it is the entire reason this is a one-hunk fork rather than a vendored 5.0.

## The delta, and the command that proves it

**Current state: no source hunk yet.** The `lua-5.1.5/` tree here is byte-identical to upstream's,
and `src/lib.rs` differs only by having Lua 5.2/5.3/5.4/5.5 stripped from the `Version` enum (with
their source trees deleted) — benilla builds 5.1 and only 5.1, and a fork that still *offered* the
other four would answer a request for one with a missing directory at build time instead of a
compile error.

This ordering is deliberate: the build pipeline is proven transparent **before** any behavioural
payload goes into it, so that when the suite moves, the patch is the only thing that could have
moved it. Landed inert, with all tests passing, exactly so that claim is checkable.

To verify the Lua sources against upstream at any time:

```sh
# 550.0.0 is the pinned version; adjust the path if cargo's registry hash differs.
diff -r third_party/lua-src/lua-5.1.5 \
  ~/.cargo/registry/src/*/lua-src-550.0.0/lua-5.1.5
```

Today that prints nothing. When the payload lands it must print **exactly** the hunk described in
decision 1215 and nothing else — if it ever prints more, something crept in.

## Status of the payload

Blocked, deliberately, on a reverse-engineering answer from the sibling repo `wow-5875-re`: what the
**real 1.12.1 client's own Lua** does when a generic-for's generator is a table. Stock Lua 5.0 is
corroboration, not the authority — the client links 5.0 but may be modified.

Checking stock 5.0 already corrected two assumptions the project had written down (it substitutes in
`OP_TFORPREP`, an opcode 5.1 deleted, *not* in `OP_TFORLOOP`; and the test is a bare `ttistable`
with **no** `__call` check, using the **global** `next` — so an addon that overwrites `next` changes
loop behaviour). Two corrections from checking one step out is the reason for checking the next one
before writing C.
