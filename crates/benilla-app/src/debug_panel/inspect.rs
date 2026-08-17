//! The **inspector surface** — the dev-chord `I` overlay: an "armed" pill plus a compact identity
//! card that follows the cursor over whatever [`MouseoverTarget`] picked. Split from the module
//! face for size only; the toggle, the card, and its readout lines live here unchanged.

use bevy::platform::collections::HashSet;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{egui, EguiContexts};

use super::{overlay_text, OVERLAY_FILL, OVERLAY_TEXT_DIM};
use crate::net::ObjectStore;
use crate::ui_script::{InspectMode, PointerOverUi};
use benilla_world::interact::{pick_at_cursor, PickParts, WorldObject};
use benilla_world::model_render::ModelKind;
use benilla_world::modkeys::DEV_CHORD;
use benilla_world::view::WorldCamera;

/// The **dev chord + `I`** (for *inspect*) arms/disarms the inspector — off the bare-letter plane the
/// game's own bindings own (0585), on whichever plane this OS leaves free (0867). Unmistakable as a
/// chord, so unlike the old bare `i` it needs no chat-bar/EditBox gate.
pub(super) fn toggle_inspect(keys: Res<ButtonInput<KeyCode>>, mut inspect: ResMut<InspectMode>) {
    if benilla_world::modkeys::dev_chord(&keys, KeyCode::KeyI) {
        inspect.enabled = !inspect.enabled;
    }
}

/// How long the inspector card shows its "copied to clipboard" confirmation after a left-click.
/// Shared with the journal's row-copy flash ([`super::journal`]).
pub(super) const COPY_FLASH_SECS: f32 = 1.2;

/// A per-kind accent so the card's header is glanceable (which *sort* of thing am I over?) before you
/// even read the label.
fn kind_color(kind: ModelKind) -> egui::Color32 {
    match kind {
        ModelKind::Doodad => egui::Color32::from_rgb(140, 220, 140), // green — props/trees
        ModelKind::Wmo => egui::Color32::from_rgb(150, 185, 240),    // blue — buildings
        ModelKind::Creature => egui::Color32::from_rgb(240, 205, 130), // gold — NPCs
        ModelKind::GameObject => egui::Color32::from_rgb(220, 165, 220), // violet — GameObjects
    }
}

/// The inspector's GameObject collision readout (decision 0763): does a hull exist, is it disabled
/// right now, and what stored state does the passability gate see. Named so the bundled `stores`
/// param stays readable.
type GoCollisionReadout = (
    Has<avian3d::prelude::Collider>,
    Has<avian3d::prelude::ColliderDisabled>,
    Option<&'static crate::go_anim::GoAnim>,
);

/// The inspector's **motion** readout: the server-dictated path a creature is riding
/// ([`crate::net::Spline`]) and the dead-reckoned state of a remote mover
/// ([`crate::net::RemoteMotion`]) — the two producers of "this thing is moving" that the animation
/// selector itself unifies. On the card because the identity lines alone cannot answer the one
/// question a "that creature looks wrong" report always turns on: *is it actually moving, and how
/// fast?* The AQ drakes are the case that earned it — a Brood of Nozdormu 190 yd overhead beating
/// its wings at a flat 1× while its path crawls at walk pace reads as frozen, and nothing on the
/// card said the path was the slow half.
type MotionReadout = (
    Option<&'static crate::net::Spline>,
    Option<&'static crate::net::RemoteMotion>,
);

/// The inspector's entity LIGHT readout (decision 0776): the lane this object's parts render
/// under, and which attach found the room — "two identical GameObjects a few yards apart, one lit
/// like the room and one like the street" is the report that made this a card line rather than a
/// rebuild with `WOW_INTERIOR_LOG`.
type EntityLightReadout = (
    &'static benilla_world::interior::InteriorAnchor,
    Has<benilla_world::interior::ContainmentAttach>,
);

