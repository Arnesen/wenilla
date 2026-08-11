//! benilla's **CVar host** — registration, knob sync, and the client's first persistence
//! (decision 0954). The engine holds the table and the Lua API ([`benilla_ui::script`]'s
//! `GetCVar`/`SetCVar`); this module is everything host-side:
//!
//! - **The registered set** ([`REGISTERED`]): only vars something actually reads — a host knob,
//!   or (since 1140) a live Lua consumer, which is the same rule seen from the UI side and the
//!   only refinement the honest-tree law has needed. Defaults are the code's own truths — the sound quartet is the client's
//!   verified CVar registration defaults (wow-re `benilla-pins.md` B10, quoted in
//!   [`crate::sound::SoundConfig`]), `uiScale`/`farclip` are benilla's shipped defaults —
//!   and a test welds each string to the constant it mirrors so they cannot drift.
//! - **Boot**: read `benilla-config/config.toml` ([`crate::local_state`]) and apply it to the knob
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

use crate::chat_bubble::BubbleConfig;
use crate::minimap::MinimapZoom;
use crate::nameplates::NameConfig;
use crate::player::camera::{LookConfig, ZoomLimit, MOUSE_SPEED_RANGE};
use crate::sound::SoundConfig;
use crate::target::ClickConfig;
use crate::ui_loot::LootConfig;
use crate::ui_script::UiScaleCvar;
use benilla_ui::script::UiScript;
use benilla_ui::widget::MINIMAP_ZOOM_LEVELS;
use benilla_world::clutter::ClutterConfig;
use benilla_world::view::{ViewDistance, FARCLIP_RANGE};

