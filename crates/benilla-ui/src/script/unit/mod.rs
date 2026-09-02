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
//! `UnitMana` note below); `UnitIsDead` returns a Lua boolean rather than the client's `1`/nil
//! (truthy either way — the shape a caller branches on is identical); `UnitIsConnected`'s backing
//! field defaults `false`, and no app-side feed sets it `true` for live units yet (decision 0434
//! §2's party-frame predicates land the binding first, the feed follows — see the field doc).

use mlua::Lua;

use super::Model;

/// One queued **selection ask** from Lua, in call order — the app resolves it to a guid and
/// commits it through the one SetSelection tail.
///
/// The three verbs share one queue because the reference shares one *function*: `TargetUnit`
/// (`0x4899d0`), `AssistUnit` (`0x489b80`), `TargetLastEnemy` and `TargetLastTarget` all reach
/// selection through the "select if it resolves" helper `0x489a40`, whose three arms are the same
/// for every caller — resolve the guid and commit, else fall back to the group roster, else a
/// **silent return** (wow-re `object-layer/scratch/selection-attack-seam.md` §1). Splitting them
/// into a queue each would also lose their relative order, which a macro can observe
/// (`TargetUnit("party1"); AssistUnit("target")` is not the same as the reverse).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectionRequest {
    /// `TargetUnit(unit)` — select the unit the token names.
    Unit(String),
    /// `AssistUnit(unit)` — select the token's own `UNIT_FIELD_TARGET`; a basis with no target is
    /// a silent no-op (the reference's shared assist tail bails before any send).
    Assist(String),
    /// `TargetLastEnemy()` — select the last *attackable* unit that was committed.
    LastEnemy,
}

