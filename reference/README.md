# `reference/` — verified facts about the 1.12.1 client, vendored

Generated artifacts describing **the reference client**, committed so benilla can measure itself
against them on any day without a re-derivation. Not documentation, not state: data, with a
generator beside it in `scripts/`.

Nothing here is Blizzard content. A catalogue of *names* is not the thing named — the same reason
`decisions/` may quote a byte address and `MAP.md` may name a file.

## `1.12-globals.tsv`

The 1.12.1 client's entire global namespace — 21,555 names, `name<TAB>type<TAB>origin`.

**Where it comes from.** wow-5875-re ships an addon (`W5875Capture`) that dumps the in-world `_G`
of the *running* reference client, and its output is committed there as a fixture. This is the
authoritative answer to "what can an addon see in 1.12", and it is strictly better evidence than
the binary's registration table would be: it is the actual runtime table, so it carries
FrameXML-defined globals as well as engine ones — which is exactly the surface an addon sees, and
therefore exactly the surface an addon-compatibility target needs (decision 1189).

**Plus what a capture structurally cannot see** (decision 1200). The twelve `Blizzard_*` addons are
**LoadOnDemand**: unless the player had opened the talent window, the trainer, the auction house and
the rest before the dump, none of their globals is in it — so the capture alone reported
`TalentFrame`, `MacroFrame`, `TradeSkillFrame`, `CraftFrame`, `TrainerFrame` and `InspectFrame` as
absent from 1.12 while listing their `*_LoadUI` stubs. The table is therefore the capture **union**
every global the shipped-UI corpus *defines*, and those 1,986 extra rows carry type **`lod`** so
the two halves stay distinguishable. The question this artifact answers is "is this a 1.12 name?",
not "was this name live at one moment in one session".

**The `origin` column** answers a second question the capture alone cannot — *whose job is it to
provide this?* — by attributing every name to its definition site in a complete 1.12 shipped-UI
corpus:

| origin | count | meaning |
|---|---|---|
| `engine` | 1,104 | the C client provides it. **benilla implements these in Rust.** |
| `framexml` | 20,427 | the shipped UI defines it. **benilla transcribes these into `assets/ui/`.** |
| `lua` | 24 | Lua 5.0's own runtime. mlua gives us these. |

Of the `framexml` rows, **1,986 carry type `lod`**: defined by the shipped UI but absent from the
capture, which is what a LoadOnDemand addon looks like from a live dump (decision 1200).

The `framexml` bulk is UI objects — `ContainerFrame1Item16IconTexture` and its 11,000 siblings —
which materialize from whatever XML we ship and are nobody's to implement. The number that
matters for the API surface is the function split: **1,100 engine functions, 1,075 FrameXML
functions.** This is the `tContains` distinction (1187 got it right for one name) applied
wholesale: a name defined in shipped Lua is FrameXML's, and hardcoding it in Rust is a category
error even when it works.

**Regenerating** — `scripts/gen-reference-globals.py`, whose header carries the full method and
the traps. It needs the director's 1.12 install and the sibling RE repo, so it is a manual
regeneration like `genmap.sh`, not something CI can run. The artifact is stable: it describes a
client that shipped in 2006.

**Reading it** — `scripts/api-coverage.sh` is the instrument. It asks a real `UiScript::new()`
what benilla exposes and reports `have / missing / beyond-1.12`, always with 1178's split.
`crates/benilla-ui/src/script/tests/reference_surface.rs` is the gate: it fails if benilla grows a
global that 1.12 does not have and nobody wrote down why.

## `1.12-shapes.tsv` and `1.12-events.tsv`

wow-5875-re's binding-shape table (what each Lua verb takes and returns; decision 1842, gated by
`ui_script/shape_gate.rs`) and the event table with the arguments each producer pushes
(decision 2140, `scripts/gen-reference-events.py`, gated by `ui_script/event_shape_gate.rs`).
Each file's header carries its column contract.

## `1.12-verb-events.tsv`

Every event the reference fires **from inside a Lua verb**, keyed by the verb — the verb's own
body, or a helper that only registered verbs call (`0x4d8c90` is the trainer's). The third
question on the event seam: the two tables above say what an event carries and whether something
fires it; this one says *who*. A stock file that calls a verb for its side effect of an event
repaints nothing when the verb fires nothing, which is how the trainer filter regressed twelve
days after its window went stock (decision 2244), and the class every migrated window shares.

**Regenerating** — `scripts/gen-reference-verb-events.py`, whose header carries the rule, the
two families it drops (glue-space verbs, the unit-field bridge) and why getters are excluded (a
getter's fire is a cache-miss re-query, a state event benilla's feeds fire off the packet). A fire
site is attributed to a function against wow-re's verified extents (the ledgers' `size=`) plus the
disassembly's padding boundaries, and a site past its candidate's extent is left out rather than
mis-filed (decision 2257: four of the first table's rows were callbacks reached by pointer, or a
site past an extent). It reads the sibling RE repo's disassembly, so it is manual, like the others.

**Reading it** — `crates/benilla-app/src/ui_script/verb_event_gate.rs` is the gate: for every
pair whose verb benilla registers, the module that registers the verb fires the event, or the
pair is declared there — `ELSEWHERE` (a feed fires it on the state the verb changes, and the
named file is checked to) or `GAP` (nothing does, with the reason). Decision 2251.
