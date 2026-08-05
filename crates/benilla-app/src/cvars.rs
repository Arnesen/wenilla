//! benilla's **CVar host** — registration, knob sync, and the client's first persistence
//! (decision 0954). The engine holds the table and the Lua API ([`benilla_ui::script`]'s
//! `GetCVar`/`SetCVar`); this module is everything host-side:
//!
//! - **The registered set** ([`REGISTERED`]): only vars backed by a real engine knob (the
//!   honest-tree rule). Defaults are the code's own truths — the sound quartet is the client's
//!   verified CVar registration defaults (wow-re `benilla-pins.md` B10, quoted in
//!   [`crate::sound::SoundConfig`]), `uiScale`/`farclip` are benilla's shipped defaults —
//!   and a test welds each string to the constant it mirrors so they cannot drift.
//! - **Boot**: read `benilla/config.toml` ([`crate::local_state`]) and apply it to the knob
//!   resources; when the UI VM exists, register the table and push the resolved session values
//!   so `GetCVar` answers what the client is actually doing.
//! - **Sync**: drain Lua `SetCVar` changes into the knob resources each frame and mark the
//!   config dirty.
//! - **Save**: dirty + one quiet second → rewrite `config.toml` atomically (and flush on
//!   `AppExit`). The file holds **only values that moved off their default** — a diff, not a
//!   dump — plus any entries this build doesn't know (a newer build's keys survive a downgrade;
//!   same posture for a hand-added key: preserved verbatim, warned once).
//!
//! **Env overrides win for the session and never touch the file**: `WOW_UI_SCALE`/`WOW_FARCLIP`
//! beat the loaded config (they exist to make taste iteration a relaunch — pinning one into the
//! config would make the A/B sticky), the session runs and saves around them, and the file keeps
//! whatever it already said for those keys.

use std::collections::{BTreeMap, HashSet};
use std::time::Instant;

use bevy::prelude::*;

use crate::clutter::ClutterConfig;
use crate::nameplates::NameConfig;
use crate::player::camera::LookConfig;
use crate::sound::SoundConfig;
use crate::target::ClickConfig;
use crate::ui_loot::LootConfig;
use crate::ui_script::UiScaleCvar;
use crate::view::{ViewDistance, FARCLIP_RANGE};
use benilla_ui::script::UiScript;

/// The host-backed CVars: `(registered name, default)`. Grows one row per knob a settings page
/// actually wires — never ahead of the knob (see the module doc).
pub(crate) const REGISTERED: &[(&str, &str)] = &[
    ("MasterVolume", "1"),
    ("SoundVolume", "1"),
    ("MusicVolume", "0.4"),
    ("AmbienceVolume", "0.6"),
    // The three 1.12 sound enables (registrar defaults all "1", wow-re B10):
    // `MasterSoundEffects` is the MASTER "Enable All Sound" checkbox (SoundOptionsFrame.lua
    // index 1 — its callback sets the engine-wide pause flag), NOT an SFX-only toggle; 1.12
    // has no `EnableSound`/`EnableSFX` at all.
    ("MasterSoundEffects", "1"),
    ("EnableMusic", "1"),
    ("EnableAmbience", "1"),
    ("uiScale", "0.9"),
    ("farclip", "777"),
    // The Controls-page trio (0961). `deselectOnClick`/`mouseInvertPitch` are 1.12's own
    // Interface Options CVars (UIOptionsFrame.lua indices 45/1); their defaults are the
    // reference behaviors benilla already shipped (empty-world click clears the target; no
    // pitch invert). `autoLootDefault` is era's — no 1.12 CVar exists, vanilla only had the
    // shift gesture — default off, like era's engine registrar.
    ("deselectOnClick", "1"),
    ("mouseInvertPitch", "0"),
    ("autoLootDefault", "0"),
    // The overhead-name trio (0992): 1.12's own UnitName* CVars (UIOptionsFrame.lua indices
    // 21/30/67) over the nameplates module's gates. Defaults mirror NameConfig::default() —
    // npc/own ON are director directives, diverging from the binary's "0" defaults on purpose
    // (the divergence and its dates live on NameConfig's doc).
    ("UnitNamePlayer", "1"),
    ("UnitNameNPC", "1"),
    ("UnitNameOwn", "1"),
    // World detail (0992): 1.12's video-panel var (the ENVIRONMENT_DETAIL slider, 0..2) over
    // the clutter-density knob — 0 is the client's bare frillDensity baseline (×1 = 16 visits),
    // each step +1×; the "2" default IS ClutterConfig's shipped ×3 (the reference's High).
    ("WorldDetail", "2"),
];

