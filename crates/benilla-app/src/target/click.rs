//! The click/USE router — the input half of targeting: a clean left-click selects the
//! [`super::Hovered`] unit, a clean right-click dispatches the context action (attack, NPC
//! interact, GameObject USE with the lock/refusal ladder), and the UI's `TargetUnit` requests +
//! Esc drain into the same [`super::Selection`]. Split from `mod.rs` (which keeps the state
//! resources and the plugin) along the state-vs-input seam; the systems here are registered by
//! [`super::TargetPlugin`] in the target chain after the hover picks and the cursor classifier.

use super::lock::GoLockInputs;
use super::*;

/// The **right-click cursor-payload leg** — the reference's WorldFrame click router (`0x481f60`
/// → object leg `0x492ce0` / terrain leg `0x492c90` / nothing leg `0x492d30`; decision 0571,
/// §5-cross-checked as wow-re cursor-dragdrop-payload.md §11 / decision 0574), transcribed onto
/// the camera arbiter's clean-click message (a drag/turn never routes here, exactly the ref's
/// click-not-drag gate `0x514ae0`):
///
/// - a **right-click over empty world** (terrain OR nothing): ANY payload clears silently — no
///   popup, no packet (both legs' action-4 arm: `ClearCursor(1,1)` unconditionally). This is
///   the "right-click dismisses the held item/spell" behavior.
/// - released over a **world object**: no payload change at all — `0x492ce0` clears only the
///   displayId-PREVIEW gate (`[0xb4b41c]`, an arm benilla doesn't carry) and INTERACT proceeds
///   normally in the systems that follow, item or spell still on the cursor.
///
/// The left-click legs live in the UI engine's world drop ([`benilla_ui::script`]'s
/// `world_drop_click`, routed by the app-fed pick — decisions 0218/0571/0574); that press is
/// consumed when it would drop, so no `WorldClick` fires for them. The put-down sound rides the
/// app's cursor-transition watcher (`crate::sound`), matching the ref's `ClearCursor` play.
pub(super) fn world_right_click_payload(
    mut right_clicks: MessageReader<WorldRightClick>,
    hovered: Res<Hovered>,
    hovered_object: Res<HoveredObject>,
    script: Option<NonSendMut<UiScript>>,
) {
    if right_clicks.read().last().is_none() {
        return;
    }
    let Some(mut script) = script else {
        return;
    };
    if hovered.target.is_some() || hovered_object.target.is_some() {
        return;
    }
    script.clear_cursor_payload();
}

/// On a [`WorldClick`], select the unit the **press** was over ([`PressPick`]) and inform the
/// server; a click on empty ground / a non-unit clears the target — except a click on NOTHING (sky
/// — no occlusion-ray hit) while a payload is held: the reference's nothing-leg deselect is
/// no-payload-gated (`0x492d30`'s local flag test), while the terrain leg deselects regardless of a
/// surviving spell/action payload (`0x5e03bb` — decisions 0571 + 0574). Skipped while the inspector
/// is armed (left-click is its copy affordance).
///
/// **The press pick, not the live hover** (decision 1122). A click may now arrive at the end of a
/// gesture that orbited the camera, and the live hover is empty by then — `update_hover` clears it
/// for the whole look session, as the reference suppresses its own hover during freelook. Reading
/// the live hover here would take the `_ =>` arm below and *clear* the player's target on every
/// drag. The reference picks once, on the down edge, and this consumes that same latch.
#[allow(clippy::too_many_arguments)] // one Bevy system's full input set
pub(super) fn select_on_click(
    mut clicks: MessageReader<WorldClick>,
    inspect: Res<InspectMode>,
    press: Res<PressPick>,
    mut selection: ResMut<Selection>,
    mut seam: crate::creature_anim::AttackSeam,
    self_q: Query<(&Guid, Has<Engaged>), With<SelfPlayer>>,
    payload_held: Res<crate::ui_script::CursorPayloadHeld>,
    mut greeting: MessageWriter<crate::sound::NpcGreetingRequest>,
    ground: Res<crate::ui_action::SpellTargeting>,
    click_cfg: Res<ClickConfig>,
) {
    let (hovered, occlusion) = (press.hovered, press.occlusion);
    // Drain the frame's clicks; act only if there was one and the inspector isn't holding left-click.
    let clicked = clicks.read().last().is_some();
    if !clicked || inspect.enabled {
        return;
    }
    // The ground-targeting cursor owns the click (decision 0792): the ref's terrain leg tries
    // the ground commit BEFORE its select/deselect half and skips it when the commit fires
    // (`0x492580`'s "otherwise"). The commit system runs after this one in the chain, so the
    // mode is still readable here — selection changes not at all, in range or out.
    if ground.active() {
        return;
    }
    let (self_guid, engaged) = self_q
        .single()
        .map(|(g, e)| (Some(g.0), e))
        .unwrap_or((None, false));
    match (hovered.target, hovered.guid) {
        (Some(entity), Some(guid)) => {
            // The NPC greets us on the SELECT gesture — the byte-verified trigger (wow-re
            // `npc-greeting.md`: the variation-cycling greeter `0x60c270` fires "before SetTarget",
            // i.e. on the left-click select, NOT the right-click interact — director-confirmed:
            // left-click greets and repeat left-clicks cycle, right-click does nothing). Fired on
            // EVERY select click on a unit (not gated on a selection change) so re-clicking the
            // same NPC steps the variation sequence; the sound crate holds the per-unit latch (a
            // re-click while the line still sounds is silent) and resolves non-NPCs to nothing.
            greeting.write(crate::sound::NpcGreetingRequest { npc: entity });
            // The one SetSelection law ([`scan::commit`]): dedup + selection + the engaged-switch
            // stop→select→re-swing. The cursor's Attack classification (alive + reaction ≤
            // neutral, hover-refreshed every frame) IS Attack `0x5ecb70`'s new-target validation.
            scan::commit(
                &mut selection,
                &mut seam,
                entity,
                guid,
                engaged,
                self_guid,
                press.attack,
            );
        }
        // Clicked nothing targetable → deselect (only sends the clear if we actually had a
        // target). The one exception is a payload held over NOTHING (sky): the ref's
        // nothing-leg deselect is no-payload-gated. Since 0843 ANY payload's empty-world left
        // press is consumed for the drop/dismiss (input.rs's `would_drop`), so a payload
        // normally never reaches this arm at all — the gate still holds the nothing-leg law
        // for the residual GameObject-hover case (`Object` pick, unit arm empty).
        // `deselectOnClick` (0961) gates the whole arm: "Sticky Targeting" checked = the CVar
        // at 0 = an empty-world click keeps the target (1.12's own inverted checkbox).
        _ => {
            // A **corpse** is an object hit, not empty world (decision 1723): the reference's
            // deselect lives in the terrain and nothing legs, and an object leg clears no
            // selection — so a left-click on a body must leave the target exactly where it was.
            // Without this the corpse would arrive here (its guid is in the other slot) and read
            // as "clicked nothing", dropping the player's target every time they clicked a body.
            if hovered.corpse.is_some() {
                return;
            }
            if click_cfg.deselect_on_click && (!payload_held.0 || occlusion.distance.is_finite()) {
                clear(&mut selection, &mut seam, engaged);
            }
        }
    }
}

