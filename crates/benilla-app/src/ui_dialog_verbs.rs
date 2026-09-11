//! The dialog engine's verbs, app half (decision 1963): the feeds behind the stock
//! `StaticPopup.lua` dialogs benilla never raised and the drains behind their buttons, each to
//! wow-re's `staticpopup-dialog-bindings.md` (VERIFIED at the bytes unless a line says INFERRED).
//!
//! * **Pet trainer** — `SMSG_PET_UNLEARN_CONFIRM {guid, cost}` latches both and owes
//!   `CONFIRM_PET_UNLEARN(cost)`; `ConfirmPetUnlearn()` answers with `CMSG_PET_UNLEARN {guid}` unless
//!   the cost outruns the purse (`ERR_NOT_ENOUGH_MONEY`, nothing sent). The talent-wipe twin
//!   (`crate::ui_talent_wipe`), latch for latch, leash for leash: the trainer walking out of
//!   `INTERACT_DISTANCE` closes the question, which is what `CheckPetUntrainerDist()` polls.
//! * **Instance boot** — every `SMSG_RAID_GROUP_ONLY {delayMs, reason}` fires an event: a positive
//!   delay arms the deadline and `INSTANCE_BOOT_START`, zero clears it and `INSTANCE_BOOT_STOP`,
//!   and only the zero leg names reason 1/2 on screen. `GetInstanceBootTimeRemaining()` reads
//!   whole seconds off the deadline; nothing clears it but a zero packet.
//! * **Area spirit healer** — `SMSG_AREA_SPIRIT_HEALER_TIME {guid, ms}` arms the wave clock and
//!   fires `AREA_SPIRIT_HEALER_IN_RANGE` when the guid is the cached healer's; Accept sends
//!   `0x2E3` with that guid, Cancel is the cancel-aura of spell 2584 plus `_OUT_OF_RANGE`. **The
//!   cache has no writer yet**: the reference's per-frame proximity scan (20 yd to acquire, 22 to
//!   retain, `0x4923b0`) is not carved for its unit filter, so no healer is ever cached here and
//!   Accept stays silent — named in the record, not guessed at.
//! * **Battlefield queue** — `SMSG_BATTLEFIELD_STATUS` fills one of three slots and fires
//!   `UPDATE_BATTLEFIELD_STATUS`; `AcceptBattlefieldPort(index, accept)` sends the slot's map id
//!   with the answer as one byte.
//! * **Meeting stone** (wow-re `meeting-stone-status.md`, 1974) — two globals: the queued area
//!   (`[0xb72038]`) and the cached status text (`[0xb7203c]`). `SMSG 0x295 {areaId, status}`
//!   latches the old area, stores the new one unconditionally, prints one of five chat lines by
//!   the status byte (with the two asymmetries §8 records: status 0 names the OLD area and is
//!   silent when it has no row; status 1 is skipped entirely when the area did not change, names
//!   the NEW one with an `UNKNOWN` fallback, and plays the `HARDCODED Meeting Stone Join` visual
//!   on the player), then — on EVERY path, an out-of-range status included — rebuilds the text
//!   (`MEETINGSTONE_TOOLTIP` over the area's name or `UNKNOWN`, into a 256-byte buffer) and fires
//!   `MEETINGSTONE_CHANGED`. World enter resets the text to the bare `UNKNOWN` and sends the empty
//!   `CMSG 0x296` once per world session; world leave drops the text to none.
//!   `CancelMeetingStoneRequest()` sends `0x293` unless in a party led by someone else
//!   (`ERR_MEETING_STONE_NOT_LEADER`). The four display-only replies (`0x297/0x298/0x299/0x2BB`)
//!   are chat lines with no state; the status-1 arm also triggers the Meeting Stones tutorial
//!   (`crate::tutorial`, 1976).

// `std::time::Instant` has no monotonic clock behind it in a browser and panics there;
// bevy's re-export is std's natively and `web_time`'s on wasm32.
use bevy::platform::time::Instant;

use benilla_protocol::messages::{BattlefieldStatus, MeetingStoneNotice};
use benilla_ui::script::{ScriptValue, UiScript};
use bevy::prelude::*;

use crate::area::AreaTableRes;
use crate::creature_anim::spell_visual::{meeting_stone_join_fx, SpellKitFx, SpellVisuals};
use crate::names::NameCache;

use crate::net::{ClientCommand, NetCommands, ObjectStore, SelfGuid, SelfPlayer};
use crate::ui_party::GroupState;
use crate::ui_script::UiInput;
use crate::ui_session::{close_npc_session_out_of_range, NpcSession};

/// The pet trainer's pending question: the latch (`0xc4d7b0/b4`) and the cost (`0xc4d7b8`).
#[derive(Resource, Default)]
pub(crate) struct PetUnlearnState {
    npc: Option<u64>,
    cost: u32,
    ask: bool,
}

impl PetUnlearnState {
    /// The inbound `SMSG_PET_UNLEARN_CONFIRM`: latch and owe the dialog.
    pub(crate) fn ask(&mut self, npc: u64, cost: u32) {
        self.npc = Some(npc);
        self.cost = cost;
        self.ask = true;
    }

    fn pending(&self) -> Option<u64> {
        self.npc
    }
}

impl NpcSession for PetUnlearnState {
    fn npc(&self) -> Option<u64> {
        self.npc
    }
    fn close(&mut self) {
        self.npc = None;
        self.cost = 0;
        self.ask = false;
    }
}

