//! The click/USE router — the input half of targeting: a clean left-click selects the
//! [`super::Hovered`] unit, a clean right-click dispatches the context action (attack, NPC
//! interact, GameObject USE with the lock/refusal ladder), and the UI's selection asks
//! (`TargetUnit` / `AssistUnit` / `TargetLastEnemy`) + Esc drain into the same
//! [`super::Selection`]. Split from `mod.rs` (which keeps the state resources and the plugin)
//! along the state-vs-input seam; the systems here are registered by [`super::TargetPlugin`] in
//! the target chain after the hover picks and the cursor classifier.

use benilla_ui::script::SelectionRequest;

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
    // ── The interact family's ACTOR gate: am I in the saddle? ────────────────────────────────
    // One predicate, the reference's own — the PLAYER's `UNIT_FIELD_MOUNTDISPLAYID` (decision
    // 0481's "one mounted predicate"; wow-re `mounted-action-gate.md`: no aura, no taxi
    // distinction, this field and nothing else). 0481 built the gate for the two action families
    // the director reported then — casts (`0x6094f0`'s reason `0x39` "You are mounted") and
    // attack-start (`0x612df0`'s `ERR_ATTACK_MOUNTED`) — and wrote interaction explicitly out of
    // scope. This is that third family (decision 1851): the right-click interact dispatchers read
    // the very same field, in their own ladders, and until now we read it in none of them.
    let self_store = self_player
        .single()
        .ok()
        .and_then(|(e, _, _)| stores.get(e).ok())
        .map(|(s, _)| s);
    let self_mounted = self_store.is_some_and(|s| s.0.unit_mount_display_id() != 0);
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
                    "right-click go guid={:?} type={ty} mounted={self_mounted} cursor={:?} unable={}",
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
                // ── The GameObject leg's OWN mounted gate — not the cast validator's ────────
                // The strategy's usable predicate `0x5f3130` carries a mounted arm of its own at
                // `0x5f31a8`: errorId `0x19f` `ERR_NOT_WHILE_MOUNTED`, then `0x5f31d6 xor al,al`,
                // so `0x5f86b0` never reaches the opener it would invoke at `0x5f86eb`. No
                // `CMSG_GAMEOBJ_USE`, no cast, no state write — the gate returns before the
                // opener, not merely before the message (wow-re `mounted-interaction-gate.md`
                // §5.1, the §5 this session dispatched).
                //
                // **It applies only to LOCK-LESS objects.** The gate sits behind
                // `0x5f3195 call 0x5f8180` / `0x5f319c jne 0x5f3231`, which resolves the object's
                // **Lock.dbc row** and skips the whole thing when one exists. So a mining node, a
                // herb or a locked chest bypasses this and is refused further down by its opener
                // CAST's own mounted block — loudly, "You are mounted" — while a spellcaster, a
                // chair, a `lockId 0` quest goober and a readable sign are refused HERE, in
                // silence. What buys the bypass is a **lock**, not a spell; the round's first
                // draft had that backwards and its own cross-check caught it.
                //
                // The test is pure **row existence**: `0x5f819c` tests the row *pointer*, and
                // nothing reads `Type[]`/`Index[]`/`Skill[]` until `0x5f83d0`. So it is
                // deliberately NOT [`benilla_formats::LockCatalog::is_locked`], which also demands
                // a non-empty slot — an all-empty Lock row is "no lock" to our opener resolver and
                // "a lock" to this gate, and the reference lets that object be used from the
                // saddle. Collapsing the two is the one divergence the RE explicitly warned about.
                //
                // **MAILBOX (type 19) is the only exemption**, and it lives inside the gate on the
                // mounted arm alone (`0x5f31bb` → `0x47cff0`: a sixteen-byte `type == 0x13`
                // equality, no table, no range). TEXT is NOT exempt — the reference refuses a sign
                // from horseback even though its opener is every bit as client-side as the
                // mailbox's, which is why this sits ABOVE both local-open branches below.
                //
                // Silent by Blizzard's omission, not our choice: `ERR_NOT_WHILE_MOUNTED` is one of
                // 1.12.1's orphan keys with no GlobalStrings value, and both of `DisplayError`'s
                // side-effect branches skip for this row (`[row+0xc] == 0x44` takes the no-sound
                // jump; `[row+0x8]` is the literal `"NONE"`), so nothing is shown, sounded or
                // fired. Whether that leaves any visible artifact at all is the one thing the
                // round could not settle from the binary — it wants one mounted click on a sign.
                let lock_id = go_inputs.templates.get(guid).map_or(0, |t| t.lock_id);
                let has_lock_row = lock_id != 0
                    && go_inputs
                        .locks
                        .as_deref()
                        .is_some_and(|l| l.0.slots(lock_id).is_some());
                let go_type = go.map_or(-1, |(s, _)| s.0.gameobject_type_id());
                if self_mounted && !has_lock_row && go_type != cursor_mode::GO_TYPE_MAILBOX {
                    debug!(
                        "right-click gameobject {guid:#x}: refused, mounted (lock-less type {go_type}, silent)"
                    );
                    return;
                }
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
                match resolve_go_action(
                    guid,
                    &mut go_inputs,
                    &player_actions.spells,
                    go,
                    self_store,
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
                        let def = go_inputs
                            .spells
                            .as_ref()
                            .and_then(|s| s.catalog.get(spell_id));
                        if crate::ui_action::reagent_totem_refusal(
                            spell_id,
                            def,
                            self_store,
                            &go_inputs.items,
                            &mut cast_errors,
                        ) {
                            return;
                        }
                        // Same funnel, same requirement validator: a mounted opener refuses with
                        // reason `0x39` and sends nothing (`0x609c6c`). Mining and Herbalism are
                        // exactly the gathering casts a rider tries without dismounting, and the
                        // server would silently dismount us instead of saying so. **After** the
                        // reagent check, which is TryCast's own order — step 5 (`0x6e4dec`) before
                        // step 7 (`0x6e4f3b`) — so a pickless mounted miner still reads "Requires
                        // Mining Pick", exactly as [`CastLadder`] orders its own two rungs.
                        if crate::ui_action::cast_mounted_refusal(self_mounted, def) {
                            debug!("right-click gameobject open-lock: refused locally — mounted");
                            cast_errors.push_local(spell_id, 0x39);
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
                        //
                        // The MOUNTED gate is not skippable the same way: an item use IS a cast
                        // (decision 0908) and reaches the same requirement validator, so a rider
                        // turning a key refuses and sends nothing. The key's ON_USE *spell* is
                        // exactly what this arm does not resolve (the stated 0914 gap below), so
                        // the record is `None` — which the predicate reads as "no exemption to
                        // claim" and refuses. That is the right way to be wrong: no 1.12 key
                        // carries Attributes bit 24. Raised by KEY rather than through
                        // [`CastErrors`], because a reason-coded entry wants a spell id and we
                        // have none — the red line is identical either way; what a spell-less
                        // entry would cost is the combat-log twin, which needs the spell's name
                        // and cannot have it here. A disclosed shortfall of 0914's gap, not a new
                        // one.
                        if crate::ui_action::cast_mounted_refusal(self_mounted, None) {
                            debug!("right-click gameobject open-by-key: refused locally — mounted");
                            ui_error_keys
                                .0
                                .push(crate::ui_action::UiError::key("SPELL_FAILED_NOT_MOUNTED"));
                            return;
                        }
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
                    "right-click corpse guid={guid:#x} bones={} lootable={} insignia={} mounted={self_mounted} cursor={:?} unable={}",
                    store.is_some_and(|s| s.0.corpse_is_bones()),
                    store.is_some_and(|s| s.0.corpse_lootable()),
                    store.is_some_and(|s| s.0.corpse_pvp_insignia()),
                    cursor.kind,
                    cursor.unable
                ),
            );
        }
        // **Leg 1's first conjunct is `!mounted`** (`0x5d6c2a jg`, the player's MOUNTDISPLAYID read
        // one test before the lootable one). The block comment above has named this conjunct since
        // the leg was transcribed and the code never carried it — a rider could loot bones from the
        // saddle. A mounted click is **not** an error here: it falls through to leg 2 exactly as the
        // unit dispatcher falls through to skin, and since no 1.12.1 player has the insignia latch,
        // what the rider actually gets is silence.
        if !self_mounted && store.is_some_and(|s| s.0.corpse_lootable()) {
            // **`GetStandState() != 0` refuses before the send** (`0x5d6c3b call [playerVtbl+0xa4]`
            // = `0x5ed570`, non-zero → `0x496720(0x85)`): sitting, kneeling or asleep, the click
            // raises the client-local red **`ERR_LOOT_NOTSTANDING`** as a `UI_ERROR_MESSAGE` and
            // sends nothing. Client-local, so the line shows even where the server would also have
            // refused — and unlike the range gray it is loud, because the player can fix it.
            // No longer scoped to the corpse leg: 1851 read the unit dispatcher's own copy
            // (`0x60bfb7` → `0x60c007 push 0x85`) off the same fork and built it too, so the
            // "unverified here" caveat this comment carried since 1729 is retired.
            let standing = self_store.is_none_or(|s| s.0.unit_stand_state() == 0);
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
                    // Same TryCast funnel as the unit skin leg, so the same mounted block — and
                    // this is the leg a mounted bones-click FALLS INTO, which makes the gate here
                    // the difference between "nothing happens" and a rider yanking insignias.
                    // The record comes off [`GoLockInputs`]'s catalog because that bundle is this
                    // system's only handle on `Spells` and the param list is at the 16 ceiling —
                    // the bundle is named for its first tenant, not scoped to it.
                    let def = go_inputs
                        .spells
                        .as_ref()
                        .and_then(|s| s.catalog.get(spell_id));
                    if crate::ui_action::cast_mounted_refusal(self_mounted, def) {
                        debug!("right-click corpse insignia: refused locally — mounted (0x39)");
                        cast_errors.push_local(spell_id, 0x39);
                    } else {
                        debug!("right-click corpse insignia: {guid:#x} (spell {spell_id})");
                        let _ = seam.net.0.send(ClientCommand::CastSpell {
                            spell_id,
                            target: Some(guid),
                        });
                    }
                }
            }
        }
        return;
    }
    let (Some(entity), Some(guid)) = (hovered.target, hovered.guid) else {
        return;
    };
    let attack = cursor.kind == cursor_mode::CursorKind::Attack;
    let target = stores.get(entity).ok().map(|(s, _)| s);
    // ── The dead-target fork of the reference's unit interact dispatcher `0x60bea0` ──────────
    // Loot routes by the same CLASSIFICATION the cursor used — dead + `UNIT_DYNFLAG_LOOTABLE` —
    // not by the cursor kind (wow-re cursor-system.md §6: the right-click "routes the same hovered
    // object by the same classification"; its dead-unit row sends CMSG_LOOT). The loot cursor's
    // base mode is Pickup(8), which a live vendor also shows, so the kind alone can't name loot.
    //
    // **The fork's first test is the rider**, and we never carried it (decision 1851): `0x60bf98`
    // reads `[ecx+0x1fc]` off the *player's* descriptor block — the one still in `ecx` from
    // `0x60bee5` — and `jg 0x60c01f` skips the whole loot leg for a mounted player, landing on the
    // skin leg. That is a **fall-through, not a refusal**: no packet, no red line, no "You are
    // mounted". The cast and attack families of this same gate (0481) each announce themselves;
    // this one is silent, which is exactly why it could sit here unbuilt without ever looking
    // broken from the inside.
    // Step 0 of the fork, hoisted because the trace prints it too: a corpse the dispatcher will
    // actually treat as one.
    let dead_fork = target.is_some_and(|s| s.0.unit_is_dead() && !s.0.unit_dynflag_dead());
    let leg = dead_unit_leg(
        self_mounted,
        dead_fork,
        target.is_some_and(|s| s.0.unit_lootable()),
        target.is_some_and(|s| s.0.unit_flags() & cursor_mode::UNIT_FLAG_SKINNABLE != 0),
        learned.skinning.is_some(),
    );
    let (loot, skin) = (leg == DeadUnitLeg::Loot, leg == DeadUnitLeg::Skin);
    // The unit twin of the GO/corpse legs' one-line answer (tag `use`). Every refusal on this
    // ladder is silent from the outside — the range gray, the mounted fall-through, a corpse
    // somebody else owns — and they are indistinguishable on screen. One run says which.
    if benilla_assets::trace::enabled_for("use") {
        benilla_assets::trace::line(
            "use",
            &format!(
                "right-click unit guid={guid:#x} dead={} lootable={} skinnable={} mounted={self_mounted} leg={} cursor={:?} unable={}",
                target.is_some_and(|s| s.0.unit_is_dead()),
                target.is_some_and(|s| s.0.unit_lootable()),
                target.is_some_and(|s| s.0.unit_flags() & cursor_mode::UNIT_FLAG_SKINNABLE != 0),
                if attack {
                    "attack"
                } else if dead_fork {
                    leg.tag()
                } else {
                    // The alive branch (`0x60c162`): `CanInteract` → the service send. Not this
                    // fork's to name, and NOT mounted-gated — a rider talks to a flight master.
                    "service"
                },
                cursor.kind,
                cursor.unable
            ),
        );
    }
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
        // The actor-eligibility block — **silent here**, and that is 1851 correcting 0481. The
        // click still SELECTED (the commit above already ran, the ref's select-then-refuse order)
        // and the melee auto-draw and swing still never happen; what changed is that no red
        // `ERR_ATTACK_*` line shows any more.
        //
        // 0481 attached `0x612df0`'s Phase A ladder to this path. The §5 this session dispatched
        // found that validator has exactly three callers image-wide — pet-attack `0x4bd40d`, the
        // Attack action/keybind `0x6131aa`, and TryCast `0x6e4efb` — and the world right-click is
        // none of them: it runs `0x60c247 call 0x5ecb70`, an extent containing no `DisplayError`
        // at all. So all eight of those red lines belong to the bar and the pet command, never to
        // the click, and the fix is not to delete six of them but to move where they are asked
        // for. [`attack_actor_blocked`] is the same ladder with the message removed; the bar keeps
        // [`attack_actor_refusal`]. (The predicate is still `0x612df0`'s and not `0x5ecb70`'s own
        // overlapping set — a separate slice — but the SILENCE is the verified law, and the swing
        // is suppressed either way.)
        if crate::ui_action::attack_actor_blocked(self_store, me.map(|(_, g, _)| g.0)).is_some() {
            // refused — selection stands, no swing, and nothing is said
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
        //
        // **The unit leg's own stand-state gate** (`0x60bfb7 call [playerVtbl+0xa4]`, non-zero →
        // `0x60c007 push 0x85; call DisplayError 0x496720`) — the exact instruction pair the corpse
        // leg carries at `0x5d6c3b`/`0x496720(0x85)`, in the same fork, one test after the lootable
        // one. The corpse leg's comment has said since 1729 that "the unit loot branch's own copy of
        // it is unverified here"; it is the same `push 0x85` and the same virtual, read off the same
        // dispatcher this session's `0x60bf98` mounted gate came out of. So the caveat retires and
        // the two legs say the same thing: sitting, kneeling or asleep, the click raises the
        // client-local red `ERR_LOOT_NOTSTANDING` and sends nothing.
        if self_store.is_some_and(|s| s.0.unit_stand_state() != 0) {
            debug!("right-click loot: refused, not standing ({guid:#x})");
            ui_error_keys
                .0
                .push(crate::ui_action::UiError::key("ERR_LOOT_NOTSTANDING"));
        } else if !cursor.unable {
            debug!("right-click loot: {guid:#x}");
            let _ = seam.net.0.send(ClientCommand::Loot { guid });
            // The kneel is client-predicted AT THE SEND: the real client's `CMSG_LOOT` sender
            // (`0x5df253`) sets the loot-target latch `[player+0x1d28]` and plays Loot 50 before
            // any server response (decision 0515). Arm the latch the anim driver's loot leg
            // reads for the self unit; the release/refusal drops it.
            loot_latch.0 = Some(guid);
        }
    } else if skin {
        // A dead SKINNABLE corpse the loot leg declined (`0x60c01f`): cast our known Skinning spell
        // at it — the unit-side mirror of the GO lock split (0239; decision 0437's gathering
        // finish). The spell comes from the reference's own learn-time latch
        // ([`crate::ui_action::LearnedAbilities`] = `[0xb700e4]`, decision 0752), which is the same
        // thing the classifier gated the Skin cursor on. Ordinarily that means an unlootable
        // corpse; **while mounted it also means a still-lootable one**, and the cast then meets the
        // cast family's own mounted gate (`0x6094f0`'s reason `0x39`) and says "You are mounted" —
        // the one leg of this dispatcher where the rider is told anything at all.
        // Range rides the cursor's melee-reach gray, like loot.
        if !cursor.unable {
            if let Some(spell_id) = learned.skinning {
                let def = go_inputs
                    .spells
                    .as_ref()
                    .and_then(|s| s.catalog.get(spell_id));
                if crate::ui_action::cast_mounted_refusal(self_mounted, def) {
                    debug!("right-click skin: refused locally — mounted (0x39)");
                    cast_errors.push_local(spell_id, 0x39);
                } else {
                    debug!("right-click skin: {guid:#x} (spell {spell_id})");
                    let _ = seam.net.0.send(ClientCommand::CastSpell {
                        spell_id,
                        target: Some(guid),
                    });
                }
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
                })
            } else {
                Some(UiError {
                    key: "ERR_USE_LOCKED_WITH_SPELL_S",
                    fill_s: Some(name),
                    fill_d: None,
                })
            }
        }
        _ => Some(UiError::key("ERR_USE_CANT_OPEN")),
    }
}