/// On a clean right-*click* (vanilla's context action — [`WorldRightClick`], never a turn-drag):
/// select the hovered unit, then act by the same classification the cursor used (wow-re
/// cursor-system.md §6). Three branches (decision 0081):
/// - **Attack** (alive + reaction ≤ neutral): auto-draw and start melee auto-attack, exactly the
///   action-bar attack's path (decision 0073's verified attack-start: SETSHEATHED then ATTACKSWING).
/// - **Loot** (dead + `UNIT_DYNFLAG_LOOTABLE` — the state, not the Pickup cursor kind, which a
///   live vendor shares): open the corpse's loot (`CMSG_LOOT`), decision 0084.
/// - **Interact** on an in-range friendly service NPC: a vendor-only NPC opens the vendor list
///   directly (`CMSG_LIST_INVENTORY`); any other service NPC — gossip, and the out-of-scope
///   banker/trainer/innkeeper/flightmaster — opens via the universal `CMSG_GOSSIP_HELLO`, whose
///   returned menu the gossip window shows (their specialized windows are their own arcs). On the
///   send, our avatar plays EmoteTalk (id 60), which stows the weapon via the anim→sheath reconcile.
///
/// The cursor's own gate grays a service beyond `SERVICE_RANGE` (`unable`); we don't send then (no
/// auto-approach yet) — the selection still lands. Attack, by contrast, is never range-gated
/// (`unable` only grays): the server holds the swing until we're in reach, as the real client does.
/// A right-click on empty ground was just a turn — it never deselects.
// The `ui_feedback` tuple is the 16-SystemParam ceiling's overflow bundle, commented at its site.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn act_on_right_click(
    mut clicks: MessageReader<WorldRightClick>,
    hovered: Res<Hovered>,
    hovered_object: Res<HoveredObject>,
    cursor: Res<WorldCursor>,
    mut selection: ResMut<Selection>,
    mut seam: crate::creature_anim::AttackSeam,
    self_player: Query<(Entity, &Guid, Has<Engaged>), With<SelfPlayer>>,
    // The interact talk gesture. The reference's NPC-interact dispatcher calls the SAME gesture
    // entry point the chat display path does, always with code 0 — so this goes through
    // `creature_anim::gesture` rather than writing a raw AnimID, and inherits its gate chain
    // (decision 1469; before that it played unconditionally, even asleep or mid-combat).
    mut gestures: ResMut<crate::creature_anim::GestureQueue>,
    // The GameObject lock-routing inputs (decisions 0239 / 0545 / 0752) as one [`GoLockInputs`]
    // (the 16-SystemParam ceiling).
    mut go_inputs: GoLockInputs,
    player_actions: Res<crate::ui_action::PlayerActions>,
    // `[0xb700e4]`'s mirror — the skin leg's spell, already gated by the classifier (0752).
    learned: Res<crate::ui_action::LearnedAbilities>,
    // The GO leg needs the object's stored state too (the Action gate, decision 0752); the unit
    // legs ignore the second member.
    stores: Query<(&ObjectStore, Option<&crate::go_anim::GoAnim>)>,
    // One tuple param (the 16-SystemParam ceiling): the red-error keys + the reason-coded cast
    // line (the opener cast's local totem refusal, decision 0552) + the loot-target latch
    // the loot branch arms (decision 0515), and the mailbox session the mailbox branch opens
    // (decision 0544).
    ui_feedback: (
        ResMut<crate::ui_action::UiErrorKeys>,
        ResMut<crate::ui_action::CastErrors>,
        ResMut<crate::ui_loot::LootLatch>,
        ResMut<crate::ui_mail::MailOpen>,
        // The reader session the TEXT branch opens (decision 1105) — like the mailbox, a
        // client-side window with no packet behind it.
        ResMut<crate::ui_item_text::ItemTextOpen>,
    ),
) {
    let (mut ui_error_keys, mut cast_errors, mut loot_latch, mut mail, mut item_text) = ui_feedback;
    if clicks.read().last().is_none() {
        return;
    }
    // A GameObject is the nearest thing under the cursor → use it (decision 0236), and never fall
    // through to unit handling: a GO is not selectable, and a right-click on it acts on the GO or
    // does nothing. The reference's `OnUse 0x5f8660` gates on the same two predicates the cursor
    // computes (§4a/§8.7): **highlightable** first — false is a silent no-op, which for us is
    // `cursor.kind == Point` — then **usable**, whose failure toasts an error and sends nothing.
    //
    // `usable`'s two arms we model land in different places, so the routing is split accordingly
    // (decision 0752): the **lock** arm comes back out of [`resolve_go_action`] as
    // `GoAction::Refuse`, which carries §8.8's toast; the **range** arm is `cursor.unable`, which
    // suppresses the send with no toast (the reference auto-walks there instead — `0x610300`, also
    // no packet). The lock arm runs first in `0x5f3130`, and it does here too: a `Refuse` toasts
    // even when we are also out of range.
    if go_is_nearest(&hovered, &hovered_object) {
        // **The interact chain's first link** (tag `use`). "I clicked it and nothing happened" spans
        // three systems — this decision, the `CMSG_GAMEOBJ_USE` it sends, and the `SMSG_SPELL_GO`
        // the server answers with — and until this line existed only the *taken* branches said
        // anything, at `debug!`, which the hardcoded log filter keeps off. Both silent refusals are
        // traced here: a `Point` cursor (the reference's highlightable no-op) and `unable` (the
        // range gray, which suppresses the send with no toast). Pairs with the `fx` kit lines, so
        // ONE run says which link is dead instead of one round-trip per link.
        if benilla_assets::trace::enabled_for("use") {
            let ty = hovered_object
                .target
                .and_then(|e| stores.get(e).ok())
                .map_or(-1, |(s, _)| s.0.gameobject_type_id());
            benilla_assets::trace::line(
                "use",
                &format!(
                    "right-click go guid={:?} type={ty} cursor={:?} unable={}",
                    hovered_object.guid.map(|g| format!("{g:#x}")),
                    cursor.kind,
                    cursor.unable
                ),
            );
        }
        if cursor.kind != cursor_mode::CursorKind::Point {
            if let Some(guid) = hovered_object.guid {
                let go = hovered_object
                    .target
                    .and_then(|e| stores.get(e).ok())
                    .map(|(s, anim)| (s, crate::go_anim::go_state(anim, s)));
                // Mailbox (GO type 19): open the mail window client-side (decision 0544), BEFORE the
                // lock fork (a mailbox is never locked). The wow-re §5 confirms the MAILBOX use
                // handler overrides the shared use-sender to a LOCAL open — it sends NO packet (no
                // CMSG_GAMEOBJ_USE); the window's own MAIL_SHOW → CheckInbox drives the first
                // CMSG_GET_MAIL_LIST. Re-clicking just re-shows (the session is already set).
                if go.is_some_and(|(s, _)| s.0.gameobject_type_id() == cursor_mode::GO_TYPE_MAILBOX)
                {
                    if !cursor.unable {
                        debug!("right-click mailbox: open mail window {guid:#x}");
                        mail.click(guid);
                    }
                    return;
                }
                // TEXT (GO type 9): a book, plaque or sign — READ it, client-side, before the lock
                // fork (a readable is never locked). Its strategy overrides the use-slot the same
                // way the mailbox does — `0x5f58c0` calls the local page-text opener
                // `0x4e32e0(goGuid, 0)` and never the shared `CMSG_GAMEOBJ_USE` sender (decision
                // 1105; wow-re cursor-system §4's "TEXT type 9 — its own handler"). Sending USE
                // instead is what left every world book dead: vmangos' `GameObject::Use` has no
                // type-9 case at all, so the packet is answered with silence.
                //
                // `arg2 == 0` here means the reference's **toggle**: right-clicking the book whose
                // reader is already open closes it. The page id + material are NOT resolved here —
                // the reader asks the object's template for them as it paints, like the reference's
                // `vtbl+0x74` getter, so a click that beats the ask-once template query still opens.
                if go.is_some_and(|(s, _)| s.0.gameobject_type_id() == cursor_mode::GO_TYPE_TEXT) {
                    if !cursor.unable {
                        if item_text.toggle_closed(guid) {
                            debug!("right-click text gameobject: re-click closes {guid:#x}");
                        } else {
                            debug!("right-click text gameobject: read {guid:#x}");
                            item_text.open_pages(guid);
                        }
                    }
                    return;
                }
                // Branch on the lock (decisions 0239 / 0545 / 0752): a lockless GameObject is USEd;
                // a lockable one casts the opener (a known OPEN_LOCK spell, or a carried key's own
                // ON_USE) at it; an unopenable lock shows the client-local red toast — "The door is
                // locked.", "Requires Herbalism", "Requires Mining 100", "Requires <key item>" —
                // and sends nothing (the ref's validate/error block `0x5f3427..` fires
                // `DisplayError` with no packet; wow-re cursor-system.md §8.4/§8.8).
                let me_store = self_player
                    .single()
                    .ok()
                    .and_then(|(e, _, _)| stores.get(e).ok())
                    .map(|(s, _)| s);
                match resolve_go_action(
                    guid,
                    &mut go_inputs,
                    &player_actions.spells,
                    go,
                    me_store,
                    &seam.net,
                ) {
                    GoAction::Use if cursor.unable => {}
                    GoAction::OpenLock(_) | GoAction::OpenByKey { .. } if cursor.unable => {}
                    GoAction::Use => {
                        debug!("right-click gameobject use: {guid:#x}");
                        if benilla_assets::trace::enabled_for("use") {
                            benilla_assets::trace::line(
                                "use",
                                &format!("SEND CMSG_GAMEOBJ_USE guid={guid:#x}"),
                            );
                        }
                        let _ = seam.net.0.send(ClientCommand::GameObjUse { guid });
                    }
                    GoAction::OpenLock(spell_id) => {
                        // The opener cast funnels through the ref's TryCast like any other cast
                        // (§8.4: `0x5f35c0 → 0x6e5a90 → 0x6e4b60`), so the pre-send totem check
                        // `0x6e4000` gates it too (decision 0552): a pickless Mining cast
                        // refuses HERE with the local red "Requires Mining Pick" and sends
                        // nothing — vmangos would answer the sent cast with the wrong code.
                        if crate::ui_action::reagent_totem_refusal(
                            spell_id,
                            go_inputs
                                .spells
                                .as_ref()
                                .and_then(|s| s.catalog.get(spell_id)),
                            me_store,
                            &go_inputs.items,
                            &mut cast_errors,
                        ) {
                            return;
                        }
                        debug!("right-click gameobject open-lock: cast {spell_id} at {guid:#x}");
                        let _ = seam.net.0.send(ClientCommand::CastSpellGameObject {
                            spell_id,
                            go_guid: guid,
                        });
                    }
                    GoAction::OpenByKey {
                        bag_index,
                        slot,
                        spell_index,
                    } => {
                        // No reagent/totem pre-check here, unlike the skill arm above: that gate is
                        // about a Mining cast without a pick, and a key has no reagents — the ref's
                        // `0x6e4000` would pass every key trivially.
                        debug!(
                            "right-click gameobject open-by-key: use item ({bag_index},{slot}) blk {spell_index} at {guid:#x}"
                        );
                        // NOT through [`crate::ui_action::CastLadder`] yet — this arm and the
                        // `OpenLock` cast above are the last two sends outside the one ladder, and
                        // folding them in is its own slice: the system is already at the
                        // SystemParam ceiling, the resolver would have to carry the key's ON_USE
                        // *spell* as well as its block ordinal, and the binder has no GameObject
                        // arm (`CastCommit::Item::on_object` is the seam that awaits it). Stated
                        // gap, decision 0914.
                        let _ = seam.net.0.send(ClientCommand::UseItem {
                            bag_index,
                            slot,
                            spell_index,
                            target: benilla_protocol::messages::UseItemTarget::Object(guid),
                        });
                    }
                    GoAction::Refuse(err) => {
                        // `None` is a case the ref is silent on too (a key-item record miss —
                        // its ask-once query is away — or the deferred key-in-hand open).
                        debug!("right-click gameobject {guid:#x}: locked, refused ({err:?})");
                        if let Some(err) = err {
                            ui_error_keys.0.push(err);
                        }
                    }
                }
            }
        }
        return;
    }
    // ── The corpse leg: `CGCorpse_C`'s own interact slot, `0x5d6bf0` (vtable `+0x60`) ───────────
    // A corpse is not a unit and never routes through the unit block below — the reference gives
    // its class a *different* function in the same slot the unit class fills with `0x60bea0`.
    // Transcribed from the disassembly at `0x5d6bf0..0x5d6cdd`, which has exactly two legs:
    //
    //   1. `[player descriptor + UNIT_FIELD_MOUNTDISPLAYID] <= 0` (**not mounted**, `0x5d6c2a jg`)
    //      AND `0x5d6e20(corpse)` (**lootable** — `CORPSE_FIELD_DYNAMIC_FLAGS` bit 0) → a player-
    //      state check on the vtable `+0xa4` slot, which on failure raises message `0x85` and
    //      sends nothing; else `SetAutoLoot` (`0x5df460`) and then `0x5df130` — **`CMSG_LOOT`**.
    //   2. else `CORPSE_FIELD_FLAGS` bit 5 (the PvP insignia) AND the `[0xb700e8]`
    //      SKIN_PLAYER_CORPSE learn latch AND `!0x6067d0(player, corpse)` → the skin cast
    //      (`0x5f05e0`). Unreachable without that latch, and nothing in 1.12.1 sets it.
    //
    // **Neither leg touches your own body**, which carries neither flag — the function runs and
    // does nothing at all. Recovering your corpse is a separate route (decision 0308's
    // `RECOVER_CORPSE` → `CMSG_RECLAIM_CORPSE`) whose click path is out for RE and is NOT
    // guessed at here; the director reports a right-click on their own body raising the resurrect
    // prompt, so a route exists and this function is not it.
    if let (Some(entity), Some(guid)) = (hovered.corpse, hovered.corpse_guid) {
        let store = stores.get(entity).ok().map(|(s, _)| s);
        // The same one-line answer the GameObject leg gives (tag `use`): a corpse's click is three
        // silent refusals deep — no flag, the range gray, the missing latch — and every one of them
        // otherwise looks identical from the outside. One run says which.
        if benilla_assets::trace::enabled_for("use") {
            benilla_assets::trace::line(
                "use",
                &format!(
                    "right-click corpse guid={guid:#x} bones={} lootable={} insignia={} cursor={:?} unable={}",
                    store.is_some_and(|s| s.0.corpse_is_bones()),
                    store.is_some_and(|s| s.0.corpse_lootable()),
                    store.is_some_and(|s| s.0.corpse_pvp_insignia()),
                    cursor.kind,
                    cursor.unable
                ),
            );
        }
        if store.is_some_and(|s| s.0.corpse_lootable()) {
            // **`GetStandState() != 0` refuses before the send** (`0x5d6c3b call [playerVtbl+0xa4]`
            // = `0x5ed570`, non-zero → `0x496720(0x85)`): sitting, kneeling or asleep, the click
            // raises the client-local red **`ERR_LOOT_NOTSTANDING`** as a `UI_ERROR_MESSAGE` and
            // sends nothing. Client-local, so the line shows even where the server would also have
            // refused — and unlike the range gray it is loud, because the player can fix it.
            // Scoped to the corpse leg: this is the gate the §5 round read, and the unit loot
            // branch's own copy of it is unverified here (1729).
            let standing = self_player
                .single()
                .ok()
                .and_then(|(e, _, _)| stores.get(e).ok())
                .is_none_or(|(s, _)| s.0.unit_stand_state() == 0);
            if !standing {
                debug!("right-click corpse loot: refused, not standing ({guid:#x})");
                ui_error_keys
                    .0
                    .push(crate::ui_action::UiError::key("ERR_LOOT_NOTSTANDING"));
                return;
            }
            // Range rides the cursor's own gray, exactly as the unit loot branch does —
            // the classifier and the click ask literally the same question, so the pouch can never
            // be lit on something a click refuses.
            if !cursor.unable {
                debug!("right-click corpse loot: {guid:#x}");
                let _ = seam.net.0.send(ClientCommand::Loot { guid });
                // Same client-side prediction as the unit corpse (decision 0515): the reference's
                // `CMSG_LOOT` sender arms `[player+0x1d28]` and kneels before any reply.
                loot_latch.0 = Some(guid);
            }
        } else if store.is_some_and(|s| s.0.corpse_pvp_insignia()) {
            // Leg 2, with its real precondition. `skin_player_corpse` is the mirror of `[0xb700e8]`
            // and is `None` for every 1.12.1 player, so this is inert for the reference's own
            // reason rather than by omission.
            if let Some(spell_id) = learned.skin_player_corpse {
                if !cursor.unable {
                    debug!("right-click corpse insignia: {guid:#x} (spell {spell_id})");
                    let _ = seam.net.0.send(ClientCommand::CastSpell {
                        spell_id,
                        target: Some(guid),
                    });
                }
            }
        }
        return;
    }
    let (Some(entity), Some(guid)) = (hovered.target, hovered.guid) else {
        return;
    };
    let attack = cursor.kind == cursor_mode::CursorKind::Attack;
    // Loot routes by the same CLASSIFICATION the cursor used — dead + `UNIT_DYNFLAG_LOOTABLE` —
    // not by the cursor kind (wow-re cursor-system.md §6: the right-click "routes the same hovered
    // object by the same classification"; its dead-unit row sends CMSG_LOOT). The loot cursor's
    // base mode is Pickup(8), which a live vendor also shows, so the kind alone can't name loot.
    let loot = stores
        .get(entity)
        .is_ok_and(|(s, _)| s.0.unit_is_dead() && s.0.unit_lootable());
    let me = self_player.single().ok();
    // The one SetSelection law ([`scan::commit`]): dedup + selection + the engaged-switch
    // stop→select→re-swing. The Attack cursor kind is Attack `0x5ecb70`'s new-target validation
    // (alive + reaction ≤ neutral) — a mid-combat click on a vendor/corpse switches and stops,
    // it never swings at them.
    let outcome = scan::commit(
        &mut selection,
        &mut seam,
        entity,
        guid,
        me.is_some_and(|(_, _, e)| e),
        me.map(|(_, g, _)| g.0),
        attack,
    );
    if attack {
        // The actor-eligibility block (decision 0481, widened to `0x612df0`'s full Phase A): the
        // click still SELECTED — the commit above already ran, matching the ref's
        // select-then-refuse order — but the melee auto-draw and the swing never happen; the red
        // `ERR_ATTACK_*` line shows instead.
        if crate::ui_action::attack_actor_refusal(
            me.and_then(|(e, _, _)| stores.get(e).ok()).map(|(s, _)| s),
            me.map(|(_, g, _)| g.0),
            &mut ui_error_keys,
        ) {
            // refused — selection stands, no swing
        } else {
            debug!("right-click attack: {guid:#x}");
            // The right-click's own StartAttack, through the one seam (auto-draw + the swing +
            // the auto-repeat cancel — `0x5ecb70`'s whole body). The commit above may already
            // have re-pointed the swing at this guid, in which case the ref's `0x5eccda` is the
            // thing that suppresses a second send, so pass it the real lock state and let the
            // seam decide: `swung` means the re-swing already went out and the lock is ours, so
            // no stop is in flight any more.
            let engaged = me.is_some_and(|(_, _, e)| e);
            seam.start(guid, engaged || outcome.swung, false);
        }
    } else if loot {
        // A dead unit carrying UNIT_DYNFLAG_LOOTABLE (the Pickup loot cursor, decision 0084): open
        // its loot (`CMSG_LOOT`). Range-gated like the interact branch — the cursor grays a corpse
        // beyond the melee interact reach (`unable`), and we don't send then (no auto-approach yet).
        // No EmoteTalk: looting is not an NPC interaction, the corpse plays no talk.
        if !cursor.unable {
            debug!("right-click loot: {guid:#x}");
            let _ = seam.net.0.send(ClientCommand::Loot { guid });
            // The kneel is client-predicted AT THE SEND: the real client's `CMSG_LOOT` sender
            // (`0x5df253`) sets the loot-target latch `[player+0x1d28]` and plays Loot 50 before
            // any server response (decision 0515). Arm the latch the anim driver's loot leg
            // reads for the self unit; the release/refusal drops it.
            loot_latch.0 = Some(guid);
        }
    } else if cursor.kind == cursor_mode::CursorKind::Skin {
        // A dead, unlootable, SKINNABLE corpse (the classifier's own gate): cast our known
        // Skinning spell at it — the unit-side mirror of the GO lock split (0239; decision 0437's
        // gathering finish). The spell comes from the reference's own learn-time latch
        // ([`crate::ui_action::LearnedAbilities`] = `[0xb700e4]`, decision 0752), which is the same
        // thing the classifier gated the Skin cursor on — so reaching here at all means we know it.
        // Range rides the cursor's melee-reach gray, like loot.
        if !cursor.unable {
            if let Some(spell_id) = learned.skinning {
                debug!("right-click skin: {guid:#x} (spell {spell_id})");
                let _ = seam.net.0.send(ClientCommand::CastSpell {
                    spell_id,
                    target: Some(guid),
                });
            }
        }
    } else if !cursor.unable {
        // An in-range friendly service NPC (the cursor already gated friendly + service + range):
        // route the interact by the classified kind (decision 0081). The Buy kind is shared by
        // banker and auctioneer (both classify to Buy(3), low-bit-first — a gossip-flagged banker
        // already classified to Speak and routes through the gossip menu), so the dispatch reads
        // the NPC's own flags to split them (decision 0604).
        let npc_flags = stores
            .get(entity)
            .map(|(s, _)| s.0.unit_npc_flags())
            .unwrap_or(0);
        if let Some(cmd) = interact_command(cursor.kind, guid, npc_flags) {
            debug!("right-click interact: {guid:#x} ({:?})", cursor.kind);
            let _ = seam.net.0.send(cmd);
            // Talk at the NPC. The gesture's own anim carries WeaponFlags `0x10`, so the
            // per-animation sheath reconcile stows a drawn weapon — a committed change that
            // persists after the talk (decisions 0080/0081; no sheath wiring here).
            if let Some((_, my_guid, _)) = me {
                gestures.push(my_guid.0, crate::creature_anim::Gesture::Talk);
            }
        }
    }
}

