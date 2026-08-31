//! **What kind of run is this, and whose** — the always-present layer, on the player's side of the
//! `dev` seam (decisions 0026 / 1173, built in 1174).
//!
//! Everything here answers a question about *the run itself*, and every answer has a
//! **player-faithful default**: nobody is driving the camera, the client starts at the login
//! screen, no rig owns the character pick, this checkout belongs to nobody in particular. A dev
//! build's instruments then move those answers off their defaults — which is the one direction the
//! seam allows. Gameplay reads this module; gameplay never reads `capture`, `debug_panel` or
//! `perf`.
//!
//! It exists because the alternative kept losing. 0026 asked for "a player-safe home or default"
//! for exactly this class of fact in June 2026 and named two of them; by August there were 24
//! references from non-dev code into the instruments (1173), because the facts had no home and the
//! nearest thing that already knew the answer was the instrument itself.
//!
//! The engine drew the same line for itself one layer down: [`benilla_world::dev_state`]'s
//! `deterministic_run` reads `$WOW_CAPTURE` on its own account, because "run deterministically" is
//! a property of the world and the world must be able to ask it with no harness above it at all.
//! **One environment variable, several readers, no shared symbol** — that is the pattern, and
//! [`scenario_active`] below is its app-side twin. Keep the two in step.

use bevy::prelude::*;

/// **Something other than the player is authoring the camera and the avatar this run.**
///
/// Inserted by the capture harness, which is the only thing that ever drives them; a player build
/// compiles no inserter, so the resource simply never exists and every `run_if` below reads as
/// "the player is in charge" for free. The *type* lives here rather than in `capture` for one
/// reason: `player::control`, `player::server_ride`, the self-model fade and the UI pass all name
/// it in a run condition, and a run condition that names a dev type is a dependency on dev.
#[derive(Resource)]
pub(crate) struct CaptureMode;

/// The rig's derived character name (decision 0651), when `$WOW_RIG` names a body.
///
/// Inserted by `capture::ProbeRigPlugin` at build time; absent otherwise — which is the player
/// answer, and the answer in any run the rig is not driving. The character-select roster reads it
/// to know that the pick is already spoken for, so it must not also honour `$WOW_CHAR`: the rig may
/// have to *create* its body first, and the `WOW_CHAR` fast path is a one-shot that structurally
/// cannot wait for that. Before 1174 the roster asked the harness directly.
#[derive(Resource)]
pub(crate) struct RigCharacter(pub(crate) String);

/// **Is a capture running?** (`$WOW_CAPTURE` set.)
///
/// Read before any plugin builds — it decides whether the net thread starts, what size the window
/// opens at, and whether clutter/anim-LOD randomness is pinned. All of those are properties of the
/// run, not of the harness, and all of them must have an answer in a build with no harness in it.
/// See the module doc for why this reads the variable rather than asking `capture`.
pub(crate) fn scenario_active() -> bool {
    std::env::var("WOW_CAPTURE").is_ok()
}

/// **Do the credentials come from the environment?** (`$WOW_USER` / `$WOW_PASS` / `$WOW_CHAR` —
/// the login screen's env fast path.)
///
/// The player answer is `false`, and it is the default: with none of these set the client opens at
/// the login screen and waits for somebody to type. Setting any of them says *"log in without
/// waiting for me to type"* — and **that is the only thing it says.** It is not a claim about who
/// is in the room; [`unattended`] is the fact for that, and keeping the two apart is decision 1769.
///
/// One environment fact, several readers, no shared symbol — the module doc's pattern, same as
/// [`scenario_active`], and the reason the roster's `WOW_CHAR` fast path asks here instead of
/// re-reading the three names.
pub(crate) fn env_login() -> bool {
    // Through `webenv`, not `std::env`: in a browser these arrive as the page's env object or
    // query string (`?user=…&pass=…&char=…`), and this answer decides the start screen before
    // any plugin runs.
    ["WOW_USER", "WOW_PASS", "WOW_CHAR"]
        .iter()
        .any(|k| crate::webenv::var(k).is_some())
}