/// Everything the identity card reads off the **net entity** under the cursor, as one named
/// [`SystemParam`] — the descriptor store and the coarse kind the line gates go by, the GameObject
/// collision readout (decision 0763), the light readout, and the two remaining inputs of the
/// **interact gate** ([`crate::target::cursor_mode::go_highlightable`]): the faction catalog and our
/// own store. A bundle because `inspect_ui` sits at Bevy's 16-param ceiling; a named struct rather
/// than the tuple it grew out of, so each member says what it is at the point of use.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct InspectStores<'w, 's> {
    stores: Query<'w, 's, &'static ObjectStore>,
    kinds: Query<'w, 's, &'static crate::net::NetEntity>,
    collision: Query<'w, 's, GoCollisionReadout>,
    lit: Query<'w, 's, EntityLightReadout>,
    motion: Query<'w, 's, MotionReadout>,
    factions: Option<Res<'w, crate::target::ring::Factions>>,
    self_store: Query<'w, 's, &'static ObjectStore, With<crate::net::SelfPlayer>>,
    /// The ask-once GO template cache — the readable head a TEXT object's line reports
    /// (decision 1105).
    go_templates: Res<'w, crate::go_templates::GameObjectTemplates>,
    /// The GameObject **animation** readout (decision 1151) — what the §243 arm is playing right
    /// now, for the card's `anim` line. Its own query rather than a `collision` member because it
    /// needs the model components: they sit on the same entity as [`crate::go_anim::GoAnim`], but
    /// a GO whose model authors no skeleton renders as a static mesh and has none of them.
    go_anims: Query<
        'w,
        's,
        (
            &'static crate::go_anim::GoAnim,
            &'static AnimationPlayer,
            &'static benilla_assets::ModelAnimations,
        ),
    >,
}

