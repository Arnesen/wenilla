//! benilla's **CVar host** — registration, knob sync, and the client's first persistence
//! (decision 0954). The engine holds the table and the Lua API ([`benilla_ui::script`]'s
//! `GetCVar`/`SetCVar`); this module is everything host-side:
//!
//! - **The registered set** ([`REGISTERED`]): only vars something actually reads — a host knob,
//!   or (since 1140) a live Lua consumer, which is the same rule seen from the UI side and the
//!   only refinement the honest-tree law has needed.
//!
//!   **A row's default is the REFERENCE's default** (decision 1804), and every row says where it
//!   stands against it — that is [`Registered::reference`], a mandatory third column with no
//!   "unknown" variant, so a new row cannot be added without answering the question. Two tests
//!   hold it: one checks each row's claim in both directions (a `Same` that drifted, *and* a
//!   `Deviates` that quietly came back into agreement), the other pins the deviation set as a
//!   readable list. Beside that, and unchanged, each default is still **welded to the code
//!   constant it mirrors** — `SoundConfig`, `NameConfig`, `ZoomLimit` and the rest — so the CVar
//!   table and the knob it feeds cannot drift apart either.
//!
//!   **The reference column has one source**: wow-re's
//!   `system/cvar/scratch/registered-defaults-census.md` and the regenerable manifest beside it,
//!   `re/cvar/cvar-register-sites.tsv` — all 214 of the reference's `CVar::Register` sites with
//!   name, help, flags, default string, callback, category and record global. 1804 dispatched the
//!   §5 round that built it; a new row looks its answer up there rather than re-deriving it, and a
//!   row that disagrees with it is a contradiction to resolve before it lands (`method.md`).
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
use crate::player::camera::{
    FollowConfig, FollowStyle, LookConfig, ZoomLimit, FOLLOW_SPEED_RANGE, MOUSE_SPEED_RANGE,
};
use crate::portrait::PaneRate;
use crate::sound::SoundConfig;
use crate::target::ClickConfig;
use crate::ui_loot::LootConfig;
use crate::ui_script::UiScaleCvar;
use crate::video::VideoConfig;
use crate::vplates::VPlateMode;
use crate::world_backdrop::{RenderScale, RENDER_SCALE_RANGE};
use benilla_ui::script::UiScript;
use benilla_ui::widget::MINIMAP_ZOOM_LEVELS;
use benilla_world::clutter::ClutterConfig;
use benilla_world::view::{MsaaSetting, ViewDistance, FARCLIP_RANGE, MSAA_RANGE};

/// One host-backed CVar: its registered name, benilla's shipped default, and — the column that
/// exists so a divergence is a *decision* rather than an accident — **what the reference ships**
/// ([`Reference`]).
///
/// **The standard this table encodes: a benilla option's default IS the reference's.** Shipping
/// something else is allowed and sometimes right, but it costs a [`Reference::Deviates`] row
/// naming the reference's own value and the reason. `Reference` has no `Default` and no "unknown"
/// variant, so adding a row means answering the question; and because [`Reference::Same`] and
/// [`Reference::Deviates`] both carry the reference's value, the test
/// [`tests::defaults_stand_where_the_reference_column_says`] checks the claim in both directions —
/// a `Same` row that stopped matching fails, and so does a `Deviates` row that has quietly come
/// back into agreement.
pub(crate) struct Registered {
    /// The registered name, in the reference's own spelling.
    pub(crate) name: &'static str,
    /// What a fresh `benilla-config` runs at — seeded into the knob, and what `GetCVar` answers
    /// until the player moves it.
    pub(crate) default: &'static str,
    /// Where that value stands against the reference's.
    ///
    /// `#[allow(dead_code)]` because this column's readers are a **human** and
    /// [`tests::defaults_stand_where_the_reference_column_says`] — nothing at runtime consults
    /// it, and nothing should: it records what the *reference* does, which is an input to the
    /// choice above it, never a value this client acts on.
    #[allow(dead_code)]
    pub(crate) reference: Reference,
}