/// **Is there nobody here?** (`$WOW_UNATTENDED`, or the two runs that are automated by
/// construction: `$WOW_CAPTURE` and `$WOW_RIG`.)
///
/// The player answer is `false` and it is the **default**, like every other answer in this module:
/// a client with nobody at the keyboard is the exception, and an exception declares itself. The
/// direction is the whole point — guessing "attended" when a probe is running costs that probe a
/// timeout, loudly; guessing "unattended" when a person is playing ships them a client that fights
/// another client for their account, silently. Those two are not worth the same, so the doubt goes
/// one way.
///
/// **This is the fact [`env_login`] was mistaken for, twice.** Env credentials mean "log in without
/// typing"; they say nothing about the room — and the director plays with
/// `WOW_USER=one WOW_PASS=pone cargo play`, which is the example line in `.cargo/config.toml`. So
/// every session they played read as "a harness": a typed password's refusal killed the process
/// (fixed on 2026-08-28 by asking `announced` — direct evidence — instead of the environment) and,
/// the half that fix left standing, a kick sent the client racing to take the account back instead
/// of to the login screen. That is decision 1262's reported bug, reappearing three weeks later
/// with not one line of its code changed, because the fact underneath it was never true. 1769.
///
/// `WOW_CAPTURE` and `WOW_RIG` are folded in because they *cannot* be a person: a capture authors
/// the camera and a rig drives the body, so a run that sets either has already said what it is and
/// cannot forget to. Everything else declares — `scripts/leg.sh`, `smoke.sh`, `cine.sh`,
/// `summon-live.sh` and the ad-hoc probe recipe in `method.md` all pass `WOW_UNATTENDED=1`.
///
/// Read by whatever may act *instead of* a person: the lost-session verdict (decision 1262 — the
/// session-loss readers never call this directly, [`crate::net::DisconnectedMessage::new`] asks
/// once at the wire edge and every reader acts on the verdict it carries) and the two `FATAL`
/// exits that keep a driverless run from burning its wall-clock on a dialog (decision 1371).
pub(crate) fn unattended() -> bool {
    ["WOW_UNATTENDED", "WOW_CAPTURE", "WOW_RIG"]
        .iter()
        .any(|k| std::env::var_os(k).is_some())
}

/// **This run cannot get past the screen it is on — may the client end it instead of a person?**
/// (decision 1371's `FATAL` marker, resting on 1769's fact.)
///
/// `true` only for a run that has declared itself [`unattended`]: it exits non-zero on the one
/// greppable marker `scripts/leg.sh` keys on, rather than parking a driverless run on a dialog for
/// its whole wall-clock. Otherwise the dialog stays up for whoever is there.
///
/// The `false` arm still speaks, when the credentials came from the environment: a probe that
/// quietly parks for 300 s instead of failing in 5 is precisely what 1769's default trades away,
/// so the log names the declaration that buys the old behaviour back. Both arms live here because
/// three call sites — two in [`crate::login`], one in [`crate::char_select`] — must not drift
/// apart on either the verdict or the marker `leg.sh` greps for.
pub(crate) fn fatal_when_driverless(why: &str) -> bool {
    if unattended() {
        error!("login: FATAL — {why}; exiting");
        return true;
    }
    if env_login() {
        warn!(
            "login: {why} — leaving the dialog up, because nothing declared this run driverless. \
             Set WOW_UNATTENDED=1 if nobody is here and it should exit non-zero instead \
             (decision 1769)."
        );
    }
    false
}

/// **Which screen the client starts on.** A player starts at the login screen, always; a capture
/// boots straight into the world (no net, no picker), and a *glue* capture boots onto the very
/// screen it photographs.
///
/// The one place this module forwards into the dev half, and deliberately the only one: the answer
/// is needed *before the plugins build* (it is `CharSelectPlugin`'s `start`), so it cannot be a
/// resource an instrument inserts the way [`CaptureMode`] and [`RigCharacter`] are. The forward is
/// one call, in one direction, and the player arm names nothing at all.
pub(crate) fn start_state() -> crate::char_select::ClientState {
    #[cfg(feature = "dev")]
    {
        crate::capture::start_state()
    }
    #[cfg(not(feature = "dev"))]
    {
        crate::char_select::ClientState::Login
    }
}