/// `config.toml`'s shape: a `[cvars]` table of `Name = "value"` strings (CVars are strings in
/// the reference too; consumers parse and clamp at their edge). BTreeMap so the file is stably
/// sorted on every save.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct LocalConfig {
    #[serde(default)]
    cvars: BTreeMap<String, String>,
}

/// The persistence state: what the file said, which keys the environment overrides this
/// session, and the dirty/debounce pair.
#[derive(Resource, Default)]
pub(crate) struct CvarPersist {
    /// The file's `[cvars]` entries, verbatim spelling — the merge base every save starts from
    /// (unknown keys ride through untouched, env-overridden keys keep their stored value).
    file: BTreeMap<String, String>,
    /// Lowercased names whose value came from an env var this session (never saved).
    env_overridden: HashSet<String>,
    /// The engine table has been registered + seeded (once, when the VM first exists).
    registered: bool,
    /// A change since the last save; `last_change` drives the one-quiet-second debounce.
    dirty: bool,
    last_change: Option<Instant>,
}

/// How long a dirty config sits before the save fires — long enough to coalesce a slider drag,
/// short enough that a crash loses one gesture, not a session ("write-on-change, debounced").
const SAVE_QUIET: std::time::Duration = std::time::Duration::from_secs(1);

pub(crate) struct CvarPlugin;

impl Plugin for CvarPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CvarPersist>()
            .add_systems(Startup, load_config)
            .add_systems(Update, (sync_cvars, save_config).chain());
    }
}

/// The knob resources one CVar write can land on, bundled so [`apply_to_knobs`] and its two
/// callers grow together (a new knob is one field + one arm).
struct Knobs<'a> {
    sound: &'a mut SoundConfig,
    scale: &'a mut UiScaleCvar,
    view: &'a mut ViewDistance,
    look: &'a mut LookConfig,
    click: &'a mut ClickConfig,
    loot: &'a mut LootConfig,
    names: &'a mut NameConfig,
    clutter: &'a mut ClutterConfig,
}

/// Apply one CVar to its knob resource (parse + the knob's own clamp). `false` = not a knob this
/// build knows (the caller decides whether that warns or rides through).
fn apply_to_knobs(name: &str, value: &str, knobs: &mut Knobs) -> bool {
    let Ok(v) = value.parse::<f32>() else {
        warn!("cvar {name}: unparseable value '{value}' ignored");
        return true; // known key, bad value — consumed, resource keeps its truth
    };
    match name.to_ascii_lowercase().as_str() {
        "mastervolume" => knobs.sound.master = v.clamp(0.0, 1.0),
        "soundvolume" => knobs.sound.sfx = v.clamp(0.0, 1.0),
        "musicvolume" => knobs.sound.music = v.clamp(0.0, 1.0),
        "ambiencevolume" => knobs.sound.ambience = v.clamp(0.0, 1.0),
        // The enables are 0/1 flags; the client's own parse is int + `!= 0`.
        "mastersoundeffects" => knobs.sound.enabled = v != 0.0,
        "enablemusic" => knobs.sound.music_enabled = v != 0.0,
        "enableambience" => knobs.sound.ambience_enabled = v != 0.0,
        "uiscale" => knobs.scale.0 = v.clamp(0.5, 1.5),
        "farclip" => knobs.view.farclip = v.clamp(*FARCLIP_RANGE.start(), *FARCLIP_RANGE.end()),
        "deselectonclick" => knobs.click.deselect_on_click = v != 0.0,
        "mouseinvertpitch" => knobs.look.invert_pitch = v != 0.0,
        "autolootdefault" => knobs.loot.auto_loot = v != 0.0,
        "unitnameplayer" => knobs.names.player = v != 0.0,
        "unitnamenpc" => knobs.names.npc = v != 0.0,
        "unitnameown" => knobs.names.own = v != 0.0,
        // The panel's 0/1/2 lands as the density multiplier ×1/×2/×3; the clamp is the 1.12
        // slider's own range (an off-grid hand-edit rides between stops, like every slider).
        "worlddetail" => knobs.clutter.density = v.clamp(0.0, 2.0) + 1.0,
        _ => return false,
    }
    true
}

