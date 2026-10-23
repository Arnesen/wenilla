//! The frame snapshot — what the page sees in `onFrame`, assembled from the same state the
//! unit frames, the nameplates and the minimap read, as plain data ([`PlainValue`]).
//!
//! Positions are **WoW coordinates** (`bevy_to_wow`: +X north, +Y west, +Z up, yards) and the
//! facing is the wire orientation in radians — the numbers a `.go xyz`, a map addon or the
//! reference's own `GetPlayerFacing` would show, not the renderer's basis. Guids are hex
//! strings: a `u64` does not survive a JavaScript number.

use bevy::platform::time::Instant;
use bevy::prelude::*;

use benilla_assets::coords::bevy_to_wow;
use benilla_protocol::events::EntityKind;
use benilla_ui::script::plain::PlainValue;
use benilla_ui::script::UnitState;
use benilla_world::world_map::CurrentMap;

use crate::area::ZoneInfo;
use crate::client_state::ClientState;
use crate::chr_classes::{ChrClassTable, ChrRaceTable};
use crate::creature_anim::select::{move_flags, MovementState};
use crate::entities::mount::MountChild;
use crate::names::NameCache;
use crate::net::{Guid, NetEntity, NetStatus, ObjectStore, Reputations, SelfGuid, SelfPlayer};
use crate::player::Player;
use crate::spell::{ActiveChannel, PendingCast};
use crate::target::{Factions, Hovered, RelUnit, Relations, Selection, UnitLookup};
use crate::ui_chat::ChatEvent;

use super::BridgeConfig;

#[path = "nearby.rs"]
mod nearby;

/// Everything the snapshot reads, as one parameter (the `UnitStores` idiom: Bevy's tuple limit
/// is sixteen, and the outbound system has its own parameters besides). Every resource is
/// `Option` for the same reason the unit feed's are — a UI-only harness runs without the net
/// stack, and a missing resource must read as "nothing there", not a validation panic.
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct BridgeReadout<'w, 's> {
    state: Res<'w, State<ClientState>>,
    time: Res<'w, Time>,
    net: Option<Res<'w, NetStatus>>,
    map: Option<Res<'w, CurrentMap>>,
    player: Option<Res<'w, Player>>,
    self_guid: Option<Res<'w, SelfGuid>>,
    self_q: Query<
        'w,
        's,
        (
            &'static ObjectStore,
            Option<&'static MovementState>,
            Has<MountChild>,
        ),
        With<SelfPlayer>,
    >,
    selection: Option<Res<'w, Selection>>,
    hovered: Option<Res<'w, Hovered>>,
    pending_cast: Option<Res<'w, PendingCast>>,
    channel: Option<Res<'w, ActiveChannel>>,
    units: Query<
        'w,
        's,
        (
            &'static Guid,
            &'static NetEntity,
            &'static Transform,
            Option<&'static ObjectStore>,
            Option<&'static MovementState>,
        ),
        Without<SelfPlayer>,
    >,
    names: Option<Res<'w, NameCache>>,
    factions: Option<Res<'w, Factions>>,
    reputations: Option<Res<'w, Reputations>>,
    group: Option<Res<'w, crate::spell::group_relation::GroupRoster>>,
    classes: Option<Res<'w, ChrClassTable>>,
    races: Option<Res<'w, ChrRaceTable>>,
    /// Click to Move's walk in flight (`self.autoMove`).
    approach: Option<Res<'w, crate::player::Approach>>,
    /// The questgiver dialog statuses the overhead `!`/`?` draw from (`questStatus`).
    quest: Option<Res<'w, crate::ui_quest::QuestGiver>>,
    /// GameObject templates, for an object row's name and lock (the hover tooltip's source).
    go_templates: Option<Res<'w, crate::go_templates::GameObjectTemplates>>,
}

/// The object manager's unit lookup for the relation functions' owner legs: the self row and the
/// unit rows this readout already queries.
impl UnitLookup for BridgeReadout<'_, '_> {
    fn unit(&self, guid: u64) -> Option<RelUnit> {
        let own = self.self_guid.as_ref().and_then(|g| g.0) == Some(guid);
        let store = if own {
            self.self_q.single().ok().map(|(store, _, _)| store)
        } else {
            self.units
                .iter()
                .find(|(g, ..)| g.0 == guid)
                .and_then(|(_, _, _, store, _)| store)
        };
        store
            .filter(|s| s.is_unit())
            .map(|store| RelUnit::of(guid, store))
    }
}

