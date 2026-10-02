<div align="center">
  <h1>benilla</h1>
  <p><b>A complete World of Warcraft 1.12.1 client, written from scratch in Rust and <a href="https://bevy.org">Bevy</a></b></p>
  <p>
    <a href="https://discord.gg/wJSJx467G4"><img src="https://img.shields.io/discord/1529280129518538922?style=for-the-badge&logo=discord&logoColor=white&label=discord&color=5865F2" alt="Discord"></a>
    <a href="https://www.youtube.com/playlist?list=PLdCnpZNKxyb8"><img src="https://img.shields.io/badge/devlog-youtube-FF0000?style=for-the-badge&logo=youtube&logoColor=white" alt="YouTube devlog"></a>
    <a href="LICENSE-MIT"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="License"></a>
  </p>
</div>

## wenilla — this fork: benilla in a browser tab

This fork adds **wenilla** on `main`: the same client compiled to WebAssembly, so a 1.12.1
server can be played from a browser with nothing installed — login, character select and the
world, with the full FrameXML/Lua UI, over a WebSocket. Nothing is rewritten: raw TCP becomes a
WebSocket through a small host (`wenilla-host`), the MPQ chain becomes single files fetched over
HTTP, and the C Lua 5.1 VM is compiled for wasm with the wasi-sdk sysroot. WebGPU is required
for the world; the host proxies only to the server you point it at, and never sends the MPQs.

```bash
scripts/web-setup.sh                                   # once: wasm target, wasm-bindgen, wasi-sdk (no sudo)
scripts/web-build.sh                                   # → web/dist/
cargo run --release -p wenilla-host -- --www web/dist --data /path/to/WoW/Data
# open http://127.0.0.1:8090/
```