/// The instance-boot clock (`[0xb4e34c]`), and what the last packet owes the UI.
#[derive(Resource, Default)]
pub(crate) struct InstanceBoot {
    deadline: Option<Instant>,
    /// Events owed, in arrival order (`INSTANCE_BOOT_START` / `_STOP`).
    events: Vec<&'static str>,
    /// Error lines owed (`ERR_RAID_GROUP_ONLY` / `_FULL`), the zero-delay leg's.
    errors: Vec<&'static str>,
}

impl InstanceBoot {
    /// `SMSG_RAID_GROUP_ONLY`: `delay > 0` arms, else clears — and the event fires either way.
    pub(crate) fn apply(&mut self, delay_ms: u32, reason: u32, now: Instant) {
        if delay_ms > 0 {
            self.deadline = Some(now + std::time::Duration::from_millis(u64::from(delay_ms)));
            self.events.push("INSTANCE_BOOT_START");
        } else {
            self.deadline = None;
            self.events.push("INSTANCE_BOOT_STOP");
            match reason {
                1 => self.errors.push("ERR_RAID_GROUP_ONLY"),
                2 => self.errors.push("ERR_RAID_GROUP_FULL"),
                _ => {}
            }
        }
    }

    /// Whole seconds left, 0 when idle or past — the reference's unsigned divide of a clamped
    /// millisecond remainder.
    pub(crate) fn secs(&self, now: Instant) -> u32 {
        self.deadline
            .map(|d| d.saturating_duration_since(now).as_secs())
            .map_or(0, |s| u32::try_from(s).unwrap_or(u32::MAX))
    }
}

/// The current-area spirit healer (`[0xb4e330/334]`) and its wave clock (`[0xb4e338]`).
#[derive(Resource, Default)]
pub(crate) struct AreaSpiritHealer {
    /// The cached healer. No writer yet — see the module doc.
    healer: Option<u64>,
    deadline: Option<Instant>,
    in_range: bool,
}

impl AreaSpiritHealer {
    /// `SMSG_AREA_SPIRIT_HEALER_TIME`: for the cached healer with a positive time, arm the clock
    /// (a zero-landing deadline reads as 1 ms in the reference) and owe `_IN_RANGE`.
    pub(crate) fn on_time(&mut self, healer: u64, ms: u32, now: Instant) {
        if self.healer == Some(healer) && ms > 0 {
            self.deadline = Some(now + std::time::Duration::from_millis(u64::from(ms)));
            self.in_range = true;
        }
    }

    fn secs(&self, now: Instant) -> u32 {
        self.deadline
            .map(|d| d.saturating_duration_since(now).as_secs())
            .map_or(0, |s| u32::try_from(s).unwrap_or(u32::MAX))
    }
}

/// The three battleground queue slots (`0xb6e9d0`, stride `0x20`), each with the moment its
/// status landed — the clock every stamp in the slot is relative to.
#[derive(Resource, Default)]
pub(crate) struct BattlefieldQueue {
    slots: [Option<(BattlefieldStatus, Instant)>; 3],
    changed: bool,
    /// The slot the player is IN (`[0x8457cc]`, the status-3 arm) and its map.
    active: Option<(usize, u32)>,
    /// The instance's two clocks (`[0xb6ebbc]`/`[0xb6ebb8]`, wow-re `battlefield-verb-family.md`
    /// §4.2): the run-time stamp `now − Δ₂` and the expiration `now + Δ₁`, set by a status-3
    /// message and zeroed by ANY non-clearing message of another status — whatever slot it is
    /// about (§10's anomaly 4, reproduced: 1972 zeroed them only for the active slot; 1974
    /// corrects it to the handler's unconditional clear).
    run_started: Option<Instant>,
    instance_expiration: Option<Instant>,
    /// The status-3 arm rebuilds the scoreboard and fires `UPDATE_BATTLEFIELD_SCORE` before
    /// `UPDATE_BATTLEFIELD_STATUS` (§4.2's ordering) — the score feed reads this first.
    score_dirty: bool,
    /// The handler's two tutorial arms (`0x2f` on queued, `0x30` on confirm; 1976), owed to the
    /// tutorial system on the next feed.
    tutorials: Vec<u32>,
}

impl BattlefieldQueue {
    /// `SMSG_BATTLEFIELD_STATUS` (§4.2): an out-of-range slot abandons the message; a zero map
    /// takes the clear arm (the slot emptied, the instance clocks zeroed only when this was the
    /// active slot — and the active index itself left alone, as the handler leaves `[0x8457cc]`);
    /// status 3 stamps the instance clocks and names the slot active; every other status zeroes
    /// the instance clocks unconditionally and un-names the slot if it was the active one.
    pub(crate) fn apply(&mut self, status: BattlefieldStatus) {
        self.apply_at(status, Instant::now());
    }

    fn apply_at(&mut self, status: BattlefieldStatus, now: Instant) {
        let index = status.slot as usize;
        let Some(slot) = self.slots.get_mut(index) else {
            return;
        };
        if status.map_id == 0 {
            if self.active.is_some_and(|(i, _)| i == index) {
                self.run_started = None;
                self.instance_expiration = None;
            }
            *slot = None;
            self.changed = true;
            return;
        }
        match status.status {
            1 => self.tutorials.push(crate::tutorial::id::BATTLEGROUND_QUEUE),
            2 => self
                .tutorials
                .push(crate::tutorial::id::PORT_TO_BATTLEGROUND),
            _ => {}
        }
        match status.in_progress {
            Some((expires_ms, elapsed_ms)) => {
                self.active = Some((index, status.map_id));
                self.instance_expiration = (expires_ms != 0)
                    .then(|| now + std::time::Duration::from_millis(u64::from(expires_ms)));
                self.run_started = (elapsed_ms != 0)
                    .then(|| now - std::time::Duration::from_millis(u64::from(elapsed_ms)));
                self.score_dirty = true;
            }
            None => {
                self.run_started = None;
                self.instance_expiration = None;
                if self.active.is_some_and(|(i, _)| i == index) {
                    self.active = None;
                }
            }
        }
        *slot = Some((status, now));
        self.changed = true;
    }