/// Which leg the reference's unit interact dispatcher `0x60bea0` takes on a **dead** target — its
/// `0x60bf75` fork, transcribed. Pure, because every conjunct in it is a fact about two units'
/// wire state and nothing else, and because the conjunct this project was missing is invisible
/// from the outside (see [`DeadUnitLeg::Nothing`]).
///
/// The reference's order, and ours:
///
/// 0. **`0x60bf75`/`0x60bf86` — is it a corpse at all?** `HEALTH <= 0` **and** the target's
///    `UNIT_DYNFLAG_DEAD` (`[+0x224]` bit 5, the feign-death bit — decision 1022) **clear**; either
///    failing routes to the alive branch `0x60c162` instead. The caller passes this as `dead`.
/// 1. **`0x60bf98` — am I mounted?** `mov eax,[ecx+0x1fc]` on the *player's* descriptor block
///    (`UNIT_FIELD_MOUNTDISPLAYID`, the one mounted signal — decision 0481), `jg 0x60c01f`. A
///    rider does not loot. It is a jump into the skin leg, not a refusal: nothing is sent and
///    nothing is said.
/// 2. **`0x6003a0` — is it lootable?** (`UNIT_DYNFLAG_LOOTABLE`, plus the decay deadline the server
///    owns.) Yes ⇒ [`DeadUnitLeg::Loot`] and `CMSG_LOOT`.
/// 3. **`0x60c01f` — is it skinnable?** `UNIT_FIELD_FLAGS` bit 26, on the TARGET, plus the
///    learn-time latch `[0xb700e4]` that says we know a Skinning spell (decision 0752). Yes ⇒
///    [`DeadUnitLeg::Skin`] and the skin cast — which, being a cast, meets the mounted gate's cast
///    face (`0x6094f0`, reason `0x39`) and is where a rider finally gets told something.
/// 4. Otherwise nothing at all (`0x60c25f`, the bare epilogue).
///
/// Note what step 1 does **not** do: it does not consult the target. A lootable-*and*-skinnable
/// corpse — an already-looted one, which is the common case for a skinner — routes to Skin while
/// mounted and to Loot on foot, off the same two units.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum DeadUnitLeg {
    /// `CMSG_LOOT` (`0x60bff7` → `0x5df2a0`).
    Loot,
    /// The skin cast (`0x60c082` → `0x5f05e0`).
    Skin,
    /// **The silent leg.** No packet, no message, no state write — the dispatcher just returns.
    /// A mounted click on an ordinary lootable corpse lands here, and that silence is why the
    /// missing gate looked like working code: the loot window simply opened, as it would on foot.
    Nothing,
}