/// benilla's default, weighed against the reference's own.
///
/// **What "the reference's default" means here** is the **registered factory default** — the
/// string the real client's `CVar::Register` (`0x63db90`) call passes for that name, byte-read —
/// or, for a setting 1.12 keeps FrameXML-side instead of as a CVar, the value
/// `UIOptionsFrame.lua` boots it at. Two readings it deliberately is **not**, both of which have
/// misled a reader before:
///
/// - **Not what the reference install's `Config.wtf` says.** That file is one player's saved
///   *diff*: `SaveConfig 0x63d980` writes only what has moved off its default, so a line's mere
///   presence is proof the registered default is something *else* (wow-re
///   `cvar/scratch/graphics-cost-cvar-census.md` §10 — the trap it exists to close).
/// - **Not, by itself, what a fresh install ends up running at.** `hwDetect` rewrites sixteen
///   video CVars out of `VideoHardware.dbc` before the first frame, and the `useUiScale`-OFF leg
///   computes a UI scale of its own. Where the client's own boot code overrides the registered
///   string like that, benilla follows the *behaviour* and the row says so — that is
///   [`Reference::Overridden`], not a deviation.
///
/// Every row's provenance — the register-site VA, or the FrameXML line — belongs in the comment
/// above it. That is not decoration: it is what lets the next reader re-check the claim instead
/// of trusting this table.
#[allow(dead_code)] // read by a human and by the test — see `Registered::reference`
pub(crate) enum Reference {
    /// The reference registers this exact default string, and benilla ships it too. The copy is
    /// deliberate: the test compares the two, so an edit to `default` that forgets the reference
    /// fails here rather than shipping.
    Same(&'static str),
    /// The reference *registers* `registered`, but its own boot code overwrites that before the
    /// first frame, and benilla's `default` is what that code lands on — faithful to the client's
    /// behaviour, which is what the standard asks for. `why` is the override.
    Overridden {
        registered: &'static str,
        why: &'static str,
    },
    /// The reference ships `value`; benilla knowingly ships something else. `why` is the reason
    /// and the decision that ruled it. **This is the variant that answers "where do we differ?"**
    /// — every row here is a standing choice somebody made, reviewable as a list.
    Deviates {
        value: &'static str,
        why: &'static str,
    },
    /// The reference has no such setting to match: benilla's own knob (`renderScale`), or an era
    /// name for something 1.12 never made settable. `why` says which — and, where the reference
    /// still *behaves* some way, what that behaviour is.
    Ours(&'static str),
}

/// A row whose default is the reference's own registered string.
const fn same(name: &'static str, default: &'static str) -> Registered {
    Registered {
        name,
        default,
        reference: Reference::Same(default),
    }
}

/// A row that follows the reference's *behaviour* where its own boot code overrides the
/// registered string — see [`Reference::Overridden`].
const fn overridden(
    name: &'static str,
    default: &'static str,
    registered: &'static str,
    why: &'static str,
) -> Registered {
    Registered {
        name,
        default,
        reference: Reference::Overridden { registered, why },
    }
}

/// A row that knowingly leaves the reference's default — `value` is the reference's, `why` the
/// recorded reason.
const fn deviates(
    name: &'static str,
    default: &'static str,
    value: &'static str,
    why: &'static str,
) -> Registered {
    Registered {
        name,
        default,
        reference: Reference::Deviates { value, why },
    }
}

/// A row the reference has no counterpart for.
const fn ours(name: &'static str, default: &'static str, why: &'static str) -> Registered {
    Registered {
        name,
        default,
        reference: Reference::Ours(why),
    }
}

/// The table as the script VM's registrar wants it — `(name, default)` pairs, in table order.
///
/// The registrar has no use for the [`Reference`] column: that column is for *us* (and for the
/// test that holds the standard), never for the engine.
pub(crate) fn registered_pairs() -> impl Iterator<Item = (&'static str, &'static str)> {
    REGISTERED.iter().map(|r| (r.name, r.default))
}

/// The host-backed CVars. Grows one row per knob a settings page actually wires — never ahead of
/// the knob (see the module doc) — and every row states where its default stands against the
/// reference's ([`Registered`]).
pub(crate) const REGISTERED: &[Registered] = &[
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
    same("realmName", ""),
    // The address of the logon server — the reference's own CVar, byte-verified in `WoW.exe`
    // (the registration's string neighbours are `realmlist.wtf`, "Address of realm list server"
    // and `us.logon.worldofwarcraft.com:3724`; wow-re `mpq/scratch/startup-order-A.md` row 62).
    // A **string** row, so it is matched ahead of the numeric parse in `apply_to_knobs`.
    // The default diverges knowingly — see `realmlist::DEFAULT_REALMLIST`.
    deviates(
        crate::realmlist::CVAR_REALMLIST,
        crate::realmlist::DEFAULT_REALMLIST,
        "us.logon.worldofwarcraft.com:3724",
        "1667: that host has not resolved since 2019, so shipping it makes every first launch a \
         DNS failure; benilla dials the machine it is running on",
    ),
    same("MasterVolume", "1"),
    same("SoundVolume", "1"),
    same("MusicVolume", "0.4"),
    same("AmbienceVolume", "0.6"),
    // The three 1.12 sound enables (registrar defaults all "1", wow-re B10):
    // `MasterSoundEffects` is the MASTER "Enable All Sound" checkbox (SoundOptionsFrame.lua
    // index 1 — its callback sets the engine-wide pause flag), NOT an SFX-only toggle; 1.12
    // has no `EnableSound`/`EnableSFX` at all.
    same("MasterSoundEffects", "1"),
    same("EnableMusic", "1"),
    same("EnableAmbience", "1"),
    // Error speech (1815) — the race/sex refusal lines your character says. A real 1.12 CVar
    // (`CVar::Register` at `0x457877`, registrar default `"1"`; wow-re
    // `re/cvar/cvar-register-sites.tsv` row 54) and a real 1.12 checkbox: SoundOptionsFrame.lua's
    // `ENABLE_ERROR_SPEECH`, index 4, which the master enable greys along with Ambience.
    same("EnableErrorSpeech", "1"),
    // Zone reverb (1153). The binary registers this one `"1"` (`0x4573be`) and we register it
    // `"0"` — the only row here that knowingly leaves the registrar's default, because the
    // reference's reverb is EAX-over-hardware and that hardware has not existed since Vista:
    // `"1"` would ship audio the real client has never actually produced (bug B236).
    // `SoundConfig::reverb` carries the evidence.
    deviates(
        "SoundReverb",
        "0",
        "1",
        "1153: the reference's reverb is EAX-over-hardware and that hardware has not existed \
         since Vista, so \"1\" would ship audio the real client has never actually produced \
         (B236)",
    ),
    // The output limiter (1551) — benilla's own, not a 1.12 CVar. The reference needs no such DSP
    // (its mix is FMOD 3's and its headroom lives in the SFX-bus auto-duck); benilla sums into f32
    // behind a hard clamp, and every WoW SFX is mastered to full scale, so two overlapping kits
    // clip. Registered so the fix can be A/B'd live against the defect it fixes.
    ours(
        "SoundOutputLimiter",
        "1",
        "1551: benilla's own — the reference's FMOD 3 mix needs no such DSP; we sum into f32 \
         behind a hard clamp, and every WoW SFX is mastered to full scale",
    ),
    overridden(
        "uiScale",
        "0.9",
        "1.0",
        "a fresh reference client never consults this CVar: `useUiScale` registers \"0\" \
         (`0x48fce4`), and the OFF leg `0x492f70` computes clamp(768/height, 0.9, 1.0) instead — \
         0.9 at 854 px tall and up, which is every window we ship against. It is 1.0 at 768 and \
         below, where our flat 0.9 does diverge; `ui_script::DEFAULT_UI_SCALE` carries that. \
         `useUiScale` itself has no row here: nothing reads it, and registering it would only \
         offer a switch whose ON path we do not implement",
    ),
    same("farclip", "350"),
    // The Controls-page trio (0961). `deselectOnClick`/`mouseInvertPitch` are 1.12's own
    // Interface Options CVars (UIOptionsFrame.lua indices 45/1); their defaults are the
    // reference behaviors benilla already shipped (empty-world click clears the target; no
    // pitch invert). `autoLootDefault` is era's — no 1.12 CVar exists, vanilla only had the
    // shift gesture — default off, like era's engine registrar.
    same("deselectOnClick", "1"),
    // *Block Trades* (decision 1764) — 1.12's own `BlockTrades` (`0x842fbc`), the General-box
    // checkbox at index 14 whose tooltip is "Block all incoming trade requests.". Registered
    // **"0"**: the reference's own `0x4bf7bc` leg only refuses when the CVar is set, so an
    // unset/absent value has to mean "trades allowed" — and a client that shipped with trades
    // blocked would refuse every trade until the player found the box. The knob is
    // [`crate::ui_trade::BlockTrades`], read by the incoming-request answerer.
    same("BlockTrades", "0"),
    // 1.12's own `autoSelfCast` — a friendly cast that binds nothing falls back to the caster.
    // The behaviour has been here since the cast arm landed, welded to a Resource default; it is a
    // CVar now because 1.12's `TOGGLEAUTOSELFCAST` binding is `GetCVar`/`SetCVar` over this exact
    // name and there was nothing for it to toggle (decision 1745).
    //
    // Register site `0x6e731d`, default string `"0"`, record `[0xceac34]`, one reader at
    // `0x6e53d7` (wow-re `cvar/scratch/registered-defaults-census.md`, 1804's §5 round). **This
    // row used to cite `0x870dc0` as the record; that is the NAME string** — corrected there.
    //
    // **benilla ships it ON and the reference registers "0"** — a named deviation that predates
    // this row (`cast_target::AutoSelfCast`): with it off, an unbindable friendly cast falls into
    // the reference's targeting-cursor machine, which is unmodeled, leaving no path at all. Flip
    // to the reference's default when that machine lands.
    deviates(
        "autoSelfCast",
        "1",
        "0",
        "1745: with it off, an unbindable friendly cast falls into the reference's \
         targeting-cursor machine, which is unmodeled — leaving no path at all. Flip when that \
         machine lands",
    ),
    // The five saved camera views and the live index (decision 1745) — the reference's own
    // sixteen names and its own shipped default strings, both read out of `WoW.exe` and owned by
    // [`crate::player::camera_view`], which is also the only writer. Registered here so they are
    // ordinary CVars: persisted as a diff like everything else, readable from a macro, and
    // reachable by `SetCVar` — which is what makes a `SaveView` survive a restart.
    same(
        crate::player::camera_view::CVAR_ACTIVE_VIEW,
        crate::player::camera_view::ACTIVE_VIEW_DEFAULT,
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[0][0],
        crate::player::camera_view::VIEW_DEFAULTS[0][0],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[0][1],
        crate::player::camera_view::VIEW_DEFAULTS[0][1],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[0][2],
        crate::player::camera_view::VIEW_DEFAULTS[0][2],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[1][0],
        crate::player::camera_view::VIEW_DEFAULTS[1][0],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[1][1],
        crate::player::camera_view::VIEW_DEFAULTS[1][1],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[1][2],
        crate::player::camera_view::VIEW_DEFAULTS[1][2],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[2][0],
        crate::player::camera_view::VIEW_DEFAULTS[2][0],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[2][1],
        crate::player::camera_view::VIEW_DEFAULTS[2][1],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[2][2],
        crate::player::camera_view::VIEW_DEFAULTS[2][2],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[3][0],
        crate::player::camera_view::VIEW_DEFAULTS[3][0],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[3][1],
        crate::player::camera_view::VIEW_DEFAULTS[3][1],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[3][2],
        crate::player::camera_view::VIEW_DEFAULTS[3][2],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[4][0],
        crate::player::camera_view::VIEW_DEFAULTS[4][0],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[4][1],
        crate::player::camera_view::VIEW_DEFAULTS[4][1],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[4][2],
        crate::player::camera_view::VIEW_DEFAULTS[4][2],
    ),
    same("mouseInvertPitch", "0"),
    ours(
        "autoLootDefault",
        "0",
        "0961: 1.12 has no auto-loot CVar at all — vanilla offers only the shift gesture, so OFF \
         IS the reference's own behaviour; the spelling is era's",
    ),
    // The overhead-name trio (0992): 1.12's own UnitName* CVars (UIOptionsFrame.lua indices
    // 21/30/67) over the nameplates module's gates. Defaults mirror `NameConfig::default()` and
    // are the binary's own, byte-read at the `0x6c7470` registrar (wow-re
    // `object-layer/scratch/overhead-name.md`, name string / default string per row, folded into
    // mask `0xce8720`) — `UnitNamePlayer` `0x86c694` → `"1"` `0x82e748`, `UnitNameNPC` `0x86c6a4`
    // and `UnitNameOwn` `0x86c6b0` → `"0"` `0x82e570`. Corroborated the other way by the
    // reference install's own `Config.wtf`, which carries `SET UnitNameNPC "1"` and
    // `SET UnitNameOwn "1"`: `SaveConfig 0x63d980` writes only what has MOVED off its default, so
    // those two lines existing is proof the defaults are not "1".
    //
    // **npc and own shipped ON here from 2026-07-12 until 1804** — see `NameConfig`'s doc.
    same("UnitNamePlayer", "1"),
    same("UnitNameNPC", "0"),
    same("UnitNameOwn", "0"),
    // The two V-plate toggles over `VPlateMode` — the engine bitmask `[0xc4da34]`'s bit 0 and
    // bit 3. 1.12 registers NO nameplate CVar (wow-re, VERIFIED — the bitmask is a plain runtime
    // global, persisted FrameXML-side as the `RegisterForSave`'d `NAMEPLATES_ON` /
    // `FRIENDNAMEPLATES_ON`), so these take the LATER-era engine's names: the `autoLootDefault`
    // posture, where benilla's persistence IS the CVar store (0954) and a setting with no 1.12
    // CVar gets the era spelling rather than an invented one.
    //
    // **`same`, not `ours`, and that distinction is the point**: the reference has no CVar to
    // match, but it very much has a *setting* to match, and it boots both halves OFF —
    // `UIOptionsFrame_Init` assigns `NAMEPLATES_ON = nil` / `FRIENDNAMEPLATES_ON = nil` and
    // `OptionsFrame_ApplySavedSettings` only calls `ShowNameplates()` on a truthy SAVED value
    // (the install's `Interface\FrameXML\UIOptionsFrame.lua` l.180-183 / l.769-775). A fresh
    // 1.12 client draws no plates until V is pressed. Enemy plates shipped ON here from 0167
    // until 1804 — `VPlateMode::default()` carries that history.
    same(crate::vplates::CVAR_ENEMIES, "0"),
    same(crate::vplates::CVAR_FRIENDS, "0"),
    // World detail (0992) — the ENVIRONMENT_DETAIL slider's 0..2, over the clutter-density knob.
    // 0 is the client's bare `frillDensity` baseline (×1 = 16 visits), each step +1×, so 0/1/2 are
    // the 16/32/48 `SetWorldDetail` itself writes.
    //
    // **Not a 1.12 CVar** (corrected 1804 — this row used to call it "1.12's video-panel var").
    // `WorldDetail` does not exist as a string in `WoW.exe` (scanned; positive control
    // `frillDensity` present), and `OptionsFrame.lua:27`'s `func = "WorldDetail"` is a *function
    // name suffix*: the slider calls the engine's `GetWorldDetail`/`SetWorldDetail`. So the
    // spelling is the API's — the `autoLootDefault` posture.
    //
    // **"1" IS the reference's own setting, and getting there took two goes.** `SetWorldDetail
    // 0x488dd0` writes **two** CVars per stop: `frillDensity` {16,32,48} and `SmallCull`
    // {0.07,0.04,0.01}. The registered pair is `frillDensity` **16** (stop 0) with `SmallCull`
    // **0.04** (stop **1**) — the reference boots an inconsistent pair — and `GetWorldDetail`
    // reads only `SmallCull`. So its own slider **reads Medium at boot**, which is this row.
    // Mid-1804 this was filed as a deviation against "0" on two true facts that are not the
    // answer: `OptionsFrame.lua:430`'s Defaults ladder yields 0, and `frillDensity` registers 16.
    // A partly-verified mechanism is not the mechanism (wow-re
    // `cvar/scratch/registered-defaults-census.md`, the §5 round 1804 dispatched).
    //
    // What IS still divergent is the grass, and that is a *mapping* difference rather than a
    // default: our stop 1 scatters ×2 (32) where the reference's boot `frillDensity` is 16
    // registered, 24 after `hwDetect` reads `VideoHardware.dbc` (row 170 on any D3D9-class part,
    // 8 on the weakest). 24 is on no stop of ours; 1649 broke that tie toward the denser stop,
    // because erring sparse is the worse failure for a knob about ground cover.
    same("WorldDetail", "1"),
    // Mouse Sensitivity (1140): 1.12's own MOUSE_SENSITIVITY slider (`UIOptionsFrameSliders` row
    // 1, 0.5..1.5 step 0.05), a MULTIPLIER over the camera's own per-pixel rate — which was a
    // frozen constant until this row. Default "1" is the shipped feel exactly, welded to
    // `LookConfig::default()`.
    //
    // **The spelling is FrameXML's, not the binary's**, and that is deliberate: `WoW.exe` holds
    // `mouseSpeed` (capital S, register site `0x402c7b`) while `UIOptionsFrame.lua`'s slider
    // writes `cvar = "mousespeed"`. The reference reconciles them by looking CVars up
    // case-insensitively (`SStrCmpI`, wow-re `cvar/cvar.md`), and so do we (`apply_to_knobs`
    // lowercases first), so both spellings answer. We take the one the interface uses, because
    // that is the one an addon will type.
    //
    // **The VALUE agrees and the MECHANISM does not** (wow-re
    // `cvar/scratch/registered-defaults-census.md` §, 1804). The reference's default is not a
    // literal at all: it is `sprintf("%1.1f", SPI_GETMOUSESPEED × 0.1)`, which is `"1.0"` on a
    // stock Windows host — so "1" is the right number. But its record has **zero readers**: the
    // slider drives the *operating system's* pointer speed through `SPI_SETMOUSESPEED` (clamped
    // [0.1, 2.0]), not an in-engine gain. benilla will not reach out and repoint the OS mouse, so
    // ours is a multiplier over our own per-pixel rate — the same dial, the same range, the same
    // resting value, a different thing underneath. Recorded here rather than filed as a deviation
    // because the default is the question this table answers, and the default matches.
    same("mousespeed", "1"),
    // Max Camera Distance (1140): 1.12's `cameraDistanceMaxFactor` (its MAX_FOLLOW_DIST slider,
    // 1..2 step 0.1) over `cameraDistanceMax`'s 15 yd base. **"1", the reference's registrar
    // value** (wow-re `ui/scratch/follow-camera.md`: "cameraDistanceMax 15.0,
    // cameraDistanceMaxFactor 1.0") — so the shipped ceiling is 15 yd, not the 30 this row
    // registered from 1140 until 1804. `ZoomLimit`'s doc carries why that changed; the slider
    // still reaches 2.
    same("cameraDistanceMaxFactor", "1"),
    // Camera Following Style (1493, re-pinned by 1502): 1.12's `cameraSmoothStyle` — the
    // auto-return that swings the camera back behind the character. Registered "1" = Smart, which
    // is BOTH the reference's registrar default (byte-verified: the argument is loaded from
    // `[0x84f4f4]` -> "1" at the `0x50ba92` register site) and the director's call; benilla behaved
    // as Never unconditionally until this row. The enum is the ENGINE's — 0 Never, 1 Smart,
    // 2 Always — NOT the 1/2/3 the reference's own dropdown writes; see `FollowStyle`.
    same("cameraSmoothStyle", "1"),
    // Its sibling selector (1502), also registered "1": the reference reads THIS style instead
    // whenever the state mask contains Track or Fear — the externally-driven states — indexing the
    // same matrices. No row on any 1.12 panel, here or there; the reader is the host.
    same("cameraSmoothTrackingStyle", "1"),
    // The auto-follow's rate (1502), °/s — 1.12's own AUTO_FOLLOW_SPEED slider
    // (`UIOptionsFrameSliders`, 90..270 by 10), registered at the binary's "180.0" (`[0xbe1070]`).
    // It sets the transition's DURATION (`|dyaw| / rate * factor`), so it is an average rate, not a
    // slew. No row yet — the slider is a one-line follow-on now that the knob exists.
    same("cameraYawSmoothSpeed", "180"),
    // Status Text (1140): 1.12's `statusBarText`, the "always show value / max on a status bar"
    // switch. **No host knob** — its consumer is Lua (TextStatusBar.xml, decision 1082, which was
    // written waiting for this key and reads it on every repaint). Default "0": the reference's
    // out-of-box look is hover-only numerals.
    //
    // **Byte-read since 1804** — register site `0x48fc34`, default string `"0"`, record
    // `[0xb4d904]`, and a whole-image census finds that record has **no engine reader at all**:
    // this CVar is FrameXML's alone, which is exactly the shape this row was built for (wow-re
    // `cvar/scratch/registered-defaults-census.md`). It used to concede "behaviour-derived, not
    // byte-read"; that hedge is retired.
    same("statusBarText", "0"),
    // Enhanced Tooltips (B230): 1.12's `UberTooltips`, the *Enhanced Tooltips* checkbox
    // (`UIOptionsFrame.lua:15`, `USE_UBERTOOLTIPS`). **No host knob** — its consumers are Lua, and
    // there are three: PetActionBar.xml forks the whole tooltip on it (a token's own text with the
    // binding appended, vs the engine's pet-spell channel), ActionBar.xml and StanceBar.xml fork
    // their anchor. Registered "1" — byte-read, not behaviour-derived: WoW.exe `0x48fdd9`, default
    // string `0x82e748`, with the sibling rows `BlockTrades`→"0" and `UnitNameRenderMode`→"2"
    // confirming the layout. Those three Lua sites each carried the reference's fork in prose and
    // then collapsed it to this default, on the stated premise that benilla shipped no CVar state
    // for anything to move. That premise expired with 0954, and this row is what un-collapses them.
    same("UberTooltips", "1"),
    // The two chat-bubble switches (1139): 1.12's own registrar CVars over the bubble gate,
    // which held them as `const bool` from 0598 until this window had a page for them. Both are
    // the binary's own (registrar `0x603280` — wow-re `object-layer/scratch/chat-bubble.md`:
    // `ChatBubbles` "1", `ChatBubblesParty` "0"); the party half shipped ON from 0598 to 1804 on
    // the director's `/p` ask, and is a click away on the Chat page.
    same("ChatBubbles", "1"),
    same("ChatBubblesParty", "0"),
    // *Detailed Loot Information* (1589, the Chat page) — 1.12's `showLootSpam`, whose subject is
    // group LOOT ROLLS (its own tooltip: "Uncheck this to hide individual loot roll messages and
    // only show the winner"). Registered `"1"`, **byte-read**: wow-re's census of `0xb4e2bc`
    // (`lootroll-chat-and-lifecycle.md` §4) has the register site at `0x48fd1c`, name `0x8430a0`,
    // default string `0x82e748` = "1", **category** 5 — and exactly four references to the global,
    // one writer and three readers, all in the roll-line composers. The knob is
    // [`crate::ui_loot::LootConfig::show_loot_spam`], welded to that default below.
    same("showLootSpam", "1"),
    // *Guild Member Alert* (1589, the Chat page) — 1.12's `guildMemberNotify`, whose registered
    // help string says what it does: "Receive notification when guild members log on/off".
    //
    // Registered **`"0"`** — this is one of the few rows that ships a feature OFF, and it is
    // byte-read rather than chosen: the register site `0x5e24c7` pushes default `0x82e570` = "0"
    // (§5, wow-re `system/object-layer/scratch/guild-signon-cvar-gate.md`). A stock 1.12 client is
    // silent when a guildmate logs in, and a whole-image census of the record global `0xc4d3c4`
    // finds exactly two readers, both inside `SMSG_GUILD_EVENT`'s handler. The knob is
    // [`crate::ui_guild::GuildMemberNotify`]; the other three conjuncts of the line's display
    // condition live on `ui_guild::apply::event`.
    same("guildMemberNotify", "0"),
    // The minimap's two zoom indices (1131). Byte-verified 1.12 CVars, both registered `"3"`
    // (wow-re, at the `RegisterCVar 0x63db90` argument slot). No options row drives these — the
    // +/- buttons on the minimap do, through `Minimap:SetZoom`, exactly as in the reference, where
    // `set_zoom` writes the live index and `CVar::Set`s the CVar in one breath. The knob is
    // [`crate::minimap::MinimapZoom`], the widget's live index is seeded from it at UI load.
    same("minimapZoom", "3"),
    same("minimapInsideZoom", "3"),
    // The addon version gate (decision 1292): 1.12's own `checkAddonVersion`, the *Load out of
    // date AddOns* checkbox INVERTED. Registrar default "1" = check enforced = box unticked —
    // byte-verified (wow-re `addon-version-gate.md` §1.1: the key appears in Config.wtf exactly
    // while force-load is on and vanishes when it is turned off, `SaveConfig 0x63d980`'s
    // skip-default rule). No host knob: its consumers are the load walk (via the persisted value,
    // [`CvarPersist::addon_version_check`]) and the gate's live per-query read in the VM.
    same("checkAddonVersion", "1"),
    // Vertical Sync — 1.12's own `gxVSync`, the Video Options checkbox at index 5
    // (`OptionsFrame.lua`'s `OptionsFrameCheckButtons["VERTICAL_SYNC"]`, in the install's
    // FrameXML). The knob is [`crate::video::VideoConfig::vsync`], which the window's
    // `present_mode` follows.
    //
    // Default "1" is BEHAVIOUR-derived, not byte-read: 1.12's registrar value for this var is not
    // pinned in wow-re, and "1" is what benilla actually ships — the primary window is born at
    // `PresentMode::default()` (Fifo), and a test in `video.rs` welds the two together.
    //
    // Two knowing departures from the reference row, both stated on [`crate::video`]: its
    // `gxRestart = 1` does not apply (wgpu swaps the presentation interval live, so the box takes
    // effect on click), and `$WOW_NOVSYNC=1` overrides it session-only, below.
    same("gxVSync", "1"),
    // **Display mode** (decisions 1627, 1650) — 1.12's own `gxWindow`, worn since 1650 as modern
    // Classic's two-entry *Display Mode* dropdown rather than 1.12's *Windowed Mode* checkbox: the
    // two states 1627 settled on ARE that client's two (its own `Graphics.lua` builds the list from
    // `VIDEO_OPTIONS_WINDOWED_FULLSCREEN` and `VIDEO_OPTIONS_WINDOWED`, and nothing else), and a
    // checkbox could only name one of them. The knob is [`crate::video::VideoConfig::display`],
    // which the window's `mode` follows.
    //
    // Default **"0" = not windowed**, which is the reference's own default and every shipped
    // game's — but "0" does NOT mean what it means in 1.12. The reference mode-sets the display;
    // we raise a **borderless** fullscreen window, and ship no exclusive mode at all.
    // [`crate::video`] carries the three-platform argument for why that is the whole of it (short
    // version: Wayland cannot do exclusive, X11's XRandR path cannot restore the desktop after a
    // crash, macOS has no such mode, and WoW itself dropped exclusive fullscreen in 8.0.1).
    //
    // Departs from the reference row's `gxRestart = 1` exactly like `gxVSync` above: ours applies
    // on the click.
    // Byte-read `"0"` at register site `0x63a889` — **for enUS**. Three defaults in this binary
    // are locale-conditional and this is one: `gxWindow` and `gxMaximize` register `"1"` on zhCN,
    // `AutoInteract` `"1"` on koKR (wow-re `cvar/scratch/registered-defaults-census.md`, which
    // caught its own instrument publishing a single arm mid-round). benilla is enUS-only, so `"0"`
    // is the answer here; the note exists so the next reader does not take a locale-conditional
    // default for an unconditional one.
    same("gxWindow", "0"),
    // The **windowed** size, `gxResolution` — 1.12's own CVar name, narrowed to half its job.
    // There it is the display mode *and* the backbuffer; here it is only what "windowed" means,
    // because fullscreen is the monitor's own size and we expose no mode list to pick from (the
    // deviation decision 1092 already records for `GxAspect`, unchanged by 1627).
    //
    // A **string** CVar, like it is in the reference — the one row [`apply_to_knobs`] has to match
    // ahead of its numeric parse. Default is the 1600×900 that was the client's only size before
    // 1627, so a windowed run is bit-for-bit where it was.
    deviates(
        "gxResolution",
        "1600x900",
        "640x480",
        "1627: narrowed to the WINDOWED size only — fullscreen is the monitor's own and we expose \
         no mode list, and 640x480 is not a window anyone would ship a client at",
    ),
    // The body panes' half-rate render (decision 1444) — **benilla's own CVar**, no 1.12
    // counterpart: the reference draws its doll inside the main pass (no second view exists to
    // rate-limit), while our RTT booths (1069) re-run the render graph per pane per frame. "1" =
    // the doll renders at half the frame rate while its pane is open; the knob is
    // [`crate::portrait::PaneRate`], and the default mirrors it (welded below).
    //
    // **Default ON (half-rate) — restored by 1607.** 1444 shipped it on; 1559 turned it off for
    // a smoother doll (a look-call); the 08-25 weak-GPU perf reports (B329) measured the cost —
    // ~1.6 ms at 1600×900, 7.6 ms at 4K, per frame while a body pane is open — and the director
    // retested the 30 fps doll as fine. Full-rate is one `SetCVar("boothHalfRate", 0)` away.
    ours(
        "boothHalfRate",
        "1",
        "1444/1607: benilla's own — the reference draws its doll inside the main pass and has no \
         second view to rate-limit",
    ),
    // The select screen's memory of who you last entered the world as (decision 1622) — 1.12's
    // own `lastCharacterIndex`, help string "Last character selected". **No host knob**: the live
    // value is the character screen's own state ([`crate::char_select::Roster::pending_index`]),
    // which this row only mirrors — the `statusBarText` posture, and why the arm in
    // [`apply_to_knobs`] is empty.
    //
    // Registered **"0"**, byte-read rather than chosen: `CVar::Register` at `0x402d93` pushes
    // default string `0x82e570` = "0", category 4, and caches the CVar* at `[0x882674]`. The value
    // is a **0-based** row (the engine's selection cell `[0x83856c]` under `"%d"`), so "0" is the
    // FIRST character and not a "no memory" sentinel — which is exactly why a stock `Config.wtf`
    // has no such line until you have played somebody other than your first character
    // (`SaveConfig 0x63d980` skips values equal to their default; `compose_file` does the same).
    // Multisample antialiasing — 1.12's own `gxMultisample`, registered at `0x63a950` with help
    // "multisample antialiasing" and flags `3` = registered | **latched**. The knob is
    // [`benilla_world::view::MsaaSetting`], read once at the world camera's spawn; its doc carries
    // the full derivation.
    //
    // **Default "1" — off — and BYTE-DERIVED, unusually indirectly.** The reference registers no
    // literal here: the default string is `snprintf("%d")`'d at runtime from field 21 of the
    // `VideoHardware.dbc` row `DetectHardware` (`0x641260`) matched the GPU to. Across the shipped
    // 193-row table that field only ever holds 1 (144 rows) or 2 (49 rows), and the three rows the
    // fallback match can reach all hold 1 — so on any GPU the 2004-era table does not list, which
    // is every machine this client runs on now, the registered string is "1". A 1 is genuinely no
    // multisampling on both of its backends, not a one-sample mode. (wow-re §5 cross-check,
    // 2026-08-26, `system/console/scratch/gxmultisample-default.md`; decision 1629.)
    //
    // Latched means a change is PENDING until the next launch — the reference's own callback
    // echoes "set pending gxRestart" — so this row persists and `GetCVar` answers it, while the
    // camera keeps what it was born with. `$WOW_MSAA` overrides it session-only, below.
    same("gxMultisample", "1"),
    // The multisample triple's other two thirds. The reference's Video dropdown formats all three
    // into one row (`MULTISAMPLING_FORMAT_STRING` = "%d-bit color %d-bit depth %dx multisample")
    // and `GetCurrentMultisampleFormat 0x48c580` looks up all three by name to find which row is
    // selected — so without these registered that lookup can never match and the dropdown would
    // sit on entry 1 forever.
    //
    // **They describe, they do not steer.** benilla does not offer a colour or depth format to
    // choose: every format `benilla_world::view::MsaaFormats` publishes carries the same pair,
    // derived from the swapchain format and `Depth32Float`. `SetMultisampleFormat` writes them
    // from the chosen entry exactly like `0x48c640` does, which is a no-op in value and the right
    // shape to keep. The defaults here are the literals that pair matches on every target we ship;
    // if a target ever disagrees the dropdown's own row wins, because it is written from the live
    // enumeration.
    deviates(
        "gxColorBits",
        "32",
        "16",
        "1643: these describe, they do not steer — the pair is our swapchain's own, and every \
         format `MsaaFormats` publishes carries it",
    ),
    deviates(
        "gxDepthBits",
        "32",
        "16",
        "1643: as `gxColorBits` — the depth half of the same descriptive pair",
    ),
    // **The texture filter policy** — 1.12's own `trilinear` and `anisotropic`, over
    // `benilla_assets::TexFilterSetting`. The defaults are the reference's registered strings, and
    // benilla had neither CVar: it hardcoded trilinear + aniso 8 at every sampler it built, which
    // is mode 5 — the *top* of what these two can ask for — shipped as the thing you get before
    // asking. `tex_filter.rs` carries the derivation and the cost.
    //
    // Latched, exactly like `gxMultisample` above and for a harder reason: a sampler is baked into
    // the `Image` at load and lives in the uploaded texture, so a live change would mean rebuilding
    // every texture in the world. The reference's own UI says "enabled upon restart".
    // `$WOW_TRILINEAR` / `$WOW_ANISO` override session-only, below.
    // **`trilinear` registers "1", not the registrar's "0"** (decision 1645, correcting 1642).
    // The reference's `CVar::Register` string is `"0"`, but `hwDetect` — registered `"1"` — runs
    // `DetectHardware 0x641260` at boot and `CVar::Set`s sixteen video CVars from the matched
    // `VideoHardware.dbc` row before the first frame, then self-clears. Every GPU this client runs
    // on is unlisted in a 2004 table, so the row is the fallback scan's, whose reachable set is
    // exactly rows 168/169/170 — and `trilinear` is 1 on 169 and 170, at both CPU tiers, with no
    // CPU bias term. Measured as well as derived: the reference's own `WoW/Logs/gx.log` reads
    // `VID: 106b` → `DID: 2` → `videoID: 170`.
    //
    // This is the same shape as `gxMultisample` above, which also registers the value the hardware
    // table yields rather than a literal the registrar never emits on a modern machine.
    overridden(
        "trilinear",
        "1",
        "0",
        "1645: `hwDetect` sets it from `VideoHardware.dbc` field 9 before the first frame, and \
         that field is 1 on both fallback rows an unlisted modern GPU can reach — measured on the \
         reference's own `Logs/gx.log` (`videoID: 170`)",
    ),
    // `anisotropic` registers `"1"` — off — and here the registrar's string IS the answer: it is
    // **not** one of `hwDetect`'s sixteen (scan of `[0x639a60, 0x639b80)`: sixteen record-pointer
    // reads, `0xc7f2e4` absent), so nothing overwrites it on any path.
    same("anisotropic", "1"),
    // **Render scale** (decision 1639) — benilla's own CVar, no 1.12 counterpart, in the
    // `boothHalfRate` / `SoundOutputLimiter` mould: the reference has no such dial because it has
    // no second buffer to hang one on. The world renders into the composite lane's off-screen image
    // at `window × this` while the UI stays at native resolution; the knob is
    // [`crate::world_backdrop::RenderScale`], clamped to its `RENDER_SCALE_RANGE`.
    //
    // The era's nearest equivalent is `gxResolution`, which drops the interface along with the
    // world and, in fullscreen, mode-sets the display — the thing 1627 deliberately stopped doing.
    //
    // Default "1" is off, and that is load-bearing rather than cautious: at 1.0 the lane reproduces
    // its pre-1639 numbers bit-for-bit, so no visual golden in the tree moves. `$WOW_RENDER_SCALE`
    // overrides it session-only, below.
    ours(
        "renderScale",
        "1",
        "1639: benilla's own — the reference has no off-screen buffer to hang a resolution dial \
         on; its nearest equivalent, `gxResolution`, drops the interface with the world",
    ),
    same(crate::char_select::CVAR_LAST_CHARACTER, "0"),
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
    /// The engine table has been registered + seeded — **once per VM**, not once per process
    /// (decision 1290). A login builds a fresh VM, so the seed has to happen again: an
    /// unregistered table answers every `GetCVar` with nil, and [`save_config`] composes
    /// `config.toml` out of that same table.
    registered: crate::ui_script::VmMemo<bool>,
    /// A change since the last save; `last_change` drives the one-quiet-second debounce.
    dirty: bool,
    last_change: Option<Instant>,
}

impl CvarPersist {
    /// One CVar as `config.toml` holds it — matched case-insensitively, so a hand-edited
    /// spelling still answers.
    ///
    /// Read from the persist state rather than from the VM's table for the callers that want a
    /// value **before, or outside, a registered table**: the addon load walk runs while the VM's
    /// CVar table does not exist yet (registration is a per-VM `Update` seed, 1291), and the
    /// select screen wants its remembered row the moment a roster lands, from a system that has
    /// no business holding the VM (1622). The 1291 fold keeps this current across VM
    /// replacements, so it is the value the reference's live read would see.
    pub(crate) fn stored(&self, name: &str) -> Option<&str> {
        self.file
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// A persist state that already holds one stored value — a launch whose `config.toml` said
    /// so, without a file. `#[cfg(test)]` and `pub(crate)` because [`Self::file`] is private:
    /// `char_select`'s restore test drives the real [`apply_roster_policy`] over a real remembered
    /// row rather than a copy of its logic (the `Roster::with_pending_pick` posture).
    #[cfg(test)]
    pub(crate) fn with_stored(name: &str, value: &str) -> Self {
        Self {
            file: BTreeMap::from([(name.to_string(), value.to_string())]),
            ..Self::default()
        }
    }

    /// The persisted `checkAddonVersion` (decision 1292) — what the addon load walk gates on.
    /// Absent = the registrar default: check ON.
    pub(crate) fn addon_version_check(&self) -> bool {
        self.stored("checkAddonVersion").is_none_or(|v| v != "0")
    }
}

/// How long a dirty config sits before the save fires — long enough to coalesce a slider drag,
/// short enough that a crash loses one gesture, not a session ("write-on-change, debounced").
const SAVE_QUIET: std::time::Duration = std::time::Duration::from_secs(1);

/// The startup fold of `config.toml` into the knob resources ([`load_config`]).
///
/// A set rather than a bare system because one knob is **read once and never again**: the world
/// camera takes its `Msaa` at spawn (decision 1629, the reference's latched `gxMultisample`), so
/// `setup_player` must not be able to run before the file has been folded in. Every other knob is
/// live-read and does not care.
///
/// This removes a **race, not an observed bug**. Measured: with the constraint deleted, a
/// `gxMultisample = "4"` in `config.toml` still reached the camera — and it did so despite
/// `PlayerPlugin` being added *before* `CvarPlugin` (`lib.rs`), i.e. the order that happened to
/// hold was the executor's choice out of an unconstrained graph, not insertion order and not
/// anything we could point at. The failure it prevents is silent (the player's setting is simply a
/// launch late) and would surface as a bug report nobody could reproduce.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CvarLoad;

pub(crate) struct CvarPlugin;

impl Plugin for CvarPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CvarPersist>()
            .add_systems(
                Startup,
                (load_config, publish_filter_policy)
                    .chain()
                    .in_set(CvarLoad),
            )
            .add_systems(Update, sync_cvars);
        // **The flush is on the exit edge, not beside its feed** (decision 1528). It used to be
        // `(sync_cvars, save_config).chain()` in `Update`, which made the "or the app exiting"
        // half of its own gate dead on the exit a player actually causes: the close button's
        // `AppExit` is not written until `PostUpdate`, so the last second of slider drags went
        // with the process. `Last` still runs after `sync_cvars` — schedule order does what the
        // `.chain()` did — and now also after every announcement.
        crate::shutdown::on_app_exit(app, save_config.into_configs());
    }
}

/// The knob resources as a **SystemParam** — the one census, fetched once, shared by all three
/// entry points ([`load_config`], [`sync_cvars`], [`fold_dying_vm_cvars`]).
///
/// It exists because the census had grown past Bevy's **16-param ceiling**: with fifteen knobs,
/// `sync_cvars` (script + persist + knobs) and the fold's `SystemState` both stopped compiling the
/// moment the plate toggles landed. Re-typing the list at every call site was already the shape
/// that made a new knob a four-place edit; bundling it makes a new knob one field here, one field
/// on [`Knobs`], and one arm in [`apply_to_knobs`], and the ceiling stops being reachable.
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct KnobParams<'w> {
    sound: ResMut<'w, SoundConfig>,
    scale: ResMut<'w, UiScaleCvar>,
    view: ResMut<'w, ViewDistance>,
    msaa: ResMut<'w, MsaaSetting>,
    msaa_formats: Res<'w, benilla_world::view::MsaaFormats>,
    look: ResMut<'w, LookConfig>,
    click: ResMut<'w, ClickConfig>,
    loot: ResMut<'w, LootConfig>,
    names: ResMut<'w, NameConfig>,
    plates: ResMut<'w, VPlateMode>,
    clutter: ResMut<'w, ClutterConfig>,
    minimap: ResMut<'w, MinimapZoom>,
    bubbles: ResMut<'w, BubbleConfig>,
    zoom: ResMut<'w, ZoomLimit>,
    follow: ResMut<'w, FollowConfig>,
    video: ResMut<'w, VideoConfig>,
    render_scale: ResMut<'w, RenderScale>,
    tex_filter: ResMut<'w, benilla_assets::TexFilterSetting>,
    pane_rate: ResMut<'w, PaneRate>,
    guild_notify: ResMut<'w, crate::ui_guild::GuildMemberNotify>,
    block_trades: ResMut<'w, crate::ui_trade::BlockTrades>,
    auto_self_cast: ResMut<'w, crate::ui_action::AutoSelfCast>,
    realmlist: ResMut<'w, crate::realmlist::Realmlist>,
}