    /// The three slots with the instant each status landed — the queue verbs' view builder
    /// (`crate::ui_battlefield`) reduces their stamps against `now`.
    pub(crate) fn slots(&self) -> &[Option<(BattlefieldStatus, Instant)>; 3] {
        &self.slots
    }

    /// `GetBattlefieldInstanceExpiration()`: `deadline − now` in ms, 0 when unset or past
    /// (`[0xb6ebb8]`, the `jns` guard).
    pub(crate) fn instance_expiration_ms(&self, now: Instant) -> u32 {
        self.instance_expiration.map_or(0, |d| {
            d.saturating_duration_since(now)
                .as_millis()
                .min(u128::from(u32::MAX)) as u32
        })
    }

    /// The map of the battleground the player is in — `LeaveBattlefield`'s payload; `None` = 0.
    pub(crate) fn active_map(&self) -> Option<u32> {
        self.active.map(|(_, map)| map)
    }

    /// `GetBattlefieldInstanceRunTime()`: ms since the status-3 stamp, 0 with none.
    pub(crate) fn run_time_ms(&self, now: Instant) -> u32 {
        self.run_started.map_or(0, |t| {
            now.saturating_duration_since(t)
                .as_millis()
                .min(u128::from(u32::MAX)) as u32
        })
    }

    /// The status-3 arm's scoreboard rebuild, once per arrival.
    pub(crate) fn take_score_dirty(&mut self) -> bool {
        std::mem::take(&mut self.score_dirty)
    }

    /// The map id `AcceptBattlefieldPort` sends for a 1-based slot, if the slot holds a queue.
    fn map_id(&self, index: u8) -> Option<u32> {
        self.slots
            .get(usize::from(index).checked_sub(1)?)
            .and_then(|s| s.as_ref())
            .map(|(s, _)| s.map_id)
    }
}

/// The cached status text's three states (`[0xb7203c]`): none from process start and after world
/// leave; the bare localized `UNKNOWN` from world enter until the server's `0x295` lands; a
/// built line after that. The two localized halves are resolved against the VM at push time.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
enum StoneText {
    #[default]
    None,
    Unknown,
    Built(String),
}

/// `SStrPrintf`'s buffer at the rebuild: 256 bytes, so 255 of text.
const STONE_TEXT_BYTES: usize = 255;

/// The meeting-stone queue: the two globals and what the wire still owes the screen.
#[derive(Resource, Default)]
pub(crate) struct MeetingStone {
    /// `[0xb72038]` — the queued area id, `0` = none.
    pub(crate) area: u32,
    text: StoneText,
    /// `0x295` arrivals since the last feed: `(the area BEFORE the store, status)`; the new area
    /// is already in `area` (the handler stores it before it switches).
    updates: Vec<(u32, u8)>,
    notices: Vec<MeetingStoneNotice>,
    /// `0x299` guids whose name has not resolved yet.
    pending_members: Vec<u64>,
    /// The VM's copy of the two globals is stale.
    dirty: bool,
}

impl MeetingStone {
    /// `SMSG 0x295`: the area is stored unconditionally; the line, the rebuild and the event
    /// follow on the next feed, with the VM.
    pub(crate) fn apply(&mut self, area: u32, status: u8) {
        let old = self.area;
        self.area = area;
        self.updates.push((old, status));
    }

    /// One of the four display-only replies.
    pub(crate) fn apply_notice(&mut self, notice: MeetingStoneNotice) {
        self.notices.push(notice);
    }

    /// The enter-world bring-up (`0x4c9f40`): the text becomes the bare `UNKNOWN`; the area is
    /// untouched (the server's reply resets it).
    fn enter_world(&mut self) {
        self.text = StoneText::Unknown;
        self.dirty = true;
    }

    /// The leave-world sweep (`0x4c9f80`): the text goes, the area stays.
    fn leave_world(&mut self) {
        self.text = StoneText::None;
        self.dirty = true;
    }
}

/// A `GlobalStrings` value as the client's `GetText` reads it: the string, or `""` when the Lua
/// global is missing (`0x882748`, the shared empty-string constant — never NULL).
fn global_text(script: &UiScript, key: &str) -> String {
    script
        .lua()
        .globals()
        .get::<String>(key)
        .unwrap_or_default()
}

/// The area's localized name, or `None` where the reference's three-part AreaTable resolve fails.
fn area_name(areas: Option<&AreaTableRes>, id: u32) -> Option<&str> {
    areas.and_then(|a| a.0.name(id))
}