/// **Does this build offer developer affordances at all?** `false` in a player build.
///
/// The generalisation of 1176's one-door rule, and the thing that record should have written
/// (decision 1179). A dev affordance does not have to live in a dev module: the world map's
/// Alt-click jump is in `ui_world_map`, the `/castvis` family is in `ui_chat`, the dev key plane's
/// cost is in `bindings`. None of them names a dev root, so none of them fails to compile, and all
/// of them shipped to players in 1174 exactly as free-fly did.
///
/// So the question a gameplay module asks is not "is the debug panel compiled in" — it is **"may I
/// offer this at all"**, and there is one place to ask it. Held by
/// [`tests::the_dev_plane_has_exactly_one_door`].
pub(crate) fn dev_affordances() -> bool {
    cfg!(feature = "dev")
}

/// **Did a dev-chord affordance just fire?** `Ctrl`+`Shift`+*key*, and `false` in a player build.
///
/// The one door onto the dev plane, and the reason it exists: the plane is
/// [`benilla_world::modkeys::dev_chord`], which lives in the **engine** (1160 moved it there —
/// "nothing about which two modifiers are the dev plane is a debug-panel opinion"), and the engine
/// is always compiled. So a gameplay module asking for a dev chord creates **no symbol into a dev
/// module**, compiles clean with `--no-default-features`, and ships a live dev affordance to a
/// player. That is exactly what happened: 1174 landed a green player build in which `Ctrl+Shift+F`
/// still flew the camera through the world, `+G` still teleported the avatar, and `+M` still muted
/// the game (decision 1176).
///
/// Routing every non-dev reader through here makes the whole plane dark at once rather than five
/// keys at a time, and makes the next dev chord dark for free. The build gate cannot see this class
/// — nothing fails to compile — so it is held by [`tests::the_dev_plane_has_exactly_one_door`]
/// instead, in the suite the gates already run.
pub(crate) fn dev_chord(keys: &ButtonInput<KeyCode>, key: KeyCode) -> bool {
    dev_affordances() && benilla_world::modkeys::dev_chord(keys, key)
}

/// The free-fly hint the take-control line carries, or empty when there is no chord to advertise.
/// A player build must not offer a key that does nothing. Spelled from
/// [`benilla_world::modkeys::DEV_CHORD`] like every other surface that names the plane, so it
/// cannot drift from what the chord actually listens for.
pub(crate) fn free_fly_hint() -> String {
    #[cfg(feature = "dev")]
    {
        format!(
            " ({} + F toggles free-fly)",
            benilla_world::modkeys::DEV_CHORD
        )
    }
    #[cfg(not(feature = "dev"))]
    {
        String::new()
    }
}

/// **This crate's source directory on a dev build, `None` in a player build** — the seam's second
/// door, and the one [`dev_affordances`] cannot be.
///
/// `dev_affordances()` answers "may I offer this", at runtime. Part of the seam is not about
/// offering anything: it is about a **string being absent from the binary**.
/// `env!("CARGO_MANIFEST_DIR")` names the build machine's home directory, and a runtime `if` does
/// not remove it — the literal is still compiled in, still in `strings`, still shipped. 1174's
/// pool-slot guard made exactly that mistake one level down ("inert in a player build" read as
/// "absent from a player build"), and decision 1175 is entirely about a binary that stops
/// depending on the machine that built it. So this `cfg` has to be real, and it lives here, once,
/// behind a value instead of scattered across the modules that need it.
///
/// Two callers, both resolving content or state and neither of them an affordance:
/// [`crate::ui_script`]'s dev probe into `assets/ui` (so editing FrameXML costs no recompile) and
/// [`crate::local_state`]'s project-folder home. Both get `None` in a player build and fall through
/// to the compiled-in copy and to `<exe dir>` respectively, with no path literal in the binary.
///
/// Held by [`tests::the_dev_plane_has_exactly_one_door`] like the rest of the seam.
pub(crate) fn dev_source_dir() -> Option<&'static std::path::Path> {
    #[cfg(feature = "dev")]
    {
        Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
    }
    #[cfg(not(feature = "dev"))]
    {
        None
    }
}