impl KnobParams<'_> {
    /// Borrow the set as [`Knobs`] for a write.
    ///
    /// **Deref-muts every resource, so call it only when a change is actually being applied**
    /// (0992's change-detection trap: the clutter re-scatter watches `is_changed::<ClutterConfig>`,
    /// and a set built on every frame — or before the change queue is known to be non-empty —
    /// re-scattered the world on every MasterVolume drag tick). Reading a field off `self`
    /// directly, as the session seed does, goes through `Deref` and flags nothing.
    fn knobs(&mut self) -> Knobs<'_> {
        Knobs {
            sound: &mut self.sound,
            scale: &mut self.scale,
            view: &mut self.view,
            msaa: &mut self.msaa,
            msaa_formats: &self.msaa_formats,
            look: &mut self.look,
            click: &mut self.click,
            loot: &mut self.loot,
            names: &mut self.names,
            plates: &mut self.plates,
            clutter: &mut self.clutter,
            minimap: &mut self.minimap,
            bubbles: &mut self.bubbles,
            zoom: &mut self.zoom,
            follow: &mut self.follow,
            video: &mut self.video,
            render_scale: &mut self.render_scale,
            tex_filter: &mut self.tex_filter,
            pane_rate: &mut self.pane_rate,
            guild_notify: &mut self.guild_notify,
            block_trades: &mut self.block_trades,
            auto_self_cast: &mut self.auto_self_cast,
            realmlist: &mut self.realmlist,
        }
    }
}

