#!/usr/bin/env python3
"""Regenerate `reference/1.12-globals.tsv` — the 1.12.1 client's global namespace, with each name
attributed to whoever provides it.

    WOW_RE=<the RE repo> scripts/gen-reference-globals.py [--wow-re DIR] [--out FILE]

**Why this exists** (decision 1189): every wrong number in the addon arc came from remembering a
list of WoW functions instead of asking the client. wow-5875-re already ships an addon
(`W5875Capture`) that dumps the in-world `_G` of the *running* reference client, and its output is
committed there as a fixture. This vendors that capture, attributes every name, and writes the
result as the one artifact benilla measures its API surface against.

**Three origins**, because "is this in 1.12?" and "whose job is it to provide?" are different
questions:

  lua       Lua 5.0's own runtime. We get these from mlua; neither we nor FrameXML write them.
  engine    The C client provides it. **This is the list benilla implements in Rust.**
  framexml  Defined by the shipped UI itself — its Lua, its XML, or a `$parent`-composed child of
            one. Runs off the player's own patch chain at runtime (1751), never hardcoded in Rust.

Attribution is by *definition site*, computed from a complete 1.12 shipped-UI corpus:

  - a name assigned or `function`-declared in shipped Lua (including a `<Script>` body inside a
    FrameXML document) is FrameXML's. Indented assignments count: vanilla FrameXML leaks globals
    out of function bodies (`button = getglobal(...)`) and those names really are in `_G`;
  - a `name="..."` on any shipped XML element is FrameXML's — including `virtual="true"`, because
    a virtual `<Font>` is a real font object and 1.12 registers named virtual frames too;
  - a name formed as `<a shipped name><a $parent suffix>` is a composed child object
    (`ContainerFrame1Item16IconTexture`), so also FrameXML's;
  - LUA_5_0 below wins over all of the above: FrameXML *clobbers* `string` in three files
    (`string = getglobal(...)`), which overwrites the stdlib table rather than defining it.

**Inputs live outside the repo** — a 1.12 install and the RE repo's runtime capture (`--wow-re`,
or `$WOW_RE`) — so this is a manual regeneration, like `genmap.sh`; the committed table is the
surface benilla tracks. It needs:

  - `<wow-re>/WoW/_w5875_fixtures/item13/W5875Capture.lua`  the captured in-world `_G`
  - `<wow-re>/WoW/_extracted_framexml/`                     FrameXML, already extracted there
  - `<wow-re>/WoW/Data/*.MPQ`                               for the twelve `Blizzard_*` addons

**The corpus is the part that is easy to get wrong** (decision 1188 §4 — three attempts were
needed, and the first two produce confidently wrong answers). FrameXML alone is not the shipped
UI: the twelve `Blizzard_*` addons live in the MPQs, and their `.lua` is *not* listed in their
`.toc` — it is pulled in by `<Script file=>` inside their XML. A corpus that misses either half
misattributes real FrameXML functions as the engine's. A complete one is 233 files, and this
script prints the count so a short one is visible rather than silent.
"""
import argparse
import os
import re
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The twelve addons that ship inside the MPQs. Their folders exist in a real install carrying only
# a `.pub` signature file, so the install cannot be the source — the archives are.
BLIZZARD_ADDONS = [
    "Blizzard_AuctionUI", "Blizzard_BattlefieldMinimap", "Blizzard_BindingUI",
    "Blizzard_CombatText", "Blizzard_CraftUI", "Blizzard_GMSurveyUI",
    "Blizzard_InspectUI", "Blizzard_MacroUI", "Blizzard_RaidUI",
    "Blizzard_TalentUI", "Blizzard_TradeSkillUI", "Blizzard_TrainerUI",
]

# Lua 5.0's own globals, as the 1.12 client actually exposes them. Note what is NOT here and is
# absent from the capture: `_G`, `print`, `require`, `dofile`, `loadfile`, and the `io`/`os`/
# `debug`/`coroutine` tables — the client sandboxes them away, which is a fidelity fact about the
# VM we present to addons, not an accident of the capture.
LUA_5_0 = {
    "assert", "collectgarbage", "error", "gcinfo", "getfenv", "getmetatable", "ipairs",
    "loadstring", "math", "next", "pairs", "pcall", "rawequal", "rawget", "rawset",
    "setfenv", "setmetatable", "string", "table", "tonumber", "tostring", "type",
    "unpack", "xpcall",
}