/// The right-click action a hovered GameObject resolves to (decisions 0239 / 0545 / 0752) — chosen
/// by its lock, through the shared chain in [`super::lock`].
pub(crate) enum GoAction {
    /// No lock (or no lock data): `CMSG_GAMEOBJ_USE` — door / lever / quest object / mailbox /
    /// unlocked chest.
    Use,
    /// A lock we can open with a **known skill spell**: cast it at the object — a known
    /// `OPEN_LOCK` (chest / vein / herb / a picked lock). `CMSG_CAST_SPELL`.
    OpenLock(u32),
    /// A lock whose **KEY** slot we satisfy: *use the key at the object* — `CMSG_USE_ITEM` with the
    /// key's wire position and `TARGET_FLAG_GAMEOBJECT`, NOT a bare cast of the key's spell
    /// (decision 0769; wow-re `cursor-system.md` §8.4 — "the client never sends a bare
    /// CMSG_CAST_SPELL for a key lock"). The distinction is the whole ballgame: `Spell::CanOpenLock`
    /// honours a `Lock.dbc` KEY slot only when `m_CastItem` is set, which only USE_ITEM supplies.
    OpenByKey {
        bag_index: u8,
        slot: u8,
        spell_index: u8,
    },
    /// A lock present that we cannot open — the client-local refusal (§8.4: `DisplayError`, **no
    /// packet**). `Some` = the red toast to queue; `None` = the ref is silent for this case too.
    Refuse(Option<crate::ui_action::UiError>),
}