/// The knob resources one CVar write can land on, bundled so [`apply_to_knobs`] and its two
/// callers grow together (a new knob is one field + one arm).
struct Knobs<'a> {
    sound: &'a mut SoundConfig,
    scale: &'a mut UiScaleCvar,
    view: &'a mut ViewDistance,
    msaa: &'a mut MsaaSetting,
    /// What the device actually offers — the ceiling `gxMultisample` is clamped to (1643).
    msaa_formats: &'a benilla_world::view::MsaaFormats,
    look: &'a mut LookConfig,
    click: &'a mut ClickConfig,
    loot: &'a mut LootConfig,
    names: &'a mut NameConfig,
    plates: &'a mut VPlateMode,
    clutter: &'a mut ClutterConfig,
    minimap: &'a mut MinimapZoom,
    bubbles: &'a mut BubbleConfig,
    zoom: &'a mut ZoomLimit,
    follow: &'a mut FollowConfig,
    video: &'a mut VideoConfig,
    render_scale: &'a mut RenderScale,
    tex_filter: &'a mut benilla_assets::TexFilterSetting,
    pane_rate: &'a mut PaneRate,
    guild_notify: &'a mut crate::ui_guild::GuildMemberNotify,
    block_trades: &'a mut crate::ui_trade::BlockTrades,
    auto_self_cast: &'a mut crate::ui_action::AutoSelfCast,
    realmlist: &'a mut crate::realmlist::Realmlist,
}

