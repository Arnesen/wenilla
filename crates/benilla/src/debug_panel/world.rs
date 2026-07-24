//! The panel's **World** section — the `.gps` of the client: who you are, which map/zone you're
//! in (and whether the zone system calls it indoors), the exact WoW-space position + facing, and
//! the terrain stream's tile residency. Pure readout except one affordance: **copy `.go xyz`** —
//! a click puts the vmangos teleport line for the current spot on the clipboard
//! (`.go xyz x y z [mapid]`, verified against vmangos `HandleGoXYZCommand`), so "it looks wrong
//! *here*" becomes a pasteable coordinate for the headless probes (`WOW_PROBE_CHAT`, live-shot
//! runs) and the FPS-journal loop closes without hand-copying numbers.

use benilla_assets::coords::bevy_to_wow;
use benilla_formats::world_to_tile;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy_egui::egui;

use super::OVERLAY_TEXT_DIM;

/// The World section's read side, bundled (the 16-param ceiling of `debug_panel_ui`): where the
/// player is — map, zone leaf, indoor claim, tile residency — and who they are (guid + the
/// ask-once name cache the unit frames use).
#[derive(SystemParam)]
pub(super) struct WorldReadout<'w> {
    player: Option<Res<'w, crate::player::Player>>,
    map: Option<Res<'w, crate::world_map::CurrentMap>>,
    catalog: Option<Res<'w, crate::world_map::MapCatalogRes>>,
    area: Res<'w, crate::terrain_stream::CurrentArea>,
    areas: Option<Res<'w, crate::area::AreaTableRes>>,
    interior: Res<'w, crate::wmo_portal::CurrentAreaInterior>,
    streamer: Res<'w, crate::terrain_stream::TerrainStreamer>,
    self_guid: Res<'w, crate::net::SelfGuid>,
    names: ResMut<'w, crate::names::NameCache>,
    net_commands: Res<'w, crate::net::NetCommands>,
}

/// Render the section: readout lines top-down (who → map → zone → position → tiles), the copy
/// affordance last.
pub(super) fn world_section(ui: &mut egui::Ui, world: &mut WorldReadout) {
    // Who: the character name through the ask-once cache (fills a frame later, like the unit
    // frames), or a plain offline line.
    match world.self_guid.0 {
        Some(guid) => {
            let name = world
                .names
                .resolve(guid, &world.net_commands)
                .unwrap_or("…")
                .to_string();
            ui.strong(name);
            ui.label(egui::RichText::new(format!("guid {guid:#x}")).color(OVERLAY_TEXT_DIM));
        }
        None => {
            ui.label(egui::RichText::new("offline — no character").color(OVERLAY_TEXT_DIM));
        }
    }

    // Map: id + Map.dbc name (the directory is the asset path, the name is the human one).
    let map_id = world.map.as_ref().map(|m| m.0);
    if let Some(id) = map_id {
        let name = world
            .catalog
            .as_ref()
            .and_then(|c| c.0.name(id))
            .unwrap_or("?");
        ui.label(format!("map {id} · {name}"));
    }

    // Zone: the leaf area and its top zone (one line when they coincide), plus the zone-text
    // indoor claim — the same authorities the splash/minimap text reads, so this readout and
    // the player-facing text can never disagree.
    if let Some(leaf) = world.area.0 {
        let (leaf_name, zone_name) = match world.areas.as_ref() {
            Some(a) => (a.0.name(leaf), a.0.top_zone(leaf).and_then(|z| a.0.name(z))),
            None => (None, None),
        };
        let mut line = match (zone_name, leaf_name) {
            (Some(z), Some(l)) if z != l => format!("{z} — {l}"),
            (_, Some(l)) => l.to_string(),
            _ => format!("area {leaf}"),
        };
        if world.interior.0.is_some() {
            line.push_str("  ·  indoors");
        }
        ui.label(line);
        ui.label(egui::RichText::new(format!("leaf area {leaf}")).color(OVERLAY_TEXT_DIM));
    }

    // Position + facing, raw WoW coords (what the wire and every probe speak); the tile the
    // feet are on and how much of the stream window is spawned.
    let Some(player) = world.player.as_ref().filter(|p| p.active) else {
        return;
    };
    let [x, y, z] = bevy_to_wow(player.pos);
    let o = player.facing().rem_euclid(std::f32::consts::TAU);
    ui.add_space(4.0);
    ui.label(egui::RichText::new(format!("{x:.1}  {y:.1}  {z:.1}")).monospace());
    ui.label(
        egui::RichText::new(format!("facing {o:.2} rad ({:.0}°)", o.to_degrees()))
            .color(OVERLAY_TEXT_DIM),
    );
    let (tx, ty) = world_to_tile(x, y);
    let (spawned, requested) = world.streamer.residency();
    ui.label(
        egui::RichText::new(format!("tile {tx},{ty}  ·  {spawned}/{requested} spawned"))
            .color(OVERLAY_TEXT_DIM),
    );

    // The teleport line: `.go xyz x y z [mapid]` (vmangos argument order). Copied, not shown —
    // the readout above already displays every number.
    if ui.button("copy .go xyz").clicked() {
        let line = match map_id {
            Some(id) => format!(".go xyz {x:.2} {y:.2} {z:.2} {id}"),
            None => format!(".go xyz {x:.2} {y:.2} {z:.2}"),
        };
        ui.ctx().copy_text(line);
    }
}