/// Startup: read `benilla/config.toml` (absent file = all defaults, not an error) and apply it
/// to the knob resources — except keys the environment overrides this session (their resources
/// already read the env var in their `Default`s). The VM does not exist yet; [`sync_cvars`]
/// seeds the table when it does.
#[allow(clippy::too_many_arguments)] // one knob resource per registered CVar family
fn load_config(
    mut persist: ResMut<CvarPersist>,
    mut sound: ResMut<SoundConfig>,
    mut scale: ResMut<UiScaleCvar>,
    mut view: ResMut<ViewDistance>,
    mut look: ResMut<LookConfig>,
    mut click: ResMut<ClickConfig>,
    mut loot: ResMut<LootConfig>,
    mut names: ResMut<NameConfig>,
    mut clutter: ResMut<ClutterConfig>,
) {
    let mut knobs = Knobs {
        sound: &mut sound,
        scale: &mut scale,
        view: &mut view,
        look: &mut look,
        click: &mut click,
        loot: &mut loot,
        names: &mut names,
        clutter: &mut clutter,
    };
    if std::env::var_os("WOW_UI_SCALE").is_some() {
        persist.env_overridden.insert("uiscale".into());
    }
    if std::env::var_os("WOW_FARCLIP").is_some() {
        persist.env_overridden.insert("farclip".into());
    }
    // The clutter A/B env drives the same knob WorldDetail lands on — same session-only law.
    if std::env::var_os("WOW_CLUTTER_DENSITY").is_some() {
        persist.env_overridden.insert("worlddetail".into());
    }
    let Some(path) = crate::local_state::config_path() else {
        return; // hermetic capture, or no install — session-only state
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            warn!("config: cannot read {}: {e}", path.display());
            return;
        }
    };
    let cfg: LocalConfig = match toml::from_str(&text) {
        Ok(c) => c,
        Err(e) => {
            // A malformed file is preserved, not clobbered: nothing loads, but nothing saves
            // over it either until a change actually happens — and the warn names the file.
            warn!(
                "config: {} is malformed ({e}) — running on defaults",
                path.display()
            );
            return;
        }
    };
    let known: HashSet<String> = REGISTERED
        .iter()
        .map(|(n, _)| n.to_ascii_lowercase())
        .collect();
    for (name, value) in &cfg.cvars {
        let key = name.to_ascii_lowercase();
        if !known.contains(&key) {
            warn!("config: unknown cvar '{name}' — preserved, not applied");
            continue;
        }
        if persist.env_overridden.contains(&key) {
            info!("config: {name} overridden by env for this session (file value kept)");
            continue;
        }
        apply_to_knobs(name, value, &mut knobs);
    }
    persist.file = cfg.cvars;
}

