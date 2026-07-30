//! The game-state `Unit*` bindings (decision 0068 §3) — the first slice of the addon-facing API that
//! reads *live game state*, and the seam that keeps this crate engine-free while doing so.
//!
//! The engine must not touch the ECS/net (decisions 0006/0061: the codec is stateless, the ECS owns
//! state). So instead of reaching outward, the app pushes a per-frame **unit snapshot** into the VM
//! via [`UiScript::set_unit`], and the `Unit*` globals here read that plain data. A frame's
//! `UnitHealth("player")` therefore resolves against a [`UnitState`] the Bevy side deposited that
//! frame — the same shape a real addon sees, with none of the coupling.
//!
//! Return shapes follow the live API (warcraft.wiki.gg): `UnitName` returns the name *or nil*;
//! `UnitHealth`/`UnitHealthMax`/`UnitLevel` return numbers (`UnitLevel` returns **−1** for a
//! unit whose level "can't be told" — [`level_reads_unknown`], the target frame's skull gate);
//! a unit that doesn't exist reports `UnitExists` false and the numbers `0` (nil for the name),
//! exactly as the client does for an absent unit token. Tokens (`"player"`, `"target"`, …) are opaque strings the host never
//! interprets — the app decides what each maps to.
//!
//! v1 gaps, stated not hidden: the snapshot carries only the *active* power slot (see the
//! `UnitPower` note below); `UnitIsDead` returns a Lua boolean rather than the client's `1`/nil
//! (truthy either way — the shape a caller branches on is identical); `UnitIsConnected`'s backing
//! field defaults `false`, and no app-side feed sets it `true` for live units yet (decision 0434
//! §2's party-frame predicates land the binding first, the feed follows — see the field doc).

use mlua::Lua;

use super::Model;

