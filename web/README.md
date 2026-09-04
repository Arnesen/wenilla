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

## JavaScript bridge

The page can see the game and drive it. `web/bridge.js` (loaded by `index.html`, and
`window.wenilla` in the devtools console) is the API; `crates/benilla-app/src/webbridge/` is the
wasm side. Two things it exists for: **web HUDs and addons in plain JS/HTML** on top of the
canvas — a proximity-chat overlay, a map, a threat meter — and **control from JS**: automation
("idle-RPG" play), and inputs the client has no path for, such as a gamepad. A third fell out of
it: **desktop notifications while the tab is in the background** (level-ups, death, whispers,
invites, mail, combat), which needs the game to keep ticking in a hidden tab, so the bridge
provides that too.

```js
wenilla.state.self.pos                       // [x, y, z] in WoW yards; .facing in radians
wenilla.on('chat', (c) => console.log(c.sender, c.senderGuid, c.text))
wenilla.on('PLAYER_LEVEL_UP', (args) => …)   // any FrameXML event, by name
wenilla.hold('MOVEFORWARD'); wenilla.release('MOVEFORWARD'); wenilla.fire('JUMP')
await wenilla.lua("return UnitName('target'), UnitHealth('target')")   // → ['Defias Thug', 61]
wenilla.chat('/say hello'); wenilla.chat('/target Bob')
wenilla.gamepad.start(); wenilla.notify.enable()   // the 🔔 button does the latter
```

`?bridge=hud` mounts `web/examples/hud.js` (self, zone, target, the nearest units with distance
and bearing, chat lines with how far the speaker is — the proximity prerequisite made visible);
`?bridge=idle` runs `web/examples/idle.js`, a 30-line attack-nearest loop. `?bridge=0` turns the
bridge off for the session.

### How it works, and what it stands on

The wasm exports nothing new. Once a frame it looks up `window.__wenilla_bridge` (the object
`bridge.js` creates); absent, the bridge is idle at that one lookup's cost. Present, the wasm
calls `onFrame(snapshot)` at `hz` (default 20/s) and `onEvent(name, payload)` as things happen,
and drains the object's `queue` of command objects. Everything is plain properties and plain
objects — the same `Reflect.get`/`call` pattern `webenv.rs` and `webprogress.rs` already use,
no `serde_json` in the shipped app, no new `web-sys` features.

Where the data comes from: the Lua UI is already fed by per-frame plain-data snapshots
(`ui_unit::snapshot`, the unit frames' own view of a descriptor, with the feign-death and rage
display rules applied), and every FrameXML event goes through one `UiScript::fire_event` — 856
call sites. The bridge reuses the first for `self`/`target`/`units[]` and taps the second for
the event stream, so a page and an addon can never disagree. Chat lines are copied off the same
router that feeds the windows and the `CHAT_MSG_*` fire, with the one thing the reference's
event never carried — the sender's **guid** — so a proximity feature can find the speaker in
`units[]`. Zone texts are the resolve that writes `GetZoneText()`'s globals, published once more
as a resource.

Where control goes: every key the client reads collapses into one `BindingsState` resource
that the movement controller, jump, sit, TAB-targeting, attack and the camera zoom read —
nothing downstream reads raw keys — so the bridge asserts *commands by name* into that state
(`MOVEFORWARD`, `JUMP`, `ACTIONBUTTON1`: the 202 names in the Key Bindings window, listed in
the `ready` event). A page cannot do anything a key cannot, and rebinding keys does not change
what the page's `hold('MOVEFORWARD')` does. `Kind::Held` commands latch (re-asserted every
frame until released; dropped when the chat box takes focus, like keys, and resumed when it
loses it, unlike keys — a stick has no re-press); host edges fire once with an amount (the
wheel's zoom notch); action-button commands run their Lua press and release bodies
back-to-back, the wheel-notch law that makes a button cast. Movement, jump, sit, sheath,
autorun and interaction have no Lua verbs in 1.12, which is why the bridge goes under the
bindings instead of through the VM for them. Casting, targeting by name, quests, bags and
reading state back out go through the VM: `lua(chunk)` evaluates a chunk and returns its
results as plain values (tables to depth 4, at most 512 values). Turning is
`Player::turn_aim` — the scripted mouse-turn the probe harness uses. NPC and gameobject
interaction (a right-click) is not exposed yet.

Considered and not chosen: synthesizing DOM key events on the canvas (no Rust change, but
chord-addressed and fragile for mouse-look), Bevy's raw input messages (the probe fleet's shape,
still chord-addressed), and the `Model` intent queues (typed, but bypasses FrameXML's own
side effects). The dev probes (`WOW_PROBE_KEY`/`_LUA`/`_CHAT`/`_LOOK`) are the working
templates for each channel, but they are compiled out of the browser build with the rest of the
dev feature, and gated on environment variables the browser does not have.

### The contract

`window.__wenilla_bridge` — the page owns these; `bridge.js` fills them in:

| property | meaning |
|---|---|
| `hz` | `onFrame` calls per second, `0` = every frame (default 20, clamped to 120) |
| `radius`, `maxUnits` | `units[]` is every streamed object within `radius` yards, nearest first, at most `maxUnits` (60, 64) |
| `events` | Lua events to relay through `onEvent('event', …)`: a list of names, or `'*'` (a firehose — debugging only) |
| `queue` | command objects (below); the wasm empties it every frame |
| `onFrame(snapshot)` | called at `hz` while the object exists |
| `onEvent(name, payload)` | called as events happen |
| `wake()` | installed by the wasm: runs one app update (the hidden-tab keepalive) |

