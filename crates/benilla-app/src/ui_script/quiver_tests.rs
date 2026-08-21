//! **"`Quiver.CastPetAction` is nil when the addon's own macro runs"** — bug B267, reproduced end
//! to end and then closed.
//!
//! ## Why this file exists
//!
//! The report is a `/run Quiver.CastPetAction("Furious Howl")` macro raising
//! `attempt to call field 'CastPetAction' (a nil value)`. The field is published on the addon's
//! **last** line of `VARIABLES_LOADED`:
//!
//! ```lua
//! if event == "VARIABLES_LOADED" then
//!     LoadLocale() Migrations() savedVariablesRestore()
//!     initSlashCommandsAndModules()      -- builds the whole config UI, HUNTERS ONLY
//!     RegisterGlobalFunctions()          -- <- Quiver.CastPetAction = … lives here
//! ```
//!
//! so anything that raises in that handler takes every global function with it. Three separate
//! walls did, one behind the other, and each is a real gap of ours:
//!
//! | | what it does | who found it |
//! |---|---|---|
//! | the Region method map was two implementations, not one | `Api._Height = WorldFrame.GetHeight` applied to a Texture raised `stale or invalid frame handle` | this bug |
//! | `Button:SetFontString` missing | every dropdown option row died on its first line | behind wall 1 |
//! | `Frame:SetMinResize`/`SetMaxResize` missing | `SideEffectMakeMoveable` died on every module frame | behind wall 2 |
//!
//! **The addon survey scored Quiver `loaded, session=ok, probe=ok` through all three.** It seats a
//! WARRIOR, and every one of these is behind `if cl == "HUNTER"` — the same blind spot shape as
//! Bagnon's (`ui_script::bagnon_render_tests`), one axis over: there the columns asked "did it
//! raise" and never "did it draw"; here they ask both, of a session the addon declines to run in.
//! So the fixture below seats a **hunter**, and that is the load-bearing part of it.
//!
//! Nothing from the corpus is committed, and the test skips cleanly on a machine without it — the
//! `ui_chat::ace_gate_tests` rule.

use std::path::{Path, PathBuf};

use benilla_ui::script::{ScriptValue, UiScript, UnitState};
use benilla_ui::toc::Toc;

/// Where the vanilla addon corpus might be — `$BENILLA_ADDON_CORPUS`, else a sibling checkout
/// resolved from this crate's manifest (a pool worktree's cwd is not stable across tool calls).
fn corpus_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(over) = std::env::var_os("BENILLA_ADDON_CORPUS") {
        out.push(PathBuf::from(over));
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for up in [2usize, 3, 4] {
        if let Some(root) = manifest.ancestors().nth(up) {
            out.push(root.join("wow-addons-vanilla"));
        }
    }
    out
}

/// The corpus root **and** a Quiver in it, or `None` — a skip, never a failure. Quiver is a live
/// third-party addon (`github.com/SabineWren/Quiver`) whose shipped file is a generated bundle, so
/// a machine that wants this test builds it into the corpus once.
fn quiver_root() -> Option<PathBuf> {
    corpus_candidates()
        .into_iter()
        .find(|c| c.join("Quiver").join("Quiver.toc").is_file())
}

macro_rules! quiver_or_skip {
    () => {
        match quiver_root() {
            Some(root) => root,
            None => {
                eprintln!(
                    "skipping: no Quiver in the addon corpus — looked in {:?} \
                     (set $BENILLA_ADDON_CORPUS; the folder needs Quiver.toc + Quiver.bundle.lua)",
                    corpus_candidates()
                );
                return;
            }
        }
    };
}

fn read_toc(root: &Path, name: &str) -> Toc {
    let path = root.join(name).join(format!("{name}.toc"));
    Toc::parse(&benilla_ui::source::decode(
        &std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    ))
}

/// One addon's `.toc` files through the same two arms the real loader uses (decision 1186).
fn load_addon_files(script: &UiScript, root: &Path, name: &str) -> Vec<String> {
    let toc = read_toc(root, name);
    let provider = |req: &str| -> Option<Vec<u8>> { std::fs::read(root.join(req)).ok() };
    let mut errors = Vec::new();
    for file in &toc.files {
        let path = benilla_ui::loader::join_ref(name, file);
        let Ok(bytes) = std::fs::read(root.join(&path)) else {
            errors.push(format!("{file}: not found"));
            continue;
        };
        if file.to_ascii_lowercase().ends_with(".lua") {
            if let Err(e) =
                script.run_chunk_named(&bytes, &benilla_ui::script::addon_chunk_name(name, file))
            {
                errors.push(format!("{file}: {e}"));
            }
            continue;
        }
        match benilla_ui::framexml::parse(&benilla_ui::source::decode(&bytes)) {
            Ok(doc) => {
                let report = benilla_ui::loader::load_in(script, &doc, &path, &provider);
                errors.extend(report.errors.into_iter().map(|e| format!("{file}: {e}")));
            }
            Err(e) => errors.push(format!("{file}: {e}")),
        }
    }
    errors
}