/// The host-backed CVars: `(registered name, default)`. Grows one row per knob a settings page
/// actually wires — never ahead of the knob (see the module doc).
pub(crate) const REGISTERED: &[(&str, &str)] = &[
    // The realm the session is on — a REAL 1.12 CVar (`0x83f2d0`, persisted, wow-re
    // `savedvariables-protocol.md`: the client builds its SavedVariables path from it), and a live
    // Lua consumer in the strongest sense the honest-tree rule asks for. `Ace/AceState.lua:27` does
    // `ace.trim(GetCVar("realmName"))` inside `SetGameState`, which every Ace addon runs at
    // PLAYER_ENTERING_WORLD — so a nil there was `gsub(nil)` and took the whole Ace family down.
    // 18 corpus folders read the name.
    //
    // The default is EMPTY, deliberately and not as a guess: the value is written from the session's
    // real realm the moment addons load (`ui_script::addons::load_third_party`), so the default only
    // ever describes a client that has not connected. wow-re records a string
    // `"Last realm connected to"` beside the registration, but that reads like the CVar's HELP text
    // rather than its value and nothing here needs to resolve it — `""` is what `ace.trim` handles
    // cleanly, and inventing a realm name would be worse than admitting we have none yet.
    ("realmName", ""),
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
    // Zone reverb (1153). The binary registers this one `"1"` (`0x4573be`) and we register it
    // `"0"` — the only row here that knowingly leaves the registrar's default, because the
    // reference's reverb is EAX-over-hardware and that hardware has not existed since Vista:
    // `"1"` would ship audio the real client has never actually produced (bug B236).
    // `SoundConfig::reverb` carries the evidence.
    ("SoundReverb", "0"),
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
    // Mouse Sensitivity (1140): 1.12's own `mousespeed` slider (UIOptionsFrameSliders, 0.5..1.5
    // step 0.05), a MULTIPLIER over the camera's own per-pixel rate — which was a frozen constant
    // until this row. Default "1" is the shipped feel exactly, welded to LookConfig::default().
    ("mousespeed", "1"),
    // Max Camera Distance (1140): 1.12's `cameraDistanceMaxFactor` (its MAX_FOLLOW_DIST slider,
    // 1..2 step 0.1) over `cameraDistanceMax`'s 15 yd base. Registered "2" — the factor fully
    // raised — because that IS benilla's shipped 30 yd ceiling, a knowing divergence from the
    // reference's registrar "1" that camera.rs has carried in prose since it was written.
    ("cameraDistanceMaxFactor", "2"),
    // Status Text (1140): 1.12's `statusBarText`, the "always show value / max on a status bar"
    // switch. **No host knob** — its consumer is Lua (TextStatusBar.xml, decision 1082, which was
    // written waiting for this key and reads it on every repaint). Default "0": the reference's
    // out-of-box look is hover-only numerals. That default is BEHAVIOUR-derived, not byte-read —
    // 1.12's registrar value for this var is not pinned in wow-re yet.
    ("statusBarText", "0"),
    // The two chat-bubble switches (1139): 1.12's own registrar CVars over the bubble gate,
    // which held them as `const bool` from 0598 until this window had a page for them.
    // `ChatBubbles` is the reference's registered "1"; `ChatBubblesParty` is ON where the binary
    // registers "0" — the director's `/p` ask, mirrored from BubbleConfig::default().
    ("ChatBubbles", "1"),
    ("ChatBubblesParty", "1"),
    // The minimap's two zoom indices (1131). Byte-verified 1.12 CVars, both registered `"3"`
    // (wow-re, at the `RegisterCVar 0x63db90` argument slot). No options row drives these — the
    // +/- buttons on the minimap do, through `Minimap:SetZoom`, exactly as in the reference, where
    // `set_zoom` writes the live index and `CVar::Set`s the CVar in one breath. The knob is
    // [`crate::minimap::MinimapZoom`], the widget's live index is seeded from it at UI load.
    ("minimapZoom", "3"),
    ("minimapInsideZoom", "3"),
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
    minimap: &'a mut MinimapZoom,
    bubbles: &'a mut BubbleConfig,
    zoom: &'a mut ZoomLimit,
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
        // The client's own parse for this one is literally `!= 0` too (`0x4574d0`: `setne al`).
        "soundreverb" => knobs.sound.reverb = v != 0.0,
        "uiscale" => knobs.scale.0 = v.clamp(0.5, 1.5),
        "farclip" => knobs.view.farclip = v.clamp(*FARCLIP_RANGE.start(), *FARCLIP_RANGE.end()),
        "deselectonclick" => knobs.click.deselect_on_click = v != 0.0,
        "mouseinvertpitch" => knobs.look.invert_pitch = v != 0.0,
        "cameradistancemaxfactor" => knobs.zoom.set_factor(v),
        // The 1.12 slider's own range; an off-grid hand-edit rides between stops, like the others.
        "mousespeed" => {
            knobs.look.sensitivity = v.clamp(*MOUSE_SPEED_RANGE.start(), *MOUSE_SPEED_RANGE.end());
        }
        "autolootdefault" => knobs.loot.auto_loot = v != 0.0,
        "unitnameplayer" => knobs.names.player = v != 0.0,
        "unitnamenpc" => knobs.names.npc = v != 0.0,
        "unitnameown" => knobs.names.own = v != 0.0,
        // A CVar with no HOST knob, because its consumer is Lua (1140). Known — so the caller
        // dirties the config and the value persists — with nothing to apply on this side.
        "statusbartext" => {}
        // The two bubble switches (1139) — flags, like every other pair here.
        "chatbubbles" => knobs.bubbles.all = v != 0.0,
        "chatbubblesparty" => knobs.bubbles.party = v != 0.0,
        // The panel's 0/1/2 lands as the density multiplier ×1/×2/×3; the clamp is the 1.12
        // slider's own range (an off-grid hand-edit rides between stops, like every slider).
        "worlddetail" => knobs.clutter.density = v.clamp(0.0, 2.0) + 1.0,
        // The two zoom indices (1131) clamp exactly like the client's `set_zoom` (`0x6daa10`:
        // clamp at 5) — the widget clamps again on the way in, so a hand-edited level lands
        // in range whichever path it takes.
        "minimapzoom" => knobs.minimap.outdoor = zoom_index(v),
        "minimapinsidezoom" => knobs.minimap.inside = zoom_index(v),
        _ => return false,
    }
    true
}

/// A stored minimap zoom level → a valid index: truncate to int and clamp into
/// `[0, MINIMAP_ZOOM_LEVELS)`, the client's own `set_zoom` clamp.
fn zoom_index(v: f32) -> u8 {
    v.clamp(0.0, f32::from(MINIMAP_ZOOM_LEVELS - 1)) as u8
}

