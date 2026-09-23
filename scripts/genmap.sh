#!/usr/bin/env bash
# Generate docs/MAP.md, the map of what is built, from what is on disk.
#
# Regenerated and committed at every land; run it by hand only to look, never edit the output. No
# timestamps, so an identical tree gives an identical map. No `pipefail`/`-e`: a grep that finds
# nothing (a single-file lib, a crate with no bins) must not abort the generator.
set -u
cd "$(dirname "$0")/.."

# Extract a quoted `key = "value"` field from a Cargo.toml.
field() { grep -m1 "^$2" "$1" 2>/dev/null | sed -E "s/^$2[[:space:]]*=[[:space:]]*\"//; s/\".*$//"; }

{
  echo "# benilla — generated map"
  echo
  echo "> **GENERATED** by \`scripts/genmap.sh\` from the code — do not edit by hand; rerun the script"
  echo "> when what's built changes. *What-is* only: the *what-changed* is in git. No timestamp —"
  echo "> identical code yields an identical map."
  echo

  echo "## Crates"
  echo
  for ct in crates/*/Cargo.toml; do
    name=$(field "$ct" name)
    desc=$(field "$ct" description)
    # Crates here write `bevy.workspace = true`: the trailing class accepts `.`, whitespace and `=`
    # (`bevy = { … }`, `bevy="…"` too) and still rejects a `bevyfoo` crate.
    if grep -qE '^(bevy|bevy_egui|avian3d)[.[:space:]=]' "$ct"; then bevy="Bevy"; else bevy="no Bevy"; fi
    echo "- **$name** ($bevy) — ${desc:-—}"
  done
  echo

  echo "## App subsystems — Bevy plugins in load order (\`crates/benilla-app/src/lib.rs\`)"
  echo
  # Accept bare (`add_plugins(FooPlugin)`) and path-qualified (`add_plugins(foo::FooPlugin)`)
  # registrations; print the plugin type name either way.
  grep -oE 'add_plugins\(([a-z_]+::)*[A-Za-z_]+Plugins?' crates/benilla-app/src/lib.rs \
    | sed -E 's/add_plugins\(//; s/([a-z_]+::)+//' | awk '!seen[$0]++ {print "- " $0}'
  echo

  # `lib.rs` adds each dev group in one line, so their members are read out of `dev.rs`.
  echo "### Behind the \`dev\` feature (\`crates/benilla-app/src/dev.rs\` — compiled out by \`--no-default-features\`)"
  echo
  grep -oE 'add_plugins\(([a-z_]+::)*[A-Za-z_]+Plugins?' crates/benilla-app/src/dev.rs \
    | sed -E 's/add_plugins\(//; s/([a-z_]+::)+//' | awk '!seen[$0]++ {print "- " $0}'
  echo

  echo "## Modules (top-level, per crate)"
  echo
  for lib in crates/*/src/lib.rs crates/*/src/main.rs; do
    [ -f "$lib" ] || continue
    crate=$(echo "$lib" | sed -E 's@crates/([^/]+)/.*@\1@')
    mods=$(grep -E '^[[:space:]]*(pub(\([a-z]+\))? )?mod [a-z_0-9]+;' "$lib" \
      | sed -E 's/.*mod ([a-z_0-9]+);.*/\1/' | sort | tr '\n' ' ')
    [ -n "$mods" ] && echo "- **$crate** ($(basename "$lib")): $mods"
  done
  echo

  echo "## CLI binaries"
  echo
  # Both forms cargo accepts: `src/bin/<name>.rs` and `src/bin/<name>/main.rs`.
  for b in crates/*/src/bin/*.rs crates/*/src/bin/*/main.rs; do
    [ -f "$b" ] || continue
    crate=$(echo "$b" | sed -E 's@crates/([^/]+)/.*@\1@')
    case "$b" in
      */main.rs) name=$(basename "$(dirname "$b")") ;;
      *) name=$(basename "$b" .rs) ;;
    esac
    echo "- \`$name\` (in $crate)"
  done | LC_ALL=C sort
  echo

  # Shaders are embedded and addressed by their owning crate, so the crate is part of the name.
  echo "## WGSL shaders (\`crates/*/src/shaders/\`, embedded)"
  echo
  for s in crates/*/src/shaders/*.wgsl; do
    [ -f "$s" ] || continue
    crate=$(echo "$s" | sed -E 's@crates/([^/]+)/.*@\1@' | tr - _)
    echo "- \`embedded://$crate/shaders/$(basename "$s")\`"
  done
  echo

  echo "## Instruments — \`\$WOW_*\` switches (env-var read sites)"
  echo
  echo "> The headless/dev instrument fleet, discovered from the code: every \`WOW_*\` env var"
  echo "> some \`.rs\` reads, and where it's read — the doc comment at the read site is the"
  # Name no dev keys here: they are listed once, in the debug panel's footer.
  echo "> semantics. (The in-window surfaces — the Ctrl+Shift dev-chord overlays — are plugins"
  echo "> above; this indexes the switches that don't announce themselves.)"
  echo
  # Match any quoted "WOW_*" literal: reads also go through helpers (`knob("WOW_FX_AGE", …)`), and
  # doc comments write the unquoted form. The probe registry `capture/probe_env.rs` is a table,
  # not a read site, so it is left out.
  grep -rHoE '"WOW_[A-Z0-9_]+"' crates --include='*.rs' 2>/dev/null \
    | grep -v '^crates/benilla-app/src/capture/probe_env\.rs:' \
    | sed -E 's@^crates/@@; s/:"/\t/; s/"$//' \
    | awk -F'\t' '{print $2 "\t" $1}' | sort -u \
    | awk -F'\t' '$1 != v { if (v) print line; v = $1; line = "- `" $1 "` — " $2; next }
                  { line = line ", " $2 }
                  END { if (v) print line }'
  echo

  # Every script, with the first sentence of its own header.
  echo "## Scripts (\`scripts/\`)"
  echo
  for f in scripts/*.py scripts/*.sh; do
    [ -f "$f" ] || continue
    # The summary: the header's first non-empty content after the shebang, comment or docstring
    # markers stripped, joined until its first blank line, cut at a sentence end or ~150 chars.
    summary=$(LC_ALL=C awk '
      NR == 1 && /^#!/ { next }
      /^@echo off/ { next }
      { line = $0
        sub(/^[[:space:]]*(r?"""|#|\/\/|rem)[[:space:]]?/, "", line)
        if (line ~ /^[[:space:]]*$/) { if (got) exit; else next }
        if (line ~ /^"""/) exit
        got = 1; out = out (out ? " " : "") line }
      END { print out }' "$f" | sed -E 's/\*\*//g; s/^[A-Za-z0-9_.\/-]+ (—|--) //; s/^([^.]*[.!?])([[:space:]]|$).*/\1/; s/^(.{150}).+/\1…/')
    echo "- \`${f#scripts/}\` — $summary"
  done
  echo

} > docs/MAP.md

echo "genmap: wrote docs/MAP.md"