/// One unit-token's game-state snapshot, pushed by the app each frame and read by the `Unit*`
/// bindings. Plain data (no mlua handles, no ECS types) — the engine-free seam decision 0068 §3
/// draws between the app's net/ECS feed and the Lua API.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UnitState {
    /// Whether the unit exists (`UnitExists`). A token with no snapshot is treated as `false` too.
    ///
    /// **Stays TRUE for an out-of-range party member** — that is the whole point of the roster
    /// fallback, and it is what separates this field from [`Self::has_object`].
    ///
    /// *Named gap, second order:* 1.12's `UnitExists 0x515fb0` is a **conjunction** — the object
    /// lookup AND a virtual `[edx+0x58]` that resolves to `IsSelectable 0x60be60`
    /// (`!(UNIT_FIELD_FLAGS & 0x02000000) || UNIT_FIELD_CREATEDBY == you`) — falling back to the
    /// GUID-only roster test on a miss. We model the fallback and not the conjunct, so a
    /// not-selectable unit created by someone else reads `exists` here and `nil` there. No corpus
    /// caller reaches it; recorded rather than folded in (wow-re
    /// `ui/scratch/unitisvisible-object-presence.md`).
    pub exists: bool,
    /// **`UnitIsVisible` — the client holds a live object for this token, and nothing else.**
    ///
    /// The reference is 57 bytes with one branch (`0x516030`):
    /// `ClntObjMgrObjectPtr(resolve(token), TYPEMASK_UNIT) != NULL`. Its body reads **zero** object
    /// fields, makes **zero** comparisons beyond `test eax,eax`, and contains **zero** float
    /// opcodes — so there is no distance term, no radius, no alpha/fade/interpolation flag and no
    /// "known to the client" bit in it. Addons read it as a range gate because **the range test is
    /// the server's**: `SMSG_DESTROY_OBJECT` and the OUT_OF_RANGE demotion unlink the object from
    /// the manager index this query searches, so the client answers with the presence of the
    /// object the server chose to send.
    ///
    /// Distinct from [`Self::exists`], and neither implies the other. The one that matters here:
    /// an out-of-range party member is `exists == true`, `has_object == false` — which is exactly
    /// pfUI's `if not UnitIsVisible(unitstr) or not UnitIsConnected(unitstr)` portrait branch.
    ///
    /// The app already computed this and did not publish it: `ui_party::feed`'s
    /// `member_unit_state` takes `store: Option<&ObjectStore>` and its own comment calls that
    /// argument "the reference's `0x468460`". Every snapshot built from a live store sets it
    /// (`ui_unit::snapshot`); the roster-record leg and a token with no snapshot leave it `false`.
    ///
    /// **One presence field serves three verbs** — `UnitIsVisible`, and the object-presence
    /// conjunct of both tapped predicates below.
    pub has_object: bool,
    /// `UnitIsTapped` — `UNIT_DYNAMIC_FLAGS` (UpdateField **143**) bit `0x4`: this unit is
    /// someone's kill credit. `0x519c90` is `test BYTE PTR [eax+0x224],0x4` behind the same
    /// object-presence check [`Self::has_object`] carries, and **nothing else** — no ownership, no
    /// GUID compare, no party/raid or health conjunct anywhere in the body (wow-re
    /// `ui/scratch/tapped-bits-and-unit-faction.md`).
    ///
    /// Set only on the live-descriptor leg, so an out-of-range unit reads `false` — which is the
    /// object-presence conjunct, for free, by the same route `has_object` gets it.
    pub tapped: bool,
    /// **`UnitIsPartyLeader`'s FIRST leg** — the server's `PLAYER_FLAGS & 0x1` on this unit's own
    /// descriptor, restricted to players (`TYPEMASK_PLAYER`, `0x10` — **not** this family's usual
    /// `8`, which is the one difference the predicate makes to the object lookup).
    ///
    /// Not derived client-side, and not the same question as "leads MY group": it is true for a
    /// stranger who leads their own party, which no comparison against our group's leader GUID can
    /// express. The second leg is that comparison, in `script::party` — the two cover disjoint
    /// failures and the reference ORs them (wow-re
    /// `ui/scratch/party-leader-and-nameplate-verbs.md`).
    pub group_leader: bool,
    /// `UnitIsTappedByPlayer` — the same field's bit `0x8` (`0x519d00`, a masked-byte clone of its
    /// sibling: 108 bytes each, differing only in the mask and the `Usage:` string).
    ///
    /// Read as a **pair**: `tapped && !tapped_by_player` is "someone else's kill", which is the
    /// grey-bar condition unit-frame addons draw (pfUI `api/unitframes.lua:2012`).
    pub tapped_by_player: bool,
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
    /// Current power of the active type (`UnitMana`).
    pub power: u32,
    /// Maximum power of the active type (`UnitManaMax`).
    pub max_power: u32,
    /// Whether the unit is dead (`UnitIsDead`). NB a released ghost is NOT dead by this
    /// predicate — its wire health is 1 (decision 0308 §1); the trio is dead / [`Self::ghost`] /
    /// dead-or-ghost, the real client's three tests.
    pub dead: bool,
    /// Whether somebody is charming this unit (`UnitIsCharmed`).
    ///
    /// **`UNIT_FIELD_CHARMEDBY != 0`, and the asymmetry is the fact worth keeping.** The binding
    /// `0x516cf0` reads fields 10/11 as one 64-bit value and tests it non-zero — not a
    /// `UNIT_FIELD_FLAGS` bit, and not a comparison against the player (its body contains no `cmp`
    /// at all). The field means *"who charms me"*; its mirror `UNIT_FIELD_CHARM` (*"whom I charm"*)
    /// is never read here. So a **charmer** answers nil, a **charmed** pet answers 1, and an
    /// ordinary **summoned** pet answers nil — `"pet"` is itself charm-else-summon in the resolver.
    /// (wow-re `system/ui/scratch/unit-verbs-controlled-charmed-creaturetype.md`.)
    pub charmed: bool,
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
    /// This unit's INVSLOT 17 is a **relic** slot rather than a ranged-weapon slot —
    /// `UnitHasRelicSlot`'s whole answer (`0x519e50`).
    ///
    /// Resolved app-side off `ChrClasses.dbc` field 16, for the same reason [`Self::class_file`]
    /// is resolved app-side: it is a static per-class column of a table this crate cannot read.
    /// The reference gates on TYPEMASK_PLAYER before it looks at the class byte, so the feed sets
    /// this only for a player — a creature, a pet and an unstreamed unit all leave it `false`,
    /// which is the nil leg.
    ///
    /// True for exactly Paladin, Shaman and Druid in 1.12 (Libram, Totem, Idol). It was believed
    /// here for a long time that no 1.12 class had one; see decision 1796 for how that survived.
    pub has_relic_slot: bool,
    /// The unit's sex on the `UnitSex` scale: `2` male, `3` female (`1` = neuter/unknown). `0`
    /// (the unfilled default) reports as nil — the API's "can't tell", like [`Self::reaction`].
    /// The app maps `UNIT_FIELD_BYTES_0` byte 2 (0 male / 1 female) onto this shape.
    pub sex: u8,
    /// This token is a PLAYER character (guid family) — the unit tooltip's level line renders
    /// "Race Class (Player)" instead of the creature type (decision 0276's verified law).
    pub is_player: bool,
    /// This token is **player-CONTROLLED** — `UNIT_FIELD_FLAGS` bit 3
    /// (`UNIT_FLAG_PVP_ATTACKABLE 0x8`, behaviourally "player-controlled"; the same bit
    /// `target::relations::ring_reaction`'s duel leg selects on). Wider than
    /// [`Self::is_player`]: a player's pet and a charmed creature are player-controlled without
    /// being players, which is exactly the distinction stock `UnitFrame_OnEnter`
    /// (UnitFrame.lua:58) needs when it decides whether the hovered plate gets the
    /// player-options newbie tip. `UnitIsPlayer`'s note used to call this reach a gap we did not
    /// carry; the unit-frame migration is what closed it.
    pub player_controlled: bool,
    /// The unit's guild membership (`GetGuildInfo(unit)`, decision 1257). `None` = guildless, or
    /// a creature, or a player whose `PLAYER_GUILDID` has not streamed yet. Filled from the
    /// PUBLIC descriptor fields 191/192 joined against the app's guild-identity cache — see
    /// [`super::guild::UnitGuild`].
    pub guild: Option<super::guild::UnitGuild>,
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
    /// **The zero value is `false`**, so a snapshot built via `..Default::default()` reports a unit
    /// DISCONNECTED — which matters more than it reads: stock `UnitFrameManaBar_Update`
    /// (UnitFrame.lua:213) greys a disconnected unit's bar and never reaches the power colour at
    /// all. A synthetic unit that means to be live must say so.
    ///
    /// The app's own feed does (`ui_unit::snapshot`, "the stated `is_connected` gap, closed"), and
    /// the party feed reads the roster status byte's `0x01` for its tokens. This doc used to call
    /// that "a pending app-side change, not yet wired"; it has been wired since, and the note
    /// outlived it. A token absent from the store (`UnitExists` false) still reports nil
    /// regardless, via the same "no snapshot" path every other predicate here uses.
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
    /// The unit's **current** PvP rank on the internal `0..=18` scale, `0` = no rank
    /// (`UnitPVPRank`; decision 1512). `PLAYER_BYTES_3` **byte 3** — a **PUBLIC** descriptor field,
    /// which is the whole reason a foreign player's rank is knowable and why the reference's
    /// inspect pane can call `UnitPVPRank("target")`. Not to be confused with the HIGHEST LIFETIME
    /// rank (`PLAYER_FIELD_BYTES` byte 3), which is private, rides
    /// [`HonorState`](super::HonorState) instead, and is what `GetPVPLifetimeStats` reports.
    ///
    /// Internal, not visual: the badge texture and `GetPVPRankInfo`'s second return are the
    /// `-4..=14` visual scale, converted in [`super::pvp`] and nowhere else.
    pub pvp_rank: u8,
    /// The unit's **city-protector title** index — `PLAYER_BYTES_3` **byte 2**, the byte next door
    /// to [`Self::pvp_rank`] (vmangos `PLAYER_BYTES_3_OFFSET_CITY_PROTECTOR_TITLE`), PUBLIC like
    /// it. `0` = none. Read by exactly one thing: `UnitPVPName`'s medal line, which appends
    /// `"\n"` + the `PVP_MEDAL<n>` GlobalString when it is non-zero (`0x6093ef`).
    ///
    /// **v1 feed gap, stated not hidden:** no app-side feed fills this yet — it rides the same
    /// dword the rank byte is decoded from, so wiring it is one line there, and until that line
    /// exists every unit reads `0` and `UnitPVPName` renders no medal. The binding leg is built
    /// against the field rather than left out, so the seam is a feed line rather than a missing
    /// arm (same shape as [`Self::is_connected`]'s stated gap).
    pub pvp_medal: u8,
    /// The unit's PvP faction group — `"Alliance"` or `"Horde"`, else `None` (`UnitFactionGroup`;
    /// decision 0646 §1). App-resolved from `UNIT_FIELD_FACTIONTEMPLATE` → `FactionTemplate.dbc`'s
    /// group mask, with the Player bit skipped: every playable race's template carries
    /// Player|<side> (mask 3 or 5), and so do the PvP-flagged city guards, so the side bit is the
    /// only one that can name a shipped `UI-PVP-<group>` texture. `None` for Monster/neutral
    /// templates is a state the icon callers explicitly handle (`if ( factionGroup and … )`).
    ///
    /// This is the **English** half — FactionGroup.dbc's `InternalName` — because that is what
    /// `UnitFactionGroup`'s first return must carry: every stock consumer concatenates it into a
    /// texture path (`"…\UI-PVP-"..factionGroup` at `PlayerFrame.lua:68`, `TargetFrame.lua:198`,
    /// `PartyMemberFrame.lua:125`; `"…\Battleground-"..` at `BattlefieldFrame.lua:195`), and
    /// `HonorFrame.lua:68` compares it against the literal `"Alliance"`.
    ///
    /// This field's doc used to say the binding "returns it twice … enUS ships the same word for
    /// both". That is true of enUS and of nothing else: `Name0` is localized, so on any other
    /// client the texture path would name a file that does not exist. The localized half lives in
    /// [`Self::faction_group_localized`] now.
    pub faction_group: Option<String>,
    /// The **localized** group name — FactionGroup.dbc's `Name0`, and `UnitFactionGroup`'s SECOND
    /// return, which stock uses as display text (`PlayerFrame.lua`'s PvP hit-area tooltip title).
    /// Never interchangeable with the English half above.
    pub faction_group_localized: Option<String>,
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
    /// In combat (`UnitAffectingCombat`) — `UNIT_FIELD_FLAGS` (descriptor index 46) **bit 19**,
    /// mask `0x00080000` (`0x517e10`, wow-re `bag-language-combat-action-bindings.md` §3,
    /// §5-cross-checked: `mov ecx,[eax+0xa0]; shr ecx,0x13; test cl,1`).
    ///
    /// **There is no player-specific combat latch**, stated as the explicit negative because it
    /// is precisely what a client is tempted to invent: a whole-image census of the
    /// `shr reg,0x13` + `test rl,1` idiom returns 7 hits, six of them this same field+bit — and
    /// two of those six are hardcoded *local-player* readers taking no token at all. `"player"`
    /// takes a fast path to the cached local-player GUID inside `0x515970` and then reads the
    /// identical bit. One wire flag answers this for every token, which is what this one field
    /// is; `benilla::ui_action::usable` already reads the same bit off the same descriptor.
    pub in_combat: bool,
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