/// Startup: read `benilla-config/config.toml` (absent file = all defaults, not an error) and apply it
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
    mut minimap: ResMut<MinimapZoom>,
    mut bubbles: ResMut<BubbleConfig>,
    mut zoom: ResMut<ZoomLimit>,
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
        minimap: &mut minimap,
        bubbles: &mut bubbles,
        zoom: &mut zoom,
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
    mut minimap: ResMut<MinimapZoom>,
    mut bubbles: ResMut<BubbleConfig>,
    mut zoom: ResMut<ZoomLimit>,
) {
    let Some(mut script) = script else {
        return;
    };
    if !persist.registered {
        script.register_cvars(REGISTERED.iter().copied());
        let flag = |b: bool| if b { "1" } else { "0" }.to_string();
        let session: [(&str, String); 23] = [
            ("MasterVolume", sound.master.to_string()),
            ("SoundVolume", sound.sfx.to_string()),
            ("MusicVolume", sound.music.to_string()),
            ("AmbienceVolume", sound.ambience.to_string()),
            ("MasterSoundEffects", flag(sound.enabled)),
            ("EnableMusic", flag(sound.music_enabled)),
            ("EnableAmbience", flag(sound.ambience_enabled)),
            ("SoundReverb", flag(sound.reverb)),
            ("uiScale", scale.0.to_string()),
            ("farclip", view.farclip.to_string()),
            ("deselectOnClick", flag(click.deselect_on_click)),
            ("mouseInvertPitch", flag(look.invert_pitch)),
            ("mousespeed", look.sensitivity.to_string()),
            ("cameraDistanceMaxFactor", zoom.factor().to_string()),
            ("autoLootDefault", flag(loot.auto_loot)),
            ("UnitNamePlayer", flag(names.player)),
            ("UnitNameNPC", flag(names.npc)),
            ("UnitNameOwn", flag(names.own)),
            // The session density on the panel scale (×1..×3 → 0..2). An env-driven off-grid
            // multiplier seeds off-grid honestly — the dropdown shows the raw number, checks
            // nothing (the 0959 out-of-range posture, dropdown-flavored).
            ("WorldDetail", (clutter.density - 1.0).to_string()),
            ("ChatBubbles", flag(bubbles.all)),
            ("ChatBubblesParty", flag(bubbles.party)),
            ("minimapZoom", minimap.outdoor.to_string()),
            ("minimapInsideZoom", minimap.inside.to_string()),
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
        minimap: &mut minimap,
        bubbles: &mut bubbles,
        zoom: &mut zoom,
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
    ///
    /// **The numeric ones**, which was every one of them until `realmName` — the first
    /// string-valued CVar in the table, and the reason this now filters rather than unwraps. It is
    /// asserted on its own terms in
    /// [`the_only_string_valued_cvar_is_the_realm_and_it_defaults_empty`]; a `parse::<f32>()` over
    /// the whole table would either panic (it did) or quietly need every future string CVar to be
    /// numeric.
    #[test]
    fn registered_defaults_mirror_the_code_truths() {
        let d: BTreeMap<&str, f32> = REGISTERED
            .iter()
            .filter_map(|(n, v)| v.parse::<f32>().ok().map(|f| (*n, f)))
            .collect();
        let sound = SoundConfig::default();
        assert_eq!(d["MasterVolume"], sound.master);
        assert_eq!(d["SoundVolume"], sound.sfx);
        assert_eq!(d["MusicVolume"], sound.music);
        assert_eq!(d["AmbienceVolume"], sound.ambience);
        assert_eq!(d["MasterSoundEffects"] != 0.0, sound.enabled);
        assert_eq!(d["EnableMusic"] != 0.0, sound.music_enabled);
        assert_eq!(d["EnableAmbience"] != 0.0, sound.ambience_enabled);
        // Welded like the rest — and deliberately NOT the binary's registrar "1" (1153).
        assert_eq!(d["SoundReverb"] != 0.0, sound.reverb);
        assert!(!sound.reverb, "zone reverb ships off (decision 1153)");
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
        assert_eq!(d["mousespeed"], LookConfig::default().sensitivity);
        assert_eq!(d["cameraDistanceMaxFactor"], ZoomLimit::default().factor());
        assert_eq!(d["autoLootDefault"] != 0.0, LootConfig::default().auto_loot);
        // The name trio (0992) welds to NameConfig's defaults the same way.
        let names = NameConfig::default();
        assert_eq!(d["UnitNamePlayer"] != 0.0, names.player);
        assert_eq!(d["UnitNameNPC"] != 0.0, names.npc);
        assert_eq!(d["UnitNameOwn"] != 0.0, names.own);
        // ClutterConfig::default() reads $WOW_CLUTTER_DENSITY; the registered default mirrors
        // the env-less ×3 literal (clutter.rs: "Default ×3 = High") on the panel's 0..2 scale.
        assert_eq!(d["WorldDetail"], 2.0);
        // The bubble pair (1139) welds to BubbleConfig's defaults — including the "1" that
        // deliberately disagrees with the binary's registered `ChatBubblesParty` "0" (0598).
        let bubbles = BubbleConfig::default();
        assert_eq!(d["ChatBubbles"] != 0.0, bubbles.all);
        assert_eq!(d["ChatBubblesParty"] != 0.0, bubbles.party);
        // The minimap pair (1131) welds to the widget's own `MINIMAP_DEFAULT_ZOOM`, which is the
        // byte-verified registration default `"3"` — one truth, mirrored in three places.
        let zoom = MinimapZoom::default();
        assert_eq!(d["minimapZoom"], f32::from(zoom.outdoor));
        assert_eq!(d["minimapInsideZoom"], f32::from(zoom.inside));
        assert_eq!(zoom.outdoor, benilla_ui::widget::MINIMAP_DEFAULT_ZOOM);
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
        let mut minimap = MinimapZoom::default();
        let mut bubbles = BubbleConfig::default();
        let mut zoom = ZoomLimit::default();
        let mut knobs = Knobs {
            sound: &mut sound,
            scale: &mut scale,
            view: &mut view,
            look: &mut look,
            click: &mut click,
            loot: &mut loot,
            names: &mut names,
            clutter: &mut clutter,
            minimap: &mut minimap,
            bubbles: &mut bubbles,
            zoom: &mut zoom,
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
        // The sensitivity multiplier clamps to the 1.12 slider's range at the knob.
        assert!(apply_to_knobs("mousespeed", "1.4", &mut knobs));
        assert_eq!(knobs.look.sensitivity, 1.4);
        assert!(apply_to_knobs("mousespeed", "9", &mut knobs));
        assert_eq!(knobs.look.sensitivity, 1.5);
        // The max-orbit factor lands as YARDS on the knob (base 15 x factor), clamped to 1..2.
        assert!(apply_to_knobs("cameraDistanceMaxFactor", "1", &mut knobs));
        assert_eq!(knobs.zoom.max, 15.0);
        assert!(apply_to_knobs("cameradistancemaxfactor", "5", &mut knobs));
        assert_eq!(knobs.zoom.max, 30.0);
        assert!(apply_to_knobs("autoLootDefault", "1", &mut knobs));
        assert!(knobs.loot.auto_loot);
        // The name trio lands on its gates (0992).
        assert!(apply_to_knobs("UnitNameNPC", "0", &mut knobs));
        assert!(!knobs.names.npc);
        assert!(apply_to_knobs("unitnameown", "1", &mut knobs));
        assert!(knobs.names.own);
        // The bubble pair lands on the spawn gate's own knob (1139).
        assert!(apply_to_knobs("ChatBubbles", "0", &mut knobs));
        assert!(!knobs.bubbles.all);
        assert!(apply_to_knobs("chatbubblesparty", "0", &mut knobs));
        assert!(!knobs.bubbles.party);
        // WorldDetail: panel 0/1/2 → density ×1/×2/×3, clamped to the 1.12 slider's range.
        assert!(apply_to_knobs("WorldDetail", "0", &mut knobs));
        assert_eq!(knobs.clutter.density, 1.0);
        assert!(apply_to_knobs("worlddetail", "7", &mut knobs));
        assert_eq!(knobs.clutter.density, 3.0);
        // The zoom pair (1131): each index lands on its own field, clamped like `set_zoom`.
        assert!(apply_to_knobs("minimapZoom", "5", &mut knobs));
        assert_eq!(knobs.minimap.outdoor, 5);
        assert_eq!(knobs.minimap.inside, 3, "the two indices are independent");
        assert!(apply_to_knobs("minimapinsidezoom", "9", &mut knobs));
        assert_eq!(knobs.minimap.inside, MINIMAP_ZOOM_LEVELS - 1);
        assert!(apply_to_knobs("minimapZoom", "-2", &mut knobs));
        assert_eq!(knobs.minimap.outdoor, 0);
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
            .init_resource::<MinimapZoom>()
            .init_resource::<BubbleConfig>()
            .init_resource::<ZoomLimit>()
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

    /// The minimap's zoom rides the same loop, driven from the **engine** rather than a Lua
    /// `SetCVar` (decision 1131): the `+`/`-` buttons call `Minimap:SetZoom`, which writes the live
    /// index and its CVar together — and that has to reach the knob and the file exactly like a
    /// settings row's write does, or the level is forgotten at the next launch.
    #[test]
    fn a_minimap_setzoom_reaches_the_knob_and_the_file() {
        use crate::local_state::test_env::{EnvGuard, ENV_LOCK};
        let _l = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("benilla-mmzoom-{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        let _c = EnvGuard::unset("WOW_CAPTURE");
        let _u = EnvGuard::unset("WOW_UI_SCALE");
        let _f = EnvGuard::unset("WOW_FARCLIP");
        let _d = EnvGuard::unset("WOW_CLUTTER_DENSITY");
        let _h = EnvGuard::set("BENILLA_HOME", tmp.to_str().unwrap());
        // The previous session left the outdoor map zoomed right in.
        crate::local_state::write_atomic(
            &tmp.join("config.toml"),
            "[cvars]\nminimapZoom = \"5\"\n",
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
            .init_resource::<MinimapZoom>()
            .init_resource::<BubbleConfig>()
            .init_resource::<ZoomLimit>()
            .add_plugins(CvarPlugin);
        app.insert_non_send_resource(UiScript::new().unwrap());
        app.update();

        // Startup restored the knob, and the VM's table answers with it — which is what the UI-load
        // seam hands to `set_minimap_zoom` when the widget is born.
        assert_eq!(app.world().resource::<MinimapZoom>().outdoor, 5);
        assert_eq!(app.world().resource::<MinimapZoom>().inside, 3);
        let seed = {
            let z = app.world().resource::<MinimapZoom>();
            (z.outdoor, z.inside)
        };
        {
            let mut script = app.world_mut().non_send_resource_mut::<UiScript>();
            assert_eq!(script.cvar("minimapZoom").as_deref(), Some("5"));
            // The UI-load seam's own order: the widget is born (at its `MinimapState` default),
            // THEN the persisted level is pushed into it. Seeding a widget that does not exist yet
            // is a no-op — which is exactly why that call sits after `load_ingame_ui`.
            script.run(r#"m = CreateFrame("Minimap", "Mini")"#).unwrap();
            script.set_minimap_zoom(seed.0, seed.1);
            assert_eq!(script.eval::<u8>("return m:GetZoom()").unwrap(), 5);
            // The player zooms out two notches with the minimap's own buttons.
            script.run("m:SetZoom(m:GetZoom() - 2)").unwrap();
        }
        app.update();
        assert_eq!(app.world().resource::<MinimapZoom>().outdoor, 3);

        app.world_mut().write_message(AppExit::Success);
        app.update();
        let text = std::fs::read_to_string(tmp.join("config.toml")).unwrap();
        assert!(
            !text.contains("minimapZoom"),
            "back at the registered default 3, so it leaves the diff entirely:\n{text}"
        );

        // …and one more notch out is a real diff again.
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run("m:SetZoom(1)")
            .unwrap();
        app.update();
        app.world_mut().write_message(AppExit::Success);
        app.update();
        let text = std::fs::read_to_string(tmp.join("config.toml")).unwrap();
        assert!(text.contains("minimapZoom = \"1\""), "{text}");
        assert_eq!(app.world().resource::<MinimapZoom>().outdoor, 1);
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
    /// **`realmName` is the table's one string-valued CVar, and it defaults EMPTY.**
    ///
    /// Empty rather than a guess: the value is written from the session's real realm by
    /// `set_realm_name`, so the default only ever describes a client that has not connected.
    /// wow-re records `"Last realm connected to"` beside the registration, but that reads like the
    /// CVar's HELP text rather than its value, and nothing here needs it resolved — `""` is what
    /// `Ace/AceState.lua:27`'s `ace.trim(GetCVar("realmName"))` handles cleanly, and inventing a
    /// realm name would be worse than admitting we have none yet.
    ///
    /// Pinned as "the ONE" so a second string CVar has to come here and think about the numeric
    /// test above rather than silently widening it.
    #[test]
    fn the_only_string_valued_cvar_is_the_realm_and_it_defaults_empty() {
        let strings: Vec<&str> = REGISTERED
            .iter()
            .filter(|(_, v)| v.parse::<f32>().is_err())
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(strings, vec!["realmName"]);
        let realm = REGISTERED
            .iter()
            .find(|(n, _)| *n == "realmName")
            .map(|(_, v)| *v);
        assert_eq!(realm, Some(""));
    }
}