impl DeadUnitLeg {
    /// The one-word tag the `use` trace prints.
    fn tag(self) -> &'static str {
        match self {
            Self::Loot => "loot",
            Self::Skin => "skin",
            Self::Nothing => "none",
        }
    }
}

fn dead_unit_leg(
    mounted: bool,
    dead: bool,
    lootable: bool,
    skinnable: bool,
    know_skinning: bool,
) -> DeadUnitLeg {
    if !dead {
        return DeadUnitLeg::Nothing;
    }
    if !mounted && lootable {
        return DeadUnitLeg::Loot;
    }
    if skinnable && know_skinning {
        return DeadUnitLeg::Skin;
    }
    DeadUnitLeg::Nothing
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

/// Drain the UI's **selection asks** — `TargetUnit(token)`, `AssistUnit(token)` and
/// `TargetLastEnemy()` — and commit each through the shared SetSelection path ([`scan::commit`]).
///
/// One drain for the three because the reference has one function for them: `TargetUnit
/// 0x4899d0`, `AssistUnit 0x489b80`, `TargetLastEnemy` and `TargetLastTarget` all reach selection
/// through the "select if it resolves" helper `0x489a40` (wow-re
/// `object-layer/scratch/selection-attack-seam.md` §1), whose three arms — resolves → commit;
/// doesn't resolve but is on the roster → commit anyway; **neither, including guid 0 → a bare
/// `ret`** — are the same for every caller. That third arm is the one worth naming: a token,
/// basis or remembered guid that does not resolve is a **no-op, not a deselect**. Draining them
/// in one ordered queue also keeps their relative order, which a macro can observe.
///
/// `TargetUnit` callers: the player frame's left-click (`TargetUnit("player")`), the party frames'
/// (`TargetUnit("partyN")`, decision 0434 phase 5), the TARGETSELF binding. Only tokens resolving
/// to a STREAMED unit act: `"player"` → our avatar; `"target"` → the current selection (a dedup
/// no-op); `"partyN"`/`"raidN"` → that roster slot when its entity is in range (an out-of-range
/// member needs the guid-only selection the phase-4 out-of-range slice owns — until then the click
/// no-ops, like the real client on a nonexistent unit); `"pet"` → the bar's cached pet guid
/// (decision 0990, the pet frame's left click); `"targettarget"` → the selection's own
/// `UNIT_FIELD_TARGET` (decision 1576, the ToT frame's). Everything else (mouseover/name) waits
/// for its wire.
///
/// `AssistUnit` resolves the same token and then runs the shared assist tail
/// ([`super::by_name::SelectCommit::assist`]) — the basis's own `UNIT_FIELD_TARGET`, which is the
/// hop `"targettarget"` already makes for the ToT frame. `TargetLastEnemy` skips the token
/// grammar entirely and reads [`scan::LastEnemy`].
pub(super) fn selection_requests(
    script: Option<NonSendMut<UiScript>>,
    // The one unit-token resolver (`crate::ui_unit::UnitTokens`) — the arms this drain used to
    // spell out inline. It is shared with the reach feed precisely so `TargetUnit("target")` and
    // `CheckInteractDistance("target", …)` can never mean two different units (B304).
    tokens: crate::ui_unit::UnitTokens,
    // `TargetLastEnemy`'s memory — `[0xb4e2e8]/[0xb4e2ec]`, stamped by `scan::remember_last_enemy`.
    last_enemy: Res<scan::LastEnemy>,
    // The one SetSelection tail, shared with `/target` and `/assist` (decision 1583). It carries
    // the classification too, which is why this drain no longer states one: hand-stating it here
    // is exactly how a `false` that the binary refutes got written down.
    mut commit: super::by_name::SelectCommit,
) {
    let Some(mut script) = script else {
        return;
    };
    let requests = script.take_selection_requests();
    if requests.is_empty() {
        return;
    }
    for request in requests {
        match request {
            // Resolved before the commit borrows the selection mutably. An unstreamed unit — an
            // out-of-range party member, a despawned pet — resolves to nothing and no-ops.
            SelectionRequest::Unit(token) => {
                if let Some((entity, guid)) = tokens.resolve(&token, &commit.selection) {
                    commit.commit(entity, guid);
                }
            }
            // `0x489ba9 call 0x515940(token)` — the same token grammar, then the shared tail. An
            // unresolvable token is silent here: the reference emits game-message `0xb8`, whose
            // id→string table is runtime-populated BSS wow-re could not statically recover — the
            // known deviation `TargetByName` already carries on this path.
            SelectionRequest::Assist(token) => match tokens.resolve(&token, &commit.selection) {
                Some((basis, _)) => commit.assist(basis, "AssistUnit"),
                None => info!("assist (AssistUnit): \"{token}\" names nothing; silent no-op"),
            },
            // `0x489b45` reads the last-attackable pair and hands it to the same `0x489a40`.
            // **Empty memory is a no-op, and that is derived now**: the shim `0x489b40` is
            // thirteen bytes with no emptiness test at all, where `TargetLastTarget 0x489b00`
            // really does reach `0x493540(0,0)` and deselect. The two shims differ exactly here
            // (wow-re `targeting-friend-and-lastenemy.md`, §5 trio — dispatched from this work).
            SelectionRequest::LastEnemy => {
                let Some(guid) = last_enemy.0 else {
                    info!("TargetLastEnemy: nothing hostile has been targeted yet; no-op");
                    continue;
                };
                match tokens.held(guid) {
                    Some((entity, guid)) => commit.commit(entity, guid),
                    // The stale-guid arm: the remembered unit has streamed out or despawned. The
                    // memory is deliberately kept (the reference never clears its globals either)
                    // and this is a bare no-op, never a deselect.
                    None => info!(
                        "TargetLastEnemy: {guid:#x} is no longer streamed; target left untouched"
                    ),
                }
            }
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

    /// The dead-target fork of `0x60bea0`, and the conjunct that was missing: **the rider does not
    /// loot** (`0x60bf98`). The director found it from the saddle, over a Young Wolf, with the loot
    /// window open — the one shape of bug that never looks like one from inside the client.
    #[test]
    fn a_mounted_player_never_takes_the_loot_leg() {
        // On foot, a lootable corpse loots. This is the control: the fix must not touch it.
        assert_eq!(
            dead_unit_leg(false, true, true, false, false),
            DeadUnitLeg::Loot
        );
        // Mounted, the same corpse: the loot leg is skipped and, with nothing to skin, the
        // dispatcher returns having done nothing. Silent — no packet and no red line.
        assert_eq!(
            dead_unit_leg(true, true, true, false, false),
            DeadUnitLeg::Nothing
        );
        // Mounted over a corpse that is BOTH lootable and skinnable (an already-looted body a
        // skinner rides up to): the fall-through reaches the skin leg, which `cursor.kind` alone
        // could never have routed — the classifier names Skin only on an unlootable corpse.
        assert_eq!(
            dead_unit_leg(true, true, true, true, true),
            DeadUnitLeg::Skin
        );
        // ...and on foot the very same body loots instead. Step 1 never consults the target.
        assert_eq!(
            dead_unit_leg(false, true, true, true, true),
            DeadUnitLeg::Loot
        );
    }

    /// The rest of the fork's ladder, so the mounted conjunct above cannot be "fixed" by
    /// collapsing a leg it shares with the others.
    #[test]
    fn the_dead_fork_keeps_its_other_three_gates() {
        // A live unit is not this fork's business at all (`0x60bf75 jg` → the alive branch).
        assert_eq!(
            dead_unit_leg(false, false, true, true, true),
            DeadUnitLeg::Nothing
        );
        // Dead, unlootable, skinnable, and we know the trade → Skin (`0x60c01f`).
        assert_eq!(
            dead_unit_leg(false, true, false, true, true),
            DeadUnitLeg::Skin
        );
        // The learn-time latch `[0xb700e4]` is the leg's second precondition (0752): a
        // non-skinner gets nothing on the same corpse.
        assert_eq!(
            dead_unit_leg(false, true, false, true, false),
            DeadUnitLeg::Nothing
        );
        // A plain looted corpse: nothing, mounted or not.
        for mounted in [false, true] {
            assert_eq!(
                dead_unit_leg(mounted, true, false, false, true),
                DeadUnitLeg::Nothing
            );
        }
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

    /// **The selection queue, end to end** — Lua in, `Selection` out — over the four ways it is
    /// reachable from a player macro. Every one of them is a shipped binding body: `AssistUnit`
    /// is ASSISTTARGET's (default `F`), `TargetLastEnemy` is TARGETLASTHOSTILE's (default `G`).
    ///
    /// What it pins, claim by claim:
    /// * `AssistUnit("target")` on a basis that has a target selects **that unit** — the shared
    ///   assist tail's one hop off `UNIT_FIELD_TARGET`;
    /// * a basis with **no** target is a silent no-op — the reference's tail bails before any send;
    /// * a **garbage token** resolves to nothing and is a no-op, never a deselect (`0x489a40`'s
    ///   arm 3 is a bare `ret`) — and, being reachable from any macro, must not panic;
    /// * `TargetLastEnemy()` re-selects the remembered guid, and with a **stale** one — the unit
    ///   despawned or streamed out — leaves the target exactly where it was.
    #[test]
    fn the_selection_queue_runs_assist_and_last_enemy_without_ever_deselecting() {
        use crate::net::NetCommands;
        use benilla_ui::script::UiScript;
        use bevy::ecs::system::RunSystemOnce;

        const ME: u64 = 1;
        const BASIS: u64 = 0xB0A2;
        const VICTIM: u64 = 0xC0DE;
        const GONE: u64 = 0x6017;
        // `UNIT_FIELD_TARGET` is a 2-field guid at index 16; HEALTH/MAXHEALTH keep the units live.
        let store =
            |pairs: &[(u16, u32)]| ObjectStore(benilla_protocol::ObjectFields::from_pairs(pairs));

        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut world = World::new();
        world.insert_resource(NetCommands(tx));
        world.init_resource::<crate::ui_cast::QueuedMeleeSpell>();
        world.init_resource::<crate::ui_action::AutoRepeatActive>();
        world.init_resource::<Messages<crate::creature_anim::SheathRequest>>();
        world.init_resource::<crate::ui_party::GroupState>();
        world.init_resource::<crate::net::GuidIndex>();
        world.init_resource::<crate::net::Reputations>();
        world.init_resource::<Selection>();
        world.init_resource::<scan::LastEnemy>();
        world.insert_non_send_resource(UiScript::new().expect("a bare VM"));

        world.spawn((SelfPlayer, Guid(ME), store(&[(22, 100), (28, 100)])));
        // The basis is pointing at the victim; the victim points at nobody.
        let basis = world
            .spawn((
                Guid(BASIS),
                store(&[(22, 100), (28, 100), (16, VICTIM as u32)]),
            ))
            .id();
        let victim = world
            .spawn((Guid(VICTIM), store(&[(22, 100), (28, 100)])))
            .id();
        let index = &mut world.resource_mut::<crate::net::GuidIndex>().0;
        index.insert(BASIS, basis);
        index.insert(VICTIM, victim);

        let run = |world: &mut World, lua: &str| {
            world
                .non_send_resource_mut::<UiScript>()
                .eval::<()>(lua)
                .expect("the binding runs");
            world
                .run_system_once(selection_requests)
                .expect("the drain runs as a one-shot system");
            world.resource::<Selection>().guid
        };
        let set = |world: &mut World, target: Option<(Entity, u64)>| {
            let mut sel = world.resource_mut::<Selection>();
            sel.target = target.map(|(e, _)| e);
            sel.guid = target.map(|(_, g)| g);
        };

        // Assist the current target: one hop onto ITS target.
        set(&mut world, Some((basis, BASIS)));
        assert_eq!(run(&mut world, r#"AssistUnit("target")"#), Some(VICTIM));

        // The victim targets nobody — assisting it is a silent no-op, not a deselect.
        assert_eq!(run(&mut world, r#"AssistUnit("target")"#), Some(VICTIM));

        // A token that names nothing: a no-op, and above all not a panic.
        assert_eq!(run(&mut world, r#"AssistUnit("nosuchunit")"#), Some(VICTIM));
        assert_eq!(run(&mut world, r#"TargetUnit("nosuchunit")"#), Some(VICTIM));

        // TargetLastEnemy with nothing remembered: also a no-op.
        assert_eq!(run(&mut world, "TargetLastEnemy()"), Some(VICTIM));

        // Remember the victim, drop the target, and press it: back onto the victim.
        world.resource_mut::<scan::LastEnemy>().0 = Some(VICTIM);
        set(&mut world, None);
        assert_eq!(run(&mut world, "TargetLastEnemy()"), Some(VICTIM));

        // A STALE memory — the unit despawned, so the guid is in no index. The target stays put;
        // the reference's helper returns without touching the selection.
        world.resource_mut::<scan::LastEnemy>().0 = Some(GONE);
        assert_eq!(run(&mut world, "TargetLastEnemy()"), Some(VICTIM));
        set(&mut world, None);
        assert_eq!(run(&mut world, "TargetLastEnemy()"), None);
    }
}
