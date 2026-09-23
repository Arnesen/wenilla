#!/usr/bin/env python3
"""Regenerate `reference/1.12-verb-events.tsv` — every FrameScript event the 1.12.1 client fires
FROM INSIDE A LUA VERB, keyed by the verb.

    scripts/gen-reference-verb-events.py [--wow-re DIR] [--out FILE]

**Why this exists.** `reference/1.12-events.tsv` (2140) says what arguments an event carries and
`reference_ui`'s two name gates (1883/1889) say whether *something* fires what a stock file listens
for. None of them says WHO fires it. That gap is the one B389/2244 fell through: the reference's
`SetTrainerServiceTypeFilter` commits through a helper whose whole body is "write the mask, re-run
the finalizer, fire TRAINER_UPDATE", the stock `Blizzard_TrainerUI.lua` therefore never repaints
the list itself, and benilla's verb set the mask and fired nothing. The name gate was green,
because the packet arm fires the same event; the tests were green, because they asserted the mask.
Our own retired `TrainerFrame.xml` had been repainting explicitly, so the behaviour left with the
file that was compensating for it — and the same shape is waiting in every other window that went
stock (1751): a verb the stock Lua calls *for its side effect of an event*.

**What a row means.** `verb` is a registered Lua binding (wow-re's `re/audit/binding-shapes.tsv`)
and `event` is fired on the verb's own call path in the reference — either from the verb's body
(`shape=body`) or from a helper that ONLY registered verbs call (`shape=helper`, `via` = the helper's
address; `0x4d8c90` is the trainer's). A helper with any non-verb caller — a packet handler, the
cursor, the tutorial signal — is excluded by construction: an event fired from there is a *state*
event, which benilla may faithfully fire from its own feeds, and demanding it of the verb would be
the exceptions list nobody wants to maintain. Hubs (`ClearCursor` → CURSOR_UPDATE, 49 callers) fall
out the same way.

**How the enclosing function is found (2257).** A site's function is the greatest start at or
below it, over the union of: every function wow-re's ledgers size (`size=` on the `fn` rows of
`system/*/ledger.tsv`, ~13k verified extents), the `names-5875.tsv` starts, every `call` target in
the full disassembly (`system/ui/scratch/disasm-full.txt`), every registered verb, and every address
that follows a padding run — an `int3` run anywhere, or a `nop` run that follows a `ret` (MSVC's
inter-function padding; a `nop` run after a `jmp` is a loop head and is NOT a boundary, which is
the phantom-start trap the first derivation fell into). Then the extent check: a site that lies at
or past `start + size` of a sized function is left UNATTRIBUTED rather than handed to that
function. Validated against the ledgers: of ~26k padding-derived starts, 49 fall strictly inside a
sized function, all in a handful of oversized rows. The first derivation (2251) had neither the
extents nor the `ret`-`nop` rule, and four of its rows were wrong the same way: a fire site in an
item-cache *callback* reached only by function pointer (`0x4ce2f0`, `0x4cba50`), a site past a
verified extent (`0x4a7835` beyond `0x4a7620`'s 191 bytes), and a fire on a shared stop path past
its worker's 34 bytes (`0x48f095`). The two families dropped explicitly below are unchanged:
glue-space verbs (the GlueXML registrar; 0x46a000..0x476000, the range 2140's `GLUE_SITES` lie in)
and the unit-field bridge / token fan-out ids below 0xb6, which are state events by definition.

**A floor, not a census.** No row for a verb is not a claim that the verb fires nothing: the
fire-site census is over the two signal helpers' literal call sites (its header says so in
capitals), and `shape=helper` stops one call deep on purpose. Gate on what IS here.

The input lives outside the repo (the sibling RE repo), so this is a manual regeneration, like
`gen-reference-events.py` — not something CI can run.
"""
import argparse
import bisect
import collections
import glob
import os
import re
import sys

