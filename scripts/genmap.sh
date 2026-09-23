#!/usr/bin/env bash
# Generate docs/MAP.md — the "what's built" map — derived entirely from what's on disk so it cannot
# drift (docs/METHOD.md / docs/METHOD.md: "the generated map … regenerated from the code, never
# hand-written"). It is regenerated and committed whenever a change lands (decision 2049); run
# it by hand only to LOOK, never commit its output yourself, and never edit it by hand.
# Deterministic: identical tree → identical output (no timestamps), so a diff means the structure
# actually changed.
# No `pipefail`/`-e`: greps that legitimately find nothing (a single-file lib, a crate with no bins)
# must not abort the generator.
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
    # `bevy.workspace = true` is the form every crate here actually uses, and the old pattern
    # (`^bevy[[:space:]]*=`) matched only `bevy =` — so workspace-inherited deps read "no Bevy".
    # benilla-assets shipped mislabelled long enough to say "(no Bevy) — Bevy AssetSource +
    # AssetLoaders" in one line. The trailing class accepts `.`, whitespace and `=`, which covers
    # `bevy.workspace =`, `bevy = { … }` and `bevy="…"`, while still rejecting a `bevyfoo` crate.
    # Load-bearing beyond tidiness: "does this crate need Bevy" is the seam question 0068 cut
    # `benilla-ui` on and 1160 cuts `benilla-world` on, so a map that lies about it misleads
    # exactly the work that reads it.
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

  # The two dev groups are one `add_plugins` line each in `lib.rs` (decisions 1173/1174), so the
  # list above names the group and not what is in it. Read their members out of `dev.rs` — a map
  # that stops naming the instruments the moment they move behind the `dev` feature is exactly the
  # drift this file exists to prevent.
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
  # Both spellings cargo accepts: `src/bin/<name>.rs` and the directory form `src/bin/<name>/main.rs`
  # (benilla-extract, benilla-world) — the file-only glob listed three of five for months.
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

  # Every shader in the tree is compiled into the binary and addressed by the crate that owns it
  # (decision 1175), so the crate is part of the shader's name now — list them all, per crate.
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
  # Don't name individual dev keys here — this line has gone stale twice (it still said 'P perf'
  # after 0585 moved P onto the chord, and 'backtick panel' after 1043 moved the last two). The
  # keys live in one place, \`debug_panel::DEV_CHORD\` and the panel footer; name the plane only.
  echo "> semantics. (The in-window surfaces — the Ctrl+Shift dev-chord overlays — are plugins"
  echo "> above; this indexes the switches that don't announce themselves.)"
  echo
  # Any quoted "WOW_*" literal in code — reads go through env::var but also through helpers
  # (`knob("WOW_FX_AGE", …)`), so match the literal, not the call. Doc comments write the
  # backticked/`$`-prefixed form, so they don't false-positive.
  # The probe registry (`capture/probe_env.rs`, 2266 §A5) names every WOW_PROBE* variable as a
  # quoted literal too; it is the table, not a read site, so it is not a place a switch is used.
  grep -rHoE '"WOW_[A-Z0-9_]+"' crates --include='*.rs' 2>/dev/null \
    | grep -v '^crates/benilla-app/src/capture/probe_env\.rs:' \
    | sed -E 's@^crates/@@; s/:"/\t/; s/"$//' \
    | awk -F'\t' '{print $2 "\t" $1}' | sort -u \
    | awk -F'\t' '$1 != v { if (v) print line; v = $1; line = "- `" $1 "` — " $2; next }
                  { line = line ", " $2 }
                  END { if (v) print line }'
  echo

  # Every script, with the first sentence of its own header (2331). The scripts are the
  # instruments the WOW_* switches are read WITH — a trace reader, a two-client probe, a corpus
  # census — and a 2026-09-22 sweep found eight of them referenced by nothing but the record that
  # built them: findable only by someone who remembered the name. This is the index; the header
  # is the source, so it cannot drift. (`winlab/` is the Windows lab laptop's, 2205/2211.)
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