/// The status-text rebuild `0x4ca070`: `MEETINGSTONE_TOOLTIP` (or `""` when missing) over the
/// queued area's name (or `UNKNOWN`), printed into a 256-byte buffer.
fn build_stone_text(script: &UiScript, areas: Option<&AreaTableRes>, area: u32) -> String {
    let name = area_name(areas, area)
        .map(str::to_string)
        .unwrap_or_else(|| global_text(script, "UNKNOWN"));
    let mut text = global_text(script, "MEETINGSTONE_TOOLTIP").replacen("%s", &name, 1);
    if text.len() > STONE_TEXT_BYTES {
        let mut cut = STONE_TEXT_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    text
}

/// The `0x295` handler's five-way table (§8), as the line it prints for `(old, new, status)` —
/// `None` where the reference prints nothing: status 0 with no row for the OLD area, status 1
/// with an unchanged area, and any status past 4.
fn stone_line(
    script: &UiScript,
    areas: Option<&AreaTableRes>,
    old: u32,
    new: u32,
    status: u8,
) -> Option<crate::ui_action::Shown> {
    match status {
        0 => {
            let name = area_name(areas, old)?;
            crate::ui_action::keyed_line_s(script, "ERR_MEETING_STONE_LEFT_QUEUE_S", &[name])
        }
        1 => {
            if new == old {
                return None;
            }
            let name = area_name(areas, new)
                .map(str::to_string)
                .unwrap_or_else(|| global_text(script, "UNKNOWN"));
            crate::ui_action::keyed_line_s(script, "ERR_MEETING_STONE_IN_QUEUE_S", &[&name])
        }
        2 => crate::ui_action::keyed_line(script, "ERR_MEETING_STONE_OTHER_MEMBER_LEFT"),
        3 => crate::ui_action::keyed_line(script, "ERR_MEETING_STONE_PARTY_KICKED_FROM_QUEUE"),
        4 => crate::ui_action::keyed_line(script, "ERR_MEETING_STONE_MEMBER_STILL_IN_QUEUE"),
        _ => None,
    }
}

/// The inputs the meeting-stone feed reads beside the VM and its own state — bundled because the
/// feed sits at Bevy's parameter ceiling otherwise.
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct MeetingStoneInputs<'w, 's> {
    areas: Option<Res<'w, AreaTableRes>>,
    names: ResMut<'w, NameCache>,
    commands: Res<'w, NetCommands>,
    visuals: Option<Res<'w, SpellVisuals>>,
    fx: MessageWriter<'w, SpellKitFx>,
    self_q: Query<'w, 's, Entity, With<SelfPlayer>>,
    tutorials: Option<MessageWriter<'w, crate::tutorial::TutorialEvent>>,
}

/// The meeting stone's feed: the `0x295` lines, the rebuild and `MEETINGSTONE_CHANGED` per
/// arrival; the four display replies; the two globals pushed when they moved.
fn feed_meeting_stone(
    script: Option<NonSendMut<UiScript>>,
    mut stone: ResMut<MeetingStone>,
    mut inputs: MeetingStoneInputs,
    mut sink: crate::ui_action::MessageSink,
) {
    let Some(mut script) = script else {
        return;
    };
    let areas = inputs.areas.as_deref();
    let mut lines = Vec::new();

    for (old, status) in std::mem::take(&mut stone.updates) {
        let new = stone.area;
        lines.extend(stone_line(&script, areas, old, new, status));
        if status == 1 && new != old {
            // The status-1 arm's extra block: `Effect_C` kind `0xc` on the local player, then
            // the Meeting Stones tutorial (`0x4ca363`, 1976).
            if let (Some(visuals), Ok(entity)) = (inputs.visuals.as_deref(), inputs.self_q.single())
            {
                if let Some(fx) = meeting_stone_join_fx(visuals, entity) {
                    inputs.fx.write(fx);
                }
            }
            if let Some(t) = inputs.tutorials.as_mut() {
                t.write(crate::tutorial::TutorialEvent::trigger(
                    crate::tutorial::id::MEETING_STONES,
                ));
            }
        }
        // Every path — the silent legs and an out-of-range status included — rebuilds and fires.
        // The two globals reach the VM BEFORE the event: the stock handler's first act is
        // `IsInMeetingStoneQueue()`, which has to see the area this packet stored.
        let text = build_stone_text(&script, areas, new);
        stone.text = StoneText::Built(text.clone());
        stone.dirty = false;
        script.set_meeting_stone(new, Some(text));
        script.fire_event("MEETINGSTONE_CHANGED", vec![]);
    }

    for notice in std::mem::take(&mut stone.notices) {
        match notice {
            MeetingStoneNotice::Success => {
                lines.extend(crate::ui_action::keyed_line(
                    &script,
                    "ERR_MEETING_STONE_SUCCESS",
                ));
            }
            MeetingStoneNotice::InProgress => {
                lines.extend(crate::ui_action::keyed_line(
                    &script,
                    "ERR_MEETING_STONE_IN_PROGRESS",
                ));
            }
            MeetingStoneNotice::MemberAdded { guid } => stone.pending_members.push(guid),
            MeetingStoneNotice::JoinFailed { code } => {
                let key = match code {
                    1 => "ERR_MEETING_STONE_MUST_BE_LEADER",
                    2 => "ERR_MEETING_STONE_GROUP_FULL",
                    3 => "ERR_MEETING_STONE_NO_RAID_GROUP",
                    _ => continue,
                };
                lines.extend(crate::ui_action::keyed_line(&script, key));
            }
        }
    }
    // `0x299`'s name-cache callback: the line when the name lands, nothing until then.
    let pending = std::mem::take(&mut stone.pending_members);
    for guid in pending {
        match inputs
            .names
            .resolve(guid, &inputs.commands)
            .map(str::to_string)
        {
            Some(name) => lines.extend(crate::ui_action::keyed_line_s(
                &script,
                "ERR_MEETING_STONE_MEMBER_ADDED_S",
                &[&name],
            )),
            None => stone.pending_members.push(guid),
        }
    }
    if !lines.is_empty() {
        crate::ui_action::show_messages(&mut script, &mut sink, "ui_dialog_verbs", lines);
    }

    if std::mem::take(&mut stone.dirty) {
        let text = match &stone.text {
            StoneText::None => None,
            StoneText::Unknown => Some(global_text(&script, "UNKNOWN")),
            StoneText::Built(t) => Some(t.clone()),
        };
        script.set_meeting_stone(stone.area, text);
    }
}

/// The enter-world bring-up's meeting-stone leg: the text reset, then the empty `CMSG 0x296`.
///
/// **Once per UI LIFECYCLE, not once per world session**, and that distinction is the whole bug.
/// The reference's run-once byte `[0xb4b424]` is *cleared by the UI teardown* — `0x490bd0` calls
/// `0x490a80` at `0x490c20`, which zeroes it at `0x490a8d` — and `UI_Init` (`0x48fbf0`) then calls
/// `0x4908c0` at `0x490168` behind the same live-player gate, finds the byte clear, and re-runs
/// this entire bring-up: `0x490a14` → `0x4c9f40` → `0x4ca1c0` → `PutUInt32(0x296)` + Send. So a
/// `ReloadUI()` **re-asks the server**, and that is what brings the icon back.
///
/// Gated on `MessageReader<EnteredWorldMessage>`, this leg never ran for a `/reload`, which never
/// leaves the world (1291) and so produces no such message. Nothing re-armed
/// [`MeetingStone::dirty`] and nothing re-queried, so `IsInMeetingStoneQueue()` and
/// `GetMeetingStoneStatusText()` answered nil for the rest of the session — and stock
/// `Minimap.xml`'s `MiniMapMeetingStoneFrame` (built `hidden="true"`, shown only by
/// `MEETINGSTONE_CHANGED`, whose single firing site image-wide is the `0x295` handler at
/// `0x4ca38f`) stayed gone while the player was still queued.
///
/// **A [`crate::ui_script::VmMemo`] claim IS the reference's gate.** A byte the UI teardown clears
/// is exactly "once per VM", and 1290's name for that is `claim` — so this reads as the same
/// question the binary asks, rather than as a workaround for the missing message. The one
/// round trip during which the icon is genuinely absent is faithful, not a defect: the reference
/// has the same gap, because only the server's reply fires the event.
///
/// The queued area itself is untouched here, matching `[0xb72038]`, which the RE round found is
/// referenced six times image-wide and by nothing in either reload closure — it survives, and
/// [`MeetingStone::enter_world`]'s `dirty` is what re-pushes it to the fresh VM.
fn meeting_stone_enter_world(
    script: Option<NonSendMut<UiScript>>,
    mut stone: ResMut<MeetingStone>,
    commands: Res<NetCommands>,
    mut asked: Local<crate::ui_script::VmMemo<bool>>,
) {
    let Some(script) = script else {
        return;
    };
    if !asked.claim(&script) {
        return;
    }
    stone.enter_world();
    let _ = commands.0.send(ClientCommand::MeetingStoneStatusQuery);
}

/// The leave-world sweep's leg: the text dropped, the area kept.
fn meeting_stone_leave_world(mut stone: ResMut<MeetingStone>) {
    stone.leave_world();
}

pub(crate) fn feed_dialog_verbs(
    script: Option<NonSendMut<UiScript>>,
    mut pet: ResMut<PetUnlearnState>,
    mut boot: ResMut<InstanceBoot>,
    mut spirit: ResMut<AreaSpiritHealer>,
    mut queue: ResMut<BattlefieldQueue>,
    mut sink: crate::ui_action::MessageSink,
    mut tutorials: Option<MessageWriter<crate::tutorial::TutorialEvent>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let now = Instant::now();

    script.set_pet_untrainer_pending(pet.pending().is_some());
    if pet.ask {
        pet.ask = false;
        script.fire_event(
            "CONFIRM_PET_UNLEARN",
            vec![ScriptValue::Int(i64::from(pet.cost))],
        );
    }

    script.set_instance_boot_secs(boot.secs(now));
    for event in std::mem::take(&mut boot.events) {
        script.fire_event(event, vec![]);
    }
    let lines: Vec<_> = std::mem::take(&mut boot.errors)
        .into_iter()
        .filter_map(|key| crate::ui_action::keyed_line(&script, key))
        .collect();
    if !lines.is_empty() {
        crate::ui_action::show_messages(&mut script, &mut sink, "ui_dialog_verbs", lines);
    }

    let secs = spirit.secs(now);
    script.set_area_spirit_healer(spirit.healer.is_some(), secs);
    if std::mem::take(&mut spirit.in_range) {
        script.fire_event("AREA_SPIRIT_HEALER_IN_RANGE", vec![]);
    }

    for id in std::mem::take(&mut queue.tutorials) {
        if let Some(t) = tutorials.as_mut() {
            t.write(crate::tutorial::TutorialEvent::trigger(id));
        }
    }
    if std::mem::take(&mut queue.changed) {
        script.fire_event("UPDATE_BATTLEFIELD_STATUS", vec![]);
    }
}

/// The pet trainer's confirm and the spirit healer's accept — the two drains over a latch.
fn drain_latch_verbs(
    script: Option<NonSendMut<UiScript>>,
    pet: Res<PetUnlearnState>,
    spirit: Res<AreaSpiritHealer>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    commands: Res<NetCommands>,
    mut sink: crate::ui_action::MessageSink,
) {
    let Some(mut script) = script else {
        return;
    };

    // ConfirmPetUnlearn: the latch, then the money gate — `cost > coinage` shows
    // ERR_NOT_ENOUGH_MONEY and sends nothing; otherwise `0x2F0` with the latched guid.
    let confirms = script.take_pet_unlearn_confirms();
    if confirms > 0 {
        if let Some(npc) = pet.pending() {
            let money = self_q
                .single()
                .ok()
                .and_then(|store| store.0.player_money())
                .unwrap_or(0);
            if pet.cost > money {
                if let Some(line) = crate::ui_action::keyed_line(&script, "ERR_NOT_ENOUGH_MONEY") {
                    crate::ui_action::show_messages(
                        &mut script,
                        &mut sink,
                        "ui_dialog_verbs",
                        [line],
                    );
                }
            } else {
                for _ in 0..confirms {
                    let _ = commands.0.send(ClientCommand::PetUnlearn { trainer: npc });
                }
            }
        }
    }

    // AcceptAreaSpiritHeal: the cached healer's guid (the binding was silent without one).
    let accepts = script.take_area_spirit_accepts();
    if let Some(healer) = spirit.healer {
        for _ in 0..accepts {
            let _ = commands
                .0
                .send(ClientCommand::AreaSpiritHealerQueue { healer });
        }
    }
}

/// The battleground port and the meeting-stone leave — the two drains over a queue.
fn drain_queue_verbs(
    script: Option<NonSendMut<UiScript>>,
    queue: Res<BattlefieldQueue>,
    group: Option<Res<GroupState>>,
    self_guid: Res<SelfGuid>,
    commands: Res<NetCommands>,
    mut sink: crate::ui_action::MessageSink,
) {
    let Some(mut script) = script else {
        return;
    };

    // AcceptBattlefieldPort: the slot's map id and the one-byte answer.
    for (index, accept) in script.take_battlefield_port_requests() {
        if let Some(map_id) = queue.map_id(index) {
            let _ = commands
                .0
                .send(ClientCommand::BattlefieldPort { map_id, accept });
        }
    }

    // CancelMeetingStoneRequest: in a party and not its leader → ERR_MEETING_STONE_NOT_LEADER;
    // otherwise `0x293`, whatever is or is not queued.
    let cancels = script.take_meeting_stone_cancels();
    if cancels > 0 {
        let not_leader = group
            .as_deref()
            .is_some_and(|g| g.in_group && Some(g.leader) != self_guid.0);
        if not_leader {
            if let Some(line) =
                crate::ui_action::keyed_line(&script, "ERR_MEETING_STONE_NOT_LEADER")
            {
                crate::ui_action::show_messages(&mut script, &mut sink, "ui_dialog_verbs", [line]);
            }
        } else {
            for _ in 0..cancels {
                let _ = commands.0.send(ClientCommand::MeetingStoneLeave);
            }
        }
    }
}

pub(crate) struct UiDialogVerbsPlugin;

impl Plugin for UiDialogVerbsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PetUnlearnState>()
            .init_resource::<InstanceBoot>()
            .init_resource::<AreaSpiritHealer>()
            .init_resource::<BattlefieldQueue>()
            .init_resource::<MeetingStone>()
            .add_systems(
                Update,
                (
                    close_npc_session_out_of_range::<PetUnlearnState>.before(feed_dialog_verbs),
                    feed_dialog_verbs.before(UiInput),
                    // In-world only: the claim is about the VM, but the query is a world
                    // packet, and the boot VM exists at the glue screen too.
                    meeting_stone_enter_world
                        .before(feed_meeting_stone)
                        .run_if(in_state(crate::char_select::ClientState::InWorld)),
                    feed_meeting_stone.before(UiInput),
                    drain_latch_verbs.after(UiInput),
                    drain_queue_verbs.after(UiInput),
                ),
            )
            .add_systems(
                OnExit(crate::char_select::ClientState::InWorld),
                meeting_stone_leave_world,
            );
    }
}

