//! The click/USE router — the input half of targeting: a clean left-click selects the
//! [`super::Hovered`] unit, a clean right-click dispatches the context action (attack, NPC
//! interact, GameObject USE with the lock/refusal ladder), and the UI's `TargetUnit` requests +
//! Esc drain into the same [`super::Selection`]. Split from `mod.rs` (which keeps the state
//! resources and the plugin) along the state-vs-input seam; the systems here are registered by
//! [`super::TargetPlugin`] in the target chain after the hover picks and the cursor classifier.

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

/// On a clean left-click (a [`WorldClick`], never a drag), select whatever unit is [`Hovered`] and
/// inform the server; a click on empty ground / a non-unit clears the target — except a click on
/// NOTHING (sky — no occlusion-ray hit) while a payload is held: the reference's nothing-leg
/// deselect is no-payload-gated (`0x492d30`'s local flag test), while the terrain leg deselects
/// regardless of a surviving spell/action payload (`0x5e03bb` — decisions 0571 + 0574). Skipped
/// while the inspector is armed (left-click is its copy affordance).
#[allow(clippy::too_many_arguments)] // one Bevy system's full input set
pub(super) fn select_on_click(
    mut clicks: MessageReader<WorldClick>,
    inspect: Res<InspectMode>,
    hovered: Res<Hovered>,
    cursor: Res<WorldCursor>,
    mut selection: ResMut<Selection>,
    net: Res<NetCommands>,
    self_q: Query<(&Guid, Has<Engaged>), With<SelfPlayer>>,
    payload_held: Res<crate::ui_script::CursorPayloadHeld>,
    occlusion: Res<PickOcclusion>,
    mut greeting: MessageWriter<crate::sound::NpcGreetingRequest>,
) {
    // Drain the frame's clicks; act only if there was one and the inspector isn't holding left-click.
    let clicked = clicks.read().last().is_some();
    if !clicked || inspect.enabled {
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
                &net,
                entity,
                guid,
                engaged,
                self_guid,
                cursor.kind == cursor_mode::CursorKind::Attack,
            );
        }
        // Clicked nothing targetable → deselect (only sends the clear if we actually had a
        // target). The one exception is a payload held over NOTHING (sky): the ref's
        // nothing-leg deselect is no-payload-gated. A terrain click deselects even with a
        // surviving spell/action payload (`0x5e03bb` is target-gated, not payload-gated) —
        // an item over terrain never reaches here (its press was consumed for the drop).
        _ => {
            if !payload_held.0 || occlusion.distance.is_finite() {
                clear(&mut selection, &net, engaged);
            }
        }
    }
}