Snapshot (`wenilla.state`): positions are `[x, y, z]` in **WoW coordinates** (+X north, +Y
west, +Z up, yards); `facing` is the wire orientation in radians (counter-clockwise from +X);
guids are hex strings (`"0xf130000000000001"`), because a u64 does not fit a JS number.

```
{ v: 1, seq, t,                      // t: seconds since the app started
  session: { state: 'login'|'charselect'|'charcreate'|'inworld', connected },
  map: { id }, zone: { id, name, realZone, subzone, minimapText, indoor, pvpType, pvpFaction, arena } | null,
  self:   Unit + { pos, facing, mounted, casting: {spellId}|null, channeling: {spellId}|null } | null,
  target: Unit | null,  hover: { guid } | null,
  units: [ Unit… ] }
Unit = { guid, kind: 'player'|'unit'|'go'|'corpse'|'dyn', name|null, pos, dist, displayId,
         health, maxHealth, power, maxPower, powerType, level, dead, ghost, inCombat,
         reaction (1..8, 0 unknown), hostile, friendly, isPlayer, pvp, class, race, targetGuid,
         moveFlags, moving, swimming, falling, speed, standState, stealthed }
```

Events (`wenilla.on(name, cb)`):

| name | payload |
|---|---|
| `ready` | `{ version, commands: [{name, kind: 'held'|'host'|'lua', category}] }` — once, when the wasm first sees the object |
| `state` | `{ state, connected }` on change (any screen, not only in-world) |
| `map` | `{ id }` on a worldport |
| `zone` | the `zone` block, on change |
| `chat` | `{ event: 'CHAT_MSG_SAY', kind: 'SAY', text, sender, senderGuid, language, channel, channelNumber, target, flag }` — every routed line |
| `event` | `{ name, args }` — a Lua event you subscribed to; `wenilla.on('PLAYER_DEAD', …)` subscribes for you |
| `lua` | `{ id, ok, values }` or `{ id, ok: false, error }` — `wenilla.lua()` turns these into a promise |
| `input` | `{ heldCleared: true }` — the wasm dropped every held command (world exit, the object vanished, `release`) |
| `error` | `{ reason, cmd? }` — an unknown command name (reported once) or a malformed queue entry |

Commands (`queue` entries; the `wenilla.*` methods build them):

| op | fields | does |
|---|---|---|
| `hold` | `cmd, down` | assert/release a held command (`MOVEFORWARD`, `TURNLEFT`, `STRAFERIGHT`, …) |
| `fire` | `cmd, amount?` | a host edge (`JUMP`, `TARGETNEARESTENEMY`, `CAMERAZOOMIN` with `amount` notches) or an action button (`ACTIONBUTTON1`: press+release) |
| `look` | `dyaw` | turn the aim by radians, positive = left |
| `lua` | `id, chunk` | evaluate; answered by a `lua` event with that `id` |
| `chat` | `text` | a line as if typed and submitted — `/say`, `/target`, `/follow`, `/sit`, `.gm` |
| `release` | | drop every hold |

### Background tabs and notifications

A hidden tab gets no `requestAnimationFrame` and its timers are throttled (to 1/s, then 1/min
after five minutes), so the game — which runs off animation frames when visible and off timers
when the canvas loses focus — stops until the tab is looked at again. Nothing is lost (the
WebSocket buffers), but nothing happens either. `bridge.js` fixes that from the page: while
hidden it runs a dedicated Worker (whose timers browsers do not throttle) that calls the
`wake()` the wasm installed at `wenilla.background.hz` (default 4/s); each wake is one full
app update. Four a second is a few percent of a core; a bot that must react faster can raise it
with `wenilla.set({backgroundHz: 20})`; `0` disables it.

On that, `wenilla.notify` shows desktop notifications for a rule set the page may edit
(`wenilla.notify.rules`): `PLAYER_LEVEL_UP`, `PLAYER_DEAD`, `RESURRECT_REQUEST`,
`PARTY_INVITE_REQUEST`, `GUILD_INVITE_REQUEST`, `DUEL_REQUESTED`, `UPDATE_PENDING_MAIL`,
`PLAYER_REGEN_DISABLED` (30 s cooldown), `QUEST_COMPLETE`, plus whispers (from `chat`) and a
disconnect (from `state`). They show only while the tab is hidden or unfocused
(`wenilla.notify.always = true` overrides), collapse by `tag`, focus the tab on click, and
badge the title. `enable()` asks the browser once and has to run from a user gesture — the
🔔 button, on both pages. Notifications need a secure context, which WebGPU already requires.

### Policy, hosting, limits

Everything here is reachable only by same-origin page script: the host's COOP/COEP isolate the
page, so a cross-origin iframe or popup cannot reach the bridge, and such a script could
already synthesize key events on the canvas and edit the saved variables in `localStorage`.
The bridge adds convenience, not reach. Whether *automation* is welcome on a realm is the
operator's call: a hosting page opts a session out with `bridge: '0'` in `window.__wenilla_env`
(read once at boot, and it beats any `?bridge=` a player passes; `?bridge=0` does the same for one
session on either page). `wenilla-realm`'s play page loads `bridge.js` like the dev page does, 🔔
included, and sets no `bridge` key: automation is on for every player until the operator adds
`bridge: '0'` to the `__wenilla_env` object `play.html` builds.

Handlers run **synchronously on the single wasm thread**: keep `onFrame` cheap (the snapshot
is built and handed over inside the frame) and push heavy work to a Worker. `'*'` relays every
`UNIT_*` fire — subscribe by name. Not in this version: party/guild blocks, unit facing, look
pitch, an `interact` (right-click) op, on-demand name resolution (names come from the cache;
a unit can be `name: null` for a frame or two), and raw packet access — deliberately.