/// The area spirit healer's aura, `0xA18` — `CancelAreaSpiritHeal`'s one spell (the engine's
/// `dialog_verbs::AREA_SPIRIT_HEALER_SPELL`), read here only to check its flags against the data.
#[cfg(test)]
const AREA_SPIRIT_HEALER_SPELL: u32 = 2584;

/// The generic cancel-aura routine's refusal (`0x6e7040`, wow-re `staticpopup-dialog-bindings.md`
/// §6): it returns without sending when the spell's `AttributesEx` has bit 13 set and bit 2
/// clear **and** `0x5ee290(player)` holds. Whether the third leg ever matters for spell 2584 is
/// decided by the first two, read off the shipped Spell.dbc in [`tests::spell_2584_never_trips_the_cancel_gate`].
#[cfg(test)]
fn cancel_gate_could_apply(attributes_ex: u32) -> bool {
    attributes_ex & 0x2000 != 0 && attributes_ex & 0x4 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The `/reload` re-query** — the meeting-stone half of 1290's class, and the reference's
    /// own behaviour rather than an invention of ours.
    ///
    /// wow-re's §5 trio (3/3 unanimous, every byte re-decoded from the raw image) settled that
    /// the run-once byte `[0xb4b424]` is cleared by the UI teardown at `0x490a8d` and that
    /// `UI_Init` re-runs the bring-up and re-sends `CMSG 0x296`. So the gate is once per **VM**,
    /// not once per world session — and against the old `MessageReader<EnteredWorldMessage>`
    /// shape this fails on the second VM, which is exactly the reported symptom: the queued
    /// player's minimap icon never comes back after a `/reload`.
    ///
    /// It also corrects wow-re's own `meeting-stone-status.md` §6, which asserted the query "is
    /// sent exactly once per world session … has no other trigger" — true of the entry points it
    /// enumerated, but it never asked who *clears* the byte.
    ///
    /// **A registered schedule, not `run_system_once`**: the gate is a `Local<VmMemo<bool>>`, and
    /// `run_system_once` builds a fresh system — and so a fresh `Local` — on every call, which
    /// would make the claim look unclaimed every frame (the same trap `ui_loot`'s tests name).
    #[test]
    fn a_rebuilt_vm_re_asks_the_server_for_the_meeting_stone_queue() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.init_resource::<MeetingStone>()
            .insert_resource(NetCommands(tx))
            .add_systems(Update, meeting_stone_enter_world);

        let queries = |rx: &crossbeam_channel::Receiver<ClientCommand>| {
            rx.try_iter()
                .filter(|c| matches!(c, ClientCommand::MeetingStoneStatusQuery))
                .count()
        };

        // No VM yet — the glue screen: nothing is asked.
        app.update();
        assert_eq!(queries(&rx), 0, "no VM, no bring-up");

        // The first VM: the bring-up runs once, however many frames pass.
        app.insert_non_send_resource(UiScript::new().expect("VM"));
        app.update();
        assert_eq!(queries(&rx), 1, "the first VM asks the server");
        app.update();
        app.update();
        assert_eq!(queries(&rx), 0, "…and does not ask again on later frames");

        // The server answers: the player IS queued, and the host holds that.
        app.world_mut().resource_mut::<MeetingStone>().area = 1519;
        app.world_mut().resource_mut::<MeetingStone>().dirty = false;

        // `ReloadUI()`: a fresh VM, and nothing on the wire.
        app.insert_non_send_resource(UiScript::new().expect("VM"));
        app.update();
        assert_eq!(
            queries(&rx),
            1,
            "a rebuilt VM re-asks — the reference's UI teardown clears the run-once byte, so \
             `UI_Init` re-sends `CMSG 0x296` (0x490a8d / 0x490168)"
        );

        let stone = app.world().resource::<MeetingStone>();
        assert!(
            stone.dirty,
            "the bring-up re-armed the push, so the fresh VM is told the queued area again"
        );
        assert_eq!(
            stone.area, 1519,
            "the queued area itself is untouched, matching `[0xb72038]` surviving the reload"
        );
    }

    /// Spell 2584's flags off the shipped Spell.dbc: the cancel-aura gate's first two legs.
    #[test]
    fn spell_2584_never_trips_the_cancel_gate() {
        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let catalog = benilla_formats::load_spell_catalog(&mut chain).expect("Spell.dbc");
        let ex = catalog
            .get(AREA_SPIRIT_HEALER_SPELL)
            .map(|d| d.attributes_ex)
            .expect("spell 2584 in Spell.dbc");
        assert!(
            !cancel_gate_could_apply(ex),
            "spell 2584 AttributesEx = {ex:#x}: the gate's third leg would decide, and it is uncarved"
        );
    }

    #[test]
    fn the_boot_clock_arms_on_a_delay_and_clears_on_zero_with_the_reason() {
        let mut boot = InstanceBoot::default();
        let now = Instant::now();
        boot.apply(60_500, 0, now);
        assert_eq!(boot.events, vec!["INSTANCE_BOOT_START"]);
        assert_eq!(boot.secs(now), 60, "whole seconds, truncated");
        boot.apply(0, 1, now);
        assert_eq!(
            boot.events,
            vec!["INSTANCE_BOOT_START", "INSTANCE_BOOT_STOP"]
        );
        assert_eq!(boot.errors, vec!["ERR_RAID_GROUP_ONLY"]);
        assert_eq!(boot.secs(now), 0);
        boot.apply(0, 7, now);
        assert_eq!(boot.errors.len(), 1, "a reason outside 1/2 names nothing");
    }

    #[test]
    fn the_spirit_healer_clock_only_arms_for_the_cached_healer() {
        let mut spirit = AreaSpiritHealer::default();
        let now = Instant::now();
        spirit.on_time(0x77, 30_000, now);
        assert!(!spirit.in_range, "no healer cached: the packet is ignored");
        spirit.healer = Some(0x77);
        spirit.on_time(0x78, 30_000, now);
        assert!(!spirit.in_range, "another guid: ignored");
        spirit.on_time(0x77, 0, now);
        assert!(!spirit.in_range, "a zero time: ignored");
        spirit.on_time(0x77, 30_000, now);
        assert!(spirit.in_range);
        assert_eq!(spirit.secs(now), 30);
    }

    #[test]
    fn the_queue_keeps_three_slots_and_answers_a_port_by_map() {
        let mut q = BattlefieldQueue::default();
        let status = |slot, map_id| BattlefieldStatus {
            slot,
            map_id,
            bracket: 0,
            instance_id: 0,
            status: 2,
            time_ms: Some(0),
            in_progress: None,
            queued: None,
        };
        q.apply(status(1, 489));
        q.apply(status(5, 30));
        assert_eq!(
            q.map_id(2),
            Some(489),
            "1-based from Lua, 0-based on the wire"
        );
        assert_eq!(q.map_id(1), None);
        assert_eq!(q.map_id(4), None);
        q.apply(status(1, 0));
        assert_eq!(q.map_id(2), None, "a zero map clears the slot");
    }

    /// The instance clocks (§4.2): stamped by status 3, zeroed by any other status of ANY slot,
    /// and by a clear of the active slot only — which leaves the active index alone.
    #[test]
    fn the_instance_clocks_follow_the_status_handler() {
        let mut q = BattlefieldQueue::default();
        let now = Instant::now();
        let mut active = BattlefieldStatus {
            slot: 0,
            map_id: 489,
            bracket: 0,
            instance_id: 3,
            status: 3,
            time_ms: None,
            in_progress: Some((90_000, 30_000)),
            queued: None,
        };
        q.apply_at(active.clone(), now);
        assert_eq!(q.active_map(), Some(489));
        assert_eq!(q.instance_expiration_ms(now), 90_000);
        assert_eq!(q.run_time_ms(now), 30_000);
        assert!(q.take_score_dirty());
        // A QUEUED update for slot 2 zeroes both clocks — the handler's unconditional arm.
        let mut queued = active.clone();
        queued.slot = 1;
        queued.map_id = 529;
        queued.status = 1;
        queued.in_progress = None;
        queued.queued = Some((60_000, 5_000));
        q.apply_at(queued, now);
        assert_eq!(
            q.active_map(),
            Some(489),
            "another slot's status leaves the active index"
        );
        assert_eq!(q.instance_expiration_ms(now), 0);
        assert_eq!(q.run_time_ms(now), 0);
        assert_eq!(q.map_id(2), Some(529));
        // Re-arm, then clear the ACTIVE slot: the clocks go, the index stays (`[0x8457cc]`).
        q.apply_at(active.clone(), now);
        active.map_id = 0;
        q.apply_at(active, now);
        assert_eq!(q.instance_expiration_ms(now), 0);
        assert_eq!(q.map_id(1), None);
        assert_eq!(
            q.active_map(),
            Some(489),
            "the clear arm never resets the active index"
        );
    }

    /// `0x295`'s store is unconditional and the old id is latched first; the feed reads both.
    #[test]
    fn the_stone_latches_the_old_area_before_storing_the_new() {
        let mut stone = MeetingStone::default();
        stone.apply(1519, 1);
        stone.apply(1519, 1);
        stone.apply(0, 0);
        stone.apply(12, 9);
        assert_eq!(stone.area, 12);
        assert_eq!(stone.updates, vec![(0, 1), (1519, 1), (1519, 0), (0, 9)]);
        stone.enter_world();
        assert_eq!(stone.text, StoneText::Unknown);
        assert_eq!(
            stone.area, 12,
            "the enter-world reset leaves the area alone"
        );
        stone.leave_world();
        assert_eq!(stone.text, StoneText::None);
    }

    /// The five-way table with its two asymmetries, and the rebuild's fallbacks and buffer.
    #[test]
    fn the_status_table_and_the_rebuild_follow_the_handler() {
        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let areas =
            AreaTableRes(benilla_formats::load_area_table_catalog(&mut chain).expect("AreaTable"));
        let s = UiScript::new().unwrap();
        s.run(
            r#"MEETINGSTONE_TOOLTIP = "Looking for more for %s" UNKNOWN = "Unknown"
               ERR_MEETING_STONE_LEFT_QUEUE_S = "You are no longer queued for %s."
               ERR_MEETING_STONE_IN_QUEUE_S = "You are now in the queue to join a party for %s."
               ERR_MEETING_STONE_OTHER_MEMBER_LEFT = "left"
               ERR_MEETING_STONE_PARTY_KICKED_FROM_QUEUE = "kicked"
               ERR_MEETING_STONE_MEMBER_STILL_IN_QUEUE = "still""#,
        )
        .unwrap();
        let a = Some(&areas);
        let text = |l: Option<crate::ui_action::Shown>| l.map(|l| l.text().to_string());
        // Status 0 names the OLD area — and is silent when it has no row.
        assert_eq!(
            text(stone_line(&s, a, 1519, 0, 0)).as_deref(),
            Some("You are no longer queued for Stormwind City.")
        );
        assert_eq!(
            text(stone_line(&s, a, 0, 0, 0)),
            None,
            "no row for area 0: silent"
        );
        assert_eq!(text(stone_line(&s, a, 999_999, 0, 0)), None);
        // Status 1 names the NEW area, falls back to UNKNOWN, and is skipped when unchanged.
        assert_eq!(
            text(stone_line(&s, a, 0, 1519, 1)).as_deref(),
            Some("You are now in the queue to join a party for Stormwind City.")
        );
        assert_eq!(
            text(stone_line(&s, a, 0, 999_999, 1)).as_deref(),
            Some("You are now in the queue to join a party for Unknown.")
        );
        assert_eq!(
            text(stone_line(&s, a, 1519, 1519, 1)),
            None,
            "unchanged: skipped"
        );
        assert_eq!(text(stone_line(&s, a, 0, 0, 2)).as_deref(), Some("left"));
        assert_eq!(text(stone_line(&s, a, 0, 0, 3)).as_deref(), Some("kicked"));
        assert_eq!(text(stone_line(&s, a, 0, 0, 4)).as_deref(), Some("still"));
        assert_eq!(
            text(stone_line(&s, a, 0, 0, 5)),
            None,
            "past the table: nothing"
        );
        // The rebuild.
        assert_eq!(
            build_stone_text(&s, a, 1519),
            "Looking for more for Stormwind City"
        );
        assert_eq!(build_stone_text(&s, a, 0), "Looking for more for Unknown");
        s.run("MEETINGSTONE_TOOLTIP = string.rep('x', 300) .. '%s'")
            .unwrap();
        assert_eq!(
            build_stone_text(&s, a, 1519).len(),
            STONE_TEXT_BYTES,
            "the 256-byte buffer"
        );
        s.run("MEETINGSTONE_TOOLTIP = nil").unwrap();
        assert_eq!(
            build_stone_text(&s, a, 1519),
            "",
            "a missing template is GetText's empty string, not the unreachable %s fallback"
        );
    }

    #[test]
    fn the_pet_question_latches_and_closes_like_the_talent_wipe() {
        let mut pet = PetUnlearnState::default();
        pet.ask(0x2b, 10_000);
        assert_eq!(pet.pending(), Some(0x2b));
        assert!(pet.ask);
        pet.close();
        assert_eq!(pet.pending(), None);
        assert_eq!(pet.cost, 0);
    }
}