/// What we know about a key-item lock slot's key when routing a refusal ([`route_lock_refusal`]).
enum KeyFact {
    /// Not held; the item template names it ("Requires Shadowforge Key").
    Named(String),
    /// Not held and the template isn't cached yet — the ref's `GetRecord` miss is silent (§8.8
    /// `0xde`); our ask-once query is away, so a later click names it.
    Unknown,
}

/// Resolve a hovered GameObject's right-click action from its lock — the reference's USE sender
/// `0x5f33e0` over the shared resolver (decisions 0239 / 0545 / **0752**).
///
/// Not-yet-queried or no `Lock.dbc` → treat as lockless (`Use`): the stream-in query makes "not
/// cached" a rare race, and `Use` is both the correct lockless action and a harmless no-op on a
/// chest whose template is still in flight.
///
/// The satisfaction decision itself lives in [`super::lock::resolve_lock`] so the cursor's `usable`
/// asks exactly the same question (§4a/§8.7 — the icon and the click agree by construction). Here
/// we only turn its answer into a packet, and the lock split has **three** arms, not two (wow-re
/// `cursor-system.md` §8.4, VERIFIED): lockless → `CMSG_GAMEOBJ_USE`; a satisfied **skill** slot →
/// `CMSG_CAST_SPELL` of the matched opener; a satisfied **key** slot → `CMSG_USE_ITEM` carrying the
/// key's position and the GO as its cast target ([`GoAction::OpenByKey`]). An unmet lock takes
/// §8.8's toast routing and sends nothing.
///
/// The key arm sent a bare cast until decision 0769, which is why keys never opened anything: the
/// server honours a KEY slot only when the cast carries `m_CastItem` (`Spell::CanOpenLock`,
/// `Spell.cpp:7892`), and only `CMSG_USE_ITEM` supplies it. That was recorded here as the server's
/// gap; it was ours, and wow-re's note says so in the same breath — "the client never sends a bare
/// CMSG_CAST_SPELL for a key lock".
pub(crate) fn resolve_go_action(
    guid: u64,
    inputs: &mut GoLockInputs,
    known: &std::collections::BTreeSet<u32>,
    go: Option<(&ObjectStore, u32)>,
    me_store: Option<&ObjectStore>,
    net: &NetCommands,
) -> GoAction {
    let Some(tmpl) = inputs.templates.get(guid) else {
        return GoAction::Use;
    };
    let Some(locks) = inputs.locks.as_deref() else {
        return GoAction::Use;
    };
    // A lockId whose row is missing is "no lock" — the ref resolver's `0x5f8180` null → FALSE with
    // spell 0 → `CMSG_GAMEOBJ_USE` (§8.4 C6).
    let Some(slots) = locks.0.slots(tmpl.lock_id).filter(|_| tmpl.lock_id != 0) else {
        return GoAction::Use;
    };
    let facts = super::lock::go_facts(go);
    let mut matched = None;
    let outcome = super::lock::resolve_lock(
        slots,
        known,
        inputs.spells.as_deref(),
        inputs.skill_lines.as_ref().map(|s| &s.catalog),
        me_store,
        &inputs.items,
        facts,
        &mut matched,
    );
    let key_entry = match outcome {
        super::lock::LockOutcome::Unlocked => return GoAction::Use,
        super::lock::LockOutcome::OpenBySpell(spell_id) => {
            // The retest instrument for B247 (decision 1312): which of the player's openers this
            // lock resolved to, by id. The spell's Spell.dbc name is what the cast bar prints, so
            // reading the id off a probe run is how "the bar says Opening - No Text" becomes a
            // machine-checkable fact instead of a screenshot.
            debug!("target: lock {} → open by spell {spell_id}", tmpl.lock_id);
            return GoAction::OpenLock(spell_id);
        }
        super::lock::LockOutcome::OpenByKey(entry) => entry,
        super::lock::LockOutcome::Unmet => {
            // Unopenable — §8.8's routing, which keys off Lock.dbc **slot 0** regardless of which
            // slot the resolver walked.
            let slot0 = slots[0];
            let key = if slot0.key_type == benilla_formats::LOCK_KEY_ITEM {
                match inputs.items.template(slot0.index, 0, net) {
                    Some(info) => KeyFact::Named(info.name.clone()),
                    None => KeyFact::Unknown,
                }
            } else {
                KeyFact::Unknown
            };
            let lock_types = inputs.lock_types.as_deref();
            return GoAction::Refuse(route_lock_refusal(
                &slot0,
                matched.is_some(),
                facts.flag_locked,
                go.map_or(-1, |(s, _)| s.0.gameobject_type_id()),
                facts.level,
                lock_types.and_then(|lt| lt.0.name(slot0.index)),
                key,
            ));
        }
    };
    // A key we carry: **use it at the object**. The resolver `0x5f85aa` only hands the caller the
    // key's ON_USE spell id AND the item object itself; it is the sender `0x6e54f0` that then
    // discriminates on cast-item-vs-caster and takes the item arm — `0x6e57d8 push 0xab`,
    // CMSG_USE_ITEM carrying `u8 bag · u8 slot · u8 spellSlot · SpellCastTargets(GO)`, with no raw
    // spell id at all (the server re-resolves the Item* from bag+slot). Decision 0769.
    //
    // So what the wire needs is the key's POSITION, which the same walker that found it gives us.
    let Some(store) = me_store else {
        return GoAction::Refuse(None);
    };
    let Some((bag_index, slot, _)) = crate::ui_items::find_item(
        &store.0,
        &inputs.items,
        key_entry,
        crate::ui_items::ItemSearch::default(),
    ) else {
        // Held a moment ago (the resolver said so) and gone now — nothing to send.
        return GoAction::Refuse(None);
    };
    // The template is ask-once — a miss queries and does nothing this click, exactly like the
    // toast's own name miss. `use_spell_index` is the BLOCK ordinal, the packet's third byte.
    match inputs
        .items
        .template(key_entry, 0, net)
        .and_then(|i| i.use_spell_index())
    {
        Some(spell_index) => GoAction::OpenByKey {
            bag_index,
            slot,
            spell_index,
        },
        None => GoAction::Refuse(None),
    }
}

