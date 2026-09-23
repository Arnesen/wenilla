#!/usr/bin/env python3
"""Regenerate `reference/1.12-events.tsv` — every FrameScript event the 1.12.1 client can
dispatch, with the ARGUMENTS its producers push.

    scripts/gen-reference-events.py [--wow-re DIR] [--out FILE]

**Why this exists** (decision 2140). benilla already gates two halves of the event seam: a stock
chain file listening for an event nothing fires
(`reference_ui::every_event_a_chain_file_registers_has_a_producer`, 1889) and an event we fire that
1.12 does not have (`every_event_we_fire_is_an_event_the_reference_has`, 1883). Both compare
NAMES. Nothing compared the *arguments*, and an event name is a plain string at both ends — so a
fire with the wrong argument count is silent on both sides, exactly as a binding with the wrong
return arity was before 1842. It is the same class of bug one API over, and it had three live
instances: `PLAYERBANKSLOTS_CHANGED`, `UNIT_PET_EXPERIENCE` and `UNIT_PET_TRAINING_POINTS`, the
last of which routes through `PetPaperDollFrame_OnEvent`'s `elseif ( arg1 == "pet" )` catch-all and
so reached nobody at all.

**The oracle is wow-5875-re's own two censuses**, vendored the way `1.12-shapes.tsv` is:

  - `re/events/event-catalog.tsv`   eventId -> name, off the name-pointer array at `.data 0xbe1198`
  - `re/events/event-firesites.tsv` every `call` AND tail-`jmp` to `FrameScript_SignalEvent`
                                    (`0x703e50`, no varargs) and `SignalEvent2` (`0x703f50`,
                                    printf-style), with the FORMAT STRING each site pushes

**Three producer families, because a literal fire site is not the whole picture.** The firesites
census is over the two signal helpers' own call sites, and its own header says in capitals that an
event with no row there is not an event with no producer:

  signal              a `0x703e50` site — `__fastcall(ecx = id)`, a plain `ret`, ZERO Lua values.
  signal2             a `0x703f50` site — the pushed format string is the argument shape.
  unit-field-bridge   the generic UpdateField -> event bridge (`0x51bbb0` registers one watch per
                      named unit-window field; `0x51bd50` -> `0x515e50` fans out over the unit
                      tokens and calls `0x703f50(id, "%s", token)`). Every event id below `0xb6`
                      that HAS a name is produced there and appears in no firesites row.
                      Cross-check: the 54 catalog ids under `0xb6` are exactly the id set
                      wow-re read out of the registration loop by hand
                      (`system/ui/scratch/unit-field-event-bridge.md` §2.1) — two independent
                      derivations, no difference.
  token-fanout        the same `0x515e50` fan-out reached with a LITERAL id in `edx` rather than a
                      computed one, so also `%s` and also absent from the firesites census. The
                      fourteen ids are wow-re's own list, in that file's header.

**Confidence, and what a consumer may gate on:**

  exact      every contributing producer's shape is known. Gate on this.
  advisory   a contributing site is one of the three whose pushed format DECLARES more varargs than
             its caller pushes (`re/audit/signalevent2-fmt-args.py` audits all 149 and finds
             exactly these): `0x496230 TRADE_REPLACE_ENCHANT`, `0x5e4527`/`0x5e7960 UPDATE_TICKET`.
             Faithful clients must not copy a bug that hands Lua undefined values.
  none       the name is in the catalog and NO producer family reaches it here. **This is not a
             claim that it has none** — see the census caveat above. Never gate on it.

Glue-space rows are dropped: `0x703d90` is called twice with different name populations, so the
same integer names a different event on the glue screen than in the world. wow-re blanks those
rows' names; this drops them by fire-site address as well, from the four TU ranges its header
names, so a future re-derivation that fills a name in cannot leak one through.

**The input lives outside the repo** (the sibling RE repo), so this is a manual regeneration, like
`gen-reference-globals.py` — not something CI can run.
"""
import argparse
import os
import sys

# The `Source\Glue\` TU ranges whose event ids belong to the GLUE name space, from
# `re/events/event-firesites.tsv`'s own header.
GLUE_SITES = [(0x46AA34, 0x46AA34), (0x46BC9D, 0x46C52E), (0x46E73D, 0x46E73D), (0x47461E, 0x47461E)]
# The three sites whose format string declares more varargs than the caller pushes.
DECLARES_MORE_THAN_IT_PUSHES = {0x496230: "TRADE_REPLACE_ENCHANT", 0x5E4527: "UPDATE_TICKET", 0x5E7960: "UPDATE_TICKET"}
# `0x515e50` callers carrying a LITERAL id — the token fan-out reached outside the generic bridge.
TOKEN_FANOUT_IDS = [0x10, 0x16, 0x1C, 0x1D, 0x1E, 0x29, 0xB7, 0xB8, 0xB9, 0xBA, 0xBB, 0x159, 0x197, 0x20A]
# `0x51bbb0`'s walk bound: `cmp esi,0xb6`. Every NAMED unit-window field below it is watched.
UNIT_WINDOW_FIELDS = 0xB6
# The zero-argument format, spelled so the column is never empty.
NONE = "()"


