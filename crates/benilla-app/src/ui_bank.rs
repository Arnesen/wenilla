//! The app-side **bank feed** (decision 0604 phase 2) — the inward half of the bank seam around
//! [`benilla_ui::script`]'s `bank` module, the twin of [`crate::ui_merchant`]'s merchant seam.
//!
//! The net bridge fills [`BankOpen`] from the wire (`SMSG_SHOW_BANK` → the banker guid — sent for
//! our own `CMSG_BANKER_ACTIVATE` *and* volunteered for the gossip menu's bank option, so the arm
//! never assumes we asked). The vault's contents never pass through here: the 24 bank slots and
//! the 6 bank bags are player-array descriptor fields streamed at login (decision 0604 — the
//! window only reveals them), fed by [`crate::ui_items`] as containers `-1`/`5..=10` beside the
//! backpack. Each frame [`feed_bank`] pushes the purchase-row snapshot
//! ([`benilla_ui::script::BankState`]: the `PLAYER_BYTES_2` byte-2 purchased count + the next
//! slot's `BankBagSlotPrices.dbc` cost), fires `BANKFRAME_OPENED` on open / `BANKFRAME_CLOSED` on
//! clear / `PLAYERBANKBAGSLOTS_CHANGED` when the purchased count moves (a successful buy has **no
//! packet** — that descriptor delta is the confirmation), and resolves the banker's name for the
//! title (the merchant feed's ask-once pattern). [`drain_bank`] pulls the Lua intents back out:
//! `PurchaseSlot()` → [`ClientCommand::BuyBankSlot`], `CloseBankFrame()` → a local clear (no
//! close opcode exists — decision 0604). The standardized NPC-session range guard
//! ([`crate::ui_session`]) applies the same client-side close out of service range.
//!
//! The right-click auto-move (deposit/withdraw while the bank is open) lives one module over in
//! [`crate::ui_items`]'s drain, the sell affordance's exact pattern.

use benilla_protocol::messages::bank_slot_result;
use bevy::prelude::*;

use benilla_ui::script::{BankState, UiScript};

use crate::net::{ClientCommand, NetCommands, ObjectStore, SelfPlayer};
use crate::ui_script::UiInput;
use crate::ui_session::{close_npc_session_out_of_range, npc_switched, NpcSession};

/// The client-side `BankBagSlotPrices.dbc` table (decision 0604: the purchase ladder is client
/// data — 10s/1g/10g/25g/50g/100g, then the 999999999 sentinel). Optional resource — absent, the
/// purchase row shows cost 0 and the popup still works (the server re-prices authoritatively).
#[derive(Resource)]
pub(crate) struct BankPrices(pub(crate) benilla_formats::BankBagSlotPrices);

/// The open bank session, filled by the net bridge (`SMSG_SHOW_BANK`) and read by [`feed_bank`].
/// Holds only the banker guid — the vault's contents are descriptor fields the container feed
/// already carries. Cleared on a client-side close and on disconnect.
#[derive(Resource, Default)]
pub(crate) struct BankOpen {
    /// The banker whose window is open; `None` = no bank open.
    pub(crate) banker: Option<u64>,
}

impl BankOpen {
    /// Open (or re-point) the window at a banker.
    pub(crate) fn open(&mut self, banker: u64) {
        self.banker = Some(banker);
    }

    /// Whether the bank window is currently open.
    pub(crate) fn is_open(&self) -> bool {
        self.banker.is_some()
    }

    /// Close the open window (a client-side close — no packet exists, decision 0604).
    pub(crate) fn clear(&mut self) {
        self.banker = None;
    }

    /// Disconnect: drop the open window (mirrors the merchant/gossip session clears).
    pub(crate) fn clear_session(&mut self) {
        self.clear();
    }
}