/// The client-local toast for an unopenable lock — the ref's routing, transcribed (wow-re
/// cursor-system.md §8.8; decision 0545). Two layers, exactly as the binary orders them:
///
/// 1. **`GO_FLAG_LOCKED` set** (a padlocked chest/door — gather nodes never set it): the `usable`
///    gate refuses with the strategy default `[strat+8]` before the rich routing ever runs —
///    DOOR "The door is locked." / BUTTON "That has already been used." / else "Item is locked."
/// 2. Flag clear: route by **Lock.dbc slot 0** — key item missing → "Requires <item>" (`0xde`),
///    skill spell unknown → "Requires <LockType.Name>" (`0xdf`, the "Requires Herbalism" case),
///    known but under-rank → "Requires <name> <rank>" (`0xe0`, rank = `Skill[0]`, else
///    GO-level×5), slot-0 type neither → "You can't open that." (`0xda`).
///
/// The `"UNKNOWN"` literal is the ref's own missing-LockType-row fallback (`0x838044`). The
/// `0xd9` chest-in-use pre-check (`0x5f81d0`) is unmodeled — its state is generally unreachable
/// through our highlightable gate (`GO_FLAG_IN_USE` already excludes the busy chest).
fn route_lock_refusal(
    slot0: &benilla_formats::LockSlot,
    opener_known: bool,
    flag_locked: bool,
    go_type: i32,
    go_level: u32,
    lock_type_name: Option<&str>,
    key: KeyFact,
) -> Option<crate::ui_action::UiError> {
    use crate::ui_action::UiError;
    if flag_locked {
        return Some(UiError::key(match go_type {
            0 => "ERR_DOOR_LOCKED",
            1 => "ERR_BUTTON_LOCKED",
            _ => "ERR_USE_LOCKED",
        }));
    }
    match slot0.key_type {
        benilla_formats::LOCK_KEY_ITEM => match key {
            KeyFact::Unknown => None,
            KeyFact::Named(name) => Some(UiError {
                key: "ERR_USE_LOCKED_WITH_ITEM_S",
                fill_s: Some(name),
                fill_d: None,
                info: false,
            }),
        },
        benilla_formats::LOCK_KEY_SKILL => {
            let name = lock_type_name.unwrap_or("UNKNOWN").to_string();
            if opener_known {
                let required = super::lock::required_skill(slot0, go_level).max(0) as u32;
                Some(UiError {
                    key: "ERR_USE_LOCKED_WITH_SPELL_KNOWN_SI",
                    fill_s: Some(name),
                    fill_d: Some(required),
                    info: false,
                })
            } else {
                Some(UiError {
                    key: "ERR_USE_LOCKED_WITH_SPELL_S",
                    fill_s: Some(name),
                    fill_d: None,
                    info: false,
                })
            }
        }
        _ => Some(UiError::key("ERR_USE_CANT_OPEN")),
    }
}

