#!/usr/bin/env python3
"""doc-links — fail on a doc link whose target does not exist ANYWHERE in the workspace.

WHY THIS IS NARROW, AND WHY THE OBVIOUS VERSION IS WRONG (decision 1925). The docs are this
project's knowledge base, so a doc link that goes nowhere is real rot — decision 1903 found a
`UNIT_FLAG_STUNNED` comment still pointing at a `MovementState::stunned` that a later decision had
deleted, and nothing caught it because nothing runs rustdoc.

The obvious fix is `-D rustdoc::broken_intra_doc_links`. Measured, it does not survive contact with
this codebase: **941 warnings, 264 of them unambiguous `crate::`/`super::`/`self::` paths** — and
almost none of that is rot.

  * **195** name a target that IS declared in its crate. They do not resolve because the target is
    a *private* module named through its parent (`crate::net::apply`, where `mod apply;` is private
    to `net`) — not a rustdoc quirk but Rust's own name resolution, so the link never worked.
    Making them resolve means widening visibility for a documentation reason, trading encapsulation
    for navigation.
  * **~50** are one crate naming another's module with `crate::` — `benilla-world` writing
    `crate::entities`, which is benilla-app's and which benilla-world cannot depend on. Those can
    never resolve; the only "fix" is de-linking prose that reads fine as prose.

Both are conventions this codebase writes on purpose. A gate on them would be 249 edits of churn
that improve nothing, and would then have to stay green forever.

What IS unambiguous rot is the residue: a link whose **leaf is declared nowhere in the workspace**
— the item is gone, so no reading of the link can be right. That was 15 sites when this was
written (1925 fixed them); this script keeps it at zero.

    scripts/doc-links.py            # the gate: exit 1 on any dead-target link
    scripts/doc-links.py --report   # + the benign counts, for anyone re-opening the question

The leaf test is a declaration grep, deliberately loose: a name declared anywhere counts, because
the failure this catches is "the thing was deleted", not "the path is spelled right".
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def run_rustdoc() -> str:
    out = subprocess.run(
        ["cargo", "doc", "--workspace", "--no-deps", "--document-private-items"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    return out.stderr + out.stdout


def declared_names() -> set[str]:
    """Every name the workspace declares, in ONE pass.

    A grep per link is what the first version did, and at ~900 links over a 19-crate tree it does
    not finish inside a gate's patience. One scan, then set lookups.
    """
    names: set[str] = set()
    # `fn` gets its own pattern: a single alternation swallows the `const` of `const fn foo` and
    # captures "fn" as the name, so `foo` is never indexed and the gate reports a live item as
    # deleted. That bug shipped for one run of this script and is the reason the two passes exist.
    pats = [
        re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)"),
        re.compile(r"\b(?:struct|enum|trait|type|const|static|union|mod)\s+([A-Za-z_][A-Za-z0-9_]*)"),
        re.compile(r"\bmacro_rules!\s+([A-Za-z_][A-Za-z0-9_]*)"),
    ]
    for rs in (ROOT / "crates").rglob("*.rs"):
        try:
            text = rs.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for pat in pats:
            names.update(pat.findall(text))
    names.discard("fn")
    return names


def main() -> int:
    report = "--report" in sys.argv
    text = run_rustdoc()
    found = re.findall(r"warning: unresolved link to `([^`]+)`\n\s*--> ([^\s:]+):(\d+)", text)

    names = declared_names()
    dead, benign = [], 0
    for target, path, line in found:
        # ONLY paths the author asserted are in THIS crate. A bare `[`SystemParam`]` or
        # `[`KeyboardInput`]` names a bevy type that is simply not imported for linking — real,
        # not rot — and there is no honest way to tell those from a deleted local name. A
        # `crate::`/`super::`/`self::`/`Self::` path carries the assertion, so its absence is
        # unambiguous.
        if not re.match(r"^(crate|super|self|Self)::", target):
            benign += 1
            continue
        leaf = target.split("::")[-1].split("(")[0].strip()
        if not leaf or not re.match(r"^[A-Za-z_][A-Za-z0-9_]*$", leaf):
            benign += 1
            continue
        if leaf in names:
            benign += 1
        else:
            dead.append((target, path, line))

    if report:
        print(f"doc-links: {len(found)} unresolved-link warnings")
        print(f"doc-links: {benign} name something that still exists (see this file's header)")

    if not dead:
        print(f"doc-links ok: no doc link points at a deleted item ({len(found)} benign)")
        return 0

    print(f"doc-links FAILED: {len(dead)} doc link(s) point at something that no longer exists:\n")
    for target, path, line in dead:
        print(f"  {path}:{line}\n      [`{target}`]")
    print(
        "\nEach names an item declared nowhere in the workspace. Repoint it at whatever replaced\n"
        "the thing, or drop the brackets — a dangling link promises navigation that is not there."
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
