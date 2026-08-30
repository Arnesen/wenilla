<div align="center">
  <h1>benilla</h1>
  <p><b>A from-scratch World of Warcraft 1.12.1 client in Rust and <a href="https://bevy.org">Bevy</a></b></p>
  <p>
    <a href="https://discord.gg/wJSJx467G4"><img src="https://img.shields.io/discord/1529280129518538922?style=for-the-badge&logo=discord&logoColor=white&label=discord&color=5865F2" alt="Discord"></a>
    <a href="https://www.youtube.com/playlist?list=PLdCnpZNKxyb8"><img src="https://img.shields.io/badge/devlog-youtube-FF0000?style=for-the-badge&logo=youtube&logoColor=white" alt="YouTube devlog"></a>
    <a href="LICENSE-MIT"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="License"></a>
  </p>
</div>

> [!IMPORTANT]
> **Issues and pull requests are closed here.** benilla is a solo project developed in a private
> tree; this repo is its export, published as squashed snapshots, so a PR here has nothing to land
> on. The best way to contribute is to join the [Discord](https://discord.gg/wJSJx467G4) and report
> the bugs you find. Questions and ideas are welcome in the same place.

## wenilla — this fork: benilla in a browser tab

This fork's `web` branch adds **wenilla**: the same client compiled to WebAssembly, so a 1.12.1
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

See [`web/README.md`](web/README.md) for the how and the porting notes. Everything else
below is upstream benilla's README, unchanged; upstream is
[samwhosung/benilla](https://github.com/samwhosung/benilla), and the web work is meant to be
upstreamable piece by piece.

benilla speaks the original 1.12.1 protocol, so it connects to any server the real client could,
and reads its game data at runtime from your own 1.12.1 install. Every file format and the network
protocol are implemented from scratch, with no original client code, no third-party WoW crates,
and no bundled game assets.

## What works

- **Formats:** readers for the full asset stack (MPQ patch chain, BLP, DBC, ADT/WDT/WDL, M2, WMO),
  wired into Bevy as an asset source.
- **World:** streamed terrain out to the horizon, portal-culled WMOs with interior lighting,
  doodads and ground clutter, swimmable liquids, sky and weather, and the client's own day/night
  lighting, fog and gamma passes.
- **Models:** GPU-skinned M2s with the full animation controller, a near feature-complete particle
  system, ribbons, and animated gameobjects from doors to lifts.
- **Characters:** customization end to end, the armor texture composite, weapons with sheathing and
  enchant glows, shapeshift forms, stealth and mounts.
- **Movement:** a WoW-feel controller, networked movement in both directions, the server-granted
  modes from slow fall to roots, a follow camera with collision, boats, zeppelins and taxi flights.
- **Networking:** SRP6 auth through world-session crypto, the object mirror into the ECS, and live
  wire coverage from movement and chat through spells, party, quests, mail, trade, vendors, bank,
  loot, the auction house and PvP honor.
- **UI:** a from-scratch FrameXML + Lua engine driving the built-in interface, from the login and
  character screens through the full HUD, the classic windows (guild, macros and key bindings
  included), chat, nameplates, floating combat text and tooltips; third-party addons load from
  a `benilla-config/AddOns/` folder beside the executable (partial: AtlasLoot and Bagnon run,
  see below).
- **Combat:** melee on the faithful swing law, ranged and Auto Shot, casting with GCD and
  cooldowns, combo points, crowd control that really holds you, and the spell visual pipeline.
- **Audio:** music, ambience and SFX under the client's own selection and crossfade rules, with
  interior and underwater transitions and zone reverb.

## Where it's going

benilla is done when a 1.12.1 player can do everything here that they could in the original
client, it looks and feels the same, and it runs from a download on Windows, Linux and macOS.
No dates; the order is what is likely, not a promise.

- The rest of the basics: battlegrounds, guild charters, the stable master, meeting stones,
  GM tickets, master loot, cinematics and whatever else turns up.
- Addons, options and performance, ongoing.
- The no-brainer fixes from VanillaFixes, SuperWoW and the like.
- Playable downloads for Windows, Linux and macOS. Linux first.

Not planned: other expansions or client versions, Warden (anticheat).

## Running it

You need a **1.12.1 (build 5875) client install** for game data, a vanilla server to connect to,
and stable Rust. Any 1.12.1 core works; [vmangos](https://github.com/vmangos/core) is what
development runs against, and cMaNGOS and the rest speak the same protocol.

```sh
WOW_DATA=/path/to/WoW/Data cargo run --release -p benilla
```

The server defaults to `localhost:3724`, the stock `realmd` auth port. Point `WOW_HOST`
at any IP or hostname, appending the auth port if yours is remapped
(`WOW_HOST=play.example.com:5000`). Credentials go in at the login screen, or set `WOW_USER` /
`WOW_PASS` to skip it.

---

Early inspiration and file format guidance came from the
[wowemulation-dev](https://github.com/wowemulation-dev) community, and
[warcraft-rs](https://github.com/wowemulation-dev/warcraft-rs) in particular.

benilla is an independent fan project, not affiliated with or endorsed by Blizzard Entertainment.
It ships **no Blizzard content** — no art, models, sounds, maps, MPQ contents or FrameXML; you
provide your own legally obtained 1.12.1 client. The interface code under
`crates/benilla-app/assets/ui/` is ours, written to the client's own layout and API names so that
the windows look right and 1.12.1 addons find the names they expect.

World of Warcraft is a trademark of Blizzard Entertainment, Inc. Our own code is licensed under
[MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.