# The capture addon's own footprint, each verified against its source rather than guessed at.
# `W5875CaptureHolder` is its `CreateFrame(..., "W5875CaptureHolder", ...)`, `W5875CaptureDB` its
# `## SavedVariables`, `SLASH_W5875CAP1` its slash token. It ran inside the client to produce the
# fixture, so its globals are in the table it dumped; they are the instrument, not the client.
#
# `__framescript_meta` is deliberately NOT here despite looking like tooling: it is the *client's*
# own shared frame metatable, published to `_G` (wow-5875-re RF-0023, `system/ui/ui.md`), and
# benilla mirrors it at `script/object.rs`. Excluding it on the strength of its underscores made
# our own faithful global read as a superset.
CAPTURE_OWN = {"W5875CaptureDB", "W5875CaptureHolder", "SLASH_W5875CAP1"}

LUA_ASSIGN = re.compile(r"^[ \t]*([A-Za-z_][A-Za-z0-9_]*)\s*=[^=]", re.M)
LUA_FUNC = re.compile(r"^[ \t]*function\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(", re.M)
LUA_MULTI = re.compile(
    r"^[ \t]*([A-Za-z_][A-Za-z0-9_]*(?:\s*,\s*[A-Za-z_][A-Za-z0-9_]*)+)\s*=[^=]", re.M
)
XML_ELEM = re.compile(r"<(\w+)([^>]*?)/?>", re.S)
XML_NAME = re.compile(r'\bname\s*=\s*"([^"]*)"')
PARENT_SUFFIX = re.compile(r'\bname\s*=\s*"\$parent([A-Za-z0-9_]*)"')
# Lua embedded in a FrameXML document, which only this pass will ever see. Two shapes, and
# missing either misattributes real FrameXML names to the engine: an inline `<Script>…</Script>`
# body (Fonts.xml defines CHAT_FONT_HEIGHTS that way), and every handler element under
# `<Scripts>` — `<OnLoad>`, `<OnEvent>`, `<PreClick>` and kin, which is where MainMenuBar.xml
# assigns SHOW_KEYRING and PERFORMANCEBAR_LOW_LATENCY. Matched by shape rather than by a list of
# handler names so a rarely-used one cannot be forgotten. `<Script file=>` is skipped: it
# references a file already in the corpus.
XML_SCRIPT = re.compile(
    r"<((?:On|Pre|Post)\w+|Script)\b(?![^>]*\bfile\s*=)[^>]*>(.*?)</\1>", re.S
)
XML_REF = re.compile(rb'<(?:Script|Include)\s+file\s*=\s*"([^"]+)"', re.I)


def captured_globals(fixture):
    """`["globals"]["keys"]` out of the capture: the in-world `_G`, as (name, lua type)."""
    recs, cur_t, inside = [], None, False
    with open(fixture, encoding="utf-8", errors="replace") as f:
        for line in f:
            if not inside:
                inside = line.strip() == '["globals"] = {'
                continue
            if '["count"]' in line:
                break
            m = re.match(r'\s*\["t"\] = "([^"]*)",', line)
            if m:
                cur_t = m.group(1)
                continue
            m = re.match(r'\s*\["name"\] = "(.*)",\s*$', line)
            if m:
                recs.append((m.group(1), cur_t))
                cur_t = None
    return recs


def join_ref(base, ref):
    """Resolve a reference against the including file's directory — `loader::join_ref`'s rule."""
    ref = ref.replace("\\", "/").strip()
    parts = ref[1:].split("/") if ref.startswith("/") else (base.split("/") if base else []) + ref.split("/")
    out = []
    for p in parts:
        if p in ("", "."):
            continue
        if p == ".." and out and out[-1] != "..":
            out.pop()
        else:
            out.append(p)
    return "/".join(out)