/// Per frame: seed the VM's table once it exists (registered set + the RESOLVED session values,
/// so `GetCVar` reflects env overrides and the loaded config alike), then drain Lua `SetCVar`
/// changes into the knob resources and mark the config dirty.
#[allow(clippy::too_many_arguments)] // one knob resource per registered CVar family
fn sync_cvars(
    script: Option<NonSendMut<UiScript>>,
    mut persist: ResMut<CvarPersist>,
    mut sound: ResMut<SoundConfig>,
    mut scale: ResMut<UiScaleCvar>,
    mut view: ResMut<ViewDistance>,
    mut look: ResMut<LookConfig>,
    mut click: ResMut<ClickConfig>,
    mut loot: ResMut<LootConfig>,
    mut names: ResMut<NameConfig>,
    mut clutter: ResMut<ClutterConfig>,
) {
    let Some(mut script) = script else {
        return;
    };
    if !persist.registered {
        script.register_cvars(REGISTERED.iter().copied());
        let flag = |b: bool| if b { "1" } else { "0" }.to_string();
        let session: [(&str, String); 16] = [
            ("MasterVolume", sound.master.to_string()),
            ("SoundVolume", sound.sfx.to_string()),
            ("MusicVolume", sound.music.to_string()),
            ("AmbienceVolume", sound.ambience.to_string()),
            ("MasterSoundEffects", flag(sound.enabled)),
            ("EnableMusic", flag(sound.music_enabled)),
            ("EnableAmbience", flag(sound.ambience_enabled)),
            ("uiScale", scale.0.to_string()),
            ("farclip", view.farclip.to_string()),
            ("deselectOnClick", flag(click.deselect_on_click)),
            ("mouseInvertPitch", flag(look.invert_pitch)),
            ("autoLootDefault", flag(loot.auto_loot)),
            ("UnitNamePlayer", flag(names.player)),
            ("UnitNameNPC", flag(names.npc)),
            ("UnitNameOwn", flag(names.own)),
            // The session density on the panel scale (×1..×3 → 0..2). An env-driven off-grid
            // multiplier seeds off-grid honestly — the dropdown shows the raw number, checks
            // nothing (the 0959 out-of-range posture, dropdown-flavored).
            ("WorldDetail", (clutter.density - 1.0).to_string()),
        ];
        for (name, value) in session {
            script.set_cvar_host(name, &value);
        }
        persist.registered = true;
    }
    // Take the changes BEFORE touching the knobs: constructing `Knobs` deref-muts every knob
    // resource, which trips Bevy change detection even when nothing is written — and the
    // clutter re-scatter is downstream of exactly that signal staying honest (0992).
    let changes = script.take_cvar_changes();
    if changes.is_empty() {
        return;
    }
    let mut knobs = Knobs {
        sound: &mut sound,
        scale: &mut scale,
        view: &mut view,
        look: &mut look,
        click: &mut click,
        loot: &mut loot,
        names: &mut names,
        clutter: &mut clutter,
    };
    for (name, value) in changes {
        if apply_to_knobs(&name, &value, &mut knobs) {
            persist.dirty = true;
            persist.last_change = Some(Instant::now());
        }
    }
}

/// Compose the file to save: the previous file as the merge base, every registered var that
/// moved off its default written, every one back at its default removed — env-overridden keys
/// untouched (the session value is the env's, not the player's).
fn compose_file(
    previous: &BTreeMap<String, String>,
    env_overridden: &HashSet<String>,
    snapshot: &[(String, String, String)],
) -> BTreeMap<String, String> {
    let mut out = previous.clone();
    for (name, value, default) in snapshot {
        let key = name.to_ascii_lowercase();
        if env_overridden.contains(&key) {
            continue;
        }
        // Match any existing entry case-insensitively so a hand-edited spelling doesn't fork.
        let existing = out.keys().find(|k| k.eq_ignore_ascii_case(name)).cloned();
        if value == default {
            if let Some(k) = existing {
                out.remove(&k);
            }
        } else {
            out.insert(existing.unwrap_or_else(|| name.clone()), value.clone());
        }
    }
    out
}