/// Apply one CVar to its knob resource (parse + the knob's own clamp). `false` = not a knob this
/// build knows (the caller decides whether that warns or rides through).
fn apply_to_knobs(name: &str, value: &str, knobs: &mut Knobs) -> bool {
    let key = name.to_ascii_lowercase();
    // **The string-valued rows**, matched ahead of the numeric parse every other row goes through
    // — which would reject them as bad values. `gxResolution` was the first (decision 1627) and
    // its comment named this as the shape a second one would join rather than a second special
    // case somewhere else; `realmList` (1667) is that second one, so this is now that shape.
    // Every arm shares the numeric miss's posture below: known key, bad value — consumed, with a
    // warn, and the resource keeps its truth.
    match key.as_str() {
        "gxresolution" => {
            match crate::video::parse_resolution(value) {
                Some(size) => knobs.video.windowed = size,
                None => warn!("cvar {name}: unparseable value '{value}' ignored"),
            }
            return true;
        }
        "realmlist" => {
            match crate::realmlist::normalize(value) {
                Some(address) => knobs.realmlist.set(&address),
                None => warn!("cvar {name}: unusable realmlist '{value}' ignored"),
            }
            return true;
        }
        _ => {}
    }
    let Ok(v) = value.parse::<f32>() else {
        warn!("cvar {name}: unparseable value '{value}' ignored");
        return true; // known key, bad value — consumed, resource keeps its truth
    };
    match key.as_str() {
        "mastervolume" => knobs.sound.master = v.clamp(0.0, 1.0),
        "soundvolume" => knobs.sound.sfx = v.clamp(0.0, 1.0),
        "musicvolume" => knobs.sound.music = v.clamp(0.0, 1.0),
        "ambiencevolume" => knobs.sound.ambience = v.clamp(0.0, 1.0),
        // The enables are 0/1 flags; the client's own parse is int + `!= 0`.
        "mastersoundeffects" => knobs.sound.enabled = v != 0.0,
        "enablemusic" => knobs.sound.music_enabled = v != 0.0,
        "enableambience" => knobs.sound.ambience_enabled = v != 0.0,
        "enableerrorspeech" => knobs.sound.error_speech = v != 0.0,
        // The client's own parse for this one is literally `!= 0` too (`0x4574d0`: `setne al`).
        "soundreverb" => knobs.sound.reverb = v != 0.0,
        "soundoutputlimiter" => knobs.sound.limiter = v != 0.0,
        "uiscale" => knobs.scale.0 = v.clamp(0.5, 1.5),
        "farclip" => knobs.view.farclip = v.clamp(*FARCLIP_RANGE.start(), *FARCLIP_RANGE.end()),
        "deselectonclick" => knobs.click.deselect_on_click = v != 0.0,
        "autoselfcast" => knobs.auto_self_cast.0 = v != 0.0,
        // The sixteen camera-view CVars have no knob to apply to: `CameraViews` is their writer,
        // not their reader (it seeds itself from the persisted file at startup, and `SaveView`
        // writes back). They are claimed here so the table's own "not a knob this build knows"
        // warning stays meaningful — an unclaimed name would look like a typo every session.
        _ if crate::player::camera_view::is_view_cvar(name) => {}
        "mouseinvertpitch" => knobs.look.invert_pitch = v != 0.0,
        "cameradistancemaxfactor" => knobs.zoom.set_factor(v),
        // The three stops are 1 Smart / 2 Always / 3 Never; anything else reads as the registrar
        // default rather than as a dead camera (`FollowStyle::from_cvar`).
        "camerasmoothstyle" => knobs.follow.style = FollowStyle::from_cvar(v),
        // Its sibling selector — the one the reference swaps in for the externally-driven states.
        "camerasmoothtrackingstyle" => knobs.follow.tracking_style = FollowStyle::from_cvar(v),
        // The auto-follow rate, clamped to 1.12's own AUTO_FOLLOW_SPEED slider range.
        "camerayawsmoothspeed" => {
            knobs.follow.yaw_speed =
                v.clamp(*FOLLOW_SPEED_RANGE.start(), *FOLLOW_SPEED_RANGE.end());
        }
        // The 1.12 slider's own range; an off-grid hand-edit rides between stops, like the others.
        "mousespeed" => {
            knobs.look.sensitivity = v.clamp(*MOUSE_SPEED_RANGE.start(), *MOUSE_SPEED_RANGE.end());
        }
        "autolootdefault" => knobs.loot.auto_loot = v != 0.0,
        "unitnameplayer" => knobs.names.player = v != 0.0,
        "unitnamenpc" => knobs.names.npc = v != 0.0,
        "unitnameown" => knobs.names.own = v != 0.0,
        // The two V-plate toggles — the bitmask's two bits, flags like every other checkbox.
        // Lowercased here like every arm; `VPlateMode`'s consts carry the registered spelling.
        "nameplateshowenemies" => knobs.plates.enemies = v != 0.0,
        "nameplateshowfriends" => knobs.plates.friends = v != 0.0,
        // Two CVars with no HOST knob, because their consumers are Lua (1140, B230). Known — so
        // the caller dirties the config and the value persists — with nothing to apply this side.
        "statusbartext" | "ubertooltips" => {}
        // The two bubble switches (1139) — flags, like every other pair here.
        "chatbubbles" => knobs.bubbles.all = v != 0.0,
        "chatbubblesparty" => knobs.bubbles.party = v != 0.0,
        // The loot-roll detail switch (1589) — a flag over the roll-line composer's two shapes.
        "showlootspam" => knobs.loot.show_loot_spam = v != 0.0,
        // Guild Member Alert (1589) — conjunct 2 of the sign-on/sign-off line's condition.
        "guildmembernotify" => knobs.guild_notify.0 = v != 0.0,
        "blocktrades" => knobs.block_trades.0 = v != 0.0,
        // The panel's 0/1/2 lands as the density multiplier ×1/×2/×3; the clamp is the 1.12
        // slider's own range (an off-grid hand-edit rides between stops, like every slider).
        "worlddetail" => knobs.clutter.density = v.clamp(0.0, 2.0) + 1.0,
        // The two zoom indices (1131) clamp exactly like the client's `set_zoom` (`0x6daa10`:
        // clamp at 5) — the widget clamps again on the way in, so a hand-edited level lands
        // in range whichever path it takes.
        "minimapzoom" => knobs.minimap.outdoor = zoom_index(v),
        "minimapinsidezoom" => knobs.minimap.inside = zoom_index(v),
        // The addon version gate (1292): no host knob — the load walk reads the persisted value
        // and the gate reads the live table — but a KNOWN key, so a toggle dirties the config
        // and persists (the statusBarText posture).
        "checkaddonversion" => {}
        // The remembered character row (1622) — same posture again: the live value is the select
        // screen's own, which writes this key rather than reading it back. Known, so entering the
        // world dirties the config and the memory survives to the next launch.
        "lastcharacterindex" => {}
        // Vertical Sync — a flag like every other checkbox here. `video::apply_present_mode`
        // watches the value and pushes it to the window; nothing else reads it.
        "gxvsync" => knobs.video.vsync = v != 0.0,
        // Display mode (1627) — a flag like every other checkbox here, and the reference's own
        // polarity: `1` is WINDOWED (the row is "Windowed Mode"). `video::apply_window_mode`
        // watches the value and pushes it to the window; nothing else reads it.
        "gxwindow" => knobs.video.display = crate::video::display_from_flag(v),
        // The body panes' half-rate render (1444) — a flag like every other checkbox here.
        "boothhalfrate" => knobs.pane_rate.half = v != 0.0,
        // Render scale (1639). Clamped at the knob's edge like every other numeric row; the
        // backdrop re-sizes on the next frame and the world camera's target factor follows it
        // in the same pass, which is what keeps the pick rays where they were.
        "renderscale" => {
            knobs.render_scale.0 = v.clamp(*RENDER_SCALE_RANGE.start(), *RENDER_SCALE_RANGE.end());
        }
        // Multisampling (1629) — the reference's own `atoi`-then-clamp `[1, 16]` at `0x63b250`.
        // Writing the knob live is faithful, not a bug: the CVar holds the PENDING value (latched),
        // and nothing reads this resource after the world camera's spawn.
        "gxmultisample" => {
            // TWO ceilings, and the second one was missing until 1643. The reference's own
            // `atoi`-then-clamp `[1, 16]` comes first; then the DEVICE's, because a count this
            // GPU does not offer is not a setting that degrades — it is a wgpu validation error
            // that kills the render thread on frame one ("Sample count 8 is not supported by
            // format Rgba16Float on this device", 2026-08-26).
            //
            // `MsaaSupportPlugin::finish` already clamped, but it runs once, before the first
            // update — so it saw `MsaaSetting::default()` and never the value `load_config` was
            // about to fold in from `config.toml`. A config written on a machine that offers 8x
            // and opened on one that stops at 4 therefore reached the camera untouched. Clamping
            // at the WRITE covers every writer there is: the file, a Lua `SetCVar`, the dropdown,
            // and the Defaults button.
            let asked = (v as u32).clamp(*MSAA_RANGE.start(), *MSAA_RANGE.end());
            let granted = knobs.msaa_formats.clamp(asked);
            if granted != asked {
                // At `warn`, the same posture as the seed clamp: the player asked for something
                // and did not get it, and this is the only place that fact exists.
                warn!(
                    "cvar {name}: this GPU does not offer {asked}x multisampling — using {granted}x"
                );
            }
            knobs.msaa.samples = granted;
        }
        // The filter policy's two halves. Both write the PENDING value — latched, like
        // `gxMultisample`: the process policy is published once at the end of `load_config` and
        // nothing reads this resource afterwards. `anisotropic` takes the reference's own
        // parse-then-clamp `[1, 16]` (`0x689110`); `trilinear` is a flag like every other.
        "trilinear" => knobs.tex_filter.trilinear = v != 0.0,
        "anisotropic" => {
            knobs.tex_filter.aniso = (v as u32).clamp(
                *benilla_assets::ANISO_RANGE.start(),
                *benilla_assets::ANISO_RANGE.end(),
            )
        }
        _ => return false,
    }
    true
}

