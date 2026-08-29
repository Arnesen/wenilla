# wenilla — benilla in a browser

The same client, compiled to WebAssembly: log in, pick a character, and play a 1.12.1
server from a browser tab — no install. Everything a browser cannot do natively is replaced
behind a seam the native build already had:

| native | browser |
|---|---|
| raw TCP to realmd/mangosd | a WebSocket to `wenilla-host`, which proxies it to the server (`benilla-protocol::transport::Conn`) |
| the MPQ patch chain on disk | single files fetched from `GET /data/<name>`, answered from the real `Data/` by the host (`benilla-formats::Chain` on wasm) — the MPQs never leave the server |
| Lua 5.1 in C, `longjmp` for errors | the same C, compiled against the wasi-sdk sysroot with LLVM's wasm SJLJ; 64-bit `lua_Integer` kept (`third_party/mlua-sys`) |
| `WOW_USER`/`WOW_PASS`/`WOW_CHAR`/`WOW_HOST` env | the page's query string: `?user=…&pass=…&char=…&host=…` |
| the window | `<canvas id="benilla">` in `web/index.html` |

## Requirements

- **WebGPU** for the world (the shared light blob and the skin palettes are storage buffers,
  which WebGL2 has none of). Chrome/Edge on Windows and macOS, Safari 26+, Firefox 141+;
  Chrome on Linux behind `chrome://flags/#enable-unsafe-webgpu`. WebGPU also needs a secure
  context — `localhost`, or https. A WebGL2 build exists (`WEB_BACKEND=webgl2`) and reaches
  the glue screens only.
- On the build machine: stable Rust, `rustup`, ~10 GB of `target/`; `scripts/web-setup.sh`
  fetches the rest (the `wasm32-unknown-unknown` target, `wasm-bindgen-cli` at the exact
  version `Cargo.lock` pins, wasi-sdk into `tools/wasi-sdk/`). No sudo.
- To *serve*: a 1.12.1 (build 5875) `Data/` directory and a reachable realmd (3724) + mangosd (8085).

## Build and run

```bash
scripts/web-setup.sh          # once
scripts/web-build.sh          # → web/dist/ (index.html, wenilla.js, wenilla_bg.wasm, .br/.gz)
cargo run --release -p wenilla-host -- --www web/dist --data /path/to/WoW/Data --upstream 127.0.0.1
# then open http://127.0.0.1:8090/
```

`wenilla-host` (`crates/wenilla-host`) is the whole server side: static files
(precompressed), `GET /data/{name}` (`HEAD` too; `GET /data/__index` lists the chain), and
`GET /ws/{port}` — a WebSocket↔TCP proxy that only ever dials `--upstream` on 3724 or 8085.
Put it behind any https front (a reverse proxy, `tailscale serve`, …) to reach it from other
machines; the page derives `ws://`/`wss://` from its own origin.

Each tab is a full independent client with its own upstream connection; players need their
own accounts (a second login on one account kicks the first, as in the real client).

## Things to know

- **Sound** starts on the first click or key in the page — browsers suspend every AudioContext
  until a user gesture; `index.html` resumes it then.
- **Persistence** — the state folder (`config.toml`, macros, key bindings, layouts, chat
  settings, saved variables, the remembered account name) lives in the browser's `localStorage`,
  one entry per file keyed by its path (`benilla:/benilla-config/config.toml`, …). Per browser
  and per origin, like a `benilla-config/` beside each native install; the server never sees it.
  Clearing site data resets it.
- **Debugging a crash**: the panic hook prints to the console; `WEB_DEBUG=1 scripts/web-build.sh`
  keeps the wasm name section so the stack trace has symbols. `index.html` also prints the real
  WGSL compile diagnostics (wgpu's browser backend does not ask for them).
- **Porting notes** — the things that differ from native and bit this port: std
  `Instant`/`SystemTime`/`env::vars_os` *panic* on wasm (use `bevy::platform::time::Instant` /
  `web-time`); the browser's WGSL compiler enforces the uniformity rule naga does not (no
  `textureSample` in a per-fragment branch — `textureSampleGrad`/`textureSampleLevel` instead);
  wasm-bindgen crate and CLI versions must match exactly; wasi-libc's `fd_prestat_get` stub
  must return EBADF (`web/wasi_stubs.js`) or libc's constructor exits.
