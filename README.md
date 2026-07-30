# benilla

[![Discord](https://img.shields.io/badge/chat-Discord-5865F2?logo=discord&logoColor=white)](https://discord.gg/wJSJx467G4)
[![YouTube](https://img.shields.io/badge/devlog-YouTube-FF0000?logo=youtube)](https://www.youtube.com/playlist?list=PLdCnpZNKxyb8)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

A from-scratch **World of Warcraft 1.12.1 client** in **Rust + [Bevy](https://bevy.org)**, speaking
the vanilla protocol to a [vmangos](https://github.com/vmangos/core) server. Every file format and
the network protocol are implemented from scratch — no original client code, no third-party WoW
crates, and no bundled game assets: benilla reads its data at runtime from your own 1.12.1 install.

## What works

- **Formats & assets** — in-repo readers for the full asset stack (MPQ patch chain, BLP, DBC,
  ADT/WDT/WDL, M2, WMO), wired into Bevy as an asset source, plus extraction CLI tools.
- **World** — streamed terrain with the distant horizon, portal-culled WMOs with interior lighting,
  doodads and ground clutter, swimmable liquids, sky and weather, and the client's own day/night
  lighting, fog, glow, and gamma passes.
- **Models & animation** — GPU-skinned M2s with the full animation controller (cross-fades,
  one-shots, fidgets), a near feature-complete particle system, ribbons, and animated gameobjects
  (doors, chests, mailboxes, lifts).
- **Characters** — customization end to end, the armor-region texture composite, held weapons and
  sheathing, and mounts.
- **Movement & travel** — a WoW-feel controller (run/strafe/jump/swim/step-up), networked movement
  in both directions, a follow-camera with collision, ridden boats and zeppelins, and taxi flights.
- **Networking** — SRP6 auth through world-session crypto, the object-descriptor mirror into the
  ECS, and live wire coverage for movement, chat, spells and auras, party, mail, trade, vendors,
  bank, trainers, quests, loot with group rolls, and death.
- **UI** — a from-scratch FrameXML + Lua UI engine driving the built-in interface: login and
  character screens, the full HUD, the classic windows (bags, spellbook, talents, quests, mail,
  merchant, trade, bank, world map, …), chat, and tooltips.
- **Combat** — melee with the faithful swing law, ranged and Auto Shot, casting with GCD, cooldowns
  and the cancel ladder, and the spell-visual pipeline (kits, missiles, impacts, ground rings).
- **Audio** — music, ambience, and SFX under the client's own selection and crossfade rules, with
  interior/underwater transitions and zone reverb.

## Not built yet

Guilds, the auction house, hunter/warlock pets, duels and inspect, the social panel
(friends/who), PvP systems, the macros/options/key-binding screens, and third-party addon
loading — the Lua/FrameXML engine currently runs only the built-in UI.

## Running it

You need a **1.12.1 (build 5875) client install** for game data, a
[vmangos](https://github.com/vmangos/core) server to connect to, and stable Rust.

```sh
WOW_DATA=/path/to/WoW/Data cargo run --release -p benilla
```

The server defaults to `localhost:3724`, where a stock vmangos `realmd` listens — point `WOW_HOST`
at any IP or hostname, and append an auth port if yours is remapped: `WOW_HOST=play.example.com:5000`.
Credentials go in at the login screen, or set `WOW_USER` / `WOW_PASS` to skip it.

## Contributing

benilla is a solo project and isn't taking pull requests for now. Bug reports and questions are
welcome — open an issue, or bring them to Discord. Comments throughout the code cite numbered
decision records and method notes from a private development log; the citations won't resolve
here, but the reasoning they carry is kept in the comments themselves.

Development happens in that private tree, and this repo is its published export: **each commit here
is one squashed snapshot** of everything that landed since the last one, titled with what changed —
not an atomic change, which is why they're large and why there are few of them.

## Acknowledgements

The [wowemulation-dev](https://github.com/wowemulation-dev) community — and
[warcraft-rs](https://github.com/wowemulation-dev/warcraft-rs) in particular — was an early
inspiration and a source of guidance on the client's file formats. benilla's readers ended up
written from scratch, but their preservation work made starting far less daunting. And none of
this runs without [vmangos](https://github.com/vmangos/core) on the other end of the socket.

## Legal

benilla is an independent, fan-made project, not affiliated with or endorsed by Blizzard
Entertainment. It contains no Blizzard code or assets and never will; you must provide your own
legally-obtained 1.12.1 client for game data. World of Warcraft is a trademark of Blizzard
Entertainment, Inc.

## License

Licensed under either of the [Apache License 2.0](LICENSE-APACHE) or the [MIT
license](LICENSE-MIT), at your option.