/// The inspector overlay, drawn only while armed: a weak top-centre "armed" pill (so it's obvious the
/// mode is on and how to leave it) and, whenever the cursor is over an identified object, a compact
/// identity card pinned to the cursor. No chrome, no panel — its own lightweight surface.
#[allow(clippy::too_many_arguments)]
pub(super) fn inspect_ui(
    mut contexts: EguiContexts,
    inspect: Res<InspectMode>,
    mouseover: Res<MouseoverTarget>,
    objects: Query<&WorldObject>,
    // The pickable mesh is a child of the net entity; its descriptor store (`ObjectStore`) lives on the
    // parent, so the readout hops child → parent.
    parents: Query<&ChildOf>,
    stores: InspectStores,
    guids: Query<&crate::net::Guid>,
    castings: Query<&crate::creature_anim::Casting>,
    drivers: Query<&crate::creature_anim::AnimDriver>,
    anim_data: Option<Res<crate::creature_anim::AnimData>>,
    spells: Option<Res<crate::ui_action::Spells>>,
    mut names: ResMut<crate::names::NameCache>,
    net_commands: Res<crate::net::NetCommands>,
    // Bundled into one param (Bevy's system-function arity ceiling): the copy-click button, and
    // the flag it must yield to — a left press this frame the UI already consumed as a
    // cursor-payload world drop (0216 §3) must not ALSO land as an inspector copy-click, the same
    // yield every other world left-press consumer gives it (see `PointerOverUi` above for the
    // hover-time twin).
    click_input: (
        Res<ButtonInput<MouseButton>>,
        Res<crate::ui_script::PlayerUiClickConsumed>,
    ),
    time: Res<Time>,
    mut copied_at: Local<Option<f32>>,
) -> Result {
    let (buttons, click_consumed) = (&click_input.0, &click_input.1);
    if !inspect.enabled {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;

    // Armed indicator — small + dim, so it states "inspect is on" without competing with the world.
    egui::Area::new(egui::Id::new("inspect_armed"))
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 8.0))
        .show(ctx, |ui| {
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(8, 4))
                .corner_radius(5.0)
                .fill(OVERLAY_FILL)
                .show(ui, |ui| {
                    overlay_text(ui);
                    // Spelled out, not ⌃⌘ — egui's default font stack has no glyph for U+2303 and
                    // would draw tofu.
                    ui.label(
                        egui::RichText::new(format!("inspect · {DEV_CHORD}+I to exit")).small(),
                    );
                });
        });

    // The identity card: only when hovering a picked object, pinned just off the cursor tip.
    let Some(obj) = mouseover.entity.and_then(|e| objects.get(e).ok()) else {
        return Ok(());
    };
    let Some(cursor) = ctx.pointer_latest_pos() else {
        return Ok(());
    };

    // A unit's decoded server vitals from its descriptor store (`ObjectStore`), if the picked mesh's
    // parent has them — proof the descriptor pipeline (UpdateFields → ObjectValues → ECS) reached it.
    let net_entity = mouseover
        .entity
        .and_then(|e| parents.get(e).ok())
        .map(|c| c.parent());
    let (factions, self_store) = (stores.factions.as_deref(), stores.self_store.single().ok());
    let go_templates = &*stores.go_templates;
    let (stores, kinds, collision, lit, motion, go_anims) = (
        &stores.stores,
        &stores.kinds,
        &stores.collision,
        &stores.lit,
        &stores.motion,
        &stores.go_anims,
    );
    let store = net_entity.and_then(|p| stores.get(p).ok());
    // The unit's server name through the query cache — asks on first hover, fills on a later frame
    // (the same ask-once path the unit frames use).
    let name_line = net_entity
        .and_then(|p| guids.get(p).ok())
        .and_then(|g| names.resolve(g.0, &net_commands))
        .map(str::to_string);
    // Line gates go by the entity's KIND, not field presence: a create-seeded store answers every
    // field (absent = 0, the descriptor truth), so "is the health field there" stopped meaning
    // "is this a unit".
    let kind = net_entity.and_then(|p| kinds.get(p).ok()).map(|n| n.kind);
    let is_unit = matches!(
        kind,
        Some(benilla_protocol::EntityKind::Unit | benilla_protocol::EntityKind::Player)
    );
    let is_player = kind == Some(benilla_protocol::EntityKind::Player);
    // The GameObject collision + state readout (decision 0763) — the line that closes the loop on
    // "this door is drawn open but I can't walk through it". It answers, for the object under the
    // cursor, the three facts the passability gate turns on: the client's stored `GAMEOBJECT_STATE`
    // (`go_anim::go_state`, i.e. what we believe open/closed is), whether a collision hull exists at
    // all, and whether it is currently disabled. A door reading `state 0 open · SOLID` is the bug
    // live on screen; `state 0 open · passable` says the gate ran and something else is blocking.
    let go_line = store
        .filter(|_| kind == Some(benilla_protocol::EntityKind::GameObject))
        .map(|s| {
            let (has_hull, disabled, anim) = net_entity
                .and_then(|p| collision.get(p).ok())
                .unwrap_or((false, false, None));
            let state = crate::go_anim::go_state(anim, s);
            let word = match state {
                0 => "open(ACTIVE)",
                1 => "closed(READY)",
                2 => "alt(DESTROYED)",
                _ => "?",
            };
            let solidity = match (has_hull, disabled) {
                (false, _) => "no hull",
                (true, true) => "passable",
                (true, false) => "SOLID",
            };
            let flags = s.0.gameobject_flags();
            let mut named = Vec::new();
            for (bit, name) in [
                (0x1, "IN_USE"),
                (0x2, "LOCKED"),
                (0x4, "INTERACT_COND"),
                (0x10, "NO_INTERACT"),
            ] {
                if flags & bit != 0 {
                    named.push(name);
                }
            }
            let flag_text = if named.is_empty() {
                String::new()
            } else {
                format!(" [{}]", named.join("|"))
            };
            // The **interact gate**, stated rather than inferred (wow-re cursor-system §4a): the
            // strategy vtable's `+0x14` **highlightable** slot is the single predicate behind the
            // cursor, the +64 brighten, the right-click USE and the pick priority — so a GO reading
            // `interact ✗` is saying all four are off *by design*, and one reading `interact ✓` while
            // showing no gear says the fault is downstream in the cursor naming. That distinction is
            // exactly what the type-8 anvil report cost a hand-derivation to make: an anvil hovers
            // (this card is up) and must still read `interact ✗`, because SPELL_FOCUS is one of the
            // types whose `+0x14` is a constant `xor al,al`.
            let reaction =
                crate::target::cursor_mode::go_reaction(factions, s.0.gameobject_faction(), self_store);
            let go_guid = net_entity.and_then(|p| guids.get(p).ok()).map(|g| g.0);
            let overrides = crate::target::cursor_mode::GoOverrides {
                channel_owned: crate::target::cursor_mode::fishing_channel_owned(
                    self_store, go_guid,
                ),
                meeting_stone_queued: crate::target::cursor_mode::meeting_stone_queued(
                    go_guid.and_then(|g| go_templates.get(g)?.meeting_stone_area),
                ),
            };
            let interact = if crate::target::cursor_mode::go_highlightable(s, reaction, overrides) {
                "interact ✓"
            } else {
                "interact ✗"
            };
            // TEXT (type 9) only: the **readable head** (decision 1105). A book that opens no
            // window is either "no page in the template" or a fault downstream, and only this
            // line tells the two apart — the symptom is identical from the chair, and the first
            // one is the reference behaving correctly. `page —` = the template says none;
            // `page ?` = its ask-once query hasn't answered yet.
            let page_text = if s.0.gameobject_type_id() == crate::target::cursor_mode::GO_TYPE_TEXT
            {
                let go_guid = net_entity.and_then(|p| guids.get(p).ok()).map(|g| g.0);
                match go_guid
                    .and_then(|g| go_templates.get(g))
                    .map(|t| t.text_page.map_or(0, |p| p.page_id))
                {
                    None => " · page ?".to_string(),
                    Some(0) => " · page —".to_string(),
                    Some(id) => format!(" · page {id}"),
                }
            } else {
                String::new()
            };
            format!(
                "go type {} · state {state} {word} · {solidity} · flags {flags:#x}{flag_text} · {interact}{page_text}",
                s.0.gameobject_type_id()
            )
        });
    let vitals_line = store.filter(|_| is_unit).map(|s| {
        format!(
            "hp {}/{} · level {}",
            s.0.unit_health().unwrap_or(0),
            s.0.unit_max_health().unwrap_or(0),
            s.0.unit_level().unwrap_or(0)
        )
    });
    // Raw bytes (not name-mapped): creature race/class don't share the player-race enum, so a label
    // would mislead. The character model will name-map these for players specifically.
    let appearance_line = store.filter(|_| is_unit).map(|s| {
        format!(
            "race {} · class {} · sex {}",
            s.0.unit_race().unwrap_or(0),
            s.0.unit_class().unwrap_or(0),
            s.0.unit_gender().unwrap_or(0)
        )
    });
    // Player-only customization: the compositor's input, shown raw so we can
    // confirm the PLAYER_BYTES decode against an in-game character.
    let customization_line = store.filter(|_| is_player).map(|s| {
        format!(
            "skin {} · face {} · hair {}/{} · facial {}",
            s.0.player_skin().unwrap_or(0),
            s.0.player_face().unwrap_or(0),
            s.0.player_hair_style().unwrap_or(0),
            s.0.player_hair_color().unwrap_or(0),
            s.0.player_facial_hair().unwrap_or(0)
        )
    });

    // A unit mid-cast (`SMSG_SPELL_START` .. GO — the `Casting` wire seam): which spell, by id and
    // display name. The director's "what is it casting?" answered on hover; the finished cast's
    // trail lives in the journal.
    let casting_line = net_entity.and_then(|p| castings.get(p).ok()).map(|c| {
        match spells.as_ref().and_then(|s| s.catalog.get(c.spell_id)) {
            Some(d) => format!("casting {} \"{}\"", c.spell_id, d.name),
            None => format!("casting {}", c.spell_id),
        }
    });
    // **Is it moving, and how fast** ([`MotionReadout`]) — the input the `anim` line below is a
    // consequence of. A creature riding a server path reports the path's constant speed (its whole
    // length over its whole duration, the same number the gait selector reads) and how far through
    // it is, so a unit that looks static is either `still` (no path at all — the server has not
    // launched one, or the last one expired) or a path so slow it cannot be seen. A remote mover
    // reports its dead-reckoning speed instead. Units and players only: a GameObject has no mover.
    let motion_line = net_entity
        .filter(|_| is_unit)
        .and_then(|p| motion.get(p).ok())
        .map(|(spline, remote)| match (spline, remote) {
            (Some(sp), _) => format!(
                "motion {:.2} yd/s · {} path {:.0}% of {:.0}s",
                sp.speed(),
                if sp.grounded { "ground" } else { "flying" },
                100.0 * sp.elapsed_frac(),
                sp.duration.as_secs_f32(),
            ),
            (None, Some(r)) if r.speed > 0.0 => {
                format!("motion {:.2} yd/s · dead-reckoned", r.speed)
            }
            _ => "motion still".to_string(),
        });
    // An `AnimationData` id as the card names it — shared by the creature and GameObject anim
    // lines below, which read the same id space.
    let fmt = |id: u16| match anim_data.as_ref().and_then(|a| a.0.name(id)) {
        Some(name) => format!("{name}({id})"),
        None => format!("{id}"),
    };
    // The animation slots this frame (requested `AnimationData` ids — the selector's choice,
    // before missing-clip substitution): the full-body base + any masked upper-body overlay.
    let anim_line = net_entity.and_then(|p| drivers.get(p).ok()).map(|d| {
        let (base, overlay) = d.playing();
        let base = base.map(&fmt).unwrap_or_else(|| "—".into());
        // The base slot's live playback rate (decision 0903) — `speed / (moveSpeed · modelScale)`
        // on a locomotion clip, a flat 1× on everything else. Shown so a "its walk is too fast"
        // report is a hover away from a number instead of a hand-worked divisor; suppressed at
        // exactly 1× so the ordinary case doesn't carry noise.
        let rate = d.rate();
        let rate = if (rate - 1.0).abs() > 1e-3 {
            format!(" · rate {rate:.2}×")
        } else {
            String::new()
        };
        match overlay {
            Some(o) => format!("anim {base} + overlay {}{rate}", fmt(o)),
            None => format!("anim {base}{rate}"),
        }
    });
    // The same line for a **GameObject** (decision 1151), which is driven by `GoAnim` rather than
    // `AnimDriver` and so never reached the branch above: the sequence the §243 arm is playing,
    // whether it is a transient one (a transition motion / a Custom block — something the §2d
    // completion advance must end) or the state's held rest pose, and the repeat it is running
    // under. A `transition · loops` reading is the "stuck open/closing" bug, stated.
    let anim_line = anim_line.or_else(|| {
        let (id, transient, repeat) = net_entity
            .and_then(|p| go_anims.get(p).ok())
            .and_then(|(go, player, anims)| crate::go_anim::armed_anim(go, player, anims))?;
        let kind = if transient { "transition" } else { "rest" };
        let repeat = match repeat {
            bevy::animation::RepeatAnimation::Forever => " · loops".to_string(),
            bevy::animation::RepeatAnimation::Never => String::new(),
            bevy::animation::RepeatAnimation::Count(n) => format!(" · ×{n}"),
        };
        Some(format!("anim {} · {kind}{repeat}", fmt(id)))
    });
    // The light lane + the attach that found it (decision 0776). Absent until the classifier has
    // resolved the anchor once — a freshly streamed object shows no line rather than a wrong one.
    let light_line = net_entity
        .and_then(|p| lit.get(p).ok())
        .map(|(anchor, containment)| {
            format!(
                "light {} · {} attach",
                anchor.law_label(),
                if containment {
                    "containment"
                } else {
                    "down-ray"
                }
            )
        });

    // The lines shown in the card — also exactly what a left-click copies to the clipboard.
    let mut lines = vec![format!("{:?}", obj.kind), obj.label.clone()];
    if let Some(name) = &name_line {
        lines.push(format!("\"{name}\""));
    }
    if obj.id != 0 {
        lines.push(format!("id {}", obj.id));
    }
    if !obj.detail.is_empty() {
        lines.push(obj.detail.clone());
    }
    if let Some(line) = &go_line {
        lines.push(line.clone());
    }
    if let Some(line) = &vitals_line {
        lines.push(line.clone());
    }
    if let Some(line) = &appearance_line {
        lines.push(line.clone());
    }
    if let Some(line) = &customization_line {
        lines.push(line.clone());
    }
    if let Some(line) = &casting_line {
        lines.push(line.clone());
    }
    if let Some(line) = &motion_line {
        lines.push(line.clone());
    }
    if let Some(line) = &anim_line {
        lines.push(line.clone());
    }
    if let Some(line) = &light_line {
        lines.push(line.clone());
    }
    lines.push(format!("{:.1} yd away", mouseover.distance));

    // The inspector owns left-click while armed (player::control suppresses left-orbit during inspect),
    // so a press over the hovered object copies the whole card to the clipboard.
    if buttons.just_pressed(MouseButton::Left) && !click_consumed.0 {
        ctx.copy_text(lines.join("\n"));
        *copied_at = Some(time.elapsed_secs());
    }
    let just_copied = copied_at.is_some_and(|t| time.elapsed_secs() - t < COPY_FLASH_SECS);

    egui::Area::new(egui::Id::new("inspect_card"))
        .fixed_pos(cursor + egui::vec2(18.0, 18.0))
        .show(ctx, |ui| {
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(8, 6))
                .corner_radius(5.0)
                .fill(OVERLAY_FILL)
                .show(ui, |ui| {
                    overlay_text(ui);
                    ui.colored_label(kind_color(obj.kind), format!("{:?}", obj.kind));
                    ui.label(egui::RichText::new(&obj.label).monospace());
                    if obj.id != 0 {
                        ui.label(
                            egui::RichText::new(format!("id {}", obj.id)).color(OVERLAY_TEXT_DIM),
                        );
                    }
                    if !obj.detail.is_empty() {
                        ui.label(egui::RichText::new(&obj.detail).color(OVERLAY_TEXT_DIM));
                    }
                    if let Some(line) = &go_line {
                        ui.label(egui::RichText::new(line).color(OVERLAY_TEXT_DIM));
                    }
                    if let Some(line) = &vitals_line {
                        ui.label(egui::RichText::new(line).color(OVERLAY_TEXT_DIM));
                    }
                    if let Some(line) = &appearance_line {
                        ui.label(egui::RichText::new(line).color(OVERLAY_TEXT_DIM));
                    }
                    if let Some(line) = &customization_line {
                        ui.label(egui::RichText::new(line).color(OVERLAY_TEXT_DIM));
                    }
                    if let Some(line) = &casting_line {
                        ui.label(egui::RichText::new(line).color(OVERLAY_TEXT_DIM));
                    }
                    if let Some(line) = &anim_line {
                        ui.label(egui::RichText::new(line).color(OVERLAY_TEXT_DIM));
                    }
                    if let Some(line) = &light_line {
                        ui.label(egui::RichText::new(line).color(OVERLAY_TEXT_DIM));
                    }
                    ui.label(
                        egui::RichText::new(format!("{:.1} yd away", mouseover.distance))
                            .color(OVERLAY_TEXT_DIM),
                    );
                    // Copy affordance, swapped for a brief confirmation after a left-click.
                    if just_copied {
                        ui.label(
                            egui::RichText::new("copied to clipboard")
                                .small()
                                .color(egui::Color32::from_rgb(140, 220, 140)),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new("left-click to copy")
                                .small()
                                .color(OVERLAY_TEXT_DIM),
                        );
                    }
                });
        });
    Ok(())
}