/// One unit-token's game-state snapshot, pushed by the app each frame and read by the `Unit*`
/// bindings. Plain data (no mlua handles, no ECS types) — the engine-free seam decision 0068 §3
/// draws between the app's net/ECS feed and the Lua API.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UnitState {
    /// Whether the unit exists (`UnitExists`). A token with no snapshot is treated as `false` too.
    pub exists: bool,
    /// The unit's name (`UnitName` → the string, or nil). `None` while the app's name-query cache
    /// hasn't answered (or the server doesn't know the unit).
    pub name: Option<String>,
    /// Current health (`UnitHealth`).
    pub health: u32,
    /// Maximum health (`UnitHealthMax`).
    pub max_health: u32,
    /// Level (`UnitLevel`).
    pub level: u32,
    /// The active power type (`UnitPowerType`): `0` mana, `1` rage, `2` focus, `3` energy,
    /// `4` happiness — the descriptor's `UNIT_FIELD_BYTES_0` byte 3.
    pub power_type: u8,
    /// Current power of the active type (`UnitPower`).
    pub power: u32,
    /// Maximum power of the active type (`UnitPowerMax`).
    pub max_power: u32,
    /// Whether the unit is dead (`UnitIsDead`). NB a released ghost is NOT dead by this
    /// predicate — its wire health is 1 (decision 0308 §1); the trio is dead / [`Self::ghost`] /
    /// dead-or-ghost, the real client's three tests.
    pub dead: bool,
    /// Whether the unit is a released ghost (`UnitIsGhost` — `PLAYER_FLAGS` bit 0x10, decision
    /// 0308 §1). Only meaningful for player tokens; creatures stay `false`.
    pub ghost: bool,
    /// The unit's reaction toward the player on the `UnitReaction` scale (`1` hated … `4` neutral …
    /// `8` exalted; `0` = unknown → `UnitReaction` returns nil). The app resolves it from the
    /// faction-template / reputation decode (the same one the selection ring uses) for the `"target"`
    /// token; other tokens leave it `0`. Read by `UnitReaction`, which the target frame tints its
    /// name plate by (`TargetFrame_CheckFaction`).
    pub reaction: u8,
    /// The unit's localized race display name (`UnitRace`'s first return, e.g. "Night Elf").
    /// `None` = unknown (the race byte hasn't streamed, or a raceless creature) → `UnitRace`
    /// returns nil, nil. The app resolves it from `UNIT_FIELD_BYTES_0` byte 0 (decision 0208 §3).
    pub race: Option<String>,
    /// The race's file/token name (`UnitRace`'s second return, e.g. "NightElf" — 1.12's 8 races),
    /// the shape texture-path splices key on.
    pub race_file: Option<String>,
    /// The unit's localized class display name (`UnitClass`'s first return, e.g. "Warrior").
    pub class: Option<String>,
    /// The class's file name (`UnitClass`'s second return): the **uppercase** token ("WARRIOR")
    /// the ref's stat-tooltip lookups key on (`getglobal(strupper(class).."_STAT_TOOLTIP")` —
    /// the ref uppercases it again defensively; we store it already uppercase).
    pub class_file: Option<String>,
    /// The unit's sex on the `UnitSex` scale: `2` male, `3` female (`1` = neuter/unknown). `0`
    /// (the unfilled default) reports as nil — the API's "can't tell", like [`Self::reaction`].
    /// The app maps `UNIT_FIELD_BYTES_0` byte 2 (0 male / 1 female) onto this shape.
    pub sex: u8,
    /// This token is a PLAYER character (guid family) — the unit tooltip's level line renders
    /// "Race Class (Player)" instead of the creature type (decision 0276's verified law).
    pub is_player: bool,
    /// The NPC subtitle line ("Stable Master") — the creature template's subname; the unit
    /// tooltip's second line. `None` for players/untitled creatures.
    pub subtitle: Option<String>,
    /// The creature TYPE display word ("Beast", "Humanoid", …) — app-resolved from
    /// `CreatureType.dbc`; the level line's class slot for hostile/neutral creatures.
    pub creature_type_name: Option<String>,
    /// Elite rank 0..4 — the creature classification every rank reader in the client shares, via
    /// the one getter `0x605620`: the level line's rank word `{"", Elite, Elite, Boss, ""}` (0276),
    /// `UnitLevel`'s world-boss −1 ([`level_reads_unknown`]), and `UnitClassification`
    /// ([`classification_word`], decision 0782). **Already gated** by the app when it fills this:
    /// the getter answers `0` unless the unit has a cached creature template *and* a zero
    /// `UNIT_FIELD_PETNUMBER`, so a player, an un-queried creature and an enslaved elite all read
    /// `0` here — the gate belongs at the one write, not in each of the three readers.
    pub rank: u32,
    /// The creature civilian flag — feeds the tooltip's green CIVILIAN line (shown only for a
    /// PvP-flagged HOSTILE unit that is also GREY/trivial to the player — the dishonorable-kill
    /// warning, the client's `0x612550` gate; a friendly civilian never shows it).
    pub civilian: bool,
    /// The creature racial-leader flag — the tooltip's white LEADER line (`0x6125c0`, gated on
    /// the same PvP bit as CIVILIAN).
    pub racial_leader: bool,
    /// PvP-flagged (`UNIT_FIELD_FLAGS` PvP bit) — the tooltip's "PvP" line, and also `UnitIsPVP`
    /// (decision 0434 §2: the party-frame PVP icon reads the same flag the tooltip line does, so
    /// there's one field, not two).
    pub pvp: bool,
    /// Skinnable (`UNIT_FIELD_FLAGS` skinnable bit) — the tooltip's RED "Skinnable" line.
    pub skinnable: bool,
    /// The unit's faction display name ("Stormwind") — the tooltip's white line between the
    /// level line and "PvP". App-resolved (Faction.dbc through the unit's faction template,
    /// every gate applied: reputation faction, race/class slot, hidden flag, the creature
    /// HIDE_FACTION_TOOLTIP type flag); `None` = no line.
    pub faction_name: Option<String>,
    /// Connection to the server is live (`UnitIsConnected`) — an offline party member's portrait
    /// desaturates (decision 0434 §2/§3: the `GROUP_LIST` status byte's `0x01` online bit).
    /// **v1 default gap, stated not hidden:** the zero value is `false`, so a snapshot built via
    /// `..Default::default()` reports a unit disconnected unless the app sets this explicitly; the
    /// per-frame feed must set it `true` for every *live* unit token it pushes (mirroring
    /// [`Self::exists`]) — a pending app-side change, not yet wired as of this field's addition. A
    /// token absent from the store (`UnitExists` false) still reports nil regardless, via the same
    /// "no snapshot" path every other predicate here uses.
    pub is_connected: bool,
    /// Away-from-keyboard (`UnitIsAFK` — decision 0434 §2's status-byte `0x40` bit). The party
    /// frame's name line and chat both key off it. App-resolved; `false` until the feed lands.
    pub is_afk: bool,
    /// Do-not-disturb (`UnitIsDND` — decision 0434 §2's status-byte `0x80` bit), the AFK flag's
    /// sibling.
    pub is_dnd: bool,
    /// Free-for-all PvP-flagged (`UnitIsPVPFreeForAll` — decision 0434 §2's status-byte `0x10`
    /// bit) — distinct from [`Self::pvp`]'s ordinary faction PvP flag; a unit can carry either, both,
    /// or neither independently (an FFA zone flags this without the ordinary flag ever setting).
    pub is_pvp_ffa: bool,
    /// The unit's PvP faction group — `"Alliance"` or `"Horde"`, else `None` (`UnitFactionGroup`;
    /// decision 0646 §1). App-resolved from `UNIT_FIELD_FACTIONTEMPLATE` → `FactionTemplate.dbc`'s
    /// group mask, with the Player bit skipped: every playable race's template carries
    /// Player|<side> (mask 3 or 5), and so do the PvP-flagged city guards, so the side bit is the
    /// only one that can name a shipped `UI-PVP-<group>` texture. `None` for Monster/neutral
    /// templates is a state the icon callers explicitly handle (`if ( factionGroup and … )`).
    ///
    /// The Era binding returns it twice — English token first, localized name second; enUS ships
    /// the same word for both (FactionGroup.dbc's `InternalName` and `Name0`).
    pub faction_group: Option<String>,
    /// The unit's GUID (`OBJECT_FIELD_GUID`) — the identity the cross-token predicates compare
    /// (`UnitIsUnit`, `UnitInParty`; decision 0434 §5's popup gating). `0` = the app's feed didn't
    /// resolve one; two zero guids never compare equal.
    pub guid: u64,
    /// The raid-target mark on this unit (`GetRaidTargetIndex`): `0` none, `1..=8` the slot.
    /// App-fed from the `GroupState` raid-target board (decision 0434 §6's verified 8-slot table
    /// law) — the phase-5 submenu reads it; the phase-6 world renders draw from the same board.
    pub raid_target: u8,
    /// The player can attack this unit (`UnitCanAttack("player", unit)`) — app-fed from the
    /// byte-confirmed `CanAttack 0x606980` predicate (decision 0172; the flag disqualifiers +
    /// reaction ≤ neutral legs TAB and the combat flash share; the Lua binding `0x516c50` is
    /// pure delegation to it, §5-VERIFIED). Like [`Self::reaction`], the feed resolves it for
    /// the `"target"` token; other tokens leave the default `false` (→ nil), which is right for
    /// every current caller (`TargetFrame_CheckLevel`'s difficulty-color gate).
    pub can_attack: bool,
    /// This token resolves to a **TYPEID_CORPSE world object** — `UnitIsCorpse`'s whole
    /// predicate (`0x5161c0`, §5-VERIFIED: a pure object-type check, no health test; a dead
    /// mob/player is NOT a corpse). No feed sets it yet — corpse objects (a released player's
    /// remains) aren't selectable in benilla — a stated gap, not an oversight.
    pub corpse_object: bool,
}