def build_corpus(wowre):
    """Every file of the 1.12 shipped UI, as {corpus-relative path: text}."""
    archives = [f"{wowre}/WoW/Data/{a}.MPQ" for a in ("patch-2", "patch", "interface", "base")]
    archives = [a for a in archives if os.path.exists(a)]
    mpqcat = os.path.join(REPO, "target", "debug", "examples", "mpqcat")
    if not os.path.exists(mpqcat):
        subprocess.run(
            ["cargo", "build", "-q", "-p", "benilla-mpq", "--example", "mpqcat"],
            cwd=REPO, check=True,
        )

    def mpq_read(archive_path):
        for arc in archives:
            r = subprocess.run([mpqcat, arc, archive_path], capture_output=True)
            if r.returncode == 0:
                return r.stdout
        return None

    files = {}
    fx = os.path.join(wowre, "WoW", "_extracted_framexml")
    for fn in sorted(os.listdir(fx)):
        p = os.path.join(fx, fn)
        if os.path.isfile(p):
            files[f"FrameXML/{fn}"] = open(p, "rb").read()
    print(f"  FrameXML: {len(files)} files")

    for addon in BLIZZARD_ADDONS:
        toc_rel = f"{addon}/{addon}.toc"
        toc = mpq_read("Interface\\AddOns\\" + toc_rel.replace("/", "\\"))
        if toc is None:
            print(f"  !! {addon}: no .toc in any archive", file=sys.stderr)
            continue
        files[toc_rel] = toc
        queue, seen = [], set()
        for line in toc.decode("utf-8", "replace").splitlines():
            line = line.strip()
            if line and not line.startswith("#"):
                queue.append(join_ref(addon, line))
        while queue:
            rel = queue.pop(0)
            if rel in seen:
                continue
            seen.add(rel)
            data = mpq_read("Interface\\AddOns\\" + rel.replace("/", "\\"))
            if data is None:
                print(f"  !! {addon}: missing {rel}", file=sys.stderr)
                continue
            files[rel] = data
            # The `.lua` is reached from inside the XML, never from the `.toc` — follow it, or
            # every Lua-side API in these twelve addons misattributes to the engine.
            if rel.lower().endswith(".xml"):
                base = rel.rsplit("/", 1)[0] if "/" in rel else ""
                for m in XML_REF.finditer(data):
                    queue.append(join_ref(base, m.group(1).decode("utf-8", "replace")))
        print(f"  {addon}: {1 + len(seen)} files")
    return {k: v.decode("utf-8", "replace") for k, v in files.items()}


def lua_defs(text, into):
    into.update(LUA_ASSIGN.findall(text))
    into.update(LUA_FUNC.findall(text))
    for grp in LUA_MULTI.findall(text):
        into.update(n.strip() for n in grp.split(","))


def shipped_names(corpus):
    """Every global the shipped UI defines, plus the `$parent` suffixes children compose with."""
    names, suffixes = set(), set()
    for rel, text in corpus.items():
        if rel.lower().endswith(".lua"):
            lua_defs(text, names)
        elif rel.lower().endswith(".xml"):
            for _tag, body in XML_SCRIPT.findall(text):
                lua_defs(body, names)
            suffixes.update(s for s in PARENT_SUFFIX.findall(text) if s)
            for tag, attrs in XML_ELEM.findall(text):
                # `<Binding name="ACTIONBUTTON1">` is a KEY-BINDING command name, not a global —
                # it never reaches `_G`. It shares the `name=` attribute with every frame element,
                # so a shape-blind harvest pulls all 228 of `Bindings.xml`'s commands in. Harmless
                # while this set was only used for *attribution* (none of them is in the capture,
                # so none could be misattributed); wrong now that the set also contributes rows
                # (decision 1200).
                if tag.lower() == "binding":
                    continue
                m = XML_NAME.search(attrs)
                if m and "$parent" not in m.group(1):
                    names.add(m.group(1))
    return names, suffixes


