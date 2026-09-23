#!/usr/bin/env bash
# The addon-API coverage instrument (decisions 1178 §, 1188 phase 0, 1189) — how much of the
# 1.12.1 client's global surface benilla presents, measured on any day.
#
#   scripts/api-coverage.sh              # the report
#   scripts/api-coverage.sh --missing    # + every engine global we do not have
#   scripts/api-coverage.sh --beyond     # + every global we have that 1.12 does not
#
# **Both sides are asked, not remembered.** The 1.12 side is `reference/1.12-globals.tsv`, the
# running reference client's own in-world `_G` (regenerate: scripts/gen-reference-globals.py). Our
# side is a real `UiScript::new()` dumped through `pairs(_G)`. The previous version of this script
# inferred both by pattern-matching source — that is how the arc got "54 C_Container references"
# (a grep over Rust, not API surface) and the 124-then-41 corpus estimates, all wrong. When the
# question is what a running system exposes, ask the running system.
#
# It also no longer measures Bagnon/Questie/WeakAuras out of the Era install: 1188 settled the
# target as 1.12.1 and the vanilla ecosystem, so an Era addon's call sites are the wrong client's
# demand. The real-addon half of the question belongs to the vanilla addon harness (1188 phase 6).
#
# **The addon corpora are not 1.12 codebases, and that shapes what a grep over them means.**
# The vanilla addon corpus (`benilla_formats::addon_corpus`) holds addons that RUN on 1.12 — which is
# not the same as addons written for 1.12 and nothing else. Most of the big ones ship one codebase
# for several clients and pick a path at load: pfUI registers every module with a version list
# (`RegisterModule("loot", "vanilla:tbc", …)`, matched against `pfUI.expansion`), pfQuest keeps a
# whole `compat/client.lua` off `GetBuildInfo`, and `libs/` directories carry vendored libraries
# with their own client targets. Addons also come in several versions of themselves — a corpus is one snapshot of each.
#
# So a call site found by grep may be TBC-era code that a 1.12 client reaches and raises on, an
# addon calling a name it defines itself under its own namespace, or genuinely 1.12 demand. The
# three read very differently and look identical to `grep -rn`. Decision 2146 is the case that
# made the point: six 2.0 globals were kept here for years on the strength of call sites that
# turned out to be all of the first two kinds.
#
# **Never quote the percentage undifferentiated** (1178's rule, and 1188 restates it). A missing
# global is one of three different things, and only reading the list tells you which:
#   · a verb for a feature benilla has not built at all      → not a gap, a backlog item
#   · a verb missing from a feature benilla ships            → a real hole, fix it
#   · a name we have that 1.12 does not                      → a superset, and not free
# The last one is the one this script insists on printing in full: an addon that feature-detects
# (`if strmatch then`) takes a path we cannot honour, and the failure surfaces far from the cause.
set -u
cd "$(dirname "$0")/.."

REF="reference/1.12-globals.tsv"
[ -f "$REF" ] || { echo "no $REF — run scripts/gen-reference-globals.py" >&2; exit 1; }

show_missing=0
show_beyond=0
for a in "$@"; do
  case "$a" in
    --missing) show_missing=1 ;;
    --beyond)  show_beyond=1 ;;
    *) echo "usage: $0 [--missing] [--beyond]" >&2; exit 2 ;;
  esac
done

ours=$(mktemp) || exit 1
trap 'rm -f "$ours"' EXIT
echo "asking benilla's VM what it exposes..." >&2
if ! cargo run -q -p benilla-ui --example dump_globals >"$ours" 2>/dev/null; then
  echo "dump_globals failed — build the workspace first" >&2
  exit 1
fi