/// The grey-band table `0x80ae98` (a byte-identical twin at `0x81dda8` drives the nameplate's
/// grey level colour), indexed by `playerLevel / 5` and clamped to 12 past the table. ONE home
/// for its three consumers: [`unit_is_grey`] (tooltip CIVILIAN gate + the plate's skull/grey
/// legs), the `GetQuestGreenRange` binding (the FrameXML `GetDifficultyColor` green→grey
/// boundary), and — through both — every level con the UI shows.
const GREY_BAND: [u32; 20] = [
    4, 4, 5, 5, 6, 6, 7, 7, 8, 9, 10, 11, 12, 12, 12, 12, 12, 12, 12, 12,
];

/// The band value for a player level — `GREY_BAND[min(playerLevel/5, 19)]` (the
/// `GetQuestGreenRange` return, §5-VERIFIED 2026-07-17: the binding `0x4e17d0` reads the
/// byte-identical `0x8076c0` twin with exactly this index clamp).
pub fn grey_band(player_level: u32) -> u32 {
    GREY_BAND
        .get((player_level / 5) as usize)
        .copied()
        .unwrap_or(12)
}

/// The trivial/GREY check (`0x5f0700`, §5-VERIFIED 2026-07-17 — was INFERRED): a unit enough
/// levels *below* the player that it cons grey — `playerLevel − unitLevel >
/// GREY_BAND[playerLevel/5]`, **strict >** (diff == band is still green). The binary's full
/// predicate also requires not-player-controlled; every consumer here applies this to creatures.
pub fn unit_is_grey(player_level: u32, unit_level: u32) -> bool {
    player_level > unit_level && player_level - unit_level > grey_band(player_level)
}