# The GlueXML registrar's verbs and fire sites (see the header): the same integer names a
# different event on the glue screen than in the world (2140 drops these too).
GLUE = (0x46A000, 0x476000)
# `0x51bbb0`'s walk bound: every named unit-window field below it is the generic bridge (2140).
UNIT_WINDOW_FIELDS = 0xB6
# A helper "only verbs call" is trusted up to this many callers; past it, it is a hub.
MAX_HELPER_CALLERS = 8

LINE = re.compile(r"^\s*([0-9a-f]+):\t(\S+)(?:\s+(.*))?$")
BRANCH = re.compile(r"^(call|jmp)\s+0x([0-9a-f]+)\s*$")
SIZE = re.compile(r"\bsize=(\d+)")


def rows(path):
    for line in open(path, encoding="utf-8"):
        if line.startswith("#"):
            continue
        f = line.rstrip("\n").split("\t")
        if f and f[0] in ("name", "addr", "eventId", "fire_site_va"):
            continue
        yield f


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--wow-re", default=os.path.expanduser("~/dev/wow-5875-re"))
    ap.add_argument(
        "--out",
        default=os.path.join(os.path.dirname(__file__), "..", "reference", "1.12-verb-events.tsv"),
    )
    a = ap.parse_args()

    starts = set()
    for f in rows(os.path.join(a.wow_re, "re/names/out/names-5875.tsv")):
        if len(f) >= 2 and f[1] == "func":
            starts.add(int(f[0], 16))
    if len(starts) < 1000:
        sys.exit(f"names-5875.tsv gave only {len(starts)} functions — wrong path?")

    # The verified extents: `size=` on the ledgers' `fn` rows (Ghidra's, via Closure.java).
    extent = {}  # fn start -> size in bytes
    for path in glob.glob(os.path.join(a.wow_re, "system/*/ledger.tsv")):
        for line in open(path, encoding="utf-8", errors="replace"):
            if not line.startswith("fn\t"):
                continue
            f = line.rstrip("\n").split("\t")
            m = SIZE.search(line)
            if m and len(f) > 1 and f[1].startswith("0x"):
                fn = int(f[1], 16)
                extent[fn] = max(extent.get(fn, 0), int(m.group(1)))
    if len(extent) < 5000:
        sys.exit(f"the ledgers gave only {len(extent)} sized functions — wrong path?")
    starts |= set(extent)

    verbs = {}  # fn address -> name
    for f in rows(os.path.join(a.wow_re, "re/audit/binding-shapes.tsv")):
        if len(f) < 2:
            continue
        fn = int(f[1], 16)
        if GLUE[0] <= fn < GLUE[1] or f[0].startswith("Get"):
            # A getter that fires an event is the cache-miss re-query (`GetInboxText` asking for
            # a body it does not hold yet, `GetAuctionItemInfo` for an item), announced when the
            # answer lands. That is a state event, and benilla's feeds fire it off the packet;
            # it is not a verb called FOR its event, which is the class this table is for.
            continue
        verbs.setdefault(fn, f[0])
    if len(verbs) < 1000:
        sys.exit(f"binding-shapes.tsv gave only {len(verbs)} verbs — wrong path?")
    starts |= set(verbs)

    sites = []  # (site va, event name)
    for f in rows(os.path.join(a.wow_re, "re/events/event-firesites.tsv")):
        if len(f) < 4 or not f[0].startswith("0x") or not f[3] or f[2] == "dyn":
            continue
        va, eid = int(f[0], 16), int(f[2])
        if GLUE[0] <= va < GLUE[1] or eid < UNIT_WINDOW_FIELDS:
            continue
        sites.append((va, f[3]))
    if len(sites) < 300:
        sys.exit(f"event-firesites.tsv gave only {len(sites)} sites — wrong path?")

    # One pass over the disassembly: every call/jmp edge, and every padding boundary — an `int3`
    # run anywhere, or a `nop` run right after a `ret`. Not a `nop` run after a `jmp`: that is a
    # loop head's alignment inside a function, and taking it as a start manufactured phantom
    # functions nothing calls, which silently dropped a third of the table once.
    edges = []
    after_pad = False
    ended = False  # the previous real instruction was a `ret`
    n_lines = 0
    for line in open(os.path.join(a.wow_re, "system/ui/scratch/disasm-full.txt"), encoding="utf-8", errors="replace"):
        m = LINE.match(line)
        if not m:
            continue
        n_lines += 1
        va, mnem, ops = int(m.group(1), 16), m.group(2), m.group(3) or ""
        if mnem == "int3" or (mnem == "nop" and (ended or after_pad)):
            after_pad = True
            continue
        if after_pad:
            starts.add(va)
            after_pad = False
        ended = mnem == "ret"
        b = BRANCH.match(f"{mnem} {ops}".strip())
        if b:
            edges.append((va, b.group(1), int(b.group(2), 16)))
    if n_lines < 1_000_000:
        sys.exit(f"disasm-full.txt gave only {n_lines} lines — wrong path?")
    starts |= {t for _, k, t in edges if k == "call"}
    ordered = sorted(starts)

    def enclosing(va):
        """The function holding `va`, or None when `va` lies past its candidate's verified extent."""
        fn = ordered[bisect.bisect_right(ordered, va) - 1]
        if fn in extent and va >= fn + extent[fn]:
            return None
        return fn

    callers = collections.defaultdict(set)
    for va, kind, target in edges:
        if kind == "call" or target in starts:  # a jmp to a function start is a tail call
            src = enclosing(va)
            if src is not None and src != target:
                callers[target].add(src)

    # (verb, event) -> {"shape", "via", "sites"}
    table = {}

    def add(verb, event, shape, via, site):
        row = table.setdefault((verb, event), {"shape": shape, "via": via, "sites": set()})
        if shape == "body":  # a body fire outranks a helper fire for the same pair
            row["shape"], row["via"] = "body", "-"
        row["sites"].add(site)

    unattributed = 0
    for va, event in sites:
        fn = enclosing(va)
        if fn is None:
            unattributed += 1
            continue
        if fn in verbs:
            add(verbs[fn], event, "body", "-", va)
            continue
        cs = callers.get(fn, set())
        if cs and len(cs) <= MAX_HELPER_CALLERS and all(c in verbs for c in cs):
            for c in cs:
                add(verbs[c], event, "helper", f"0x{fn:x}", va)

    out = os.path.normpath(a.out)
    with open(out, "w", encoding="utf-8") as fh:
        fh.write(
            "# The 1.12.1 client's FrameScript events fired FROM INSIDE A LUA VERB, keyed by the verb.\n"
            "# GENERATED — scripts/gen-reference-verb-events.py. Source: wow-5875-re's\n"
            "# re/audit/binding-shapes.tsv + re/events/event-firesites.tsv + re/names/out/names-5875.tsv\n"
            "# + system/ui/scratch/disasm-full.txt.\n"
            "#\n"
            "# shape  body   = a fire site inside the verb's own extent.\n"
            "#        helper = a fire site inside a function that ONLY registered verbs call\n"
            "#                 (at most 8 of them); `via` is that helper. A helper with any\n"
            "#                 non-verb caller is a state event's producer and is NOT here.\n"
            "# sites  the fire sites themselves, for the grep into wow-re.\n"
            "# A floor, not a census: no row is not a claim of no fire (see the generator's header).\n"
            "# Sites are attributed against the ledgers' verified function extents; a site past its\n"
            "# candidate's extent is left out rather than mis-filed (decision 2257).\n"
            f"# {len(table)} pairs over {len({v for v, _ in table})} verbs and"
            f" {len({e for _, e in table})} events; {unattributed} site(s) past a verified extent.\n"
            "verb\tevent\tshape\tvia\tsites\n"
        )
        for (verb, event), row in sorted(table.items()):
            fh.write(
                "\t".join(
                    (
                        verb,
                        event,
                        row["shape"],
                        row["via"],
                        "|".join(f"0x{s:x}" for s in sorted(row["sites"])),
                    )
                )
                + "\n"
            )
    print(
        f"wrote {out}: {len(table)} pairs, {len({v for v, _ in table})} verbs,"
        f" {len({e for _, e in table})} events"
    )


if __name__ == "__main__":
    main()
