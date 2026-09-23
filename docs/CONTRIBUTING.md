# Contributing

benilla is a faithful 1.12.1 client. It is the foundation people build on, not the place to get
creative: a change is accepted when it makes benilla more like 1.12.1 or fixes a bug, with
evidence from the reference, in one small piece, with the gates green. Everything else is a
fork, and forks are welcome.

## What gets in

- A fix for a bug, with how to see it before and after.
- A step closer to 1.12.1: a missing packet, verb, window, effect or behaviour, done the way
  the real client does it.
- A correction where benilla and the reference disagree, with the reference fact stated.

## What does not

- Features 1.12.1 does not have, and behaviour changed because it seems better. A deviation
  from the reference is the maintainer's call and is recorded where it lives; a pull request is
  not the place to propose one.
- Anything from a WoW install: art, models, sounds, maps, data. The one exception is interface
  code (FrameXML and GlueXML), and only through the migration recipe in `docs/METHOD.md`.
- Big or mixed changes. One change per pull request, small enough to read in one sitting.

## How a change is judged

1. `scripts/gates.sh` is green: fmt, clippy with warnings denied, the workspace tests, the
   player build.
2. The reference fact is stated: what 1.12.1 does, and where that is known from (the client's
   behaviour you observed, a DBC field, a FrameXML line, a packet capture). The names and shapes
   under `reference/` are the surface benilla tracks.
3. A comment where it matters, one line, saying what the code does and the 1.12 fact behind it.
   No history.
4. The commit message says what changed, for a player or a developer, in one line.

## Setting up

- A 1.12.1 install of your own: `WOW_DATA=<path>`, or a `WoW` link at the repo root. benilla
  reads it and never writes into it.
- A server to test against: any 1.12.1 server. The project runs against a local vmangos
  (`WOW_HOST`, `WOW_USER`, `WOW_PASS`).
- `cargo play` builds and runs the play profile. `scripts/check.sh` verifies a round of work;
  `scripts/gates.sh` is what a pull request must pass.
- Bugs, questions and ideas go to the Discord linked from the README. Issues are off on purpose.

Working with an AI agent is expected. The agent reads `AGENTS.md`, and the same rules bind it.