/// The "level reads ??" gate — the engine tooltip's byte-pinned leg set (`0x529fe0` §2-LEVEL):
/// never players; level 0 (unstreamed), world-boss rank 3, or a much-higher hostile (reaction
/// ≤ 2 on the 1..8 API scale, unit ≥ player + 10 — **inclusive**, byte-confirmed). The Lua
/// `UnitLevel`'s −1 law was §5-VERIFIED against this (wow-re `ui/scratch/
/// level-assess-bindings.md`, 2026-07-17): its two −1 legs are exactly the boss + hostile-≥10
/// legs here (no trivial-gray term exists in `0x517fc0`, and none is possible at ≥ +10), with
/// ONE divergence the binding handles itself — a raw level ≤ 0 returns VERBATIM, not −1
/// (same skull outcome in FrameXML; the tooltip's "??" keeps the level-0 leg per its own law).
pub fn level_reads_unknown(u: &UnitState, player_level: u32) -> bool {
    let much_higher_hostile =
        u.reaction != 0 && u.reaction <= 2 && u.level >= player_level.saturating_add(10);
    !u.is_player && (u.level == 0 || u.rank == 3 || much_higher_hostile)
}

/// `UnitClassification`'s return — the classification-word table, **byte-verified** at `0x850424`
/// (decision 0782): the binding `0x516d90` is nothing but `TABLE[rank(unit)]`, indexed by the same
/// gated rank [`UnitState::rank`] carries. The table is exactly five entries long (the sixth dword
/// at `0x850438` is an unrelated `"UnitExists"` literal), and rank comes pre-clamped to 0..4 by
/// `CreatureInfo+0x20`, so an out-of-range value can only be our own bug — it reads `"normal"`,
/// which is also what the binding's unresolved-token path pushes (`0x516dc4` loads index 0).
///
/// Note `"rareelite"` is a real return here even though 1.12 has no rare-elite *art* and no
/// rare-elite tooltip *word* — the reference's `TargetFrame_CheckClassification` sends it to the
/// same Elite border as `"elite"`, and the tooltip's `{"", Elite, Elite, Boss, ""}` prints ELITE for
/// it. The distinction exists in the API and nowhere in the pixels.
pub fn classification_word(rank: u32) -> &'static str {
    match rank {
        1 => "elite",
        2 => "rareelite",
        3 => "worldboss",
        4 => "rare",
        _ => "normal",
    }
}