/// EmoteTalk's `AnimationData.dbc` id (60) — the one-shot talk the client plays on our avatar when
/// we interact with an NPC. Its WeaponFlags `0x10` drives the per-animation sheath reconcile to stow
/// the drawn weapon — a committed change that persists after the talk (nothing restores it; the
/// interact stow is the talk emote, not a sheath wire — decisions 0080/0081).
const EMOTE_TALK: u16 = 60;

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
#[allow(clippy::too_many_arguments)]
pub(super) fn act_on_right_click(
    mut clicks: MessageReader<WorldRightClick>,
    hovered: Res<Hovered>,
    hovered_object: Res<HoveredObject>,
    cursor: Res<WorldCursor>,
    mut selection: ResMut<Selection>,
    net: Res<NetCommands>,
    self_player: Query<(Entity, &Guid, Has<Engaged>), With<SelfPlayer>>,
    mut sheath: MessageWriter<crate::creature_anim::SheathRequest>,
    mut emote: MessageWriter<crate::creature_anim::EmoteAnim>,
    mut play_seq: ResMut<crate::creature_anim::PlaySeq>,
    // The GameObject lock-routing inputs (decisions 0239 + 0545) as one [`GoLockInputs`]
    // (the 16-SystemParam ceiling).
    mut go_inputs: GoLockInputs,
    player_actions: Res<crate::ui_action::PlayerActions>,
    stores: Query<&ObjectStore>,
    // One tuple param (the 16-SystemParam ceiling): the red-error keys + the reason-coded cast
    // line (the opener cast's local totem refusal, decision 0552) + the loot-target latch
    // the loot branch arms (decision 0515), and the mailbox session the mailbox branch opens
    // (decision 0544).
    ui_feedback: (
        ResMut<crate::ui_action::UiErrorKeys>,
        ResMut<crate::ui_action::CastErrors>,
        ResMut<crate::ui_loot::LootLatch>,
        ResMut<crate::ui_mail::MailOpen>,
    ),
) {
    let (mut ui_error_keys, mut cast_errors, mut loot_latch, mut mail) = ui_feedback;
    if clicks.read().last().is_none() {
        return;
    }
    // A GameObject is the nearest thing under the cursor → use it (decision 0236), and never fall
    // through to unit handling: a GO is not selectable, and a right-click on it acts on the GO or
    // does nothing (out of range / not usable). `classify_cursor` already resolved highlightable +
    // usable into the cursor — **any** non-Point GO cursor that isn't grayed (the Interact gear, or a
    // data-named Mail / Mine / GatherHerbs / PickLock) means highlightable AND usable (wow-re cursor-
    // system §4a: mode 0 ⇔ not highlightable, Unable* ⇔ not usable); Point or unable ⇒ don't send.
    // The action itself is the lock split ([`resolve_go_action`]), independent of the cursor shape —
    // which is exactly why the gate can no longer key on the Interact gear alone (that coupling was
    // decision 0243's known interim). The USE is verified for the right button (wow-re
    // cursor-system.md §6); a left-click USE folds in with the dispatched RE verdict.
    if go_is_nearest(&hovered, &hovered_object) {
        if cursor.kind != cursor_mode::CursorKind::Point && !cursor.unable {
            if let Some(guid) = hovered_object.guid {
                // Mailbox (GO type 19): open the mail window client-side (decision 0544), BEFORE the
                // lock fork (a mailbox is never locked). The wow-re §5 confirms the MAILBOX use
                // handler overrides the shared use-sender to a LOCAL open — it sends NO packet (no
                // CMSG_GAMEOBJ_USE); the window's own MAIL_SHOW → CheckInbox drives the first
                // CMSG_GET_MAIL_LIST. Re-clicking just re-shows (the session is already set).
                let go_store = hovered_object.target.and_then(|e| stores.get(e).ok());
                if go_store
                    .is_some_and(|s| s.0.gameobject_type_id() == cursor_mode::GO_TYPE_MAILBOX)
                {
                    debug!("right-click mailbox: open mail window {guid:#x}");
                    mail.click(guid);
                    return;
                }
                // Branch on the lock (decisions 0239 + 0545): a lockless GameObject is USEd; a
                // lockable one casts a known OPEN_LOCK spell at it; an unopenable lock shows the
                // client-local red toast — "Requires Herbalism", "Requires Mining 100", "Requires
                // <key item>" — and sends nothing (the ref's validate/error block `0x5f3427..`
                // fires `DisplayError` with no packet; wow-re cursor-system.md §8.4/§8.8).
                let me_store = self_player
                    .single()
                    .ok()
                    .and_then(|(e, _, _)| stores.get(e).ok());
                match resolve_go_action(
                    guid,
                    &mut go_inputs,
                    &player_actions.spells,
                    go_store,
                    me_store,
                    &net,
                ) {
                    GoAction::Use => {
                        debug!("right-click gameobject use: {guid:#x}");
                        let _ = net.0.send(ClientCommand::GameObjUse { guid });
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
                        let _ = net.0.send(ClientCommand::CastSpellGameObject {
                            spell_id,
                            go_guid: guid,
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
        .is_ok_and(|s| s.0.unit_is_dead() && s.0.unit_lootable());
    let me = self_player.single().ok();
    // The one SetSelection law ([`scan::commit`]): dedup + selection + the engaged-switch
    // stop→select→re-swing. The Attack cursor kind is Attack `0x5ecb70`'s new-target validation
    // (alive + reaction ≤ neutral) — a mid-combat click on a vendor/corpse switches and stops,
    // it never swings at them.
    let outcome = scan::commit(
        &mut selection,
        &net,
        entity,
        guid,
        me.is_some_and(|(_, _, e)| e),
        me.map(|(_, g, _)| g.0),
        attack,
    );
    if attack {
        // The mounted attack block (decision 0481, the shared `0x613039` refusal): the click
        // still SELECTED — the commit above already ran, matching the ref's select-then-refuse
        // order — but the melee auto-draw and the swing never happen; the red
        // ERR_ATTACK_MOUNTED line shows instead.
        if crate::ui_action::attack_mounted_refusal(
            me.and_then(|(e, _, _)| stores.get(e).ok()),
            &mut ui_error_keys,
        ) {
            // refused — selection stands, no swing
        } else {
            debug!("right-click attack: {guid:#x}");
            // Auto-draw through the anim layer's ONE setter (decision 0080) — a SNAP, no
            // ceremony, no sound: the attack path passes `(newState=1, bInstant=1,
            // bFireEvent=1)` at `0x5ecd80` (wow-re `sheath-policy.md`). The setter's
            // idempotency is the client's own "no-op if already melee".
            if let Some((e, _, _)) = me {
                sheath.write(crate::creature_anim::SheathRequest {
                    entity: e,
                    state: 1,
                    ceremony: false,
                });
            }
            // The commit's engaged-switch law may already have re-pointed the swing at this
            // guid (the ref's `0x5ecb70` skips a second send while the attack lock is set) —
            // only the fresh attack-start still owes its ATTACKSWING.
            if !outcome.swung {
                let _ = net.0.send(ClientCommand::AttackSwing { guid });
            }
        }
    } else if loot {
        // A dead unit carrying UNIT_DYNFLAG_LOOTABLE (the Pickup loot cursor, decision 0084): open
        // its loot (`CMSG_LOOT`). Range-gated like the interact branch — the cursor grays a corpse
        // beyond the melee interact reach (`unable`), and we don't send then (no auto-approach yet).
        // No EmoteTalk: looting is not an NPC interaction, the corpse plays no talk.
        if !cursor.unable {
            debug!("right-click loot: {guid:#x}");
            let _ = net.0.send(ClientCommand::Loot { guid });
            // The kneel is client-predicted AT THE SEND: the real client's `CMSG_LOOT` sender
            // (`0x5df253`) sets the loot-target latch `[player+0x1d28]` and plays Loot 50 before
            // any server response (decision 0515). Arm the latch the anim driver's loot leg
            // reads for the self unit; the release/refusal drops it.
            loot_latch.0 = Some(guid);
        }
    } else if cursor.kind == cursor_mode::CursorKind::Skin {
        // A dead, unlootable, SKINNABLE corpse (the classifier's own gate): cast our known
        // Skinning spell at it — the unit-side mirror of the GO lock split (0239; decision 0437's
        // gathering finish). The spell is discovered from the known set (`Effect[0] ==
        // SPELL_EFFECT_SKINNING`), never hardcoded — the same no-literal law as the OPEN_LOCK
        // scan; a player without Skinning sends nothing (the "requires" toast is 0239's own
        // later-polish note). Range rides the cursor's melee-reach gray, like loot.
        if !cursor.unable {
            let skinner = go_inputs.spells.as_ref().and_then(|s| {
                player_actions
                    .spells
                    .iter()
                    .find(|&&id| {
                        s.catalog
                            .get(id)
                            .is_some_and(|d| d.effect_1 == benilla_formats::SPELL_EFFECT_SKINNING)
                    })
                    .copied()
            });
            match skinner {
                Some(spell_id) => {
                    debug!("right-click skin: {guid:#x} (spell {spell_id})");
                    let _ = net.0.send(ClientCommand::CastSpell {
                        spell_id,
                        target: Some(guid),
                    });
                }
                None => debug!("right-click skin: {guid:#x} — no known skinning spell"),
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
            .map(|s| s.0.unit_npc_flags())
            .unwrap_or(0);
        if let Some(cmd) = interact_command(cursor.kind, guid, npc_flags) {
            debug!("right-click interact: {guid:#x} ({:?})", cursor.kind);
            let _ = net.0.send(cmd);
            // Play EmoteTalk on our avatar — the reconcile stows a drawn weapon, persistently
            // (decisions 0080/0081; no sheath wiring here).
            if let Some((e, _, _)) = me {
                emote.write(crate::creature_anim::EmoteAnim {
                    entity: e,
                    anim_id: EMOTE_TALK,
                    seq: play_seq.next(),
                });
            }
        }
    }
}

/// The GameObject lock chain's full data set, as ONE [`SystemParam`] (decisions 0239 + 0545):
/// the ask-once GO-template cache, Lock.dbc + LockType.dbc, the spell + skill-line catalogs, and
/// the ask-once item-template cache (key-item names for the "Requires <key>" toast). The Option
/// members are absent without client data.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct GoLockInputs<'w> {
    templates: Res<'w, crate::go_templates::GameObjectTemplates>,
    locks: Option<Res<'w, crate::go_templates::Locks>>,
    lock_types: Option<Res<'w, crate::go_templates::LockTypes>>,
    spells: Option<Res<'w, crate::ui_action::Spells>>,
    skill_lines: Option<Res<'w, crate::ui_spellbook::SkillLines>>,
    items: ResMut<'w, crate::items::Items>,
}

/// The right-click action a hovered GameObject resolves to (decisions 0239 + 0545) — chosen by
/// its lock.
enum GoAction {
    /// No lock (or no lock data): `CMSG_GAMEOBJ_USE` — door / lever / quest object / mailbox /
    /// unlocked chest.
    Use,
    /// A lock we can open: cast this known `OPEN_LOCK` spell at the object (chest / vein / herb / a
    /// picked lock).
    OpenLock(u32),
    /// A lock present that we cannot open — the client-local refusal (§8.4: `DisplayError`, **no
    /// packet**). `Some` = the red toast to queue; `None` = the ref is silent for this case too.
    Refuse(Option<crate::ui_action::UiError>),
}

/// What we know about a key-item lock slot's key when routing a refusal ([`route_lock_refusal`]).
enum KeyFact {
    /// The key is in our bags — the ref would open with it (the key-item ON_USE cast, deferred);
    /// never toast "Requires <key>" at a player holding the key.
    Held,
    /// Not held; the item template names it ("Requires Shadowforge Key").
    Named(String),
    /// Not held and the template isn't cached yet — the ref's `GetRecord` miss is silent (§8.8
    /// `0xde`); our ask-once query is away, so a later click names it.
    Unknown,
}

/// Resolve a hovered GameObject's right-click action from its lock (decision 0239). Not-yet-queried
/// or no `Lock.dbc` → treat as lockless (`Use`): the stream-in query makes "not cached" a rare race,
/// and `Use` is both the correct lockless action and a harmless no-op on a chest whose template is
/// still in flight. A lockable object matches the lock's **skill** slots against the player's
/// **known** spells (an `OPEN_LOCK` whose `EffectMiscValue` is the slot's `LockType`) — "Opening"
/// for keyless chests, Mining / Herb Gathering / Pick Lock for skill locks — then rank-gates the
/// cast exactly as the ref's resolver does (`0x5f850f`): a known-but-under-rank opener refuses
/// **client-side** (the "Requires Mining 100" toast), it never reaches the wire. An unresolvable
/// rank (skill lines / self store not streamed yet) fails OPEN — cast and let the server re-check
/// (vmangos answers `LOW_CASTLEVEL`), never a wrongly-refused click. The effective rank is
/// value + both bonuses (vmangos `GetSkillValue`; the ref compare `0x6e3760`'s exact bonus
/// handling is unpinned — INTERIM, decision 0545). Item-key locks aren't openable yet (the
/// key-item ON_USE cast is deferred), but a missing key names itself ([`KeyFact`]).
fn resolve_go_action(
    guid: u64,
    inputs: &mut GoLockInputs,
    known: &std::collections::HashSet<u32>,
    go_store: Option<&ObjectStore>,
    me_store: Option<&ObjectStore>,
    net: &NetCommands,
) -> GoAction {
    let (spells, skill_lines, lock_types) = (
        inputs.spells.as_deref(),
        inputs.skill_lines.as_deref(),
        inputs.lock_types.as_deref(),
    );
    let items = &mut *inputs.items;
    let Some(tmpl) = inputs.templates.get(guid) else {
        return GoAction::Use;
    };
    let Some(locks) = inputs.locks.as_deref() else {
        return GoAction::Use;
    };
    // A lockId whose row is missing (or all-empty) is "no lock" — the ref resolver's `0x5f8180`
    // null → FALSE with spell 0 → `CMSG_GAMEOBJ_USE` (§8.4 C6).
    if !locks.0.is_locked(tmpl.lock_id) {
        return GoAction::Use;
    }
    let Some(slots) = locks.0.slots(tmpl.lock_id) else {
        return GoAction::Use;
    };
    // The resolver scan (§8.4 `0x5f83d0`, §8.8's timing): remember the FIRST known matching
    // opener unconditionally — the ref writes the spell-id out-param at `0x5f84f8` BEFORE the
    // rank test, and that nonzero-ness is the "Requires Herbalism" (0xdf) vs "Requires Mining
    // 100" (0xe0) discriminator — then rank-gate the actual cast.
    let mut matched: Option<u32> = None;
    if let Some(spells) = spells {
        for slot in slots
            .iter()
            .filter(|s| s.key_type == benilla_formats::LOCK_KEY_SKILL)
        {
            let Some(&spell_id) = known.iter().find(|&&id| {
                spells.catalog.get(id).and_then(|d| d.open_lock_type) == Some(slot.index)
            }) else {
                continue;
            };
            matched.get_or_insert(spell_id);
            let rank = skill_lines
                .and_then(|sl| sl.catalog.spell_to_line(spell_id))
                .and_then(|line| me_store.map(|s| effective_skill_rank(s, line)))
                .unwrap_or(u32::MAX);
            if rank >= slot.skill {
                return GoAction::OpenLock(spell_id);
            }
        }
    }
    // Unopenable — gather the facts the toast routing reads (§8.8 keys off Lock.dbc slot 0).
    let slot0 = slots[0];
    let key = if slot0.key_type == benilla_formats::LOCK_KEY_ITEM {
        if me_store.is_some_and(|s| crate::ui_items::count_of(&s.0, items, slot0.index) > 0) {
            KeyFact::Held
        } else if let Some(info) = items.template(slot0.index, 0, net) {
            KeyFact::Named(info.name.clone())
        } else {
            KeyFact::Unknown
        }
    } else {
        KeyFact::Unknown
    };
    GoAction::Refuse(route_lock_refusal(
        &slot0,
        matched.is_some(),
        go_store.is_some_and(|s| s.0.gameobject_flags() & 0x2 != 0),
        go_store.map_or(-1, |s| s.0.gameobject_type_id()),
        go_store.map_or(0, |s| s.0.gameobject_level()),
        lock_types.and_then(|lt| lt.0.name(slot0.index)),
        key,
    ))
}

/// The player's effective rank on a skill line — descriptor value + both bonuses, floored at 0
/// (vmangos `Player::GetSkillValue`'s sum; the ref's `0x6e3760` bonus handling is unpinned —
/// INTERIM, decision 0545). `0` when the line isn't in the skill block.
fn effective_skill_rank(store: &ObjectStore, line: u32) -> u32 {
    for i in 0..benilla_protocol::messages::PLAYER_SKILL_SLOTS {
        if let Some(s) = store.0.player_skill(i) {
            if u32::from(s.skill_id) == line {
                let v = i32::from(s.value) + i32::from(s.temp_bonus) + i32::from(s.perm_bonus);
                return v.max(0) as u32;
            }
        }
    }
    0
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
            KeyFact::Held | KeyFact::Unknown => None,
            KeyFact::Named(name) => Some(UiError {
                key: "ERR_USE_LOCKED_WITH_ITEM_S",
                fill_s: Some(name),
                fill_d: None,
            }),
        },
        benilla_formats::LOCK_KEY_SKILL => {
            let name = lock_type_name.unwrap_or("UNKNOWN").to_string();
            if opener_known {
                let required = if slot0.skill != 0 {
                    slot0.skill
                } else {
                    go_level * 5
                };
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
        // Buy(3) is banker OR auctioneer (the ladder's shared leg). A pure banker (bit 8 the
        // lowest service bit — the only way Buy classified) opens the bank directly
        // (`CMSG_BANKER_ACTIVATE`, decision 0604); the auctioneer stays on the gossip fallback
        // (its window is its own arc).
        CursorKind::Buy if npc_flags & cursor_mode::npc_flags::BANKER != 0 => {
            Some(ClientCommand::BankerActivate { guid })
        }
        CursorKind::Speak | CursorKind::Buy | CursorKind::Trainer | CursorKind::Interact => {
            Some(ClientCommand::GossipHello { guid })
        }
        // Repair and Cast are UI-overlay modes only; Inspect and the data-named GameObject
        // cursors (Mail/Mine/GatherHerbs/PickLock) belong to the GO branch, which routes above
        // and never reaches this NPC-service dispatch — a unit never classifies to any of these.
        CursorKind::Point
        | CursorKind::Attack
        | CursorKind::Skin
        | CursorKind::Repair
        | CursorKind::Inspect
        | CursorKind::Mail
        | CursorKind::Mine
        | CursorKind::GatherHerbs
        | CursorKind::PickLock
        | CursorKind::Cast => None,
    }
}

/// Drain the ESC chain's `ClearTarget()` (the last leg of `BenillaOnEscape` — the ref's
/// `ToggleGameMenu` order, `UIParent.lua:1492`) and commit the deselect. The target drops ONLY
/// when nothing earlier in the chain ate the press: a mid-cast ESC cancels the cast instead, an
/// open window closes instead — the two-press behavior the raw-key clear this replaces couldn't
/// express (it ran beside the chain, so the first ESC both canceled the cast AND dropped the
/// target — the director's 0449 report). EditBox precedence rides the chain too: a focused box
/// consumes ESCAPE before `BenillaOnEscape` ever runs.
pub(super) fn clear_target_requests(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    mut selection: ResMut<Selection>,
    net: Res<NetCommands>,
    engaged: Query<(), (With<Engaged>, With<SelfPlayer>)>,
) {
    let Some(mut script) = script else {
        return;
    };
    if script.take_target_clear() {
        clear(&mut selection, &net, !engaged.is_empty());
    }
}

/// Drain the UI's `TargetUnit(token)` requests and commit each through the shared SetSelection path
/// ([`scan::commit`]) — the app half of the reference's `TargetUnit` Lua shim. Callers: the player
/// frame's left-click (`TargetUnit("player")`) and the party frames' (`TargetUnit("partyN")`,
/// decision 0434 phase 5). Only tokens resolving to a STREAMED unit act: `"player"` → our avatar;
/// `"target"` → the current selection (a dedup no-op); `"partyN"` → that roster slot when its
/// entity is in range (an out-of-range member needs the guid-only selection the phase-4
/// out-of-range slice owns — until then the click no-ops, like the real client on a nonexistent
/// unit). Everything else (pet/mouseover/name) waits for its wire.
pub(super) fn target_unit_requests(
    script: Option<NonSendMut<UiScript>>,
    mut selection: ResMut<Selection>,
    net: Res<NetCommands>,
    self_q: Query<(Entity, &Guid, Has<Engaged>), With<SelfPlayer>>,
    group: Res<crate::ui_party::GroupState>,
    index: Res<crate::net::GuidIndex>,
) {
    let Some(mut script) = script else {
        return;
    };
    let requests = script.take_target_requests();
    if requests.is_empty() {
        return;
    }
    let me = self_q.single().ok();
    let engaged = me.is_some_and(|(_, _, e)| e);
    let self_guid = me.map(|(_, g, _)| g.0);
    for token in requests {
        let resolved = match token.as_str() {
            "player" => me.map(|(e, g, _)| (e, g.0)),
            "target" => selection.target.zip(selection.guid),
            t => t
                .strip_prefix("party")
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|n| (1..=4).contains(n))
                .and_then(|n| group.party_slots().nth(n - 1).map(|m| m.guid))
                .and_then(|g| index.0.get(&g).map(|e| (*e, g))),
        };
        if let Some((entity, guid)) = resolved {
            // Both resolvable tokens are self or the current target, so the engaged-switch law
            // can only ever STOP here (the self exception / the dedup) — never re-swing.
            scan::commit(
                &mut selection,
                &net,
                entity,
                guid,
                engaged,
                self_guid,
                false,
            );
        }
    }
}

/// Drop the current target and tell the server (`CMSG_SET_SELECTION` guid 0). A no-op when nothing is
/// selected, so it never sends a redundant clear. Losing the target also ends melee auto-attack when
/// one is running (`engaged`, our server-echoed [`Engaged`]): `CMSG_ATTACKSTOP` — the ref stops
/// swinging and drops the attack stance on Esc/click-off/target-death alike (the stance itself falls
/// when the `SMSG_ATTACKSTOP` echo removes [`Engaged`]). Weapons *stay drawn* — combat never stows.
pub(super) fn clear(selection: &mut Selection, net: &NetCommands, engaged: bool) {
    if selection.target.take().is_some() {
        selection.guid = None;
        let _ = net.0.send(ClientCommand::SetSelection { guid: 0 });
        if engaged {
            let _ = net.0.send(ClientCommand::AttackStop);
        }
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
        // miss and the key-in-hand (deferred open) cases are silent, like the ref.
        let key_slot = LockSlot {
            key_type: LOCK_KEY_ITEM,
            index: 11000,
            skill: 0,
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
        assert!(route_lock_refusal(&key_slot, false, false, 0, 0, None, KeyFact::Held).is_none());
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
            // Buy with no banker bit (a pure auctioneer) stays on the gossip fallback.
            (CursorKind::Buy, 0x1000),
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

    /// The banker split (decision 0604): Buy(3) is banker OR auctioneer — the BANKER flag routes
    /// the direct `CMSG_BANKER_ACTIVATE`, its absence falls to the gossip universal. A
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
