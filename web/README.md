# wenilla — benilla in a browser

The same client, compiled to WebAssembly: log in, pick a character, and play a 1.12.1
server from a browser tab — no install. Everything a browser cannot do natively is replaced
behind a seam the native build already had:

| native | browser |
|---|---|
| raw TCP to realmd/mangosd | a WebSocket to `wenilla-host`, which proxies it to the server (`benilla-protocol::transport::Conn`) |
| the MPQ patch chain on disk | single files fetched from `GET /data/<name>`, answered from the real `Data/` by the host (`benilla-formats::Chain` on wasm) — the MPQs never leave the server |
| Lua 5.1 in C, `longjmp` for errors | the same C, compiled against the wasi-sdk sysroot with LLVM's wasm SJLJ; 64-bit `lua_Integer` kept (`third_party/mlua-sys`) |
| `WOW_USER`/`WOW_PASS`/`WOW_CHAR`/`WOW_HOST` env | a `window.__wenilla_env = {user, pass, host, …}` object the page sets before `init()`, else the query string `?user=…&pass=…&char=…&host=…` — but the three credential keys are read from the query only when the env object carries `dev_query_creds: "1"` (the dev `index.html` sets it; a login-gated host such as `wenilla-realm` fetches the credentials itself and never puts them in a URL) |
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
**`wenilla-host` is for local testing only.** `/data` serves the game files you point it at to
anyone who can reach the socket, with no login, so it binds to `127.0.0.1` by default and must
not be exposed on the open internet — the files are Blizzard's, and whoever serves them is
distributing them. A private network you control (a `tailscale serve` front, a LAN) is fine
for your own testing (`--bind 0.0.0.0:8090` logs a warning); hosting for other players is
[`wenilla-realm`](../crates/wenilla-realm/README.md), which puts the same routers behind a
session cookie. The page derives `ws://`/`wss://` from its own origin, so any https front works.

Each tab is a full independent client with its own upstream connection; players need their
own accounts (a second login on one account kicks the first, as in the real client).

## Things to know

- **Hosting behind a login** — `crates/wenilla-realm` wraps this host's routers behind a session
  cookie (the wasm, `/data/*` and `/ws/*` all need it; same-origin requests carry the cookie by
  themselves), renders its own play page that fetches the player's hidden game credentials over
  the session and hands them to the client through `window.__wenilla_env`, and adds an admin
  panel for the server. The packaged one-VM deployment is github.com/Arnesen/wenilla-realm.
- **Sound** starts on the first click or key in the page — browsers suspend every AudioContext
  until a user gesture; `index.html` resumes it then.
- **Full screen** — the "full screen" button on both pages (`web/platform.js`) is not just
  `requestFullscreen`: in full screen it also takes a Keyboard Lock on `Escape`, `Tab` and
  `KeyW`, so Esc opens the game menu instead of leaving full screen, Tab cycles targets instead
  of walking browser focus, and Ctrl+W doesn't close the tab mid-pull. Chrome then exits on a
  *press-and-hold* of Esc (it says so once, in its own toast). The lock follows F11 and every
  other way in or out too — it is driven by `fullscreenchange`, not by the button. Chromium
  only; elsewhere the button is plain full screen.
- **Persistence** — the state folder (`config.toml`, macros, key bindings, layouts, chat
  settings, saved variables, the remembered account name) lives in the browser's `localStorage`,
  one entry per file keyed by its path (`benilla:/benilla-config/config.toml`, …). Per browser
  and per origin, like a `benilla-config/` beside each native install; the server never sees it.
  Clearing site data resets it. Both pages also call `navigator.storage.persist()`
  (`web/platform.js`), which asks the browser not to *evict* that storage under disk pressure —
  the difference between "the player cleared site data" and "the player silently lost their
  keybinds". The grant is heuristic and often only arrives after repeat visits; there is no
  fallback if it is refused. It covers quota storage only, so the downloaded `/data/*` archives
  — which live in the ordinary HTTP cache, warmed by `boot.js` — are **not** protected by it;
  holding those across eviction would mean rehosting them in the Cache API, which nothing here
  does today.
- **Performance** — the client is CPU-bound on its single wasm thread (Bevy's task pools are
  single-threaded on `wasm32`): ~11 ms/frame in an outdoor scene on a laptop iGPU, scaling
  with visible objects, not pixels. `scripts/web-build.sh` runs `wasm-opt -O3` (+6 %). Any CVar
  can be pinned from the URL for that session (`?renderScale=0.75&farclip=200&worlddetail=0`) —
  `renderScale` helps a weak GPU, the others a weak CPU. On a high-refresh display an 11 ms
  frame alternates between 2 and 3 refresh intervals, which reads as judder; a 60 Hz mode is
  smoother until the web build is multithreaded.
- **Debugging a crash**: the panic hook prints to the console; `WEB_DEBUG=1 scripts/web-build.sh`
  keeps the wasm name section so the stack trace has symbols. `index.html` also prints the real
  WGSL compile diagnostics (wgpu's browser backend does not ask for them).
- **Porting notes** — the things that differ from native and bit this port: std
  `Instant`/`SystemTime`/`env::vars_os` *panic* on wasm (use `bevy::platform::time::Instant` /
  `web-time`); the browser's WGSL compiler enforces the uniformity rule naga does not (no
  `textureSample` in a per-fragment branch — `textureSampleGrad`/`textureSampleLevel` instead);
  wasm-bindgen crate and CLI versions must match exactly; wasi-libc's `fd_prestat_get` stub
  must return EBADF (`web/wasi_stubs.js`) or libc's constructor exits.