/// A VM shaped like the reporter's session: our whole interface, and a **hunter** at the keyboard.
///
/// The class is the point (see the module doc). Everything else is the addon-survey seat's own
/// minimum — a named player on a named realm, with a faction group, because a character in a real
/// session always has all three and a nil one is a failure mode no player can produce.
fn seat_a_hunter(root: &Path) -> UiScript {
    let mut s = UiScript::new().expect("VM");
    s.set_screen_size(1024.0, 768.0);
    s.register_cvars(crate::cvars::REGISTERED.iter().copied());
    s.set_realm_name("Harness");
    s.set_unit(
        "player",
        Some(UnitState {
            exists: true,
            name: Some("Harness".into()),
            health: 100,
            max_health: 100,
            level: 60,
            power_type: 0,
            power: 100,
            max_power: 100,
            race: Some("Night Elf".into()),
            race_file: Some("NightElf".into()),
            class: Some("Hunter".into()),
            class_file: Some("HUNTER".into()),
            sex: 2,
            is_player: true,
            faction_group: Some("Alliance".into()),
            ..Default::default()
        }),
    );
    // A HUNTER'S SPELLBOOK. Quiver's `Api.Spell.FindSpellIndex` opens
    // `GetSpellTabInfo(GetNumSpellTabs())` and adds `offset + numSpells`, so an EMPTY book is
    // `attempt to perform arithmetic on local 'tabOffset'` every tick — the reference answers nil
    // there too, so that is the addon's own defect on a spell-less character and not a gap of
    // ours. Seating a book is what lets this test TICK, which is where the module OnUpdates live.
    // Two tabs and four spells is the addon-survey fixture's own shape, with a hunter's names.
    {
        use benilla_ui::script::{SpellBookState, SpellSlotView, SpellTabView};
        let slot = |spell_id: u32, name: &str, rank: Option<&str>| SpellSlotView {
            spell_id,
            name: name.to_string(),
            rank: rank.map(str::to_string),
            texture: Some("Interface\\Icons\\INV_Misc_QuestionMark".into()),
            ..Default::default()
        };
        s.set_spellbook(SpellBookState {
            tabs: vec![
                SpellTabView {
                    name: "General".into(),
                    texture: Some("Interface\\Icons\\INV_Misc_QuestionMark".into()),
                    offset: 0,
                    num_spells: 2,
                },
                SpellTabView {
                    name: "Marksmanship".into(),
                    texture: Some("Interface\\Icons\\Ability_Marksmanship".into()),
                    offset: 2,
                    num_spells: 2,
                },
            ],
            slots: vec![
                slot(6603, "Attack", None),
                slot(75, "Auto Shot", None),
                slot(2973, "Raptor Strike", Some("Rank 1")),
                slot(1978, "Serpent Sting", Some("Rank 1")),
            ],
        });
    }

    let info = super::addons::info_from_toc("Quiver", &read_toc(root, "Quiver"));
    s.register_addons(vec![info], Some(root.to_path_buf()), None, None);
    let failures = super::load_default_ui(&s);
    assert!(
        failures.is_empty(),
        "our own FrameXML failed to load: {failures:#?}"
    );
    s
}

/// **The reported symptom.** Load Quiver into a hunter's session, drive the session start the
/// client drives, and ask the question the macro asked.
///
/// The assertion is deliberately the addon's own field and not "no errors": B267 was filed as a
/// nil field, and a future wall inside `initSlashCommandsAndModules` would take this field out
/// again even if it left a different error behind.
#[test]
fn quiver_publishes_its_global_functions_for_a_hunter() {
    let root = quiver_or_skip!();
    let mut s = seat_a_hunter(&root);

    let load_errors = load_addon_files(&s, &root, "Quiver");
    assert!(load_errors.is_empty(), "Quiver's files: {load_errors:#?}");

    s.fire_event("ADDON_LOADED", vec![ScriptValue::Str("Quiver".into())]);
    for e in ["VARIABLES_LOADED", "PLAYER_LOGIN", "PLAYER_ENTERING_WORLD"] {
        s.fire_event(e, Vec::new());
    }

    // The macro the report ran: `/run Quiver.CastPetAction("Furious Howl"); …`
    assert_eq!(
        s.eval::<String>("return type(Quiver.CastPetAction)")
            .unwrap(),
        "function",
        "B267: the addon's own field is nil because its VARIABLES_LOADED handler died \
         before publishing it — errors so far: {:#?}",
        s.errors()
    );
    // The whole of `RegisterGlobalFunctions`, not just the one the report named — they publish
    // together, so any of them missing means the same handler died at the same place.
    for name in [
        "CastNoClip",
        "CastPetAction",
        "FdPrepareTrap",
        "GetSecondsRemainingReload",
        "GetSecondsRemainingShoot",
        "PredMidShot",
        "TrinketSwap1",
        "TrinketSwap2",
    ] {
        assert_eq!(
            s.eval::<String>(&format!("return type(Quiver.{name})"))
                .unwrap(),
            "function",
            "Quiver.{name} was never published"
        );
    }
}

/// The session start itself raises **nothing** — the three walls, stated as the thing they were.
///
/// Separate from the field assertion above because it fails differently and more usefully: this is
/// the test that names a *new* wall the moment one appears, instead of reporting a nil field.
#[test]
fn quiver_survives_a_hunter_session_start_without_raising() {
    let root = quiver_or_skip!();
    let mut s = seat_a_hunter(&root);
    assert!(load_addon_files(&s, &root, "Quiver").is_empty());

    s.fire_event("ADDON_LOADED", vec![ScriptValue::Str("Quiver".into())]);
    for e in ["VARIABLES_LOADED", "PLAYER_LOGIN", "PLAYER_ENTERING_WORLD"] {
        s.fire_event(e, Vec::new());
    }
    // …and then a second of frames, because Quiver's modules are OnUpdate-driven (the auto-shot
    // timer, the range indicator, the aspect tracker) and a handler that only raises once it is
    // *running* is invisible to the event burst alone.
    for _ in 0..10 {
        s.tick(0.1);
    }
    let raised = s.take_errors();
    assert!(
        raised.is_empty(),
        "Quiver raised at session start: {raised:#?}"
    );
}