/// The bank window is an NPC session: the standardized range guard ([`crate::ui_session`])
/// client-side-closes it — the exact `CloseBankFrame` clear — when the player walks out of the
/// banker's service range or the banker despawns.
impl NpcSession for BankOpen {
    fn npc(&self) -> Option<u64> {
        self.banker
    }

    fn close(&mut self) {
        self.clear();
    }
}

/// Bank-slot purchase refusals (`SMSG_BUY_BANK_SLOT_RESULT` — vmangos sends it only on failure)
/// queued by the net bridge for the red error line.
#[derive(Resource, Default)]
pub(crate) struct BankErrors(pub Vec<u32>);

pub(crate) struct UiBankPlugin;

impl Plugin for UiBankPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BankOpen>()
            .init_resource::<BankErrors>()
            .add_systems(
                Update,
                (
                    // Range-close before the feed so the clear turns into BANKFRAME_CLOSED the
                    // same frame; push before the input pass; drain after it (the merchant shape).
                    close_npc_session_out_of_range::<BankOpen>.before(feed_bank),
                    feed_bank.before(UiInput),
                    drain_bank.after(UiInput),
                ),
            );
    }
}

/// The red error line for a `SMSG_BUY_BANK_SLOT_RESULT` code — the reference GlobalStrings'
/// `ERR_BANKSLOT_*` texts verbatim (decision 0604: the codes map 1:1 onto them). `OK` never
/// prints (vmangos doesn't send it; tolerated silently if a server ever does).
fn bank_slot_error_key(result: u32) -> Option<&'static str> {
    Some(match result {
        bank_slot_result::FAILED_TOO_MANY => "ERR_BANKSLOT_FAILED_TOO_MANY", // 0x100
        bank_slot_result::INSUFFICIENT_FUNDS => "ERR_BANKSLOT_INSUFFICIENT_FUNDS", // 0x101
        bank_slot_result::NOTBANKER => "ERR_BANKSLOT_NOTBANKER",             // 0x102
        _ => return None,
    })
}

/// Push the purchase-row snapshot + the open/close/purchased-count events (module doc).
///
/// **`BANKFRAME_OPENED` carries no arguments**, which is a change 1751's swap forced and a
/// correction either way: the banker's name used to ride it as `arg1` (the merchant feed's title
/// pattern), and the reference's own `BankFrame_OnEvent` reads no `arg1` at all — it titles the
/// window with `UnitName("npc")`, off the interaction token [`crate::ui_session`] already points
/// at the banker. Firing an argument the reference does not fire is a divergence an addon can
/// see, and it bought nothing once the reference's file was the one reading the event.
#[allow(clippy::too_many_arguments)]
fn feed_bank(
    script: Option<NonSendMut<UiScript>>,
    open: Res<BankOpen>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    prices: Option<Res<BankPrices>>,
    mut errors: ResMut<BankErrors>,
    mut last: Local<crate::ui_script::VmMemo<Option<BankState>>>,
    mut last_banker: Local<crate::ui_script::VmMemo<Option<u64>>>,
    mut sink: crate::ui_action::MessageSink,
) {
    let Some(mut script) = script else {
        return;
    };
    let last = last.get(&script);
    let last_banker = last_banker.get(&script);
    // Purchase refusals go to the surface — and the voice — their message record names: the
    // insufficient-funds row carries error-speech line `0x16` (decision 1815). A code with no key
    // keeps its "(n)" line and stays silent, because there is no record behind it to sound.
    let lines: Vec<_> = errors
        .0
        .drain(..)
        .filter_map(|result| match bank_slot_error_key(result) {
            Some(key) => {
                let text = script.lua().globals().get::<String>(key).ok()?;
                (!text.is_empty()).then(|| crate::ui_action::Shown::keyed(key, text))
            }
            None if result == bank_slot_result::OK => None,
            None => Some(crate::ui_action::Shown::unkeyed(
                benilla_ui::messages::MsgKind::Error,
                format!("Bank slot purchase failed ({result})."),
            )),
        })
        .collect();
    crate::ui_action::show_messages(&mut script, &mut sink, "ui_bank", lines);
    let store = self_q.iter().next();
    let purchased = store
        .and_then(|s| s.0.player_bank_bag_slots_purchased())
        .unwrap_or(0);
    let fresh = open.banker.map(|_| BankState {
        num_purchased: u32::from(purchased),
        next_cost: prices
            .as_ref()
            .and_then(|p| p.0.next_slot_price(purchased))
            .unwrap_or(0),
    });
    // A different banker while the window is already open is a real close+open (decision 0096 /
    // [`npc_switched`]) — both sounds, and the OnHide-queued close intent is consumed so the
    // drain doesn't clear the session we just re-opened.
    let switched = npc_switched(*last_banker, open.banker);
    if fresh == *last && !switched {
        return;
    }
    script.set_bank(fresh.clone());
    if switched {
        script.fire_event("BANKFRAME_CLOSED", vec![]);
        script.fire_event("BANKFRAME_OPENED", vec![]);
        let _ = script.take_bank_close();
    } else {
        match (&*last, &fresh) {
            (None, Some(_)) => script.fire_event("BANKFRAME_OPENED", vec![]),
            // The purchased count moved while open — the no-packet buy confirmation (decision
            // 0604): the reference event repaints the bag row + purchase frame.
            (Some(_), Some(_)) => script.fire_event("PLAYERBANKBAGSLOTS_CHANGED", vec![]),
            (Some(_), None) => script.fire_event("BANKFRAME_CLOSED", vec![]),
            (None, None) => {}
        }
    }
    *last = fresh;
    *last_banker = open.banker;
}