/// The file's header comment — where these values come from and where the law lives.
const HEADER: &str = "\
# benilla local config (decision 0954) — CVar values that moved off their defaults.
# Managed by the client; hand edits are read on next launch and preserved on save.
";

/// Dirty + one quiet second (or the app exiting) → rewrite `config.toml` atomically.
fn save_config(
    script: Option<NonSendMut<UiScript>>,
    mut persist: ResMut<CvarPersist>,
    mut exits: MessageReader<AppExit>,
) {
    let exiting = exits.read().next().is_some();
    if !persist.dirty {
        return;
    }
    let quiet = persist
        .last_change
        .is_none_or(|t| t.elapsed() >= SAVE_QUIET);
    if !(quiet || exiting) {
        return;
    }
    let Some(script) = script else { return };
    let Some(path) = crate::local_state::config_path() else {
        persist.dirty = false; // hermetic/session-only: nothing to write, stop retrying
        return;
    };
    let snapshot = script.cvars_snapshot();
    let cvars = compose_file(&persist.file, &persist.env_overridden, &snapshot);
    let body = toml::to_string(&LocalConfig {
        cvars: cvars.clone(),
    })
    .expect("string map serializes");
    match crate::local_state::write_atomic(&path, &format!("{HEADER}{body}")) {
        Ok(()) => {
            persist.file = cvars;
            persist.dirty = false;
        }
        Err(e) => {
            warn!("config: cannot write {}: {e}", path.display());
            persist.dirty = false; // don't retry every frame into the same error
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_script::DEFAULT_UI_SCALE;

    /// Every registered default IS the code constant it mirrors — parse-compared so "1" vs
    /// "1.0" cannot fail it, welded so neither side can drift alone.
    #[test]
    fn registered_defaults_mirror_the_code_truths() {
        let d: BTreeMap<&str, f32> = REGISTERED
            .iter()
            .map(|(n, v)| (*n, v.parse::<f32>().unwrap()))
            .collect();
        let sound = SoundConfig::default();
        assert_eq!(d["MasterVolume"], sound.master);
        assert_eq!(d["SoundVolume"], sound.sfx);
        assert_eq!(d["MusicVolume"], sound.music);
        assert_eq!(d["AmbienceVolume"], sound.ambience);
        assert_eq!(d["MasterSoundEffects"] != 0.0, sound.enabled);
        assert_eq!(d["EnableMusic"] != 0.0, sound.music_enabled);
        assert_eq!(d["EnableAmbience"] != 0.0, sound.ambience_enabled);
        assert_eq!(d["uiScale"], DEFAULT_UI_SCALE);
        // ViewDistance::default() reads $WOW_FARCLIP; the registered default mirrors the
        // env-less 777 literal (view.rs doc: "Default 777").
        assert_eq!(d["farclip"], 777.0);
        // The Controls trio (0961) welds to its knob Defaults the same way.
        assert_eq!(
            d["deselectOnClick"] != 0.0,
            ClickConfig::default().deselect_on_click
        );
        assert_eq!(
            d["mouseInvertPitch"] != 0.0,
            LookConfig::default().invert_pitch
        );
        assert_eq!(d["autoLootDefault"] != 0.0, LootConfig::default().auto_loot);
        // The name trio (0992) welds to NameConfig's defaults the same way.
        let names = NameConfig::default();
        assert_eq!(d["UnitNamePlayer"] != 0.0, names.player);
        assert_eq!(d["UnitNameNPC"] != 0.0, names.npc);
        assert_eq!(d["UnitNameOwn"] != 0.0, names.own);
        // ClutterConfig::default() reads $WOW_CLUTTER_DENSITY; the registered default mirrors
        // the env-less ×3 literal (clutter.rs: "Default ×3 = High") on the panel's 0..2 scale.
        assert_eq!(d["WorldDetail"], 2.0);
    }

    #[test]
    fn apply_parses_clamps_and_reports_unknowns() {
        let mut sound = SoundConfig::default();
        let mut scale = UiScaleCvar(0.9);
        let mut view = ViewDistance { farclip: 777.0 };
        let mut look = LookConfig::default();
        let mut click = ClickConfig::default();
        let mut loot = LootConfig::default();
        let mut names = NameConfig::default();
        // Literal fields, not Default: ClutterConfig::default() reads the env A/B vars.
        let mut clutter = ClutterConfig {
            density: 3.0,
            scale: 1.0,
            alpha_ref: 0.5,
            fade_far: 70.0,
        };
        let mut knobs = Knobs {
            sound: &mut sound,
            scale: &mut scale,
            view: &mut view,
            look: &mut look,
            click: &mut click,
            loot: &mut loot,
            names: &mut names,
            clutter: &mut clutter,
        };
        assert!(apply_to_knobs("MusicVolume", "0.7", &mut knobs));
        assert_eq!(knobs.sound.music, 0.7);
        // Clamps are the knob's own: volume to [0,1], farclip to FARCLIP_RANGE.
        assert!(apply_to_knobs("mastervolume", "7", &mut knobs));
        assert_eq!(knobs.sound.master, 1.0);
        assert!(apply_to_knobs("farclip", "50", &mut knobs));
        assert_eq!(knobs.view.farclip, *FARCLIP_RANGE.start());
        // Enable flags: any nonzero is on, zero is off (the client's int-parse + != 0).
        assert!(apply_to_knobs("EnableMusic", "0", &mut knobs));
        assert!(!knobs.sound.music_enabled);
        assert!(apply_to_knobs("mastersoundeffects", "1", &mut knobs));
        assert!(knobs.sound.enabled);
        // The Controls trio lands on its knobs (case-insensitive like everything else).
        assert!(apply_to_knobs("deselectonclick", "0", &mut knobs));
        assert!(!knobs.click.deselect_on_click);
        assert!(apply_to_knobs("MouseInvertPitch", "1", &mut knobs));
        assert!(knobs.look.invert_pitch);
        assert!(apply_to_knobs("autoLootDefault", "1", &mut knobs));
        assert!(knobs.loot.auto_loot);
        // The name trio lands on its gates (0992).
        assert!(apply_to_knobs("UnitNameNPC", "0", &mut knobs));
        assert!(!knobs.names.npc);
        assert!(apply_to_knobs("unitnameown", "1", &mut knobs));
        assert!(knobs.names.own);
        // WorldDetail: panel 0/1/2 → density ×1/×2/×3, clamped to the 1.12 slider's range.
        assert!(apply_to_knobs("WorldDetail", "0", &mut knobs));
        assert_eq!(knobs.clutter.density, 1.0);
        assert!(apply_to_knobs("worlddetail", "7", &mut knobs));
        assert_eq!(knobs.clutter.density, 3.0);
        // A bad value is consumed (known key) and the resource keeps its truth.
        assert!(apply_to_knobs("uiScale", "banana", &mut knobs));
        assert_eq!(knobs.scale.0, 0.9);
        assert!(!apply_to_knobs("bogus", "1", &mut knobs));
    }

    #[test]
    fn compose_writes_the_diff_and_preserves_what_it_does_not_own() {
        let previous: BTreeMap<String, String> = [
            ("FutureKnob".to_string(), "3".to_string()), // a newer build's key: preserved
            ("uiScale".to_string(), "0.8".to_string()),  // env-overridden this session
            ("farclip".to_string(), "400".to_string()),  // will return to default
        ]
        .into();
        let env: HashSet<String> = ["uiscale".to_string()].into();
        let snapshot = vec![
            // (name, value, default)
            ("MusicVolume".into(), "0.7".into(), "0.4".into()), // moved: written
            ("MasterVolume".into(), "1".into(), "1".into()),    // default: absent
            ("uiScale".into(), "1.2".into(), "0.9".into()),     // env value: file keeps 0.8
            ("farclip".into(), "777".into(), "777".into()),     // back to default: removed
        ];
        let out = compose_file(&previous, &env, &snapshot);
        assert_eq!(out.get("MusicVolume").map(String::as_str), Some("0.7"));
        assert!(!out.contains_key("MasterVolume"));
        assert_eq!(out.get("uiScale").map(String::as_str), Some("0.8"));
        assert!(!out.contains_key("farclip"));
        assert_eq!(out.get("FutureKnob").map(String::as_str), Some("3"));
    }

    /// End to end on a real App: a pre-written `config.toml` loads into the knobs at Startup, a
    /// Lua `SetCVar` drains into the knobs and — on the exit flush — lands back in the file as a
    /// diff (the moved value present, the untouched ones absent). This is the whole 0954 slice-1
    /// loop in one place: file → knobs → VM table → Lua write → knobs → file.
    #[test]
    fn a_lua_setcvar_lands_in_config_toml_end_to_end() {
        use crate::local_state::test_env::{EnvGuard, ENV_LOCK};
        let _l = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("benilla-cvar-e2e-{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        let _c = EnvGuard::unset("WOW_CAPTURE");
        let _u = EnvGuard::unset("WOW_UI_SCALE");
        let _f = EnvGuard::unset("WOW_FARCLIP");
        let _d = EnvGuard::unset("WOW_CLUTTER_DENSITY");
        let _h = EnvGuard::set("BENILLA_HOME", tmp.to_str().unwrap());
        crate::local_state::write_atomic(
            &tmp.join("config.toml"),
            "[cvars]\nMusicVolume = \"0.1\"\n",
        )
        .unwrap();

        let mut app = App::new();
        app.add_plugins(bevy::MinimalPlugins)
            .insert_resource(SoundConfig::default())
            .insert_resource(UiScaleCvar(DEFAULT_UI_SCALE))
            .insert_resource(ViewDistance { farclip: 777.0 })
            .init_resource::<LookConfig>()
            .init_resource::<ClickConfig>()
            .init_resource::<LootConfig>()
            .init_resource::<NameConfig>()
            .init_resource::<ClutterConfig>()
            .add_plugins(CvarPlugin);
        app.insert_non_send_resource(UiScript::new().unwrap());

        // Startup: the file's MusicVolume reaches the knob; Update: the VM table seeds from it.
        app.update();
        assert_eq!(app.world().resource::<SoundConfig>().music, 0.1);
        assert_eq!(
            app.world_mut()
                .non_send_resource_mut::<UiScript>()
                .cvar("MusicVolume")
                .as_deref(),
            Some("0.1")
        );

        // The Lua write (what a settings slider will do) reaches the knob on the next frame…
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run(r#"SetCVar("MusicVolume", 0.75)"#)
            .unwrap();
        app.update();
        assert_eq!(app.world().resource::<SoundConfig>().music, 0.75);

        // …and the exit flush writes the diff: the moved value, nothing at its default.
        app.world_mut().write_message(AppExit::Success);
        app.update();
        let text = std::fs::read_to_string(tmp.join("config.toml")).unwrap();
        assert!(text.contains("MusicVolume = \"0.75\""), "{text}");
        assert!(!text.contains("MasterVolume"), "defaults stay out:\n{text}");
        let back: LocalConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.cvars.len(), 1, "a diff, not a dump: {text}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_toml_round_trips() {
        let cfg = LocalConfig {
            cvars: [("MusicVolume".to_string(), "0.7".to_string())].into(),
        };
        let text = format!("{HEADER}{}", toml::to_string(&cfg).unwrap());
        let back: LocalConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.cvars, cfg.cvars);
        // The header survives as comments; a hand edit with comments parses too.
        let hand = "# my note\n[cvars]\nFarclip = \"500\"\n";
        let parsed: LocalConfig = toml::from_str(hand).unwrap();
        assert_eq!(parsed.cvars.get("Farclip").map(String::as_str), Some("500"));
    }
}