/// The **CIVILIAN predicate `0x612550`** — "killing this would be a dishonorable kill": the unit's
/// PvP bit **and** the creature-query civilian flag **and** the unit is HOSTILE to the player
/// (`UnitReaction` ≤ 2) **and** the kill would be GREY/trivial ([`unit_is_grey`]). A friendly
/// civilian, or one that still cons, is not one.
///
/// **ONE home, two callers**, which is the whole reason it is a function: the unit tooltip's green
/// CIVILIAN line ([`super::tooltip_unit`], decision 0276) and `UnitPVPName`'s civilian arm
/// ([`super::pvp`] — `0x609370` leg B, which prefixes `PVP_RANK_CIVILIAN` onto a non-player's
/// name). The engine gates both on the same call; a second copy here would let the tooltip and the
/// name disagree about whether the same mob is a civilian.
///
/// **Scope of the verification.** wow-re's honor carve leaves `0x612550` itself uncarved (its §12,
/// "named by its use here"); the four terms are decision 0276's tooltip-line law, which is where
/// this composition was verified. So this is the tooltip's gate reused at the second call site the
/// carve proves shares it — not an independently byte-checked predicate.
pub fn is_civilian_kill(u: &UnitState, player_level: u32) -> bool {
    u.civilian && u.pvp && u.reaction != 0 && u.reaction <= 2 && unit_is_grey(player_level, u.level)
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
                // Folded on the way IN as well as on the way out — the map's key type is the
                // canonical lowercase token, so a feed that ever pushed `"Target"` could not create
                // a second, shadowing entry.
                Some(s) => {
                    model.units_by_lower.insert(token.to_ascii_lowercase(), s);
                }
                None => {
                    model.units_by_lower.remove(&token.to_ascii_lowercase());
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

    /// Push the player's rest snapshot — the `PLAYER_BYTES_2` byte 3 rest state, the
    /// `PLAYER_REST_STATE_EXPERIENCE` pool (raw wire value, base kill-XP units) and the
    /// `PLAYER_FLAGS_RESTING` bit — taken together so the `GetRestState`/`GetXPExhaustion`/
    /// `IsResting` trio can never read a half-updated rest picture. Player-level fields, same
    /// shape as [`Self::set_money`]; the app calls this each frame any of the three moves
    /// (decision 1082).
    pub fn set_rest_state(&mut self, state: u8, pool: u32, resting: bool) {
        let mut model = self.model_mut();
        model.rest_state = state;
        model.rest_pool = pool;
        model.resting = resting;
    }

    /// Push the two **play-time** bits of `PLAYER_FLAGS` — 12 (`PartialPlayTime`) and 13
    /// (`NoPlayTime`), decision 1746. Taken together for the same reason the rest trio is:
    /// stock `PlayerFrame_UpdatePlaytime` (PlayerFrame.lua:244) tests them as an if/elseif pair
    /// and a half-updated pair would paint the wrong one of the two icons.
    pub fn set_play_time(&mut self, partial: bool, none: bool) {
        let mut model = self.model_mut();
        model.partial_play_time = partial;
        model.no_play_time = none;
    }

    /// Push the account's **rested billing minutes**, from the `SMSG_AUTH_RESPONSE` that admitted
    /// the session — what `GetBillingTimeRested()` returns (decision 1820). Set once at login: the
    /// client parks it in a process-lifetime global and nothing else on the wire ever writes it.
    pub fn set_billing_time_rested(&mut self, minutes: u32) {
        self.model_mut().billing_time_rested = minutes;
    }

    /// Enter the **UI-load sound-suppression scope** — the reference's `0x458f50`, an `inc` on the
    /// counted depth every name-keyed `PlaySound` reads. Counted, not a flag, exactly as the
    /// client has it: nesting is legal and only the outermost exit re-enables sound.
    pub fn push_sound_suppression(&self) {
        let mut model = self.model_mut();
        model.sound_suppression = model.sound_suppression.saturating_add(1);
    }

    /// Leave it — `0x458f60`'s `dec`. Saturating on the way down too: an unbalanced pop is a bug
    /// in the caller, and silently wrapping to a permanently-muted UI would be the worse failure.
    pub fn pop_sound_suppression(&self) {
        let mut model = self.model_mut();
        model.sound_suppression = model.sound_suppression.saturating_sub(1);
    }

    /// Push whether a cinematic is playing — what `InCinematic()` answers.
    pub fn set_in_cinematic(&mut self, playing: bool) {
        self.model_mut().in_cinematic = playing;
    }

    /// Push Exhaustion.dbc — `(rest-state byte, localized name, factor)` rows for the
    /// `GetRestState`/`GetXPExhaustion` bindings (they read the table exactly as `0x48d350` /
    /// `0x48d3f0` read the client's own copy; decision 1087). Called once at startup off the
    /// patch chain; an empty push is ignored so a failed DBC read keeps the shipped-table
    /// fallback the model seeds.
    pub fn set_exhaustion_rows(&mut self, rows: Vec<(u8, String, f64)>) {
        if rows.is_empty() {
            return;
        }
        let mut model = self.model_mut();
        model.exhaustion = rows.into_iter().map(|(id, n, f)| (id, (n, f))).collect();
    }

    /// Push the player's banked combo points (`PLAYER_FIELD_BYTES` byte 1) and the unit they sit
    /// on (`PLAYER_FIELD_COMBO_TARGET`) — the pair `Player::SetComboPoints` writes together, taken
    /// together so they can never be read half-updated. Player-level PRIVATE fields, same shape as
    /// [`Self::set_money`]; the app calls this each frame either moves, including the drop back to
    /// zero (decisions 0869, 0875).
    ///
    /// **Raw wire values** — `GetComboPoints`'s class and current-target gates live in the binding,
    /// where the binary puts them.
    pub fn set_combo_points(&mut self, points: u8, target: u64) {
        let mut model = self.model_mut();
        model.combo_points = points;
        model.combo_target = target;
    }

    /// Drain the selection asks queued since the last call — the app resolves each to a guid and
    /// commits it through the one SetSelection tail. See [`SelectionRequest`] for why the three
    /// verbs share one ordered queue.
    pub fn take_selection_requests(&mut self) -> Vec<SelectionRequest> {
        std::mem::take(&mut self.model_mut().selection_requests)
    }

    /// Drain the `TargetNearestFriend([reverse])` presses queued since the last call — one `bool`
    /// per call, the cycle direction (`false` forward, `true` backward). Its own queue rather than
    /// a [`SelectionRequest`] variant because it names no unit: the reference runs it through the
    /// TAB cycler `0x493f60(reverse, mode 2)` and straight into `SetSelection`, never through the
    /// select-if-resolves helper the other three share.
    pub fn take_target_nearest_friend_requests(&mut self) -> Vec<bool> {
        std::mem::take(&mut self.model_mut().target_nearest_friend_requests)
    }

    /// Drain the `(name, exactMatch)` pairs `TargetByName` queued since the last call — the app
    /// runs the shared by-name resolver (decision 0886) and commits the selection.
    pub fn take_target_by_name_requests(&mut self) -> Vec<(String, bool)> {
        std::mem::take(&mut self.model_mut().target_by_name_requests)
    }

    /// Drain the `ClearTarget()` trigger: `true` if it fired with a live target since the last
    /// call — the ESC chain's last leg; the app commits the deselect (SetSelection guid 0).
    pub fn take_target_clear(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().target_clear)
    }

    /// Drain the unit tokens `DropItemOnUnit` queued since the last call — the app runs every gate
    /// and, on the pet leg, casts the learned Feed Pet spell at the held item (`0x48d960`).
    pub fn take_drop_item_on_unit(&mut self) -> Vec<String> {
        std::mem::take(&mut self.model_mut().drop_item_on_unit)
    }
}

