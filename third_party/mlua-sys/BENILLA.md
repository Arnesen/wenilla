# benilla's `mlua-sys` copy — what differs from upstream

Upstream: `mlua-sys 0.10.0` (crates.io), byte-identical except for ONE change in
`src/lua51/lua.rs`: `lua_Integer` stays `i64` on `wasm32` (upstream makes it `i32` on every
32-bit target). The vendored Lua (`third_party/lua-src`) defines `LUA_INTEGER long long` on wasm
so C and Rust agree. Reason: the browser build; `benilla-ui` is written against `mlua::Integer =
i64` at ~190 sites, and a 32-bit integer would silently truncate there.

Wired in through `[patch.crates-io]` in the workspace root. Verify the diff with:

    diff -r ~/.cargo/registry/src/*/mlua-sys-0.10.0 third_party/mlua-sys