/// The Era power-token string for a power-type index (`UnitPowerType`'s second return,
/// `UNIT_POWER_UPDATE`'s arg2). Unknown indices report as mana, the descriptor default.
pub fn power_token(ty: u8) -> &'static str {
    match ty {
        1 => "RAGE",
        2 => "FOCUS",
        3 => "ENERGY",
        4 => "HAPPINESS",
        _ => "MANA",
    }
}

impl super::UiScript {
    /// Push (or clear) a unit token's snapshot. `Some(state)` stores it under `token`; `None` removes
    /// it (so `UnitExists(token)` reports false and the numeric getters return `0`). The app's
    /// per-frame feed (decision 0068 §3) calls this for `"player"`/`"target"`/… before the VM's
    /// event dispatch, so a frame's `OnEvent` sees the current values.
    pub fn set_unit(&mut self, token: &str, state: Option<UnitState>) {
        {
            let mut model = self.model_mut();
            match state {
                Some(s) => {
                    model.units.insert(token.to_string(), s);
                }
                None => {
                    model.units.remove(token);
                }
            }
        }
        // A push for a tooltip's LIVE unit token re-drives its health bar (the byte law's
        // HEALTH watcher — decision 0276; no line rebuild).
        super::tooltip_unit::on_unit_push(&self.lua, token);
    }

    /// Push the player's purse in copper (`PLAYER_FIELD_COINAGE`), read by the `GetMoney` global. A
    /// player-level snapshot (like the unit tokens) — the app calls this each frame the coinage
    /// field changes (decision 0081 phase 4).
    pub fn set_money(&mut self, copper: u64) {
        self.model_mut().money = copper;
    }

    /// Push the player's experience within the level (`PLAYER_XP`) and the level's requirement
    /// (`PLAYER_NEXT_LEVEL_XP`), read by the `UnitXP`/`UnitXPMax` globals. A player-level pair (like
    /// [`Self::set_money`]) — the app calls this each frame the XP fields change (the MainMenuBar XP
    /// bar feed). Both PRIVATE descriptor fields, so this only ever carries our own avatar's values.
    pub fn set_player_xp(&mut self, xp: u32, next_level_xp: u32) {
        let mut model = self.model_mut();
        model.player_xp = xp;
        model.player_next_level_xp = next_level_xp;
    }

    /// Drain the unit tokens `TargetUnit` queued since the last call — the app resolves each to a
    /// streamed entity and commits the selection (the reference's `TargetUnit` → SetSelection path).
    pub fn take_target_requests(&mut self) -> Vec<String> {
        std::mem::take(&mut self.model_mut().target_requests)
    }

    /// Drain the `ClearTarget()` trigger: `true` if it fired with a live target since the last
    /// call — the ESC chain's last leg; the app commits the deselect (SetSelection guid 0).
    pub fn take_target_clear(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().target_clear)
    }
}

/// Pick the interesting token from a directional two-unit call (`UnitIsEnemy(a, b)`): whichever
/// arg isn't `"player"` — our snapshot stores the relationship on the non-player unit (target),
/// and the ref calls both argument orders.
fn pick_unit_token(a: &Option<String>, b: &Option<String>) -> Option<String> {
    match (a, b) {
        (Some(x), _) if x != "player" => Some(x.clone()),
        (_, Some(y)) => Some(y.clone()),
        (x, _) => x.clone(),
    }
}

/// Read a unit token's snapshot under a short model borrow, mapping it through `f`; `default` when the
/// token is absent (the "unit doesn't exist" path).
fn with_unit<T>(
    lua: &Lua,
    token: &Option<String>,
    default: T,
    f: impl FnOnce(&UnitState) -> T,
) -> T {
    let model = lua.app_data_ref::<Model>().expect("model app_data");
    match token.as_ref().and_then(|t| model.units.get(t)) {
        Some(u) => f(u),
        None => default,
    }
}

/// The `Unit*`/`GetQuestGreenRange` Lua binding registrations — split from this module's
/// state+laws half purely for size (same seam as the other script modules' install fns).
mod bindings;
#[cfg(test)]
mod tests;

pub(super) use bindings::install;