awk -F'\t' -v ref="$REF" -v show_missing="$show_missing" -v show_beyond="$show_beyond" '
  # ── the reference side ──────────────────────────────────────────────────────────────────────
  FNR == NR {
    if ($0 ~ /^#/) next
    origin[$1] = $3
    n_ref[$3]++
    if ($2 == "function") n_ref_fn[$3]++
    next
  }
  # ── our side ───────────────────────────────────────────────────────────────────────────────
  {
    ours[$1] = $2
    n_ours++
    o = ($1 in origin) ? origin[$1] : "beyond"
    if (o == "engine" || o == "lua") have++
    else if (o == "framexml") transcribable[$1] = 1
    else {
      # Categorize the superset. Only the last bucket is an API-target question; the first two are
      # benilla being benilla, and the third is our Lua runtime being 5.1 where 1.12 is 5.0.
      if ($1 ~ /^Benilla/)                                    bridge[$1] = 1
      else if ($1 ~ /^__/)                                    internal[$1] = 1
      else if ($1 ~ /^(_G|_VERSION|coroutine|print|select)$/)  lua51[$1] = 1
      else                                                     api[$1] = 1
    }
  }
  END {
    surface = n_ref["engine"] + n_ref["lua"]
    missing = surface - have
    pct = surface ? sprintf("%.0f%%", 100 * have / surface) : "-"
    printf "\nthe 1.12.1 surface — %s (%d names: the running client'\''s _G, plus what the\n                       shipped UI defines behind a LoadOnDemand window — decision 1200)\n", ref, \
      n_ref["engine"] + n_ref["framexml"] + n_ref["lua"]
    printf "  engine    %5d functions, %5d other   <- benilla implements these in Rust\n", \
      n_ref_fn["engine"], n_ref["engine"] - n_ref_fn["engine"]
    printf "  framexml  %5d functions, %5d other   <- benilla transcribes these into assets/ui\n", \
      n_ref_fn["framexml"], n_ref["framexml"] - n_ref_fn["framexml"]
    printf "  lua       %5d functions, %5d other   <- mlua provides these\n", \
      n_ref_fn["lua"], n_ref["lua"] - n_ref_fn["lua"]

    printf "\nbenilla'\''s VM — %d globals (UiScript::new(), asked at runtime)\n\n", n_ours
    printf "1.12 globals: %d   we have: %d (%s)   missing: %d   beyond-1.12: %d (listed)\n", \
      surface, have, pct, missing, length(api)
    print  "  ^ never quote this without the split: unbuilt-feature vs missing-verb vs superset."

    printf "\nbeyond 1.12, by kind — every one of these is a deliberate exception or a bug:\n"
    printf "  %3d  benilla host bridge      Benilla*, called only by our own FrameXML\n", length(bridge)
    printf "  %3d  VM internals             __benilla_*, pushed by the tick\n", length(internal)
    printf "  %3d  Lua 5.1 past 1.12'\''s 5.0  ", length(lua51); dump(lua51, "")
    printf "  %3d  WoW API past 1.12        the phase-5 list\n", length(api)
    printf "  %3d  ours in Rust that 1.12 defines in FrameXML\n", length(transcribable)
    dump(transcribable, "       ")

    if (show_beyond || length(api) <= 30) {
      printf "\nWoW API beyond 1.12 (%d):\n", length(api)
      dump(api, "  ")
    }
    if (show_missing) {
      printf "\nmissing engine globals (%d):\n", missing
      for (k in origin)
        if ((origin[k] == "engine" || origin[k] == "lua") && !(k in ours)) miss[k] = 1
      dump(miss, "  ")
    }
    print ""
  }
  # Print a set as wrapped, sorted, space-separated names.
  function dump(set, indent,   k, sorted, i, n, line) {
    n = 0
    for (k in set) sorted[++n] = k
    asort_names(sorted, n)
    line = indent
    for (i = 1; i <= n; i++) {
      if (length(line) + length(sorted[i]) + 1 > 96) { print line; line = indent }
      line = line sorted[i] " "
    }
    if (line != indent) print line
  }
  # Insertion sort — `asort` is a gawk extension and macOS ships BSD awk.
  function asort_names(a, n,   i, j, t) {
    for (i = 2; i <= n; i++) {
      t = a[i]
      for (j = i - 1; j >= 1 && a[j] > t; j--) a[j + 1] = a[j]
      a[j + 1] = t
    }
  }
' "$REF" "$ours"