/// Freeze the texture filter policy for the process, and say what it resolved to.
///
/// **A separate system, chained after [`load_config`], deliberately.** `load_config` returns early
/// on an absent or malformed file, and the policy has to be published on every one of those paths:
/// the sampler lanes are an async `AssetLoader` and a set of ordinary systems, none of which can
/// read a resource the others own, so a run that never published would be reading
/// [`benilla_assets::tex_filter`]'s fallback while a player's `config.toml` said otherwise.
///
/// The log line is not decoration — it is the same reasoning as `video::log_display_session`
/// (1627). Every filtering report this client will get comes from a machine nobody here can run,
/// and "which mode was that run actually in" must be readable off the log a player pastes rather
/// than reasoned about.
fn publish_filter_policy(filter: Res<benilla_assets::TexFilterSetting>) {
    benilla_assets::publish_tex_filter(*filter);
    let mode = filter.mode();
    let name = match mode {
        3 => "bilinear + nearest-mip select, aniso off",
        4 => "trilinear, aniso off",
        _ => "trilinear + aniso",
    };
    info!(
        "texture filter: mode {mode} ({name}) — trilinear={} anisotropic={}",
        u8::from(filter.trilinear),
        filter.aniso
    );
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
fn load_config(mut persist: ResMut<CvarPersist>, mut params: KnobParams) {
    let mut knobs = params.knobs();
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
    // `$WOW_NOVSYNC=1` is the measurement uncap: session-only, exactly like the taste-iteration
    // overrides above. Pinning it into the config would make an instrument run sticky.
    if crate::video::novsync_env() {
        persist.env_overridden.insert("gxvsync".into());
    }
    // The filter policy's A/B levers, under the same law: pricing mode 3 against mode 5 on one
    // machine in one session is exactly what these are for, and a value that stuck in
    // `config.toml` would silently denominate every later reading.
    if std::env::var_os("WOW_TRILINEAR").is_some() {
        persist.env_overridden.insert("trilinear".into());
    }
    if std::env::var_os("WOW_ANISO").is_some() {
        persist.env_overridden.insert("anisotropic".into());
    }
    // `$WOW_WIN`, a capture scenario, or any instrumented run owns the window's geometry for the
    // session (decision 1627), so the two CVars that would otherwise move it mid-run are
    // session-only under exactly the same law as the four above.
    if crate::video::windowed_env() {
        persist.env_overridden.insert("gxwindow".into());
        persist.env_overridden.insert("gxresolution".into());
    }
    // `$WOW_MSAA` is the multisampling A/B lever (1629), session-only under the same law as every
    // override above: a value pinned into the file would make a measurement sticky across
    // relaunches.
    if std::env::var_os("WOW_MSAA").is_some() {
        persist.env_overridden.insert("gxmultisample".into());
    }
    // `$WOW_RENDER_SCALE` is the render-scale A/B lever (1639), and doubly session-only: it is
    // also the supersampling instrument this machine prices pixels with, and an instrument run
    // that pinned 4× into the file would come back at 4× the next time the client opened.
    if std::env::var_os("WOW_RENDER_SCALE").is_some() {
        persist.env_overridden.insert("renderscale".into());
    }
    // `$WOW_HOST` is the realmlist for the session (1667) — every probe, smoke run and harness leg
    // sets it, and a value pinned into the file would silently repoint the player's client at
    // whatever a test dialed. `Realmlist::default()` has already taken it; this keeps it off disk.
    if std::env::var_os("WOW_HOST").is_some() {
        persist.env_overridden.insert("realmlist".into());
    }
    let cvars = match stored_config() {
        StoredConfig::Absent => return, // no file, hermetic capture, or no install
        StoredConfig::Bad(msg) => {
            // A malformed file is preserved, not clobbered: nothing loads, but nothing saves
            // over it either until a change actually happens — and the warn names the file.
            warn!("{msg}");
            return;
        }
        StoredConfig::Table(t) => t,
    };
    let known: HashSet<String> = REGISTERED
        .iter()
        .map(|r| r.name.to_ascii_lowercase())
        .collect();
    for (name, value) in &cvars {
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
    persist.file = cvars;
}

/// What the one read of `config.toml` found.
enum StoredConfig {
    /// No file, no install, or a hermetic capture — every value is its registered default.
    Absent,
    /// The file's `[cvars]` table.
    Table(BTreeMap<String, String>),
    /// The file is there but unreadable or malformed. The string is what [`load_config`] warns
    /// with — carried rather than logged, because this read happens before the `App` (and so
    /// before `LogPlugin`) exists.
    Bad(String),
}

/// Read `config.toml`.
///
/// **One parser, two callers at two different times** — [`load_config`] at `Startup`, and the
/// primary window literal in [`crate::run`], which has to know `gxWindow`/`gxResolution` *before*
/// the window exists ([`crate::video::boot_window_mode`] carries why booting windowed and flipping
/// a frame later is not good enough).
///
/// Deliberately **not** cached in a `OnceLock`, though it was written that way first. Three reads
/// of a sub-kilobyte file at process start is not a cost worth a global, and a process-wide cache
/// is actively wrong: every test that lays a config down and then runs `load_config` would be
/// answered from whatever the *first* test in the binary happened to see, and `local_state`'s home
/// law can legitimately move under a run. The thing worth having exactly one of is this function,
/// not its result.
fn stored_config() -> StoredConfig {
    let Some(path) = crate::local_state::config_path() else {
        return StoredConfig::Absent; // hermetic capture, or no install — session-only state
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return StoredConfig::Absent,
        Err(e) => return StoredConfig::Bad(format!("config: cannot read {}: {e}", path.display())),
    };
    match toml::from_str::<LocalConfig>(&text) {
        Ok(cfg) => StoredConfig::Table(cfg.cvars),
        Err(e) => StoredConfig::Bad(format!(
            "config: {} is malformed ({e}) — running on defaults",
            path.display()
        )),
    }
}

/// One CVar as `config.toml` holds it, matched case-insensitively — **before the `App` exists**
/// (decision 1627).
///
/// Every other consumer wants [`CvarPersist::stored`], which answers from the same values once
/// they are a resource and stays current across a VM replacement (1291). This one exists for the
/// single caller that cannot wait for a resource: the primary window has to be *built* with its
/// display mode already resolved.
pub(crate) fn boot_cvar(name: &str) -> Option<String> {
    match stored_config() {
        StoredConfig::Table(t) => t
            .into_iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v),
        StoredConfig::Absent | StoredConfig::Bad(_) => None,
    }
}

/// Per frame: seed the VM's table once it exists (registered set + the RESOLVED session values,
/// so `GetCVar` reflects env overrides and the loaded config alike), then drain Lua `SetCVar`
/// changes into the knob resources and mark the config dirty.
fn sync_cvars(
    script: Option<NonSendMut<UiScript>>,
    mut persist: ResMut<CvarPersist>,
    mut params: KnobParams,
) {
    let Some(mut script) = script else {
        return;
    };
    if persist.registered.claim(&script) {
        // Read-only borrows for the seed: field access through `ResMut`'s `Deref` flags nothing,
        // which is the half of 0992's change-detection trap this system has to keep.
        let KnobParams {
            sound,
            scale,
            view,
            look,
            click,
            loot,
            names,
            plates,
            clutter,
            minimap,
            bubbles,
            zoom,
            follow,
            video,
            render_scale,
            pane_rate,
            guild_notify,
            block_trades,
            auto_self_cast,
            msaa,
            msaa_formats,
            tex_filter,
            realmlist,
        } = &params;
        // The config file's values go in FIRST (decision 1291): registration — ours below, or an
        // addon's `RegisterCVar` later — starts a key at its saved value. This is what carries a
        // knobless CVar (`statusBarText`) and an addon-declared one across a VM replacement; the
        // knob-derived session rows below still win for every key a host knob backs, and an
        // env-overridden key keeps its env value the same way (its knob carries it).
        script.set_cvar_saved_base(
            persist
                .file
                .iter()
                .filter(|(k, _)| !persist.env_overridden.contains(&k.to_ascii_lowercase()))
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        script.register_cvars(registered_pairs());
        // The Video dropdown's menu — what this device actually accepts, enumerated once at
        // `finish()` by `view::MsaaSupportPlugin` (decision 1631) and handed over whole. Pushed
        // here rather than owned by the VM because the list is a fact about the render adapter,
        // which `benilla-ui` has no way to ask and should not grow one.
        script.set_multisample_formats(
            msaa_formats
                .formats
                .iter()
                .map(
                    |&(color_bits, depth_bits, samples)| benilla_ui::script::MultisampleFormat {
                        color_bits,
                        depth_bits,
                        samples,
                    },
                )
                .collect(),
        );
        let flag = |b: bool| if b { "1" } else { "0" }.to_string();
        let session: [(&str, String); 43] = [
            ("MasterVolume", sound.master.to_string()),
            ("SoundVolume", sound.sfx.to_string()),
            ("MusicVolume", sound.music.to_string()),
            ("AmbienceVolume", sound.ambience.to_string()),
            ("MasterSoundEffects", flag(sound.enabled)),
            ("EnableMusic", flag(sound.music_enabled)),
            ("EnableAmbience", flag(sound.ambience_enabled)),
            ("EnableErrorSpeech", flag(sound.error_speech)),
            ("SoundReverb", flag(sound.reverb)),
            ("SoundOutputLimiter", flag(sound.limiter)),
            ("uiScale", scale.0.to_string()),
            ("farclip", view.farclip.to_string()),
            ("deselectOnClick", flag(click.deselect_on_click)),
            ("autoSelfCast", flag(auto_self_cast.0)),
            ("mouseInvertPitch", flag(look.invert_pitch)),
            ("mousespeed", look.sensitivity.to_string()),
            ("cameraDistanceMaxFactor", zoom.factor().to_string()),
            ("cameraSmoothStyle", follow.style.cvar().to_string()),
            (
                "cameraSmoothTrackingStyle",
                follow.tracking_style.cvar().to_string(),
            ),
            ("cameraYawSmoothSpeed", follow.yaw_speed.to_string()),
            ("autoLootDefault", flag(loot.auto_loot)),
            ("showLootSpam", flag(loot.show_loot_spam)),
            ("guildMemberNotify", flag(guild_notify.0)),
            ("BlockTrades", flag(block_trades.0)),
            ("UnitNamePlayer", flag(names.player)),
            ("UnitNameNPC", flag(names.npc)),
            ("UnitNameOwn", flag(names.own)),
            (crate::vplates::CVAR_ENEMIES, flag(plates.enemies)),
            (crate::vplates::CVAR_FRIENDS, flag(plates.friends)),
            // The session density on the panel scale (×1..×3 → 0..2). An env-driven off-grid
            // multiplier seeds off-grid honestly — the dropdown shows the raw number, checks
            // nothing (the 0959 out-of-range posture, dropdown-flavored).
            ("WorldDetail", (clutter.density - 1.0).to_string()),
            ("ChatBubbles", flag(bubbles.all)),
            ("ChatBubblesParty", flag(bubbles.party)),
            ("minimapZoom", minimap.outdoor.to_string()),
            ("minimapInsideZoom", minimap.inside.to_string()),
            ("gxVSync", flag(video.vsync)),
            // The reference's polarity: the CVar is `gxWindow`, so `1` is the WINDOWED state.
            (
                "gxWindow",
                flag(video.display == crate::video::DisplayMode::Windowed),
            ),
            // The one string-valued row, composed in the reference's spelling.
            (
                "gxResolution",
                format!("{}x{}", video.windowed.x, video.windowed.y),
            ),
            ("boothHalfRate", flag(pane_rate.half)),
            ("renderScale", render_scale.0.to_string()),
            ("gxMultisample", msaa.samples.to_string()),
            ("trilinear", flag(tex_filter.trilinear)),
            ("anisotropic", tex_filter.aniso.to_string()),
            // The other string-valued row (1667): what the next logon attempt will actually dial,
            // including a `$WOW_HOST` the player never typed.
            (
                crate::realmlist::CVAR_REALMLIST,
                realmlist.address().to_string(),
            ),
        ];
        for (name, value) in session {
            script.set_cvar_host(name, &value);
        }
    }
    // Take the changes BEFORE touching the knobs: constructing `Knobs` deref-muts every knob
    // resource, which trips Bevy change detection even when nothing is written — and the
    // clutter re-scatter is downstream of exactly that signal staying honest (0992).
    let changes = script.take_cvar_changes();
    if changes.is_empty() {
        return;
    }
    let mut knobs = params.knobs();
    for (name, value) in changes {
        if apply_to_knobs(&name, &value, &mut knobs) {
            persist.dirty = true;
            persist.last_change = Some(Instant::now());
        }
    }
}

/// Fold the dying VM's CVar table into the persist state — the session edge's half of decision
/// 1291's bridge (the seed in [`sync_cvars`] is the other). Called from
/// [`crate::ui_script::end_ui_session`] **after** the shutdown events (an addon's
/// `PLAYER_LOGOUT` handler may `SetCVar`, and in the reference that write lands in an
/// engine-side store that survives) and **before** the VM is replaced.
///
/// Two steps, both about the writes the per-frame sync never got to see:
/// 1. drain the dying VM's change queue into the host knobs — a `SetCVar` in the final frame
///    would otherwise be overwritten by the stale knob when the next VM's seed runs;
/// 2. fold the table into `persist.file` with the same compose the saver uses, so the next VM's
///    saved base — and the next save — both start from what the player actually set.
///
/// `dirty` is left alone: if nothing changed, the fold is an identity; if something did, the
/// change that did it already marked the config dirty.
pub(crate) fn fold_dying_vm_cvars(world: &mut World) {
    // A world with no persist state has no file to bridge — a test world or a stripped scenario
    // that never added the plugin. It is checked up front because the knob set below is fetched
    // NON-optionally, and the two facts are one: any world carrying `CvarPersist` carries every
    // knob too (the plugin's own `load_config`/`sync_cvars` take them the same way, and would
    // have panicked at startup otherwise).
    if !world.contains_resource::<CvarPersist>() {
        return;
    }
    let mut state: bevy::ecs::system::SystemState<(
        Option<NonSendMut<UiScript>>,
        ResMut<CvarPersist>,
        KnobParams,
    )> = bevy::ecs::system::SystemState::new(world);
    let (script, mut persist, mut params) = state.get_mut(world);
    let Some(mut script) = script else {
        return;
    };
    let changes = script.take_cvar_changes();
    if !changes.is_empty() {
        let mut knobs = params.knobs();
        for (name, value) in changes {
            if apply_to_knobs(&name, &value, &mut knobs) {
                persist.dirty = true;
                persist.last_change = Some(Instant::now());
            }
        }
    }
    let snapshot = script.cvars_snapshot();
    if snapshot.is_empty() {
        return; // a VM that never registered (a capture) has nothing to say about the file
    }
    persist.file = compose_file(&persist.file, &persist.env_overridden, &snapshot);
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
    // **An empty table is not a player who cleared their settings.** The file is composed from the
    // VM's live table, so a VM whose table was never registered would compose the player's
    // `config.toml` back out *stripped* — a silent, irreversible loss of everything they had set.
    // The seed above is what keeps that from happening; this is the floor under it, because the
    // failure is one-way and the next regression in that seed must not be able to reach the disk.
    if snapshot.is_empty() {
        warn!("config: the VM has no registered cvars — refusing to compose the file from nothing");
        persist.dirty = false; // nothing to save, and retrying every frame changes nothing
        return;
    }
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

    /// **The standard, enforced: a benilla option's default IS the reference's** (decision 1804)
    /// — every row's [`Reference`] column stands up.
    ///
    /// This is the half that could not be a convention. Before it, the reference's value for a row
    /// lived only in the prose above that row, which meant a default could be *chosen* without
    /// anyone establishing what the client it imitates does — and five of them were: NPC and own
    /// overhead names, enemy V-plates, party chat bubbles and the camera's max-distance factor all
    /// shipped ON or raised while a stock 1.12 client ships them off or low, each one a reasonable
    /// call on its own day and none of them visible as a *set* until somebody went looking. A
    /// third column, mandatory and typed, is what makes the question unskippable; this test is
    /// what makes the answer stay true.
    ///
    /// It checks **both directions**, which is the part that matters over years:
    /// - a [`Reference::Same`] row whose default has drifted off the reference's fails — you
    ///   cannot edit a default and leave the claim behind;
    /// - a [`Reference::Deviates`] or [`Reference::Overridden`] row that has quietly come back
    ///   into agreement *also* fails, because a stale deviation note is worse than none: it hides
    ///   that we are already faithful and invites the next reader to "restore" a divergence.
    ///
    /// Values are parse-compared where both sides are numeric, so `"1"` and `"1.0"` are one claim.
    #[test]
    fn defaults_stand_where_the_reference_column_says() {
        /// One value against another — numeric when both parse, textual otherwise (`gxResolution`,
        /// `realmList`).
        fn agrees(ours: &str, theirs: &str) -> bool {
            match (ours.parse::<f32>(), theirs.parse::<f32>()) {
                (Ok(a), Ok(b)) => a == b,
                _ => ours == theirs,
            }
        }
        for row in REGISTERED {
            let name = row.name;
            match &row.reference {
                Reference::Same(value) => assert!(
                    agrees(row.default, value),
                    "{name}: the row claims the reference registers {value:?} and we ship the \
                     same, but our default is {:?}. If the reference really does differ, this is \
                     a `deviates` row and owes a reason.",
                    row.default,
                ),
                Reference::Deviates { value, why } => {
                    assert!(
                        !agrees(row.default, value),
                        "{name}: a `deviates` row that no longer deviates — our {:?} IS the \
                         reference's. Demote it to `same`; a stale deviation hides that we are \
                         faithful again.",
                        row.default,
                    );
                    assert!(!why.trim().is_empty(), "{name}: a deviation owes a reason");
                }
                Reference::Overridden { registered, why } => {
                    assert!(
                        !agrees(row.default, registered),
                        "{name}: an `overridden` row whose default is just the registered string \
                         {registered:?} — that is `same`, and saying otherwise buries a real \
                         override behind a false one.",
                    );
                    assert!(
                        !why.trim().is_empty(),
                        "{name}: an override owes its mechanism"
                    );
                }
                Reference::Ours(why) => assert!(
                    !why.trim().is_empty(),
                    "{name}: a CVar the reference does not have owes the reason it exists",
                ),
            }
        }
    }

    /// **The inventory** — the exact set of options benilla ships at something other than what a
    /// stock 1.12 client ships, as one readable list.
    ///
    /// The list is the deliverable, not the assertion: a reviewer (or the director) reads *this*
    /// to answer "where do we differ, and is each one still worth it?", and growing it is a
    /// deliberate edit rather than a side effect of adding a row. [`Reference::Overridden`] and
    /// [`Reference::Ours`] are deliberately **not** here — those rows follow the reference's
    /// behaviour, or have no reference behaviour to follow.
    ///
    /// Every name below is argued at its own row; this is the index, and the reason it is sorted
    /// is that the table's order is a load order, not a ranking.
    #[test]
    fn the_options_that_leave_the_reference_are_this_list_and_no_other() {
        let mut names: Vec<&str> = REGISTERED
            .iter()
            .filter(|r| matches!(r.reference, Reference::Deviates { .. }))
            .map(|r| r.name)
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec![
                "SoundReverb",
                "autoSelfCast",
                "gxColorBits",
                "gxDepthBits",
                "gxResolution",
                "realmList",
            ],
        );
    }

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
            .filter_map(|r| r.default.parse::<f32>().ok().map(|f| (r.name, f)))
            .collect();
        let sound = SoundConfig::default();
        assert_eq!(d["MasterVolume"], sound.master);
        assert_eq!(d["SoundVolume"], sound.sfx);
        assert_eq!(d["MusicVolume"], sound.music);
        assert_eq!(d["AmbienceVolume"], sound.ambience);
        assert_eq!(d["MasterSoundEffects"] != 0.0, sound.enabled);
        assert_eq!(d["EnableMusic"] != 0.0, sound.music_enabled);
        assert_eq!(d["EnableAmbience"] != 0.0, sound.ambience_enabled);
        assert_eq!(d["EnableErrorSpeech"] != 0.0, sound.error_speech);
        // Welded like the rest — and deliberately NOT the binary's registrar "1" (1153).
        assert_eq!(d["SoundReverb"] != 0.0, sound.reverb);
        assert_eq!(d["SoundOutputLimiter"] != 0.0, sound.limiter);
        assert!(sound.limiter, "the output limiter ships on (decision 1551)");
        assert!(!sound.reverb, "zone reverb ships off (decision 1153)");
        assert_eq!(d["uiScale"], DEFAULT_UI_SCALE);
        // ViewDistance::default() reads $WOW_FARCLIP; the registered default mirrors the
        // env-less 350 literal (view.rs doc: "Default 350" — the reference's own, 1624).
        assert_eq!(d["farclip"], 350.0);
        // Same shape as farclip: `MsaaSetting::default()` reads $WOW_MSAA, so the registered
        // default mirrors the env-less literal — 1, the reference's own (1629).
        assert_eq!(d["gxMultisample"], 1.0);
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
        // The follow trio (1493/1502) welds to FollowConfig's own defaults — and all three ARE
        // the binary's registrar values, byte-verified: "1"/"1"/"180.0". The one corner of this
        // arc that agrees with the reference outright.
        let follow = FollowConfig::default();
        assert_eq!(
            d["cameraSmoothStyle"],
            follow.style.cvar().parse::<f32>().unwrap()
        );
        assert_eq!(
            d["cameraSmoothTrackingStyle"],
            follow.tracking_style.cvar().parse::<f32>().unwrap()
        );
        assert_eq!(d["cameraYawSmoothSpeed"], follow.yaw_speed);
        assert_eq!(FollowStyle::default(), FollowStyle::Smart);
        assert_eq!(d["autoLootDefault"] != 0.0, LootConfig::default().auto_loot);
        // The roll-detail switch (1589) welds to the same knob's default — and that default IS
        // the binary's registered "1", so this row agrees with the reference on both sides.
        assert_eq!(
            d["showLootSpam"] != 0.0,
            LootConfig::default().show_loot_spam
        );
        // …and the one row that ships a feature OFF, byte-read at `0x5e24c7` (§5).
        assert_eq!(
            d["guildMemberNotify"] != 0.0,
            crate::ui_guild::GuildMemberNotify::default().0
        );
        assert_eq!(d["guildMemberNotify"], 0.0, "the binary registers \"0\"");
        // Block Trades (1764) welds the same way, and its "0" is behaviour: the refusal leg only
        // fires when the CVar is SET, so an unset value must read as "trades allowed".
        assert_eq!(
            d["BlockTrades"] != 0.0,
            crate::ui_trade::BlockTrades::default().0
        );
        assert_eq!(d["BlockTrades"], 0.0, "an unset BlockTrades allows trades");
        // The name trio (0992) welds to NameConfig's defaults the same way — and all three are
        // the binary's own registrar values now (1804), not two director pins over one.
        let names = NameConfig::default();
        assert_eq!(d["UnitNamePlayer"] != 0.0, names.player);
        assert_eq!(d["UnitNameNPC"] != 0.0, names.npc);
        assert_eq!(d["UnitNameOwn"] != 0.0, names.own);
        assert!(
            names.player && !names.npc && !names.own,
            "the binary registers UnitNamePlayer \"1\", NPC \"0\", Own \"0\""
        );
        // The V-plate pair welds to VPlateMode's defaults — both OFF, which is the reference's
        // own boot state on both of its halves (the `[0xc4da34]` bitmask and FrameXML's
        // `NAMEPLATES_ON = nil`). Enemy plates were the 0167 director pin until 1804.
        let plates = VPlateMode::default();
        assert_eq!(d[crate::vplates::CVAR_ENEMIES] != 0.0, plates.enemies);
        assert_eq!(d[crate::vplates::CVAR_FRIENDS] != 0.0, plates.friends);
        assert!(
            !plates.enemies && !plates.friends,
            "a fresh 1.12 client draws no plates until V is pressed"
        );
        // ClutterConfig::default() reads $WOW_CLUTTER_DENSITY; the registered default mirrors
        // the env-less ×2 literal (clutter.rs: "Default ×2 = Medium", 1649) on the panel's 0..2
        // scale. The weld is the point: the CVar's default and the engine's must be the same
        // ground cover, or a fresh config writes a row the world does not agree with.
        assert_eq!(d["WorldDetail"], 1.0);
        // The bubble pair (1139) welds to BubbleConfig's defaults — both the binary's own since
        // 1804 (`ChatBubbles` "1", `ChatBubblesParty` "0"; the party half was 0598's director pin).
        let bubbles = BubbleConfig::default();
        assert_eq!(d["ChatBubbles"] != 0.0, bubbles.all);
        assert_eq!(d["ChatBubblesParty"] != 0.0, bubbles.party);
        assert!(bubbles.all && !bubbles.party, "the binary's own pair");
        // The minimap pair (1131) welds to the widget's own `MINIMAP_DEFAULT_ZOOM`, which is the
        // byte-verified registration default `"3"` — one truth, mirrored in three places.
        let zoom = MinimapZoom::default();
        assert_eq!(d["minimapZoom"], f32::from(zoom.outdoor));
        assert_eq!(d["minimapInsideZoom"], f32::from(zoom.inside));
        assert_eq!(zoom.outdoor, benilla_ui::widget::MINIMAP_DEFAULT_ZOOM);
        // VSync welds to the video knob, which in turn welds to the window literal's boot
        // mode (`video::tests`) — so the registered "1" cannot drift from what we ship.
        assert_eq!(d["gxVSync"] != 0.0, VideoConfig::default().vsync);
        // The pane half-rate (1444) welds to the portrait knob's shipped default.
        assert_eq!(d["boothHalfRate"] != 0.0, PaneRate::default().half);
        // Render scale (1639) welds to OFF. Not a taste default: the whole tree of visual
        // goldens is denominated in a 1:1 backdrop, so a registered value other than 1 would
        // silently re-render every one of them through a resample.
        assert_eq!(d["renderScale"], 1.0);
    }

    #[test]
    fn apply_parses_clamps_and_reports_unknowns() {
        let mut sound = SoundConfig::default();
        let mut scale = UiScaleCvar(0.9);
        let mut view = ViewDistance { farclip: 350.0 };
        let mut look = LookConfig::default();
        let mut click = ClickConfig::default();
        let mut loot = LootConfig::default();
        let mut names = NameConfig::default();
        let mut plates = VPlateMode::default();
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
        let mut follow = FollowConfig::default();
        let mut video = VideoConfig::default();
        let mut pane_rate = PaneRate::default();
        let mut guild_notify = crate::ui_guild::GuildMemberNotify::default();
        let mut block_trades = crate::ui_trade::BlockTrades::default();
        // Literal, not Default: MsaaSetting::default() reads $WOW_MSAA.
        let mut msaa = MsaaSetting { samples: 1 };
        // What an Apple GPU answers for the trio we render into (Rgba16Float / Depth32Float /
        // the swapchain). 8 and 16 are NOT in it — which is the whole point below.
        let msaa_formats = benilla_world::view::MsaaFormats {
            formats: vec![(32, 32, 1), (32, 32, 2), (32, 32, 4)],
        };
        // Literal for the same reason: RenderScale::default() reads $WOW_RENDER_SCALE.
        let mut render_scale = RenderScale(1.0);
        // Literal for the same reason again (1642): TexFilterSetting::default() reads
        // $WOW_TRILINEAR / $WOW_ANISO. These are what ships (1645).
        let mut tex_filter = benilla_assets::TexFilterSetting {
            trilinear: true,
            aniso: 1,
        };
        // Literal for the same reason once more (1667): Realmlist::default() reads $WOW_HOST, and
        // a shell that happens to export it must not decide what this test asserts against.
        let mut realmlist =
            crate::realmlist::Realmlist::unpinned(crate::realmlist::DEFAULT_REALMLIST);
        let mut auto_self_cast = crate::ui_action::AutoSelfCast::default();
        let mut knobs = Knobs {
            sound: &mut sound,
            auto_self_cast: &mut auto_self_cast,
            scale: &mut scale,
            view: &mut view,
            look: &mut look,
            click: &mut click,
            loot: &mut loot,
            names: &mut names,
            plates: &mut plates,
            clutter: &mut clutter,
            minimap: &mut minimap,
            bubbles: &mut bubbles,
            zoom: &mut zoom,
            follow: &mut follow,
            video: &mut video,
            render_scale: &mut render_scale,
            pane_rate: &mut pane_rate,
            guild_notify: &mut guild_notify,
            block_trades: &mut block_trades,
            msaa: &mut msaa,
            tex_filter: &mut tex_filter,
            msaa_formats: &msaa_formats,
            realmlist: &mut realmlist,
        };
        assert!(apply_to_knobs("MusicVolume", "0.7", &mut knobs));
        assert_eq!(knobs.sound.music, 0.7);
        // The second string-valued row (1667): it must reach the knob rather than being rejected
        // by the numeric parse every other row goes through, and a value that is not an address
        // must be consumed (known key) while leaving the knob's truth alone.
        assert!(apply_to_knobs(
            "realmList",
            "logon.example.org:3724",
            &mut knobs
        ));
        assert_eq!(knobs.realmlist.address(), "logon.example.org:3724");
        assert!(apply_to_knobs(
            "realmlist",
            r#"SET realmlist "elsewhere.example.org""#,
            &mut knobs
        ));
        assert_eq!(knobs.realmlist.address(), "elsewhere.example.org");
        assert!(apply_to_knobs("realmList", "not an address", &mut knobs));
        assert_eq!(
            knobs.realmlist.address(),
            "elsewhere.example.org",
            "a known key with a bad value is consumed, and the resource keeps its truth",
        );
        // Clamps are the knob's own: volume to [0,1], farclip to FARCLIP_RANGE.
        assert!(apply_to_knobs("mastervolume", "7", &mut knobs));
        assert_eq!(knobs.sound.master, 1.0);
        assert!(apply_to_knobs("farclip", "50", &mut knobs));
        assert_eq!(knobs.view.farclip, *FARCLIP_RANGE.start());
        // Multisampling clamps to the reference's own [1, 16] and takes an int the way its `atoi`
        // does — the value reaching the camera is a sample COUNT, where 1 is none (1629).
        assert!(apply_to_knobs("gxMultisample", "4", &mut knobs));
        assert_eq!(knobs.msaa.samples, 4);
        // The filter policy's two rows: `anisotropic` takes the reference's own [1, 16] clamp,
        // `trilinear` is a flag. Both write the pending value; the process policy is already
        // published by the time either can be typed (1642).
        assert!(apply_to_knobs("anisotropic", "99", &mut knobs));
        assert_eq!(knobs.tex_filter.aniso, *benilla_assets::ANISO_RANGE.end());
        assert!(apply_to_knobs("anisotropic", "0", &mut knobs));
        assert_eq!(knobs.tex_filter.aniso, *benilla_assets::ANISO_RANGE.start());
        // Both directions: the knob starts at what ships (on), so only the flip to 0 proves the
        // arm does anything.
        assert!(apply_to_knobs("trilinear", "0", &mut knobs));
        assert!(!knobs.tex_filter.trilinear);
        assert!(apply_to_knobs("trilinear", "1", &mut knobs));
        assert!(knobs.tex_filter.trilinear);
        // **The DEVICE's ceiling, not the reference's** (decision 1643). 99 clamps to the
        // reference's 16 and then to the 4 this GPU offers — before 1643 it stopped at 16 and the
        // camera was handed a sample count wgpu refuses, killing the render thread on frame one.
        assert!(apply_to_knobs("gxmultisample", "99", &mut knobs));
        assert_eq!(knobs.msaa.samples, 4);
        // The realistic route in: a config written where 8x exists, opened where it does not.
        assert!(apply_to_knobs("gxMultisample", "8", &mut knobs));
        assert_eq!(
            knobs.msaa.samples, 4,
            "a device that stops at 4x must never be handed an 8"
        );
        // A count the device DOES offer is untouched.
        assert!(apply_to_knobs("gxmultisample", "2", &mut knobs));
        assert_eq!(knobs.msaa.samples, 2);
        assert!(apply_to_knobs("gxmultisample", "0", &mut knobs));
        assert_eq!(knobs.msaa.samples, *MSAA_RANGE.start());
        // Render scale takes a fraction and clamps to its own range at both ends (1639).
        assert!(apply_to_knobs("renderScale", "0.75", &mut knobs));
        assert_eq!(knobs.render_scale.0, 0.75);
        assert!(apply_to_knobs("renderscale", "9", &mut knobs));
        assert_eq!(knobs.render_scale.0, *RENDER_SCALE_RANGE.end());
        assert!(apply_to_knobs("renderscale", "0", &mut knobs));
        assert_eq!(knobs.render_scale.0, *RENDER_SCALE_RANGE.start());
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
        // The following style lands as the ENGINE's enum (0 Never / 1 Smart / 2 Always), and the
        // "3" the reference's own dropdown writes for Never still means Never.
        assert!(apply_to_knobs("cameraSmoothStyle", "0", &mut knobs));
        assert_eq!(knobs.follow.style, FollowStyle::Never);
        assert!(apply_to_knobs("camerasmoothstyle", "2", &mut knobs));
        assert_eq!(knobs.follow.style, FollowStyle::Always);
        assert!(apply_to_knobs("cameraSmoothStyle", "3", &mut knobs));
        assert_eq!(knobs.follow.style, FollowStyle::Never);
        assert!(apply_to_knobs("cameraSmoothStyle", "1", &mut knobs));
        assert_eq!(knobs.follow.style, FollowStyle::Smart);
        // Its two siblings land on the same knob — the tracking selector, and the rate, which
        // clamps to 1.12's AUTO_FOLLOW_SPEED slider range.
        assert!(apply_to_knobs("cameraSmoothTrackingStyle", "2", &mut knobs));
        assert_eq!(knobs.follow.tracking_style, FollowStyle::Always);
        assert_eq!(knobs.follow.style, FollowStyle::Smart, "and only that one");
        assert!(apply_to_knobs("cameraYawSmoothSpeed", "270", &mut knobs));
        assert_eq!(knobs.follow.yaw_speed, 270.0);
        assert!(apply_to_knobs("cameraYawSmoothSpeed", "9000", &mut knobs));
        assert_eq!(knobs.follow.yaw_speed, *FOLLOW_SPEED_RANGE.end());
        // The max-orbit factor lands as YARDS on the knob (base 15 x factor), clamped to 1..2.
        assert!(apply_to_knobs("cameraDistanceMaxFactor", "1", &mut knobs));
        assert_eq!(knobs.zoom.max, 15.0);
        assert!(apply_to_knobs("cameradistancemaxfactor", "5", &mut knobs));
        assert_eq!(knobs.zoom.max, 30.0);
        assert!(apply_to_knobs("autoLootDefault", "1", &mut knobs));
        assert!(knobs.loot.auto_loot);
        assert!(apply_to_knobs("showLootSpam", "0", &mut knobs));
        assert!(!knobs.loot.show_loot_spam);
        // Guild Member Alert (1589) — the row that ships OFF, so its ON is the interesting write.
        assert!(apply_to_knobs("guildMemberNotify", "1", &mut knobs));
        assert!(knobs.guild_notify.0);
        // Block Trades (1764) — the other row that ships OFF; its ON is what refuses a trade.
        assert!(apply_to_knobs("BlockTrades", "1", &mut knobs));
        assert!(knobs.block_trades.0);
        // The name trio lands on its gates (0992).
        assert!(apply_to_knobs("UnitNameNPC", "0", &mut knobs));
        assert!(!knobs.names.npc);
        assert!(apply_to_knobs("unitnameown", "1", &mut knobs));
        assert!(knobs.names.own);
        // …and the plate pair on the two bits of the bitmask, either casing.
        assert!(apply_to_knobs(
            crate::vplates::CVAR_ENEMIES,
            "0",
            &mut knobs
        ));
        assert!(!knobs.plates.enemies);
        assert!(apply_to_knobs("nameplateshowfriends", "1", &mut knobs));
        assert!(knobs.plates.friends);
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
            ("farclip".into(), "350".into(), "350".into()),     // back to default: removed
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
        let _l = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

        let mut app = cvar_app();

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

    /// A client whose CVar host is real: every knob resource the [`KnobParams`] census wants,
    /// [`CvarPlugin`] itself, and a VM for the table to live in. The three end-to-end tests below
    /// each stand a whole client up, and the census is one row per knob — copied per test, adding
    /// a knob meant editing every copy.
    fn cvar_app() -> App {
        let mut app = App::new();
        app.add_plugins(bevy::MinimalPlugins)
            .insert_resource(SoundConfig::default())
            .insert_resource(UiScaleCvar(DEFAULT_UI_SCALE))
            .insert_resource(ViewDistance { farclip: 350.0 })
            .insert_resource(MsaaSetting { samples: 1 })
            // Literal for the same reason (1642): TexFilterSetting::default() reads
            // $WOW_TRILINEAR / $WOW_ANISO. These are what ships (1645).
            .insert_resource(benilla_assets::TexFilterSetting {
                trilinear: true,
                aniso: 1,
            })
            // The device menu the Video dropdown reads. A real-shaped list, not empty: these
            // tests exercise `GetCurrentMultisampleFormat`'s lookup, which needs rows to find.
            .insert_resource(benilla_world::view::MsaaFormats {
                formats: vec![(32, 32, 1), (32, 32, 2), (32, 32, 4)],
            })
            .init_resource::<LookConfig>()
            .init_resource::<ClickConfig>()
            .init_resource::<LootConfig>()
            .init_resource::<NameConfig>()
            .init_resource::<VPlateMode>()
            .init_resource::<ClutterConfig>()
            .init_resource::<MinimapZoom>()
            .init_resource::<BubbleConfig>()
            .init_resource::<ZoomLimit>()
            .init_resource::<FollowConfig>()
            .init_resource::<VideoConfig>()
            // Literal, not Default: RenderScale::default() reads $WOW_RENDER_SCALE.
            .insert_resource(RenderScale(1.0))
            // Literal for the same reason again (1667): Realmlist::default() reads $WOW_HOST.
            .insert_resource(crate::realmlist::Realmlist::unpinned(
                crate::realmlist::DEFAULT_REALMLIST,
            ))
            .init_resource::<PaneRate>()
            .init_resource::<crate::ui_guild::GuildMemberNotify>()
            .init_resource::<crate::ui_trade::BlockTrades>()
            .init_resource::<crate::ui_action::AutoSelfCast>()
            .add_plugins(CvarPlugin);
        app.insert_non_send_resource(UiScript::new().unwrap());
        app
    }

    /// **The reported bug, end to end** (decision 1622): "char screen doesn't remember the last
    /// logged in char, the ref does". Two launches over one `benilla-config/`, with the real
    /// [`CvarPlugin`] and the real [`crate::char_select`] systems in between — entering the world
    /// as somebody has to survive the quit and bring the screen back to them.
    ///
    /// The seam this covers and the per-module tests cannot: `set_cvar_engine`'s queued change is
    /// only *persisted* if [`apply_to_knobs`] answers `true` for the name. A knobless CVar that
    /// falls through to `_ => return false` reaches the VM's table, reads back correctly all
    /// session, and is silently dropped at the save — which is this bug again, one layer down.
    #[test]
    fn entering_the_world_survives_the_quit_and_comes_back_selected() {
        use crate::char_select::{ClientState, Roster};
        use crate::local_state::test_env::{EnvGuard, ENV_LOCK};
        let _l = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = std::env::temp_dir().join(format!("benilla-lastchar-{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        let _c = EnvGuard::unset("WOW_CAPTURE");
        let _u = EnvGuard::unset("WOW_UI_SCALE");
        let _f = EnvGuard::unset("WOW_FARCLIP");
        let _d = EnvGuard::unset("WOW_CLUTTER_DENSITY");
        let _w = EnvGuard::unset("WOW_CHAR");
        let _s = EnvGuard::unset("WOW_CHARSELECT_PICK");
        let _h = EnvGuard::set("BENILLA_HOME", tmp.to_str().unwrap());
        let roster = || {
            (1..=4)
                .map(|g| crate::char_select::test_character(g, &format!("Char{g}")))
                .collect::<Vec<_>>()
        };

        // ── Launch 1: the roster lands, and the player enters the world as the third row. ────
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut app = cvar_app();
        app.add_plugins(bevy::state::app::StatesPlugin);
        crate::char_select::add_test_systems(&mut app, tx);
        app.update(); // Startup loads the (absent) file; the first Update seeds the VM table
        app.world_mut().write_message(crate::net::CharListMessage {
            characters: roster(),
            realm: None,
        });
        app.update();
        assert_eq!(
            app.world().resource::<Roster>().selected(),
            Some(0),
            "nothing remembered yet, so the first row — the behaviour that was already right",
        );
        app.world_mut().resource_mut::<Roster>().pending_pick = Some(3); // guid 3 = row 2
        app.update();
        app.world_mut().write_message(AppExit::Success);
        app.update();

        let text = std::fs::read_to_string(tmp.join("config.toml")).unwrap();
        assert!(
            text.contains("lastCharacterIndex = \"2\""),
            "entering the world must reach the file, 0-based like Config.wtf:\n{text}"
        );

        // ── Launch 2: a fresh client over the same folder, and the roster arrives. ───────────
        let (tx, _rx2) = crossbeam_channel::unbounded();
        let mut app = cvar_app();
        app.add_plugins(bevy::state::app::StatesPlugin);
        crate::char_select::add_test_systems(&mut app, tx);
        app.update();
        app.world_mut().write_message(crate::net::CharListMessage {
            characters: roster(),
            realm: None,
        });
        app.update();

        assert_eq!(
            app.world().resource::<Roster>().selected(),
            Some(2),
            "the second launch must stand the SAME character on the stage — the whole report",
        );
        assert_eq!(
            *app.world().resource::<State<ClientState>>().get(),
            ClientState::CharSelect,
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The minimap's zoom rides the same loop, driven from the **engine** rather than a Lua
    /// `SetCVar` (decision 1131): the `+`/`-` buttons call `Minimap:SetZoom`, which writes the live
    /// index and its CVar together — and that has to reach the knob and the file exactly like a
    /// settings row's write does, or the level is forgotten at the next launch.
    #[test]
    fn a_minimap_setzoom_reaches_the_knob_and_the_file() {
        use crate::local_state::test_env::{EnvGuard, ENV_LOCK};
        let _l = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

        let mut app = cvar_app();
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
    /// **The table's string-valued CVars, named — and each one's default asserted on its own
    /// terms.**
    ///
    /// Pinned as a closed list so a new string CVar has to come here and think about the numeric
    /// test above rather than silently widening it. That is exactly what happened at 1627, when
    /// this test still said "the ONE": `gxResolution` arrived and the list grew by one, on purpose.
    ///
    /// **`realmName` defaults EMPTY** — empty rather than a guess: the value is written from the
    /// session's real realm by `set_realm_name`, so the default only ever describes a client that
    /// has not connected. wow-re records `"Last realm connected to"` beside the registration, but
    /// that reads like the CVar's HELP text rather than its value, and nothing here needs it
    /// resolved — `""` is what `Ace/AceState.lua:27`'s `ace.trim(GetCVar("realmName"))` handles
    /// cleanly, and inventing a realm name would be worse than admitting we have none yet.
    ///
    /// **`gxResolution` defaults to the pre-1627 window** (decision 1627), and **`realmList` to
    /// `localhost`** (1667). These are the rows [`apply_to_knobs`] matches ahead of its numeric
    /// parse, so each default is asserted through the same parser the live value goes through — a
    /// spelling this table accepts but [`crate::video::parse_resolution`] or
    /// [`crate::realmlist::normalize`] rejects would otherwise ship as a silent fall back.
    ///
    /// The list itself is the load-bearing half: a new string-valued row that forgets its arm in
    /// `apply_to_knobs` is a CVar the player can set and the client will never honour, and this is
    /// what makes adding one impossible to do quietly.
    #[test]
    fn the_string_valued_cvars_are_the_realm_and_the_windowed_size() {
        let mut strings: Vec<&str> = REGISTERED
            .iter()
            .filter(|r| r.default.parse::<f32>().is_err())
            .map(|r| r.name)
            .collect();
        strings.sort_unstable(); // the list is the claim, not where the rows sit in the table
        assert_eq!(strings, vec!["gxResolution", "realmList", "realmName"]);
        let default_of = |name: &str| {
            REGISTERED
                .iter()
                .find(|r| r.name == name)
                .map(|r| r.default)
                .expect("registered")
        };
        assert_eq!(default_of("realmName"), "");
        assert_eq!(
            crate::video::parse_resolution(default_of("gxResolution")),
            Some(crate::video::DEFAULT_WINDOWED)
        );
        // Same posture for the third row (1667): a default this table accepts but
        // `realmlist::normalize` rejects would ship as a client that silently cannot dial.
        assert_eq!(
            crate::realmlist::normalize(default_of(crate::realmlist::CVAR_REALMLIST)).as_deref(),
            Some(crate::realmlist::DEFAULT_REALMLIST),
        );
    }
}