/// Drain the Lua intents: `PurchaseSlot()` → the buy wire, `CloseBankFrame()` → the local clear.
fn drain_bank(
    script: Option<NonSendMut<UiScript>>,
    mut open: ResMut<BankOpen>,
    net: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    if script.take_bank_purchase() {
        if let Some(banker) = open.banker {
            debug!("bank: purchase next bag slot at {banker:#x}");
            let _ = net.0.send(ClientCommand::BuyBankSlot { guid: banker });
        }
    }
    if script.take_bank_close() && open.is_open() {
        debug!("bank: client-side close");
        open.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal-code → red-line map: the three vmangos failure codes print the reference
    /// GlobalStrings verbatim, OK stays silent, an unknown code prints its number.
    #[test]
    fn bank_slot_error_keys() {
        assert_eq!(
            bank_slot_error_key(bank_slot_result::FAILED_TOO_MANY),
            Some("ERR_BANKSLOT_FAILED_TOO_MANY")
        );
        assert_eq!(
            bank_slot_error_key(bank_slot_result::INSUFFICIENT_FUNDS),
            Some("ERR_BANKSLOT_INSUFFICIENT_FUNDS")
        );
        assert_eq!(
            bank_slot_error_key(bank_slot_result::NOTBANKER),
            Some("ERR_BANKSLOT_NOTBANKER")
        );
        assert_eq!(bank_slot_error_key(bank_slot_result::OK), None);
        assert_eq!(bank_slot_error_key(99), None, "the feed's own \"(n)\" arm");
        // Every key is a catalog row, and the insufficient-funds one is the spoken one (1815).
        for key in [
            "ERR_BANKSLOT_FAILED_TOO_MANY",
            "ERR_BANKSLOT_INSUFFICIENT_FUNDS",
            "ERR_BANKSLOT_NOTBANKER",
        ] {
            let r = benilla_ui::messages::by_key(key).expect("catalog row");
            assert_eq!(r.kind, benilla_ui::messages::MsgKind::Error, "{key}");
        }
        assert_eq!(
            benilla_ui::messages::by_key("ERR_BANKSLOT_INSUFFICIENT_FUNDS")
                .unwrap()
                .type_tag,
            0x16
        );
    }
}