impl BridgeReadout<'_, '_> {
    pub(crate) fn state_name(&self) -> &'static str {
        match self.state.get() {
            ClientState::Login => "login",
            ClientState::CharSelect => "charselect",
            ClientState::InWorld => "inworld",
        }
    }

    pub(crate) fn connected(&self) -> bool {
        self.net.as_ref().is_some_and(|n| n.connected)
    }

    pub(crate) fn map_id(&self) -> Option<u32> {
        self.map.as_ref().map(|m| m.0)
    }

    /// The bridge's clock: seconds since the app started (`Time::elapsed`).
    pub(crate) fn now(&self) -> f64 {
        self.time.elapsed_secs_f64()
    }

    /// The whole frame. Cheap when there is nothing to see (the glue screens): the
    /// session block and nulls.
    pub(crate) fn build(
        &self,
        cfg: &BridgeConfig,
        seq: u64,
        zone: Option<&ZoneInfo>,
    ) -> PlainValue {
        let mut m: Vec<(String, PlainValue)> = vec![
            ("v".into(), PlainValue::Num(f64::from(super::VERSION))),
            ("seq".into(), PlainValue::Num(seq as f64)),
            ("t".into(), PlainValue::Num(self.now())),
            (
                "session".into(),
                PlainValue::Map(vec![
                    ("state".into(), PlainValue::Str(self.state_name().into())),
                    ("connected".into(), PlainValue::Bool(self.connected())),
                ]),
            ),
            (
                "map".into(),
                PlainValue::Map(vec![(
                    "id".into(),
                    self.map_id()
                        .map_or(PlainValue::Null, |id| PlainValue::Num(f64::from(id))),
                )]),
            ),
            (
                "zone".into(),
                zone.filter(|z| z.zone_id != 0)
                    .map_or(PlainValue::Null, zone_payload),
            ),
        ];

        let now = Instant::now();
        let chr = crate::ui_unit::UnitNames {
            races: self.races.as_deref().map(|t| &t.0),
            classes: self.classes.as_deref().map(|t| &t.0),
        };
        let self_row = self.self_q.single().ok();
        let self_store = self_row.map(|(store, _, _)| store);
        let player_pos = self.player.as_ref().map(|p| p.pos);

        // ── self ──
        let me = match (self.player.as_deref(), self_row) {
            (Some(player), Some((store, motion, mounted))) => {
                let guid = self.self_guid.as_ref().and_then(|g| g.0).unwrap_or(0);
                let name = self.peek_name(guid);
                let unit =
                    crate::ui_unit::snapshot(store, guid, name, 0, chr, self.creature_types());
                let mut u = unit_fields(guid, EntityKind::Player, &unit, motion, None);
                u.push(("pos".into(), pos_payload(player.pos)));
                u.push((
                    "facing".into(),
                    PlainValue::Num(f64::from(player.facing().rem_euclid(std::f32::consts::TAU))),
                ));
                u.push(("mounted".into(), PlainValue::Bool(mounted)));
                u.push((
                    "autoMove".into(),
                    PlainValue::Bool(self.approach.as_ref().is_some_and(|a| a.active())),
                ));
                u.push((
                    "casting".into(),
                    self.pending_cast
                        .as_ref()
                        .and_then(|p| p.current())
                        .map_or(PlainValue::Null, spell_payload),
                ));
                u.push((
                    "channeling".into(),
                    self.channel
                        .as_ref()
                        .and_then(|c| c.current(now))
                        .map_or(PlainValue::Null, spell_payload),
                ));
                PlainValue::Map(u)
            }
            _ => PlainValue::Null,
        };
        m.push(("self".into(), me));

        // ── units within the radius, nearest first ──
        let target_guid = self.selection.as_ref().and_then(|s| s.guid);
        let (candidates, target_candidate) = nearby::select(
            self.units.iter().map(|row| {
                let (guid, _, transform, _, _) = row;
                let d2 =
                    player_pos.map_or(f32::INFINITY, |p| p.distance_squared(transform.translation));
                (d2, Some(guid.0) == target_guid, row)
            }),
            cfg.radius * cfg.radius,
            cfg.max_units,
        );
        let payload = |(d2, (guid, net, transform, store, motion)): (
            f32,
            (
                &Guid,
                &NetEntity,
                &Transform,
                Option<&ObjectStore>,
                Option<&MovementState>,
            ),
        )| {
            self.unit_row(
                guid.0,
                net,
                transform,
                store,
                motion,
                self_store,
                d2.sqrt(),
                chr,
            )
        };
        let mut target = PlainValue::Null;
        let rows = candidates
            .into_iter()
            .map(|candidate| {
                let row = payload(candidate);
                let (_, (guid, ..)) = candidate;
                if Some(guid.0) == target_guid {
                    target = row.clone();
                }
                row
            })
            .collect();
        // A selected target shares its constructed payload. Targets outside the radius or
        // nearest-K result are still reported independently, including when max_units is zero.
        if target == PlainValue::Null {
            if let Some(candidate) = target_candidate {
                target = payload(candidate);
            }
        }
        m.push(("target".into(), target));
        m.push((
            "hover".into(),
            self.hovered
                .as_ref()
                .and_then(|h| h.guid)
                .map_or(PlainValue::Null, |g| {
                    PlainValue::Map(vec![("guid".into(), guid_payload(g))])
                }),
        ));
        m.push(("units".into(), PlainValue::List(rows)));
        PlainValue::Map(m)
    }

    /// The creature-type resolver's sources: the name cache's templates, no form table (the
    /// snapshot reads no `creatureType`).
    fn creature_types(&self) -> crate::creature_type::CreatureTypeSources<'_> {
        crate::creature_type::CreatureTypeSources {
            names: self.names.as_deref(),
            forms: None,
            words: None,
        }
    }

    fn peek_name(&self, guid: u64) -> Option<String> {
        self.names
            .as_ref()
            .and_then(|n| {
                n.resolve_unit(guid, None, crate::query_cache::Ask::Read)
                    .map(str::to_string)
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn unit_row(
        &self,
        guid: u64,
        net: &NetEntity,
        transform: &Transform,
        store: Option<&ObjectStore>,
        motion: Option<&MovementState>,
        self_store: Option<&ObjectStore>,
        dist: f32,
        chr: crate::ui_unit::UnitNames<'_>,
    ) -> PlainValue {
        let go = match (net.kind, store, self.go_templates.as_deref()) {
            (EntityKind::GameObject, Some(store), Some(t)) => t.of(&store.0),
            _ => None,
        };
        let name = match go {
            Some(t) if !t.name.is_empty() => Some(t.name.clone()),
            _ => self.peek_name(guid),
        };
        let mut u = match store {
            Some(store) => {
                let reaction = match (self.reputations.as_deref(), net.kind) {
                    // `Relations::reaction` returns the raw 0..7 rank, which is `UnitReaction − 1`;
                    // the `+ 1` lands it on the Lua 1..8 scale every other caller uses
                    // (`ui_unit.rs`, `ui_tooltip`) — and the one `unit_fields`' `hostile`/
                    // `friendly` classify on. The unit toward the player, as the ring asks it.
                    (Some(reps), EntityKind::Player | EntityKind::Unit) => {
                        let me = self.self_guid.as_ref().and_then(|g| g.0);
                        let rel = Relations {
                            factions: self.factions.as_deref(),
                            reputations: reps,
                            me,
                            roster: self.group.as_deref(),
                            units: self,
                        };
                        let rank = self_store.zip(me).map_or(
                            benilla_formats::Reaction::Neutral as u8,
                            |(own, me)| {
                                rel.reaction(&RelUnit::of(guid, store), &RelUnit::of(me, own))
                            },
                        );
                        rank + 1
                    }
                    _ => 0,
                };
                let unit = crate::ui_unit::snapshot(
                    store,
                    guid,
                    name,
                    reaction,
                    chr,
                    self.creature_types(),
                );
                unit_fields(guid, net.kind, &unit, motion, store.0.unit_target())
            }
            None => vec![
                ("guid".into(), guid_payload(guid)),
                ("kind".into(), PlainValue::Str(kind_name(net.kind).into())),
                (
                    "name".into(),
                    name.map_or(PlainValue::Null, PlainValue::Str),
                ),
            ],
        };
        if let Some(t) = go {
            u.push(("locked".into(), PlainValue::Bool(t.lock_id != 0)));
        }
        if let Some(store) = store {
            u.extend(object_fields(
                guid,
                net.kind,
                store,
                self.quest.as_ref().and_then(|q| q.statuses().get(&guid).copied()),
            ));
        }
        u.push(("pos".into(), pos_payload(transform.translation)));
        u.push(("dist".into(), PlainValue::Num(f64::from(dist))));
        u.push((
            "displayId".into(),
            net.display_id
                .map_or(PlainValue::Null, |d| PlainValue::Num(f64::from(d))),
        ));
        PlainValue::Map(u)
    }
}

/// `UNIT_NPC_FLAGS` bits (1.12, vmangos `UnitDefines.h`), as the `services` names a page filters on.
const NPC_SERVICES: [(u32, &str); 15] = [
    (0x1, "gossip"),
    (0x2, "questgiver"),
    (0x4, "vendor"),
    (0x8, "flightmaster"),
    (0x10, "trainer"),
    (0x20, "spirithealer"),
    (0x40, "spiritguide"),
    (0x80, "innkeeper"),
    (0x100, "banker"),
    (0x200, "petitioner"),
    (0x400, "tabarddesigner"),
    (0x800, "battlemaster"),
    (0x1000, "auctioneer"),
    (0x2000, "stablemaster"),
    (0x4000, "repair"),
];
/// `UNIT_DYNAMIC_FLAGS`: a body with loot for us, and one another player or group tapped.
const DYNFLAG_LOOTABLE: u32 = 0x1;
const DYNFLAG_TAPPED: u32 = 0x4;
/// `UNIT_FLAG_SKINNABLE` in `UNIT_FIELD_FLAGS`.
const UNIT_FLAG_SKINNABLE: u32 = 0x0400_0000;
/// `GAMEOBJECT_TYPE_ID` names, 1.12's `GameobjectTypes` (herbs and veins are chests with a lock).
const GO_TYPES: [&str; 26] = [
    "door", "button", "questgiver", "chest", "binder", "generic", "trap", "chair", "spellfocus",
    "text", "goober", "transport", "areadamage", "camera", "mapobject", "motransport",
    "duelarbiter", "fishingnode", "ritual", "mailbox", "auctionhouse", "guardpost", "spellcaster",
    "meetingstone", "flagstand", "fishinghole",
];

/// What a row adds for automation: the template entry, an NPC's services and overhead quest
/// status, a body's loot/tap/skin state, a GameObject's type. `questStatus` folds the dialog
/// statuses (see `quest_markers::marker_model`) to what the mark means: `available` (yellow `!`),
/// `reward` (yellow or blue `?`), `incomplete` (grey `?`), `unavailable` (grey `!`, too low).
fn object_fields(
    guid: u64,
    kind: EntityKind,
    store: &ObjectStore,
    quest_status: Option<u32>,
) -> Vec<(String, PlainValue)> {
    let s = &store.0;
    let mut u = vec![(
        "entry".into(),
        s.object_entry()
            .or_else(|| benilla_protocol::guid::entry(guid))
            .map_or(PlainValue::Null, |e| PlainValue::Num(f64::from(e))),
    )];
    match kind {
        EntityKind::Unit | EntityKind::Player => {
            let npc = s.unit_npc_flags();
            let dynamic = s.unit_dynamic_flags();
            u.push(("npcFlags".into(), PlainValue::Num(f64::from(npc))));
            u.push((
                "services".into(),
                PlainValue::List(
                    NPC_SERVICES
                        .iter()
                        .filter(|(bit, _)| npc & bit != 0)
                        .map(|(_, name)| PlainValue::Str((*name).into()))
                        .collect(),
                ),
            ));
            u.push(("lootable".into(), PlainValue::Bool(dynamic & DYNFLAG_LOOTABLE != 0)));
            u.push(("tapped".into(), PlainValue::Bool(dynamic & DYNFLAG_TAPPED != 0)));
            u.push((
                "skinnable".into(),
                PlainValue::Bool(s.unit_flags() & UNIT_FLAG_SKINNABLE != 0),
            ));
            let status = match quest_status {
                Some(5) => PlainValue::Str("available".into()),
                Some(4 | 6 | 7) => PlainValue::Str("reward".into()),
                Some(3) => PlainValue::Str("incomplete".into()),
                Some(1) => PlainValue::Str("unavailable".into()),
                _ => PlainValue::Null,
            };
            u.push(("questStatus".into(), status));
        }
        EntityKind::GameObject => {
            let ty = s.gameobject_type_id();
            u.push(("goType".into(), PlainValue::Num(f64::from(ty))));
            u.push((
                "goTypeName".into(),
                usize::try_from(ty)
                    .ok()
                    .and_then(|i| GO_TYPES.get(i))
                    .map_or(PlainValue::Null, |n| PlainValue::Str((*n).into())),
            ));
        }
        _ => {}
    }
    u
}

/// The unit fields every row shares with the unit frames' own snapshot.
fn unit_fields(
    guid: u64,
    kind: EntityKind,
    unit: &UnitState,
    motion: Option<&MovementState>,
    target: Option<u64>,
) -> Vec<(String, PlainValue)> {
    let flags = motion.map_or(0, |m| m.flags);
    let mut u = vec![
        ("guid".into(), guid_payload(guid)),
        ("kind".into(), PlainValue::Str(kind_name(kind).into())),
        (
            "name".into(),
            unit.name.clone().map_or(PlainValue::Null, PlainValue::Str),
        ),
        ("health".into(), PlainValue::Num(f64::from(unit.health))),
        (
            "maxHealth".into(),
            PlainValue::Num(f64::from(unit.max_health)),
        ),
        ("power".into(), PlainValue::Num(f64::from(unit.power))),
        (
            "maxPower".into(),
            PlainValue::Num(f64::from(unit.max_power)),
        ),
        (
            "powerType".into(),
            PlainValue::Num(f64::from(unit.power_type)),
        ),
        ("level".into(), PlainValue::Num(f64::from(unit.level))),
        ("dead".into(), PlainValue::Bool(unit.dead)),
        ("ghost".into(), PlainValue::Bool(unit.ghost)),
        ("inCombat".into(), PlainValue::Bool(unit.in_combat)),
        ("reaction".into(), PlainValue::Num(f64::from(unit.reaction))),
        (
            "hostile".into(),
            PlainValue::Bool((1..=3).contains(&unit.reaction)),
        ),
        ("friendly".into(), PlainValue::Bool(unit.reaction >= 5)),
        ("isPlayer".into(), PlainValue::Bool(unit.is_player)),
        ("pvp".into(), PlainValue::Bool(unit.pvp)),
        (
            "class".into(),
            unit.class.clone().map_or(PlainValue::Null, PlainValue::Str),
        ),
        (
            "race".into(),
            unit.race.clone().map_or(PlainValue::Null, PlainValue::Str),
        ),
        (
            "targetGuid".into(),
            target
                .filter(|&g| g != 0)
                .map_or(PlainValue::Null, guid_payload),
        ),
        ("moveFlags".into(), PlainValue::Num(f64::from(flags))),
        (
            "moving".into(),
            PlainValue::Bool(flags & move_flags::ANY_MOVE != 0),
        ),
        (
            "swimming".into(),
            PlainValue::Bool(flags & move_flags::SWIMMING != 0),
        ),
        (
            "falling".into(),
            PlainValue::Bool(flags & move_flags::FALLING != 0),
        ),
        (
            "speed".into(),
            PlainValue::Num(f64::from(motion.map_or(0.0, |m| m.speed))),
        ),
        (
            "standState".into(),
            PlainValue::Num(f64::from(motion.map_or(0, |m| m.stand_state))),
        ),
        (
            "stealthed".into(),
            PlainValue::Bool(motion.is_some_and(|m| m.stealthed)),
        ),
    ];
    if let Some(sub) = &unit.subtitle {
        u.push(("subtitle".into(), PlainValue::Str(sub.clone())));
    }
    u
}

fn kind_name(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Player => "player",
        EntityKind::Unit => "unit",
        EntityKind::GameObject => "go",
        EntityKind::DynamicObject => "dyn",
        EntityKind::Corpse => "corpse",
        EntityKind::Other => "other",
    }
}

pub(crate) fn guid_payload(guid: u64) -> PlainValue {
    PlainValue::Str(format!("0x{guid:x}"))
}

fn pos_payload(bevy: Vec3) -> PlainValue {
    let [x, y, z] = bevy_to_wow(bevy);
    PlainValue::List(vec![
        PlainValue::Num(f64::from(x)),
        PlainValue::Num(f64::from(y)),
        PlainValue::Num(f64::from(z)),
    ])
}

fn spell_payload(spell_id: u32) -> PlainValue {
    PlainValue::Map(vec![(
        "spellId".into(),
        PlainValue::Num(f64::from(spell_id)),
    )])
}

/// The `zone` event / snapshot block.
pub(crate) fn zone_payload(z: &ZoneInfo) -> PlainValue {
    PlainValue::Map(vec![
        ("id".into(), PlainValue::Num(f64::from(z.zone_id))),
        ("name".into(), PlainValue::Str(z.zone_text.clone())),
        ("realZone".into(), PlainValue::Str(z.real_zone_text.clone())),
        ("subzone".into(), PlainValue::Str(z.subzone_text.clone())),
        (
            "minimapText".into(),
            PlainValue::Str(z.minimap_text.clone()),
        ),
        ("indoor".into(), PlainValue::Bool(z.indoor)),
        ("pvpType".into(), PlainValue::Str(z.pvp_type.into())),
        ("pvpFaction".into(), PlainValue::Str(z.pvp_faction.clone())),
        ("arena".into(), PlainValue::Bool(z.arena)),
    ])
}

/// The `chat` event: the routed line with its event name, the reference's arg fields by name,
/// and the sender's guid — the one thing `CHAT_MSG_*` never carried.
pub(crate) fn chat_payload(event: &str, e: &ChatEvent) -> PlainValue {
    PlainValue::Map(vec![
        ("event".into(), PlainValue::Str(event.into())),
        (
            "kind".into(),
            PlainValue::Str(event.strip_prefix("CHAT_MSG_").unwrap_or(event).into()),
        ),
        ("text".into(), PlainValue::Str(e.text.clone())),
        ("sender".into(), PlainValue::Str(e.sender.clone())),
        (
            "senderGuid".into(),
            if e.sender_guid == 0 {
                PlainValue::Null
            } else {
                guid_payload(e.sender_guid)
            },
        ),
        ("language".into(), PlainValue::Str(e.language.clone())),
        ("channel".into(), PlainValue::Str(e.channel.clone())),
        (
            "channelNumber".into(),
            PlainValue::Num(f64::from(e.channel_number)),
        ),
        ("target".into(), PlainValue::Str(e.target.clone())),
        ("flag".into(), PlainValue::Str(e.flag.clone())),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_go_out_in_wow_coordinates_and_guids_as_hex() {
        // WoW (10, 20, 30) is Bevy (-20, 30, -10); the round trip is the payload's contract.
        let bevy = benilla_assets::coords::wow_to_bevy([10.0, 20.0, 30.0]);
        assert_eq!(
            pos_payload(bevy),
            PlainValue::List(vec![
                PlainValue::Num(10.0),
                PlainValue::Num(20.0),
                PlainValue::Num(30.0)
            ])
        );
        assert_eq!(
            guid_payload(0xF130_0000_0000_0001),
            PlainValue::Str("0xf130000000000001".into())
        );
    }

    #[test]
    fn a_chat_payload_names_its_kind_and_keeps_the_guid() {
        let e = ChatEvent {
            kind: Some(crate::ui_chat::ChatEventKind::Say),
            text: "hi".into(),
            sender: "Bob".into(),
            sender_guid: 0x42,
            ..Default::default()
        };
        let PlainValue::Map(m) = chat_payload("CHAT_MSG_SAY", &e) else {
            panic!()
        };
        let get = |k: &str| m.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        assert_eq!(get("kind"), Some(PlainValue::Str("SAY".into())));
        assert_eq!(get("senderGuid"), Some(PlainValue::Str("0x42".into())));
        let system = ChatEvent::text_only(crate::ui_chat::ChatEventKind::System, "x".into());
        let PlainValue::Map(m) = chat_payload("CHAT_MSG_SYSTEM", &system) else {
            panic!()
        };
        assert!(m
            .iter()
            .any(|(k, v)| k == "senderGuid" && *v == PlainValue::Null));
    }
}