/// The nearest [`WorldObject`] under the cursor this frame, or `None`. Consumers read `entity` and look
/// up its [`WorldObject`]; `point`/`distance` are the world-space hit.
#[derive(Resource, Default)]
pub struct MouseoverTarget {
    pub entity: Option<Entity>,
    pub point: Vec3,
    pub distance: f32,
}

/// Ray-cast from the cursor into the world and record the nearest [`WorldObject`] hit. Restricted to
/// entities carrying `WorldObject` (so terrain, particle billboards, and other un-identified meshes are
/// transparent to the pick), and skipped entirely unless inspection is active.
pub(super) fn update_mouseover(
    inspect: Res<InspectMode>,
    pointer_over_ui: Res<PointerOverUi>,
    mut target: ResMut<MouseoverTarget>,
    window: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    objects: Query<Entity, With<WorldObject>>,
    parts: PickParts,
) {
    if !inspect.enabled {
        if target.entity.is_some() {
            *target = MouseoverTarget::default();
        }
        return;
    }
    target.entity = None;
    // The pointer is over the dev UI (e.g. the now-overlaid debug panel), not the world — don't pick
    // behind it. This replaces the old "is the cursor in the inset world viewport?" test, which no
    // longer means anything now the panel overlays a full-screen view.
    if pointer_over_ui.0 {
        return;
    }
    let Ok((camera, cam_tf)) = camera.single() else {
        return;
    };
    let Ok(window) = window.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return; // cursor left the window, or we're in mouselook (hidden)
    };
    let identified: HashSet<Entity> = objects.iter().collect();
    if let Some((entity, point, distance)) =
        pick_at_cursor(cursor, camera, cam_tf, &identified, &parts)
    {
        target.entity = Some(entity);
        target.point = point;
        target.distance = distance;
    }
}