def composed(name, roots, suffixes, depth=0):
    """Is `name` `<a shipped name><a $parent suffix>`, recursively? A numeric tail on the parent
    side is allowed: `ContainerFrame1` and `Item16` come from Lua's `name..i`."""
    if depth > 8:
        return False
    for i in range(1, len(name)):
        head, tail = name[:i], name[i:]
        if tail not in suffixes:
            continue
        if head in roots or head.rstrip("0123456789") in roots:
            return True
        if composed(head, roots, suffixes, depth + 1):
            return True
    return False


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--wow-re", default=os.environ.get("WOW_RE"))
    ap.add_argument("--out", default=os.path.join(REPO, "reference", "1.12-globals.tsv"))
    args = ap.parse_args()
    if not args.wow_re:
        sys.exit(
            "this table is derived from the RE repo's runtime capture of the reference client's "
            "_G: pass --wow-re or set WOW_RE. The committed reference/1.12-globals.tsv is the "
            "surface benilla tracks."
        )

    fixture = os.path.join(args.wow_re, "WoW", "_w5875_fixtures", "item13", "W5875Capture.lua")
    if not os.path.exists(fixture):
        sys.exit(f"no capture fixture at {fixture} — is --wow-re right?")

    print("building the 1.12 shipped-UI corpus...")
    corpus = build_corpus(args.wow_re)
    print(f"  TOTAL: {len(corpus)} files" + ("" if len(corpus) == 233 else "  ** expected 233 **"))

    recs = [(n, t) for n, t in captured_globals(fixture) if n not in CAPTURE_OWN]
    defined, suffixes = shipped_names(corpus)
    print(f"captured globals: {len(recs)}   shipped definitions: {len(defined)}   "
          f"$parent suffixes: {len(suffixes)}")

    rows, tally = [], {}
    captured = set()
    for name, t in recs:
        captured.add(name)
        if name in LUA_5_0:
            origin = "lua"
        elif name in defined or composed(name, defined, suffixes):
            origin = "framexml"
        else:
            origin = "engine"
        rows.append((name, t, origin))
        tally[(origin, t)] = tally.get((origin, t), 0) + 1

    # ── The LoadOnDemand half the capture could not see (decision 1200) ──────────────────────
    #
    # The fixture is the in-world `_G` of a *running* client, and the twelve `Blizzard_*` addons
    # are LoadOnDemand: unless the player had opened the talent window, the trade-skill window and
    # the rest before the dump, their globals are not in it. `TalentFrame`, `MacroFrame`,
    # `TradeSkillFrame`, `CraftFrame`, `TrainerFrame`, `InspectFrame` and ~350 of their children
    # are real 1.12 names that the capture alone reports as absent — which made benilla's own
    # transcriptions of those windows look like they were inventing names.
    #
    # So the table is the capture UNION every global the shipped UI *defines*. That is the honest
    # membership test for the question we actually ask it — "is this a 1.12 name?" — and it is
    # strictly better than the capture alone, which answers "was this name live at one moment in
    # one session". The two halves stay distinguishable: a name only the corpus knows gets type
    # `lod`, so a consumer that cares can tell.
    #
    # Type inference is deliberately coarse: a `function X(` declaration is a `function`, an XML
    # `name=` is a frame (`table`), and a bare assignment could be anything, so it says `lod`
    # rather than guessing. Nothing downstream keys on the type of these rows.
    lod = 0
    for name in sorted(defined - captured):
        rows.append((name, "lod", "framexml"))
        tally[("framexml", "lod")] = tally.get(("framexml", "lod"), 0) + 1
        lod += 1
    print(f"shipped-UI definitions the capture did not contain: {lod} (LoadOnDemand + unrealized)")

    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as f:
        f.write("# The 1.12.1 client's global namespace. GENERATED — scripts/gen-reference-globals.py.\n")
        f.write("# Source: wow-5875-re's W5875Capture fixture (the running client's in-world _G),\n")
        f.write("# attributed against the complete 1.12 shipped-UI corpus. Decision 1189.\n")
        f.write("# UNION every name that corpus defines but the capture did not contain (type\n")
        f.write("# `lod`): the twelve Blizzard_* addons are LoadOnDemand, so a live dump misses\n")
        f.write("# them unless the player had opened those windows. Decision 1200.\n")
        f.write("# name\ttype\torigin(lua|engine|framexml)\n")
        for row in sorted(rows):
            f.write("\t".join(row) + "\n")

    print(f"\nwrote {args.out}  ({len(rows)} names)")
    for k in sorted(tally):
        print(f"  {k[0]:9s} {k[1]:9s} {tally[k]}")


main()