/// The interact packet a right-click on a friendly service NPC sends, by its classified
/// [`cursor_mode::CursorKind`] (decision 0081). A **vendor-only** NPC (Pickup) opens the stock list
/// directly; a **flight master** (Taxi) opens the taxi map directly (byte-verified — decision 0496
/// `CMSG_TAXIQUERYAVAILABLENODES`; the gossip taxi option still reaches the same menu
/// server-side, so a gossip-routed flight master isn't broken by this); every other service kind
/// opens the universal gossip menu — `CMSG_GOSSIP_HELLO` works on any interactable creature
/// (verified: the server passes `UNIT_NPC_FLAG_NONE`), and the gossip window shows whatever menu
/// comes back (the banker/trainer/innkeeper *windows* are their own arcs, but the generic hello is
/// faithful and harmless). Non-service kinds (Attack is handled above; Point/Skin aren't
/// interacts) send nothing. A lootable corpse also shows Pickup (the loot base mode) but never
/// reaches here — the loot branch routes it by classification first.
fn interact_command(
    kind: cursor_mode::CursorKind,
    guid: u64,
    npc_flags: u32,
) -> Option<ClientCommand> {
    use cursor_mode::CursorKind;
    match kind {
        CursorKind::Pickup => Some(ClientCommand::ListInventory { guid }),
        CursorKind::Taxi => Some(ClientCommand::TaxiQueryNodes { guid }),
        // Buy(3) is banker OR auctioneer (the ladder's shared leg), so it forks twice. A pure
        // banker (bit 8 — the lowest service bit, and the only way Buy classified) opens the bank
        // directly (`CMSG_BANKER_ACTIVATE`, decision 0604).
        CursorKind::Buy if npc_flags & cursor_mode::npc_flags::BANKER != 0 => {
            Some(ClientCommand::BankerActivate { guid })
        }
        // An auctioneer (bit 12) greets its house (decision 1511). Note what this does NOT do:
        // open anything. The greeting is a round trip, and the window opens when the *reply*
        // lands — that is the real client's own law, and it is also what makes the house id
        // (which only the reply carries) available before the sell pane needs a deposit rate.
        // An auctioneer that also carries the gossip bit never reaches here: the service ladder
        // is first-match-wins from the low bits, so bit 0 pre-empts bit 12 and the menu's own
        // auctioneer option asks the server for the same greeting.
        CursorKind::Buy if npc_flags & cursor_mode::npc_flags::AUCTIONEER != 0 => {
            Some(ClientCommand::AuctionHello { auctioneer: guid })
        }
        // A stable master asks for its own pet list — the client sends `MSG_LIST_STABLED_PETS`
        // ITSELF on the bit-13 interact leg (`0x5f05a1` → `0x5f05bc` → `0x5e02a0`, whose sole
        // caller that is), and the window opens when the reply lands. This corrects decision
        // 1676's "server-initiated, off the gossip option": that is how it opens against vmangos
        // (`GOSSIP_OPTION_STABLEPET` → `SendStablePet`), but it is not what the client does.
        //
        // Gated on the gossip bit being absent, which is the honest bound: the cursor ladder above
        // is first-match-wins and reaches its STABLEMASTER leg only when no lower service bit is
        // set, but the reference's *interaction* dispatcher is a different function whose own
        // ordering the carve did not enumerate. A stable master carrying a gossip menu therefore
        // keeps the menu — where its own stable option asks the server for the same list — and one
        // without a menu takes this leg, which is the case the bare bit-13 test exists for.
        CursorKind::Speak
            if npc_flags & cursor_mode::npc_flags::STABLEMASTER != 0
                && npc_flags & cursor_mode::npc_flags::GOSSIP == 0 =>
        {
            Some(ClientCommand::ListStabledPets { npc: guid })
        }
        CursorKind::Speak | CursorKind::Buy | CursorKind::Trainer | CursorKind::Interact => {
            Some(ClientCommand::GossipHello { guid })
        }
        // Repair and Cast are UI-overlay modes only; Inspect and the data-named GameObject
        // cursors (Mail/Mine/GatherHerbs/PickLock) belong to the GO branch, which routes above
        // and never reaches this NPC-service dispatch — a unit never classifies to any of
        // these. LootAll is loot-only (the corpse route above), never a service.
        CursorKind::Point
        | CursorKind::Attack
        | CursorKind::Skin
        | CursorKind::LootAll
        | CursorKind::Repair
        | CursorKind::Inspect
        | CursorKind::Mail
        | CursorKind::Mine
        | CursorKind::GatherHerbs
        | CursorKind::PickLock
        | CursorKind::Cast => None,
    }
}