/// Pick the interesting token from a directional two-unit call (`UnitIsEnemy(a, b)`): whichever
/// arg isn't `"player"` — our snapshot stores the relationship on the non-player unit (target),
/// and the ref calls both argument orders.
///
/// **The `"player"` test folds case**, like every other token comparison here and like the client's
/// own resolver. It is the kind of site the map's fold does not cover on its own: an addon passing
/// `UnitIsEnemy("Player", "target")` would otherwise be read as naming a non-player first arg, and
/// this would answer about the wrong unit — a wrong ANSWER rather than a missing one, which is the
/// worse failure of the two.
fn pick_unit_token(a: &Option<String>, b: &Option<String>) -> Option<String> {
    match (a, b) {
        (Some(x), _) if !x.eq_ignore_ascii_case("player") => Some(x.clone()),
        (_, Some(y)) => Some(y.clone()),
        (x, _) => x.clone(),
    }
}

/// The token prefixes 1.12's resolver `0x515970` recognises, in its own order.
///
/// The order is the binary's and is preserved even though our recogniser is order-insensitive:
/// `partypet` must be tested before `party` and `raidpet` before `raid` for a *resolver* to pick the
/// right unit, and the day this grows from a boolean into a resolver, the list is already right.
///
/// **These are PREFIX tests.** The compare length is the literal's, and `_strnicmp` stops at either
/// string's NUL, so `"playerfoo"` matches `player` — which is why it is a quiet nil rather than a
/// raise. `npc` is the sole FULL-STRING compare in the resolver, so `"npctarget"` matches nothing
/// and raises; it is deliberately absent from this list and handled separately below.
const UNIT_TOKEN_PREFIXES: [&str; 8] = [
    "player",
    "pet",
    "target",
    "partypet",
    "party",
    "raidpet",
    "raid",
    "mouseover",
];

