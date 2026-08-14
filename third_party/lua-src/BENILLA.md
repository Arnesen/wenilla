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

**Current state: three hunks, in three files.** `src/lib.rs` additionally differs by having Lua
5.2/5.3/5.4/5.5 stripped from the `Version` enum (with their source trees deleted) — benilla builds
5.1 and only 5.1, and a fork that still *offered* the other four would answer a request for one with
a missing directory at build time instead of a compile error.

| file | hunk | why | record |
|---|---|---|---|
| `lvm.c` | 5.0's `OP_TFORPREP` table→`next` substitution, folded into the top of `OP_TFORLOOP` | `for k, v in someTable do` raised "attempt to call a table value"; it was the first session-start error for **60** corpus addons | 1215 |
| `luaconf.h` | `LUA_COMPAT_LSTR` `1` → `2` | 5.1 kept 5.0's nesting machinery and put an advisory error in front of it; two corpus addons died on "nesting of `[[...]]` is deprecated" | — |
| `lparser.c` | 5.0's compat-semicolon skip restored at the top of `constructor()`'s field loop — 5.0's own line, verbatim, comment included | one extra `;` after a field separator inside a table constructor (`Back_Title = AL["Factions"];;`, ×20 in AtlasLoot's `ButtonRegistry.lua`) parses on the client and died here with "unexpected symbol near `;`"; statement-level `;;` stays rejected by both dialects | 1315 |

The ordering was deliberate and it held: the fork landed **inert**, byte-identical to upstream, so
the build pipeline was proven transparent *before* any behavioural payload went into it — and when
the suite then moved, the patch was the only thing that could have moved it. Both hunks are measured
against the 218-addon corpus rather than argued: the long-string one alone took loaded 134 → 139 and
survivors 99 → 104.

To verify the Lua sources against upstream at any time:

```sh
# 550.0.0 is the pinned version; adjust the path if cargo's registry hash differs.
diff -r third_party/lua-src/lua-5.1.5 \
  ~/.cargo/registry/src/*/lua-src-550.0.0/lua-5.1.5
```

That must print **exactly** the three hunks in the table above and nothing else. If it ever prints
more, something crept in.

## Status of the payload

**Landed.** The reverse-engineering answer this was deliberately blocked on came back from
`wow-5875-re` (`system/ui/scratch/lua-generic-for.md`) and is what the `lvm.c` comment cites, detail
by detail: a **bare type-tag** test that never consults a metatable (so a table carrying `__call`
still gets `next`), the **global** `next` read raw and fetched fresh at every loop entry (so an addon
assigning `next = myfn` changes every later generic-for in the session), and userdata *not*
substituted.

Waiting was the right call twice over. Checking stock 5.0 first corrected two assumptions the
project had already written down — the substitution happens in `OP_TFORPREP`, an opcode 5.1 deleted,
*not* in `OP_TFORLOOP`; and the test is a bare `ttistable` with no `__call` check. Stock 5.0 was
corroboration, never the authority: the client links 5.0 but may be modified, and only the byte-read
of the client settles what it actually does.