/// Drain the ESC chain's `ClearTarget()` (the ref's ESC ladder's last close/cancel leg (`ToggleGameMenu` — the ref's
/// `ToggleGameMenu` order, `UIParent.lua:1492`) and commit the deselect. The target drops ONLY
/// when nothing earlier in the chain ate the press: a mid-cast ESC cancels the cast instead, an
/// open window closes instead — the two-press behavior the raw-key clear this replaces couldn't
/// express (it ran beside the chain, so the first ESC both canceled the cast AND dropped the
/// target — the director's 0449 report). EditBox precedence rides the chain too: a focused box
/// consumes ESCAPE before `ToggleGameMenu` ever runs.
pub(super) fn clear_target_requests(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    mut selection: ResMut<Selection>,
    mut seam: crate::creature_anim::AttackSeam,
    engaged: Query<(), (With<Engaged>, With<SelfPlayer>)>,
) {
    let Some(mut script) = script else {
        return;
    };
    if script.take_target_clear() {
        clear(&mut selection, &mut seam, !engaged.is_empty());
    }
}

/// Drain the UI's `TargetUnit(token)` requests and commit each through the shared SetSelection path
/// ([`scan::commit`]) — the app half of the reference's `TargetUnit` Lua shim. Callers: the player
/// frame's left-click (`TargetUnit("player")`) and the party frames' (`TargetUnit("partyN")`,
/// decision 0434 phase 5). Only tokens resolving to a STREAMED unit act: `"player"` → our avatar;
/// `"target"` → the current selection (a dedup no-op); `"partyN"`/`"raidN"` → that roster slot
/// when its entity is in range (an out-of-range member needs the guid-only selection the phase-4
/// out-of-range slice owns — until then the click no-ops, like the real client on a nonexistent
/// unit); `"pet"` → the bar's cached pet guid (decision 0990, the pet frame's left click);
/// `"targettarget"` → the selection's own `UNIT_FIELD_TARGET` (decision 1576, the ToT frame's).
/// Everything else (mouseover/name) waits for its wire.
#[allow(clippy::too_many_arguments)]
pub(super) fn target_unit_requests(
    script: Option<NonSendMut<UiScript>>,
    // The one unit-token resolver (`crate::ui_unit::UnitTokens`) — the arms this drain used to
    // spell out inline. It is shared with the reach feed precisely so `TargetUnit("target")` and
    // `CheckInteractDistance("target", …)` can never mean two different units (B304).
    tokens: crate::ui_unit::UnitTokens,
    // The one SetSelection tail, shared with `/target` and `/assist` (decision 1583). It carries
    // the classification too, which is why this drain no longer states one: hand-stating it here
    // is exactly how a `false` that the binary refutes got written down.
    mut commit: super::by_name::SelectCommit,
) {
    let Some(mut script) = script else {
        return;
    };
    let requests = script.take_target_requests();
    if requests.is_empty() {
        return;
    }
    for token in requests {
        // Resolved before the commit borrows the selection mutably. An unstreamed unit — an
        // out-of-range party member, a despawned pet — resolves to nothing and no-ops.
        let resolved = tokens.resolve(&token, &commit.selection);
        if let Some((entity, guid)) = resolved {
            commit.commit(entity, guid);
        }
    }
}