/// Does 1.12's resolver RECOGNISE this token — whether or not it names a live unit?
///
/// This is the whole raise/nil distinction, so it is worth being exact about what it is not: it does
/// not ask whether a unit exists. `"party5"` on a solo character, `"pet"` with no pet, and
/// `"playerfoo"` are all *recognised* and answer nil; only a token the resolver's nine compares
/// never match raises `Unknown unit name`.
pub(crate) fn token_recognised(token: &str) -> bool {
    // `npc` full-string, everything else a prefix — and folded, like every compare in the resolver
    // (`SStrCmpI` -> `_strnicmp`, ASCII only; 1247).
    token.eq_ignore_ascii_case("npc")
        || UNIT_TOKEN_PREFIXES
            .iter()
            // BYTES, not a string slice. `token[..p.len()]` panics when the token is multibyte
            // UTF-8 and the prefix length lands mid-character ("byte index N is not a char
            // boundary") — an addon passing a non-ASCII token would take the client down. The
            // client's own compare is `_strnicmp` over bytes with an ASCII-only fold, so byte
            // comparison is both the safe form and the faithful one.
            .any(|p| {
                token.len() >= p.len()
                    && token.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes())
            })
}

/// The gate every `Unit*` binding puts an addon-supplied token through.
///
/// **An unrecognised token RAISES and does not return** — `0x515970` falls off the end of its nine
/// compares into `luaL_error(L, "Unknown unit name: %s")`, which longjmps (wow-re
/// `system/ui/scratch/unit-token-grammar.md`). Ours answered nil for everything unknown, which is
/// 1203's shape pointed the other way: a failure the client reports, silently swallowed.
///
/// The split is THREE-way, not two, and the two quiet legs are as carved:
///   * **absent argument** — quiet nil *here*, because this helper is only the resolver's half.
///     Whether a nil ever reaches it is the BINDING's question, and it is settled per binding:
///     wow-re's census of all 83 entries at `0x850438` found 53 that gate the token position with
///     `lua_isstring` and raise `Usage:`, against 13 unit-token bindings with no gate at all
///     (decision 1834). The gated ones call `binding_abi::string_arg` before they get here, so a
///     nil never arrives; the quiet 13 pass it straight through. This comment used to say the
///     gates were "NOT uniform … only those two poles are verified" and decline to guess, which
///     was the right call at the time — the table now exists, so it is applied rather than feared;
///   * **`""`** — quiet nil;
///   * a **recognised** token naming nothing (`"party5"` solo, `"playerfoo"`) — quiet nil.
pub(crate) fn check_unit_token(token: &Option<String>) -> mlua::Result<()> {
    match token {
        Some(t) if !t.is_empty() && !token_recognised(t) => {
            Err(mlua::Error::runtime(format!("Unknown unit name: {t}")))
        }
        _ => Ok(()),
    }
}

/// Read a unit token's snapshot under a short model borrow, mapping it through `f`; `default` when the
/// token is absent (the "unit doesn't exist" path).
///
/// Raises for an unrecognised token ([`check_unit_token`]) before it looks anything up.
fn with_unit<T>(
    lua: &Lua,
    token: &Option<String>,
    default: T,
    f: impl FnOnce(&UnitState) -> T,
) -> mlua::Result<T> {
    check_unit_token(token)?;
    let model = lua.app_data_ref::<Model>().expect("model app_data");
    Ok(match token.as_ref().and_then(|t| model.unit(t)) {
        Some(u) => f(u),
        None => default,
    })
}

/// The `Unit*`/`GetQuestGreenRange` Lua binding registrations — split from this module's
/// state+laws half purely for size (same seam as the other script modules' install fns).
mod bindings;
#[cfg(test)]
mod tests;

pub(super) use bindings::install;