def rows(path):
    for line in open(path, encoding="utf-8"):
        if line.startswith("#"):
            continue
        f = line.rstrip("\n").split("\t")
        if f and f[0] in ("eventId", "fire_site_va"):
            continue
        yield f


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--wow-re", default=os.path.expanduser("~/dev/wow-5875-re"))
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "..", "reference", "1.12-events.tsv"))
    a = ap.parse_args()

    catalog = {}
    for f in rows(os.path.join(a.wow_re, "re/events/event-catalog.tsv")):
        if len(f) >= 2 and f[1]:
            catalog.setdefault(int(f[0]), f[1])
    if len(catalog) < 300:
        sys.exit(f"event-catalog.tsv gave only {len(catalog)} names — wrong path?")

    # name -> {ids, producers, formats, notes}
    ev = {}

    def add(name, eid, producer, fmt, note=None):
        e = ev.setdefault(name, {"ids": set(), "producers": set(), "formats": set(), "notes": []})
        e["ids"].add(eid)
        e["producers"].add(producer)
        e["formats"].add(fmt)
        if note and note not in e["notes"]:
            e["notes"].append(note)

    dropped_glue = 0
    for f in rows(os.path.join(a.wow_re, "re/events/event-firesites.tsv")):
        if len(f) < 4:
            continue
        va = int(f[0], 16)
        dispatcher, eid, name = f[1], f[2], f[3]
        if any(lo <= va <= hi for lo, hi in GLUE_SITES):
            dropped_glue += 1
            continue
        if not name or eid == "dyn":
            continue
        fmt = NONE if dispatcher == "SignalEvent" else (f[4] if len(f) > 4 and f[4] else NONE)
        note = None
        if va in DECLARES_MORE_THAN_IT_PUSHES:
            note = f"0x{va:x} declares more varargs than it pushes — do not copy"
        add(name, int(eid), "signal" if dispatcher == "SignalEvent" else "signal2", fmt, note)

    for eid, name in catalog.items():
        if eid < UNIT_WINDOW_FIELDS:
            add(name, eid, "unit-field-bridge", "%s")
    for eid in TOKEN_FANOUT_IDS:
        if eid in catalog:
            add(catalog[eid], eid, "token-fanout", "%s")

    # Every remaining catalog name, so the table is the whole surface and a consumer can tell
    # "no producer found" from "not an event".
    for eid, name in catalog.items():
        if name not in ev:
            ev.setdefault(name, {"ids": set(), "producers": set(), "formats": set(), "notes": []})["ids"].add(eid)

    out = os.path.normpath(a.out)
    with open(out, "w", encoding="utf-8") as fh:
        fh.write(
            "# The 1.12.1 client's FrameScript events and the ARGUMENTS their producers push.\n"
            "# GENERATED — scripts/gen-reference-events.py. Source: wow-5875-re's\n"
            "# re/events/event-catalog.tsv + event-firesites.tsv. Decision 2140.\n"
            "#\n"
            "# arg_formats  `|`-separated, one per distinct producer shape. `()` = zero Lua values.\n"
            "#              Directives: %s string, %d/%u number, %f number. An event with two\n"
            "#              producers of different shapes carries both, and which one a given\n"
            "#              transition takes is a question about the transition, not the name.\n"
            "# conf         exact = gate on it. advisory = a contributing site declares more\n"
            "#              varargs than it pushes. none = no producer family reaches it HERE,\n"
            "#              which is not a claim that it has none; never gate on it.\n"
            "# producers    signal (0x703e50, no varargs) | signal2 (0x703f50, format-driven) |\n"
            "#              unit-field-bridge (0x51bbb0/0x51bd50 -> 0x515e50, always %s) |\n"
            "#              token-fanout (0x515e50 with a literal id, also %s).\n"
            f"# {len(ev)} events, {sum(1 for e in ev.values() if e['producers'])} with a known producer;"
            f" {dropped_glue} glue-space fire sites dropped.\n"
            "name\tids\tproducers\targ_formats\tconf\tnote\n"
        )
        for name in sorted(ev):
            e = ev[name]
            conf = "none" if not e["producers"] else ("advisory" if e["notes"] else "exact")
            fh.write(
                "\t".join(
                    (
                        name,
                        "|".join(str(i) for i in sorted(e["ids"])),
                        "|".join(sorted(e["producers"])),
                        "|".join(sorted(e["formats"])) or "?",
                        conf,
                        "; ".join(e["notes"]),
                    )
                )
                + "\n"
            )
    print(f"wrote {out}: {len(ev)} events, {sum(1 for e in ev.values() if e['producers'])} produced")


if __name__ == "__main__":
    main()