/// Drop the current target and tell the server (`CMSG_SET_SELECTION` guid 0). A no-op when nothing is
/// selected, so it never sends a redundant clear. Losing the target also ends melee auto-attack when
/// one is running (`engaged`, our server-echoed [`Engaged`]): `CMSG_ATTACKSTOP` — the ref stops
/// swinging and drops the attack stance on Esc/click-off/target-death alike (the stance itself falls
/// when the `SMSG_ATTACKSTOP` echo removes [`Engaged`]). Weapons *stay drawn* — combat never stows.
pub(super) fn clear(
    selection: &mut Selection,
    seam: &mut crate::creature_anim::AttackSeam,
    engaged: bool,
) {
    if selection.target.take().is_some() {
        selection.guid = None;
        let _ = seam.net.0.send(ClientCommand::SetSelection { guid: 0 });
        // `SetSelection 0x493540`'s own `0x493a08 call 0x5ecac0` — the real StopAttack, so
        // losing the target also un-queues a pending on-next-swing strike. It was a bare
        // `CMSG_ATTACKSTOP` before the seam existed.
        seam.stop(engaged);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The §8.8 toast routing, case by case (decision 0545). The slot/flag/type combinations
    /// mirror real data: Peacebloom (lock 29: skill slot, LockType 2, Skill 0), a rank-155 vein
    /// (lock 42: LockType 3, Skill 155), a keyed door, a padlocked chest.
    #[test]
    fn lock_refusals_route_like_the_reference() {
        use benilla_formats::{LockSlot, LOCK_KEY_ITEM, LOCK_KEY_SKILL};
        let skill_slot = |index, skill| LockSlot {
            key_type: LOCK_KEY_SKILL,
            index,
            skill,
            action: 0,
        };
        // Herb, Herbalism unknown → 0xdf "Requires %s" filled with the LockType name.
        let e = route_lock_refusal(
            &skill_slot(2, 0),
            false,
            false,
            3,
            0,
            Some("Herbalism"),
            KeyFact::Unknown,
        )
        .unwrap();
        assert_eq!(
            (e.key, e.fill_s.as_deref(), e.fill_d),
            ("ERR_USE_LOCKED_WITH_SPELL_S", Some("Herbalism"), None)
        );
        // Vein, Mining known but rank < 155 → 0xe0 "Requires %s %d" with the slot's Skill[0].
        let e = route_lock_refusal(
            &skill_slot(3, 155),
            true,
            false,
            3,
            0,
            Some("Mining"),
            KeyFact::Unknown,
        )
        .unwrap();
        assert_eq!(
            (e.key, e.fill_s.as_deref(), e.fill_d),
            (
                "ERR_USE_LOCKED_WITH_SPELL_KNOWN_SI",
                Some("Mining"),
                Some(155)
            )
        );
        // Skill[0] == 0 → the required rank falls back to GO-level × 5 (`0x5f3490`).
        let e = route_lock_refusal(
            &skill_slot(3, 0),
            true,
            false,
            3,
            20,
            Some("Mining"),
            KeyFact::Unknown,
        )
        .unwrap();
        assert_eq!(e.fill_d, Some(100));
        // A missing LockType row fills the ref's literal fallback (`0x838044`).
        let e = route_lock_refusal(
            &skill_slot(9999, 0),
            false,
            false,
            3,
            0,
            None,
            KeyFact::Unknown,
        )
        .unwrap();
        assert_eq!(e.fill_s.as_deref(), Some("UNKNOWN"));
        // Key lock, key absent + named → 0xde "Requires %s" with the item name; the template
        // miss is silent, like the ref (a key we DO hold never reaches the toast at all — the
        // resolver returns `OpenByKey` and the click casts it, decision 0752).
        let key_slot = LockSlot {
            key_type: LOCK_KEY_ITEM,
            index: 11000,
            skill: 0,
            action: 1,
        };
        let e = route_lock_refusal(
            &key_slot,
            false,
            false,
            0,
            0,
            None,
            KeyFact::Named("Shadowforge Key".into()),
        )
        .unwrap();
        assert_eq!(
            (e.key, e.fill_s.as_deref()),
            ("ERR_USE_LOCKED_WITH_ITEM_S", Some("Shadowforge Key"))
        );
        assert!(
            route_lock_refusal(&key_slot, false, false, 0, 0, None, KeyFact::Unknown).is_none()
        );
        // GO_FLAG_LOCKED set → the strategy default REPLACES the rich routing (§8.8's usable
        // gate): door 0xdc, button 0xdd, chest/else 0xdb — even on a skill lock.
        for (go_type, key) in [
            (0, "ERR_DOOR_LOCKED"),
            (1, "ERR_BUTTON_LOCKED"),
            (3, "ERR_USE_LOCKED"),
        ] {
            let e = route_lock_refusal(
                &skill_slot(1, 0),
                false,
                true,
                go_type,
                0,
                Some("Pick Lock"),
                KeyFact::Unknown,
            )
            .unwrap();
            assert_eq!(e.key, key);
            assert_eq!(e.fill_s, None);
        }
        // Slot-0 type neither key nor skill → 0xda "You can't open that."
        let odd = LockSlot {
            key_type: 7,
            index: 0,
            skill: 0,
            action: 0,
        };
        let e = route_lock_refusal(&odd, false, false, 3, 0, None, KeyFact::Unknown).unwrap();
        assert_eq!(e.key, "ERR_USE_CANT_OPEN");
    }

    #[test]
    fn interact_routes_vendor_direct_and_gossip_universal() {
        use cursor_mode::CursorKind;
        // A vendor-only NPC (Pickup) opens the stock list directly (decision 0081).
        assert!(matches!(
            interact_command(CursorKind::Pickup, 0x42, 0x4),
            Some(ClientCommand::ListInventory { guid: 0x42 })
        ));
        // A flight master (Taxi) opens the taxi map directly (byte-verified — decision 0496).
        assert!(matches!(
            interact_command(CursorKind::Taxi, 0x77, 0x8),
            Some(ClientCommand::TaxiQueryNodes { guid: 0x77 })
        ));
        // Gossip (Speak) and the out-of-scope service kinds route through the universal hello.
        for (kind, flags) in [
            (CursorKind::Speak, 0x1),
            (CursorKind::Trainer, 0x10),
            (CursorKind::Interact, 0x80),
        ] {
            assert!(matches!(
                interact_command(kind, 0x99, flags),
                Some(ClientCommand::GossipHello { guid: 0x99 })
            ));
        }
        // Non-service cursors aren't interacts (Attack is handled by the attack branch).
        for kind in [CursorKind::Point, CursorKind::Attack, CursorKind::Skin] {
            assert!(interact_command(kind, 0x1, 0).is_none());
        }
    }

    /// The auctioneer split (decision 1511): a pure auctioneer greets its house rather than
    /// falling to the gossip universal, and the greeting is all it does — the window opens when
    /// the *reply* lands, which is the real client's own law.
    #[test]
    fn interact_routes_pure_auctioneer_to_the_hello() {
        use cursor_mode::CursorKind;
        assert!(matches!(
            interact_command(CursorKind::Buy, 0x51, cursor_mode::npc_flags::AUCTIONEER),
            Some(ClientCommand::AuctionHello { auctioneer: 0x51 })
        ));
    }

    /// The stable-master split (decision 1677): the client asks for the pet list **itself** on the
    /// bit-13 interact leg — it is not purely server-initiated, which is what 1676 had. A stable
    /// master carrying a gossip menu keeps the menu, whose own stable option asks the server for
    /// the same list; one without a menu takes this leg.
    #[test]
    fn interact_routes_a_menuless_stable_master_to_the_pet_list() {
        use cursor_mode::CursorKind;
        assert!(matches!(
            interact_command(
                CursorKind::Speak,
                0x64,
                cursor_mode::npc_flags::STABLEMASTER
            ),
            Some(ClientCommand::ListStabledPets { npc: 0x64 })
        ));
        // With a gossip menu, the universal hello still wins — the menu carries the stable option.
        assert!(matches!(
            interact_command(
                CursorKind::Speak,
                0x64,
                cursor_mode::npc_flags::STABLEMASTER | cursor_mode::npc_flags::GOSSIP
            ),
            Some(ClientCommand::GossipHello { guid: 0x64 })
        ));
    }

    /// The banker split (decision 0604): Buy(3) is banker OR auctioneer — the BANKER flag routes
    /// the direct `CMSG_BANKER_ACTIVATE`, its absence falls to the auctioneer fork above. A
    /// gossip-flagged banker never reaches this fork (the low-bit-first ladder classified Speak).
    #[test]
    fn interact_routes_pure_banker_direct() {
        use cursor_mode::CursorKind;
        assert!(matches!(
            interact_command(CursorKind::Buy, 0x42, cursor_mode::npc_flags::BANKER),
            Some(ClientCommand::BankerActivate { guid: 0x42 })
        ));
        // Banker + auctioneer both set: the ladder's low-bit order (banker is bit 8) wins.
        assert!(matches!(
            interact_command(CursorKind::Buy, 0x42, 0x1100),
            Some(ClientCommand::BankerActivate { guid: 0x42 })
        ));
    }
}
