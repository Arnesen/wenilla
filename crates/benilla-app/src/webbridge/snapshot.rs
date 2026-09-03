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
use crate::char_select::ClientState;
use crate::chr_classes::ChrClassTable;
use crate::creature_anim::select::{move_flags, MovementState};
use crate::entities::mount::MountChild;
use crate::names::NameCache;
use crate::net::{Guid, NetEntity, NetStatus, ObjectStore, Reputations, SelfGuid, SelfPlayer};
use crate::player::Player;
use crate::target::{ring_reaction, Factions, Hovered, Selection};
use crate::ui_cast::{ActiveChannel, PendingCast};
use crate::ui_chat::ChatEvent;

use super::BridgeConfig;

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
    classes: Option<Res<'w, ChrClassTable>>,
}

impl BridgeReadout<'_, '_> {
    pub(crate) fn state_name(&self) -> &'static str {
        match self.state.get() {
            ClientState::Login => "login",
            ClientState::CharSelect => "charselect",
            ClientState::CharCreate => "charcreate",
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
        let chr = self.classes.as_deref().map(|t| &t.0);
        let self_row = self.self_q.single().ok();
        let self_store = self_row.map(|(store, _, _)| store);
        let player_pos = self.player.as_ref().map(|p| p.pos);

        // ── self ──
        let me = match (self.player.as_deref(), self_row) {
            (Some(player), Some((store, motion, mounted))) => {
                let guid = self.self_guid.as_ref().and_then(|g| g.0).unwrap_or(0);
                let name = self.peek_name(guid);
                let unit = crate::ui_unit::snapshot(store, name, 0, chr);
                let mut u = unit_fields(guid, EntityKind::Player, &unit, motion, None);
                u.push(("pos".into(), pos_payload(player.pos)));
                u.push((
                    "facing".into(),
                    PlainValue::Num(f64::from(player.facing().rem_euclid(std::f32::consts::TAU))),
                ));
                u.push(("mounted".into(), PlainValue::Bool(mounted)));
                u.push((
                    "casting".into(),
                    self.pending_cast
                        .as_ref()
                        .and_then(|p| p.current(now))
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
        let mut rows: Vec<(f32, PlainValue)> = Vec::new();
        let mut target = PlainValue::Null;
        let target_guid = self.selection.as_ref().and_then(|s| s.guid);
        let radius2 = cfg.radius * cfg.radius;
        for (guid, net, transform, store, motion) in self.units.iter() {
            let d2 =
                player_pos.map_or(f32::INFINITY, |p| p.distance_squared(transform.translation));
            let is_target = Some(guid.0) == target_guid;
            if d2 > radius2 && !is_target {
                continue;
            }
            let row = self.unit_row(
                guid.0,
                net,
                transform,
                store,
                motion,
                self_store,
                d2.sqrt(),
                chr,
            );
            if is_target {
                target = row.clone();
            }
            if d2 <= radius2 {
                rows.push((d2, row));
            }
        }
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        rows.truncate(cfg.max_units);
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
        m.push((
            "units".into(),
            PlainValue::List(rows.into_iter().map(|(_, r)| r).collect()),
        ));
        PlainValue::Map(m)
    }

    fn peek_name(&self, guid: u64) -> Option<String> {
        self.names
            .as_ref()
            .and_then(|n| n.peek(guid).map(str::to_string))
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
        chr: Option<&benilla_formats::ChrClasses>,
    ) -> PlainValue {
        let name = self.peek_name(guid);
        let mut u = match store {
            Some(store) => {
                let reaction = match (self.reputations.as_deref(), net.kind) {
                    // `ring_reaction` returns the raw 0..7 rank, which is `UnitReaction − 1`; the
                    // `+ 1` lands it on the Lua 1..8 scale every other caller uses (`ui_unit.rs`,
                    // `ui_tooltip`) — and the one `unit_fields`' `hostile`/`friendly` classify on.
                    (Some(reps), EntityKind::Player | EntityKind::Unit) => {
                        ring_reaction(self.factions.as_deref(), reps, Some(store), self_store) + 1
                    }
                    _ => 0,
                };
                let unit = crate::ui_unit::snapshot(store, name, reaction, chr);
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