/// The pre-connect **account guard**, consulted by the login policy's env fast path
/// ([`crate::login`]). Returns `Err(explanation)` when this build lives in a worktree pool slot and
/// the fast path is about to authenticate as an account that belongs to somebody else — the
/// director's `one` (a login KICKS their live session mid-play) or another slot's `probeN` (the
/// kicked client's 0065 teardown despawns every net entity, so a parallel session's probe reads a
/// unit-less world and prints garbage; it happened, method.md records it).
///
/// Slot identity comes from the compiled-in manifest path, because that is what the pool guarantees
/// is unique per session: every slot has its own checkout and its own `target/`. Outside a pool slot
/// (the primary checkout, which is the director's) the guard is inert — it has no business having an
/// opinion about a login it cannot attribute. That is why this lives here and not in `preflight`
/// (decision 0649's home for it, until 1174 made `preflight` dev-only): gameplay's login policy
/// calls it, and gameplay may not call an instrument.
///
/// `WOW_ALLOW_ACCOUNT=1` is the escape hatch for the rare deliberate cross-account run; it turns the
/// refusal into a warning rather than silence, because the kick still happens.
pub(crate) fn account_guard(user: &str) -> Result<(), String> {
    guard_for(compiled_slot(), user)
}

/// The pool slot this binary was *compiled* in, or `None`.
///
/// A player build is never in a pool slot, so 1174 read `env!("CARGO_MANIFEST_DIR")`
/// unconditionally on the grounds that the ladder collapses to `Ok(())` anyway. It does — but the
/// path is still a **string in the shipped binary**, naming the build machine's home directory,
/// which is the one thing decision 1175 exists to end (its falsifier is literally
/// `strings <player binary> | grep -c '/Users/…'`). It costs one `cfg` to not ship it, and it is
/// what makes that falsifier readable: after this, the only source-tree paths left in a player
/// binary are debuginfo and panic metadata, never data the program acts on.
#[cfg(feature = "dev")]
fn compiled_slot() -> Option<u32> {
    pool_slot(env!("CARGO_MANIFEST_DIR"))
}

#[cfg(not(feature = "dev"))]
fn compiled_slot() -> Option<u32> {
    None
}

/// [`account_guard`]'s decision, with the slot passed in so the ladder is testable from any
/// checkout (the real one reads a compile-time path that differs per worktree).
fn guard_for(slot: Option<u32>, user: &str) -> Result<(), String> {
    let Some(slot) = slot else {
        return Ok(());
    };
    let mine = format!("probe{slot}");
    let user_lc = user.to_ascii_lowercase();
    if user_lc == mine {
        return Ok(());
    }
    let whose = if user_lc == "one" {
        "the DIRECTOR's account — logging in on it kicks them out of their live session mid-play"
    } else if user_lc.starts_with("probe") && user_lc[5..].chars().all(|c| c.is_ascii_digit()) {
        "ANOTHER worktree slot's probe account — logging in on it kicks that session's probe out \
         of the world, and its next sample reads a unit-less world"
    } else {
        return Ok(()); // a bystander account (`two`, a fresh test account): not ours to police
    };
    Err(format!(
        "the env fast path is about to log in as `{user}` from pool-{slot}, and that is {whose}. \
         This slot's identity is WOW_USER=probe{slot} WOW_PASS=pprobe{slot} \
         WOW_CHAR=Probe{spelled} (method.md \"The local vmangos server\").",
        spelled = spell_digit(slot)
    ))
}

