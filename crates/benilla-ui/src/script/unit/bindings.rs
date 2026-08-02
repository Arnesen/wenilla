//! The `Unit*` global registrations (see the parent module's doc for the seam and the return
//! shapes): every binding reads the per-token [`UnitState`](super::UnitState) snapshot store
//! through the parent's `with_unit`/`pick_unit_token` helpers.

use mlua::{Lua, Value};

use super::super::Model;
use super::{
    classification_word, grey_band, level_reads_unknown, pick_unit_token, power_token, with_unit,
};

/// The two class ids `GetComboPoints 0x51a190` accepts — the literals `4` and `0xb` it compares
/// the class byte `[[player+0x110]+0x79]` against. That byte is `UNIT_FIELD_BYTES_0` byte 1, the
/// same value [`PlayerReqState::class_id`](super::super::PlayerReqState) carries; `UnitClass`'s own
/// binding (`0x518350`) reads it through the identical `[obj+0x110]+0x79` chain, which is what
/// identifies it. The *names* are the conventional vanilla ids — the client's class table is
/// heap-built, so nothing in the file maps 4→Rogue by itself (decision 0875).
const CLASS_ROGUE: u32 = 4;
const CLASS_DRUID: u32 = 11;

/// Register the `Unit*` globals reading the per-token snapshot store (the same style/place the
/// object model and stdlib register their globals — bare globals on `_G`, matching the live API
/// surface).
pub(in crate::script) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    g.set(
        "UnitExists",
        lua.create_function(|lua, token: Option<String>| {
            Ok(with_unit(lua, &token, false, |u| u.exists))
        })?,
    )?;

    g.set(
        "UnitName",
        lua.create_function(|lua, token: Option<String>| {
            let name = with_unit(lua, &token, None, |u| u.name.clone());
            match name {
                Some(n) => Ok(Value::String(lua.create_string(&n)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    g.set(
        "UnitHealth",
        lua.create_function(|lua, token: Option<String>| {
            Ok(with_unit(lua, &token, 0i64, |u| i64::from(u.health)))
        })?,
    )?;

    g.set(
        "UnitHealthMax",
        lua.create_function(|lua, token: Option<String>| {
            Ok(with_unit(lua, &token, 0i64, |u| i64::from(u.max_health)))
        })?,
    )?;

    // UnitLevel (`0x517fc0`, §5-VERIFIED 2026-07-17): the raw UNIT_FIELD_LEVEL — a raw ≤ 0
    // VERBATIM (never −1) — or **−1** iff world-boss rank 3 (unconditional) / hostile
    // (reaction ≤ 1 internal) AND ≥ 10 levels above the player (inclusive). The FrameXML
    // target frame branches its skull on `<= 0` (`TargetFrame_CheckLevel`), so a level-0
    // (unstreamed) unit skulls through the verbatim 0, exactly as the reference does. Not
    // carried: the dormant attackable-decay override (`max(1, raw − round(min(b,100)·0.05))`,
    // `b` INFERRED and 0 in normal play — it can never drive the value ≤ 0 anyway).
    g.set(
        "UnitLevel",
        lua.create_function(|lua, token: Option<String>| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(u) = token.as_ref().and_then(|t| model.units.get(t)) else {
                return Ok(0i64);
            };
            Ok(if u.level == 0 {
                0
            } else if level_reads_unknown(u, model.player_req.level) {
                -1
            } else {
                i64::from(u.level)
            })
        })?,
    )?;

    // UnitIsCorpse (`0x5161c0`, §5-VERIFIED 2026-07-17) → 1/nil: a pure OBJECT-TYPE check —
    // the token resolves to a live TYPEID_CORPSE world object (a released player's remains).
    // NO health test: a dead mob or dead player is NOT a corpse (the ref target frame shows a
    // dead mob's level number, not the skull). Reads [`UnitState::corpse_object`], which no
    // feed sets yet — corpse objects aren't selectable in benilla — so this returns nil today,
    // faithfully.
    g.set(
        "UnitIsCorpse",
        lua.create_function(|lua, token: Option<String>| {
            Ok(if with_unit(lua, &token, false, |u| u.corpse_object) {
                Value::Integer(1)
            } else {
                Value::Nil
            })
        })?,
    )?;

    // UnitCanAttack (`0x516c50`, §5-VERIFIED 2026-07-17) → 1/nil: pure delegation to the
    // `CanAttack 0x606980` predicate (decision 0172), read from the non-player token's app-fed
    // snapshot ([`UnitState::can_attack`]). Directional in the live API; our snapshot carries
    // the player→unit direction, the only order the shipped FrameXML calls
    // (`UnitCanAttack("player", "target")`).
    g.set(
        "UnitCanAttack",
        lua.create_function(|lua, (a, b): (Option<String>, Option<String>)| {
            let token = pick_unit_token(&a, &b);
            Ok(if with_unit(lua, &token, false, |u| u.can_attack) {
                Value::Integer(1)
            } else {
                Value::Nil
            })
        })?,
    )?;

    // GetQuestGreenRange (`0x4e17d0`, §5-VERIFIED 2026-07-17) — the green→grey boundary the
    // FrameXML `GetDifficultyColor` buckets by (ref QuestLogFrame.lua l.593):
    // `GRAYBAND[min(playerLevel/5, 19)]` off the binary's `0x8076c0` table, byte-identical to
    // the `0x81dda8`/`0x80ae98` twins [`grey_band`] transcribes. No args; 0 with no player.
    g.set(
        "GetQuestGreenRange",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(grey_band(model.player_req.level)))
        })?,
    )?;

    g.set(
        "UnitIsDead",
        lua.create_function(|lua, token: Option<String>| {
            Ok(with_unit(lua, &token, false, |u| u.dead))
        })?,
    )?;

    // The other two of the client's death trio (decision 0308 §1): a released ghost has health 1,
    // so IsDead is false for it and the popup flow branches on all three.
    g.set(
        "UnitIsGhost",
        lua.create_function(|lua, token: Option<String>| {
            Ok(with_unit(lua, &token, false, |u| u.ghost))
        })?,
    )?;
    g.set(
        "UnitIsDeadOrGhost",
        lua.create_function(|lua, token: Option<String>| {
            Ok(with_unit(lua, &token, false, |u| u.dead || u.ghost))
        })?,
    )?;

    // UnitReaction(unit, other) → 1..8 (hated..exalted) or nil. The live API is directional (unit's
    // reaction toward `other`); our feed only resolves it for the "target" token toward the player,
    // which is the sole caller (`TargetFrame_CheckFaction`), so the `other` arg is accepted and
    // unused. `0` (unknown / not yet streamed) reports as nil, the API's "can't tell" — the target
    // frame paints its name plate blue then, exactly like the reference.
    g.set(
        "UnitReaction",
        lua.create_function(|lua, (token, _other): (Option<String>, Option<String>)| {
            let r = with_unit(lua, &token, 0u8, |u| u.reaction);
            Ok(if r == 0 {
                Value::Nil
            } else {
                Value::Integer(i64::from(r))
            })
        })?,
    )?;

    // UnitIsEnemy / UnitIsFriend — the reaction-thresholded pair (the ref target-select sound
    // branch, TargetFrame_OnShow). v1 derives both from the same snapshot as `UnitReaction`:
    // enemy = reaction ≤ 2 (hated/hostile), friend = reaction ≥ 5 (friendly+); the live API's
    // extra PvP inputs (duels, flagged players, sanctuaries) are deferred with the rest of the
    // PvP wire. The pair is directional in the live API but our snapshot only carries the
    // target↔player reaction, so the binding reads whichever arg isn't "player" (the ref calls
    // both orders: UnitIsEnemy("target","player"), UnitIsFriend("player","target")). Unknown
    // reaction (0) → nil for both, the API's "can't tell". `1`/nil returns, era-style.
    g.set(
        "UnitIsEnemy",
        lua.create_function(|lua, (a, b): (Option<String>, Option<String>)| {
            let token = pick_unit_token(&a, &b);
            let r = with_unit(lua, &token, 0u8, |u| u.reaction);
            Ok(if (1..=2).contains(&r) {
                Value::Integer(1)
            } else {
                Value::Nil
            })
        })?,
    )?;
    g.set(
        "UnitIsFriend",
        lua.create_function(|lua, (a, b): (Option<String>, Option<String>)| {
            let token = pick_unit_token(&a, &b);
            let r = with_unit(lua, &token, 0u8, |u| u.reaction);
            Ok(if r >= 5 {
                Value::Integer(1)
            } else {
                Value::Nil
            })
        })?,
    )?;

    // UnitIsPlayer(unit) → 1 if the unit is a player character, else nil. Reads the snapshot's
    // guid-family flag (the same one the unit tooltip's "(Player)" line keys on). The target frame's
    // faction tint branches on it (`TargetFrame_CheckFaction`): a player-controlled unit takes the
    // red/blue player legs, an NPC the reaction swatch. (The live gate is `UnitPlayerControlled`,
    // which also covers pets/charmed creatures; we resolve only the player half of it — the extra
    // reach needs a player-controlled flag we don't carry, and a player's own alt is the case here.)
    g.set(
        "UnitIsPlayer",
        lua.create_function(|lua, token: Option<String>| {
            Ok(if with_unit(lua, &token, false, |u| u.is_player) {
                Value::Integer(1)
            } else {
                Value::Nil
            })
        })?,
    )?;

    // The identity predicates (decision 0434 §5 — the unit popup's menu pick + gating). Same-token
    // is trivially the same unit; otherwise both snapshots must carry a real (nonzero) guid.
    g.set(
        "UnitIsUnit",
        lua.create_function(|lua, (a, b): (Option<String>, Option<String>)| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let (Some(a), Some(b)) = (a, b) else {
                return Ok(Value::Nil);
            };
            let (Some(ua), Some(ub)) = (model.units.get(&a), model.units.get(&b)) else {
                return Ok(Value::Nil);
            };
            if !ua.exists || !ub.exists {
                return Ok(Value::Nil);
            }
            if a == b || (ua.guid != 0 && ua.guid == ub.guid) {
                Ok(Value::Integer(1))
            } else {
                Ok(Value::Nil)
            }
        })?,
    )?;

    // UnitInParty(unit) → 1 when the unit is the player-in-a-group or one of the party1..4
    // members (a party token directly, or any token whose guid matches the roster's). Raid
    // membership waits for the raid arc (the party.rs module doc's stated v1 gap).
    g.set(
        "UnitInParty",
        lua.create_function(|lua, token: Option<String>| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(t) = token else {
                return Ok(Value::Nil);
            };
            let Some(u) = model.units.get(&t) else {
                return Ok(Value::Nil);
            };
            let grouped = !model.party.members.is_empty();
            if !u.exists || !grouped {
                return Ok(Value::Nil);
            }
            let hit = (t.starts_with("party") && !t.starts_with("partypet"))
                || t == "player"
                || (u.guid != 0
                    && (model.units.get("player").is_some_and(|p| p.guid == u.guid)
                        || model
                            .party
                            .members
                            .iter()
                            .any(|m| m.guid != 0 && m.guid == u.guid)));
            Ok(if hit { Value::Integer(1) } else { Value::Nil })
        })?,
    )?;

    // UnitCanCooperate(a, b) → 1 for a friendly PLAYER unit. DEVIATION, stated: the client
    // resolves faction-template cooperation masks; this reads the snapshot's is_player +
    // UnitIsFriend's reaction>=5 — the same verdict for every case the popup gates on (invite/
    // whisper a same-faction player), without the faction machinery the engine doesn't carry.
    g.set(
        "UnitCanCooperate",
        lua.create_function(|lua, (a, b): (Option<String>, Option<String>)| {
            let token = pick_unit_token(&a, &b);
            let ok = with_unit(lua, &token, false, |u| {
                u.exists && u.is_player && u.reaction >= 5
            });
            Ok(if ok { Value::Integer(1) } else { Value::Nil })
        })?,
    )?;

    // GetRaidTargetIndex(unit) → the mark slot 1..8, or nil unmarked (decision 0434 §6's board,
    // fed per token).
    g.set(
        "GetRaidTargetIndex",
        lua.create_function(|lua, token: Option<String>| {
            let idx = with_unit(lua, &token, 0u8, |u| u.raid_target);
            Ok(if idx > 0 {
                Value::Integer(i64::from(idx))
            } else {
                Value::Nil
            })
        })?,
    )?;

    // The party-frame status predicates (decision 0434 §2/§3): connection, AFK/DND, and the two PvP
    // flags. All 1/nil, the live API's own shape for these — unlike UnitIsDead/UnitIsGhost above (a
    // stated v1 shortcut, module doc), these are new and follow the era shape from the start.
    g.set(
        "UnitIsConnected",
        lua.create_function(|lua, token: Option<String>| {
            Ok(if with_unit(lua, &token, false, |u| u.is_connected) {
                Value::Integer(1)
            } else {
                Value::Nil
            })
        })?,
    )?;
    g.set(
        "UnitIsAFK",
        lua.create_function(|lua, token: Option<String>| {
            Ok(if with_unit(lua, &token, false, |u| u.is_afk) {
                Value::Integer(1)
            } else {
                Value::Nil
            })
        })?,
    )?;
    g.set(
        "UnitIsDND",
        lua.create_function(|lua, token: Option<String>| {
            Ok(if with_unit(lua, &token, false, |u| u.is_dnd) {
                Value::Integer(1)
            } else {
                Value::Nil
            })
        })?,
    )?;
    // UnitIsPVP reads the same `pvp` field the unit tooltip's "PvP" line already does (one flag,
    // two callers) — see the field doc.
    g.set(
        "UnitIsPVP",
        lua.create_function(|lua, token: Option<String>| {
            Ok(if with_unit(lua, &token, false, |u| u.pvp) {
                Value::Integer(1)
            } else {
                Value::Nil
            })
        })?,
    )?;
    g.set(
        "UnitIsPVPFreeForAll",
        lua.create_function(|lua, token: Option<String>| {
            Ok(if with_unit(lua, &token, false, |u| u.is_pvp_ffa) {
                Value::Integer(1)
            } else {
                Value::Nil
            })
        })?,
    )?;
    // UnitFactionGroup(unit) → (englishGroup, localizedName), the pair the PvP icon law reads:
    // the first names the `UI-PVP-<group>` texture, the second titles the player frame's hit-area
    // tooltip. nil, nil for a unit with no side (Monster/neutral templates) or no snapshot — the
    // ref's icon branches gate on exactly that (decision 0646 §1/§3).
    g.set(
        "UnitFactionGroup",
        lua.create_function(|lua, token: Option<String>| {
            match with_unit(lua, &token, None, |u| u.faction_group.clone()) {
                Some(group) => {
                    let s = Value::String(lua.create_string(&group)?);
                    Ok((s.clone(), s))
                }
                None => Ok((Value::Nil, Value::Nil)),
            }
        })?,
    )?;

    // UnitRace(unit) → (localized, raceFile) or nil, nil; UnitClass(unit) → (localized,
    // classFileName) — the paper doll's "Level %d %s %s" line + the CLASSFILENAME-keyed
    // stat-tooltip lookups (decision 0208 §3). Unknown (feed not landed / a raceless creature)
    // reports nil, nil — the live API's shape for an absent unit.
    g.set(
        "UnitRace",
        lua.create_function(|lua, token: Option<String>| {
            let pair = with_unit(lua, &token, None, |u| {
                u.race.clone().zip(u.race_file.clone())
            });
            match pair {
                Some((loc, file)) => Ok((
                    Value::String(lua.create_string(&loc)?),
                    Value::String(lua.create_string(&file)?),
                )),
                None => Ok((Value::Nil, Value::Nil)),
            }
        })?,
    )?;
    g.set(
        "UnitClass",
        lua.create_function(|lua, token: Option<String>| {
            let pair = with_unit(lua, &token, None, |u| {
                u.class.clone().zip(u.class_file.clone())
            });
            match pair {
                Some((loc, file)) => Ok((
                    Value::String(lua.create_string(&loc)?),
                    Value::String(lua.create_string(&file)?),
                )),
                None => Ok((Value::Nil, Value::Nil)),
            }
        })?,
    )?;
    // UnitSex(unit) → 2 male, 3 female (1 = neuter — no 1.12 unit feed produces it); nil when the
    // unit is absent or the sex hasn't streamed (`0`), the API's "can't tell".
    g.set(
        "UnitSex",
        lua.create_function(|lua, token: Option<String>| {
            let sex = with_unit(lua, &token, 0u8, |u| u.sex);
            Ok(if sex == 0 {
                Value::Nil
            } else {
                Value::Integer(i64::from(sex))
            })
        })?,
    )?;

    // UnitClassification(unit) → "normal" | "elite" | "rareelite" | "worldboss" | "rare" (decision
    // 0782, byte-verified `0x516d90`): a plain table index by the gated rank, and it answers a
    // STRING for every input — never nil. An absent snapshot deliberately reports "normal" rather
    // than nil because the binary does: its unresolved-token path loads table index 0 and pushes
    // that, so a frame reading it gets the plain border art instead of a nil comparison.
    g.set(
        "UnitClassification",
        lua.create_function(|lua, token: Option<String>| {
            let rank = with_unit(lua, &token, 0u32, |u| u.rank);
            Ok(classification_word(rank).to_string())
        })?,
    )?;

    // UnitPowerType(unit) → (index, token): (0, "MANA"), (1, "RAGE"), … (the live API also returns
    // alt-power color components; addons that read those handle nil).
    g.set(
        "UnitPowerType",
        lua.create_function(|lua, token: Option<String>| {
            let ty = with_unit(lua, &token, 0u8, |u| u.power_type);
            Ok((i64::from(ty), power_token(ty).to_string()))
        })?,
    )?;

    // UnitPower/UnitPowerMax(unit [, powerType]) — the snapshot carries the *active* power only
    // (the app feeds `POWER<active>`), so an explicit `powerType` argument serves the active type's
    // value and `0` for any other: stated v1 shape, not hidden (a druid's mana-in-bear-form needs
    // the full 5-slot feed later).
    g.set(
        "UnitPower",
        lua.create_function(|lua, (token, ty): (Option<String>, Option<i64>)| {
            Ok(with_unit(lua, &token, 0i64, |u| match ty {
                Some(t) if t != i64::from(u.power_type) => 0,
                _ => i64::from(u.power),
            }))
        })?,
    )?;
    g.set(
        "UnitPowerMax",
        lua.create_function(|lua, (token, ty): (Option<String>, Option<i64>)| {
            Ok(with_unit(lua, &token, 0i64, |u| match ty {
                Some(t) if t != i64::from(u.power_type) => 0,
                _ => i64::from(u.max_power),
            }))
        })?,
    )?;

    // UnitXP/UnitXPMax(unit) → the player's XP within the level / the level's requirement. Player-
    // level values (PLAYER_XP is PRIVATE, only our own avatar's), but the live API is unit-tokened:
    // it returns the values only for the "player" token and 0 for any other unit — faithfully, no
    // creature/other player exposes XP. `0` until the app's feed lands (SetMinMaxValues clamps).
    let is_player = |token: &Option<String>| token.as_deref() == Some("player");
    g.set(
        "UnitXP",
        lua.create_function(move |lua, token: Option<String>| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(if is_player(&token) {
                i64::from(model.player_xp)
            } else {
                0
            })
        })?,
    )?;
    g.set(
        "UnitXPMax",
        lua.create_function(move |lua, token: Option<String>| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(if is_player(&token) {
                i64::from(model.player_next_level_xp)
            } else {
                0
            })
        })?,
    )?;

    // GetMoney() → the player's purse in copper (a player-level global, not a unit token). The coin
    // display + the merchant window's money line read it; `0` until the app's coinage feed lands.
    g.set(
        "GetMoney",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.money as i64)
        })?,
    )?;

    // GetComboPoints() → the player's banked combo points, 0..5 (a player-level global, not a unit
    // token — the 1.12 binding takes no arguments). `ComboFrame` shows/hides on it, and the
    // combat-text COMBO_POINTS arm reads it.
    //
    // TWO gates before the byte, transcribed from `0x51a190` (§5 byte-read, decision 0875) — the
    // reference `ComboFrame.lua` carries no class check at all, so BOTH of them live here or
    // nowhere:
    //
    //   51a1c3  mov eax,[esi+0x110]; mov al,[eax+0x79]   ; the class byte
    //   51a1cc  cmp al,4 / cmp al,0xb → jne push 0.0     ; ROGUE or DRUID, nothing else
    //   51a1f3  edx=[esi+0xe68]; eax=[edx+0x838] …0x83c  ; PLAYER_FIELD_COMBO_TARGET
    //   51a205  cmp against [0xb4e2d8]/[0xb4e2dc]        ; == the CURRENT target, or push 0.0
    //   51a234  mov al,[ecx+0x1029]                      ; only now, PLAYER_FIELD_BYTES byte 1
    //
    // The class gate is why a *warrior* never sees a dot even though the server really does bank a
    // point for them on a victim's dodge: that byte reaches the usable walk's leg 5 (which has no
    // class test, and is what greys Overpower — 0869) but stops here, before any UI can see it.
    // The target gate is why combo points read as "per target": re-targeting empties the dots
    // without the count moving, and selecting the banked unit again refills them.
    //
    // Both comparisons are the binary's own plain equality — no null special case. With nothing
    // banked and no target both GUIDs are 0, which passes the target gate and reads a byte that is
    // 0 anyway; the server writes and clears the pair together.
    g.set(
        "GetComboPoints",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let class = model.player_req.class_id;
            if class != CLASS_ROGUE && class != CLASS_DRUID {
                return Ok(0_i64);
            }
            let target = model.units.get("target").map_or(0, |u| u.guid);
            if model.combo_target != target {
                return Ok(0_i64);
            }
            Ok(i64::from(model.combo_points))
        })?,
    )?;

    // TargetUnit(unit) — select a unit by token (the reference's `TargetUnit` Lua shim → SetSelection;
    // the caller here is `PlayerFrame_OnClick`'s left-click branch → `TargetUnit("player")`, and the
    // TARGETSELF binding). Queues the raw token; the app resolves it to a streamed entity and commits
    // the selection. A nil/absent unit is ignored, as is any token the app can't resolve — the real
    // client no-ops `TargetUnit` on a unit that doesn't exist.
    g.set(
        "TargetUnit",
        lua.create_function(|lua, token: Option<String>| {
            if let Some(token) = token {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                model.target_requests.push(token);
            }
            Ok(())
        })?,
    )?;

    // ClearTarget() — drop the current selection (the reference API returns 1 when it cleared,
    // nil when there was nothing to clear; `ToggleGameMenu`'s ESC chain depends on the nil to
    // fall through to opening the menu). Reads the same per-token store `UnitExists("target")`
    // answers from; the app commits the actual deselect (SetSelection guid 0 + the engaged
    // attack-stop) from the drained trigger.
    g.set(
        "ClearTarget",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if model.units.get("target").is_some_and(|u| u.exists) {
                model.target_clear = true;
                Ok(Value::Integer(1))
            } else {
                Ok(Value::Nil)
            }
        })?,
    )?;

    Ok(())
}