`wenilla-host` is a local testing tool: it serves the game data you point it at with no login,
so keep it on loopback or a private network and never on the open internet. Hosting players
is [`wenilla-realm`](crates/wenilla-realm/README.md), which gates everything behind a session.
See [`web/README.md`](web/README.md) for the how and the porting notes, [`AGENTS.md`](AGENTS.md)
for the map of the repository and the rules that bite, and [`docs/UPSTREAM.md`](docs/UPSTREAM.md)
for how upstream is merged in. Everything else below is upstream benilla's README, unchanged;
upstream is [samwhosung/benilla](https://github.com/samwhosung/benilla), merged into `main` as it
publishes, and the web work is meant to be upstreamable piece by piece.

benilla speaks the original 1.12.1 protocol, so it connects to any server the real client could,
and reads its game data at runtime from your own 1.12.1 install. Every file format and the network
protocol are implemented from scratch, with no original client code, no third-party WoW crates,
and no bundled game assets.

## What's inside

- **Formats:** readers for the whole asset stack (the MPQ patch chain, BLP, DBC, ADT, WDT, WDL, M2
  and WMO), wired into Bevy as an asset source.
- **World:** terrain streamed out to the horizon, portal-culled buildings with interior lighting,
  doodads and ground clutter, swimmable water, sky and weather, and the client's own day/night
  lighting, fog and gamma.
- **Models:** GPU-skinned M2s with the full animation controller, particles, ribbons and the spell
  visuals, and characters end to end: customization, the armor composite, weapons with their
  enchant glows, forms, stealth and mounts.
- **Movement:** networked movement in both directions, the server-granted modes from slow fall to
  roots, a follow camera with collision, boats, zeppelins and taxi flights.
- **Networking:** SRP6 auth through world-session crypto and the object mirror into the ECS,
  covering the game from movement and chat through combat, spells, groups and raids, quests,
  trade, mail, the auction house and battlegrounds.
- **Interface:** a FrameXML and Lua engine that runs the stock 1.12 interface off your install's
  patch chain, and third-party 1.12 addons: Questie, pfUI, Bagnon, Bartender2 and most others
  run. By choice, the options window and the ESC menu follow the Classic Era client's rather than
  1.12's.
- **Audio:** music, ambience and effects under the client's own selection and crossfade rules,
  with interior and underwater transitions and zone reverb.

The format readers and the UI engine core are plain Rust with no Bevy in them, and the world
renderer runs with no game attached. [`docs/MAP.md`](docs/MAP.md) maps every crate and subsystem,
generated from the code.

## Status

Complete and fully playable. What is left:

- The long tail of small features that separates a working client from a finished one, tracked
  as [issues](https://github.com/samwhosung/benilla/issues).
- Addons, options and performance, ongoing.

benilla is a faithful 1.12.1 client and the foundation people build on. There are no prebuilt
downloads: a packaged build, a particular server's changes or anything 1.12.1 never had belongs
in a fork, and forks are welcome. GitHub lists
[every public fork](https://github.com/samwhosung/benilla/forks).

Not planned: other expansions or client versions, Warden (anticheat).

## Running it

benilla builds and runs on macOS, Linux and Windows. You need:

- **An English 1.12.1 client (build 5875)** for the game data. benilla only reads it.
- **A 1.12.1 server with Warden off.** [vmangos](https://github.com/vmangos/core) is what
  development runs against, and it ships with Warden off; cMaNGOS and the other 1.12.1 cores speak
  the same protocol.
- **Stable Rust and a C compiler**, because the client's Lua is built from source: on macOS the
  Xcode command line tools, on Linux the ALSA and udev development packages and pkg-config, on
  Windows the MSVC build tools that the Rust installer sets up.

```sh
WOW_DATA=/path/to/WoW/Data cargo run --release -p benilla
```

On Windows, in PowerShell:

```powershell
$env:WOW_DATA="C:\path\to\WoW\Data"; cargo run --release -p benilla
```

Each release is a tag on the [Releases page](https://github.com/samwhosung/benilla/releases):
`git checkout <tag>` first runs that release, and `main` is the development tip.

`WOW_DATA` names the install's `Data` folder; a link to the install named `WoW` at the repo root
does the same (`ln -s /path/to/WoW WoW`, or on Windows a junction, which needs no admin rights:
`New-Item -ItemType Junction -Path WoW -Target C:\path\to\WoW`). The server defaults to
`localhost:3724`, the stock auth port. Point `WOW_HOST` at another (`WOW_HOST=play.example.com`, or
`play.example.com:5000` for a remapped port), or set it from the Realmlist button on the login
screen, which remembers it. Credentials go in at the login screen, or set `WOW_USER` and `WOW_PASS`
to skip it.

Settings, screenshots and addons live in `benilla-config/` at the repo root: a 1.12 addon goes in
`benilla-config/AddOns/`. [`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md) has the rest, from the
player build to the tests.

## Contributing

Issues and pull requests are open. [`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md) says where to
start, what gets in, how a change is judged and what happens to a pull request once it is open.
Bugs, questions and ideas are welcome on the [Discord](https://discord.gg/wJSJx467G4) too.

---

Early inspiration and file format guidance came from the
[wowemulation-dev](https://github.com/wowemulation-dev) community, and
[warcraft-rs](https://github.com/wowemulation-dev/warcraft-rs) in particular.

benilla is an independent fan project, not affiliated with or endorsed by Blizzard Entertainment.
It ships **no Blizzard content**: no art, models, sounds, maps, MPQ contents or FrameXML. You
provide your own legally obtained 1.12.1 client, and the stock interface runs off its FrameXML at
runtime. The few files under `crates/benilla-app/assets/ui/` are our own, not copies of it:
adapters over stock files, and the settings windows and script error log benilla draws itself.

World of Warcraft is a trademark of Blizzard Entertainment, Inc. Our own code is licensed under
[MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option. The two vendored components
under `third_party/`, the kira audio engine and a Lua 5.1 patched to the 1.12 client's dialect,
keep their own upstream licenses, alongside each.