/// This slot's index as the word the probe identity spells it with (`pool-4` → `"four"`), or `None`
/// outside a pool slot. The one place anything else should ask "which session am I?" — the rig keys
/// its derived character names off it, and the preflight banner names it.
pub(crate) fn slot_word() -> Option<&'static str> {
    compiled_slot().map(spell_digit)
}

/// The pool slot index in a `…/benilla-wt/pool-<N>/…` manifest path; `None` for the primary
/// checkout and any non-pool worktree. Dev-only, because [`compiled_slot`] is its only caller and
/// a player build must not name a manifest path at all (1175).
#[cfg(any(feature = "dev", test))]
fn pool_slot(manifest_dir: &str) -> Option<u32> {
    let mut parts = manifest_dir.split('/');
    while let Some(part) = parts.next() {
        if part == "benilla-wt" {
            return parts.next()?.strip_prefix("pool-")?.parse().ok();
        }
    }
    None
}

/// The probe character's name suffix for slot `n` — vmangos names carry no digits, so the pool
/// index is spelled (`pool-4` → `Probefour`).
fn spell_digit(n: u32) -> &'static str {
    match n {
        0 => "zero",
        1 => "one",
        2 => "two",
        3 => "three",
        4 => "four",
        5 => "five",
        6 => "six",
        7 => "seven",
        8 => "eight",
        9 => "nine",
        _ => "<n>",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The dev plane has exactly one door** (decision 1176).
    ///
    /// The player-build gate (`cargo build -p benilla --no-default-features`) catches a *symbol*
    /// crossing into a dev module. It cannot catch this: `benilla_world::modkeys::dev_chord` is
    /// engine, always compiled, so a gameplay module that asks it for `Ctrl+Shift+F` compiles
    /// perfectly in a player build and ships free-fly to a player. 1174 landed exactly that, green.
    ///
    /// So the rule is checked instead of remembered, the way 0789 checks the probe clock: outside
    /// the dev roots, `dev_chord` is [`super::dev_chord`]'s to call and nobody else's. A dev module
    /// may call the engine's directly — it does not exist in a player build to be reached.
    ///
    /// This does NOT police [`benilla_world::modkeys::DEV_CHORD`], the display string: naming the
    /// plane in a label is not offering a key.
    #[test]
    fn the_dev_plane_has_exactly_one_door() {
        // Everything that is compiled out by `--no-default-features`, plus this module, which is
        // the door itself. Anything else asking the engine for a dev chord is the bug.
        const DEV_ROOTS: &[&str] = &[
            "capture/",
            "debug_panel/",
            "asset_churn.rs",
            "dev.rs",
            "hover_log.rs",
            "perf/",
            "preflight.rs",
            "probe_shield.rs",
            "lib.rs",
            "run_mode.rs",
        ];
        // Assembled at runtime so the checker does not flag its own source — the same trick, and
        // the same proof of teeth, as `capture::probes`' clock test.
        let needle = format!("modkeys::{}(", "dev_chord");
        // The second clause: `#[cfg(feature = "dev")]` itself. Seam knowledge has exactly three
        // addresses — this module (the always-present layer), `dev.rs` (the group), and `lib.rs`
        // (the module declarations). A `cfg` attribute anywhere else means a gameplay module has
        // learned the seam exists, which is the state 1174's whole diff cleared the tree out of,
        // and which spreads one file at a time if nothing objects.
        let cfg_needle = format!("feature = {}dev{}", '"', '"');

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("src is readable") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let rel = path
                    .strip_prefix(&src)
                    .expect("under src")
                    .to_string_lossy()
                    .replace('\\', "/");
                if DEV_ROOTS.iter().any(|r| rel.starts_with(r)) {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("source is readable");
                for (n, line) in text.lines().enumerate() {
                    // Doc comments name the function constantly; only real calls matter.
                    let t = line.trim();
                    if t.starts_with("//") {
                        continue;
                    }
                    if t.contains(&needle) || t.contains(&cfg_needle) {
                        offenders.push(format!("{rel}:{}  {t}", n + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "a dev-chord affordance outside the dev roots must go through `run_mode::dev_chord`, \
             which is `false` in a player build — the engine's `modkeys::dev_chord` is always \
             compiled, so calling it directly ships a live dev key to a player and the build gate \
             cannot see it (decision 1176). And a `#[cfg(feature = \"dev\")]` outside `run_mode`, \
             `dev.rs` and `lib.rs` means a gameplay module has learned the seam exists — ask \
             `run_mode::dev_affordances()` instead (decision 1179). Offenders:\n  {}",
            offenders.join("\n  "),
        );
    }

    /// **Two questions, two facts** (decision 1769) — and the fixture is the line the director
    /// actually types, so a future merge of the two answers fails here rather than in their game.
    #[test]
    fn env_credentials_are_not_a_claim_about_who_is_in_the_room() {
        use crate::local_state::test_env::{EnvGuard, ENV_LOCK};
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _decl = EnvGuard::unset("WOW_UNATTENDED");
        let _cap = EnvGuard::unset("WOW_CAPTURE");
        let _rig = EnvGuard::unset("WOW_RIG");

        // A player build's defaults: nothing set, nothing assumed.
        {
            let _u = EnvGuard::unset("WOW_USER");
            let _p = EnvGuard::unset("WOW_PASS");
            let _c = EnvGuard::unset("WOW_CHAR");
            assert!(!env_login() && !unattended());
        }
        // `WOW_USER=one WOW_PASS=pone cargo play` — `.cargo/config.toml`'s own example, and the
        // director's launch line since 2026-08-29. Log them in without typing, yes. Decide things
        // on their behalf, no.
        let _u = EnvGuard::set("WOW_USER", "one");
        let _p = EnvGuard::set("WOW_PASS", "pone");
        assert!(env_login(), "the fast path still submits for them");
        assert!(
            !unattended(),
            "credentials in the environment are not an empty chair",
        );
        // The declaration, and the two runs that are automated by construction.
        for key in ["WOW_UNATTENDED", "WOW_CAPTURE", "WOW_RIG"] {
            let _d = EnvGuard::set(key, "1");
            assert!(unattended(), "{key} declares the run driverless");
        }
        assert!(!unattended(), "and each guard puts it back");
    }

    #[test]
    fn the_guard_only_polices_accounts_that_belong_to_someone() {
        // Our own slot's probe: the whole point of the identity.
        assert!(guard_for(Some(4), "probe4").is_ok());
        assert!(guard_for(Some(4), "PROBE4").is_ok()); // vmangos accounts are case-insensitive
                                                       // The director's account, and a neighbouring slot's probe: both kick a live session.
        let director = guard_for(Some(4), "one").unwrap_err();
        assert!(director.contains("DIRECTOR") && director.contains("WOW_USER=probe4"));
        // The override hint belongs to the caller that can act on it, not to the reason.
        assert!(!director.contains("WOW_ALLOW_ACCOUNT"));
        assert!(guard_for(Some(4), "probe7")
            .unwrap_err()
            .contains("ANOTHER worktree slot"));
        // A bystander account is nobody's to police, and outside a pool slot we have no standing.
        assert!(guard_for(Some(4), "two").is_ok());
        assert!(guard_for(None, "one").is_ok());
    }

    #[test]
    fn the_probe_character_name_spells_the_slot() {
        // vmangos player names carry no digits, so `Probe4` cannot exist — the pool index is spelled.
        assert!(guard_for(Some(0), "one")
            .unwrap_err()
            .contains("WOW_CHAR=Probezero"));
        assert!(guard_for(Some(9), "one")
            .unwrap_err()
            .contains("WOW_CHAR=Probenine"));
    }

    #[test]
    fn the_slot_is_read_off_the_manifest_path() {
        assert_eq!(
            pool_slot("/Users/sam/dev/benilla-wt/pool-7/crates/benilla"),
            Some(7)
        );
        assert_eq!(pool_slot("/Users/sam/dev/benilla-wow/crates/benilla"), None);
        assert_eq!(
            pool_slot("/Users/sam/dev/benilla-wow/.claude/worktrees/x/crates/benilla"),
            None
        );
    }
}
