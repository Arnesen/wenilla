//! The **NPC-session range guard** — the one standardized rule for every UI window bound to a live
//! NPC (merchant stock, gossip menu, questgiver panel): when the player walks out of the
//! NPC-service range, or the NPC despawns, the window client-side-closes. That close is the exact
//! no-packet clear the window's own close button does (vanilla sends nothing for any of the three);
//! each window's feed then diffs the cleared state into its `*_CLOSED` event and the panel hides.
//!
//! The threshold is the byte-verified NPC-service gate the cursor grays at
//! ([`SERVICE_RANGE_SQ`], 5.5556 yd center-to-center — `crate::target::cursor_mode`), so a window
//! closes exactly where the cursor says its NPC is out of service — one law for "out of service".
//! Whether the real client's auto-close shares that exact gate is INFERRED (its close mechanism
//! isn't RE'd); the constant is the knob if it reads too eager or too lax in play.

use bevy::prelude::*;

use crate::net::{GuidIndex, SelfPlayer};
use crate::target::SERVICE_RANGE_SQ;
use benilla_world::schedule::WorldStage;

/// Owns the cross-window session state: [`InteractNpc`] and the one system that feeds it.
///
/// It has a plugin of its own because [`InteractNpc`] has **two** consumers that must not depend on
/// each other — the portrait booth's `"npc"` token (decision 0081) and the interaction face-me
/// (`crate::net::motion`'s display-facing chain, decision 1467). It used to be initialised and fed
/// from inside the portrait plugin, which silently made a *facing* law contingent on the portrait
/// plugin being mounted; a shared resource belongs to the thing it describes.
pub(crate) struct UiSessionPlugin;

impl Plugin for UiSessionPlugin {
    fn build(&self, app: &mut App) {
        // In `WorldStage::Net` so the facing chain can order itself against it. Deliberately
        // unordered w.r.t. the apply pass: the sessions it reads are open for *seconds*, so
        // whether a window's first frame is seen now or next frame is invisible against an
        // ~8-frame facing ease.
        app.init_resource::<InteractNpc>()
            .add_systems(Update, feed_interact_npc.in_set(WorldStage::Net));
    }
}

/// A UI session bound to a live NPC (or, for the mailbox, a GameObject): the shared face the range
/// guard closes through. Implemented by [`crate::ui_merchant::MerchantOpen`],
/// [`crate::ui_gossip::GossipState`], [`crate::ui_quest::QuestGiver`],
/// [`crate::ui_trainer::TrainerOpen`], [`crate::ui_taxi::TaxiState`],
/// [`crate::ui_mail::MailOpen`], [`crate::ui_trade::TradeSession`], [`crate::ui_bank::BankOpen`]
/// and [`crate::ui_auction::AuctionOpen`] — plus the two free-standing *questions*, which are
/// sessions for the same reason a window is ([`crate::ui_binder::BinderState`],
/// [`crate::ui_talent_wipe::TalentWipeState`]: the reference re-runs its own interact-range test
/// against a latched guid, so "walked away" retracting the dialog is its law, not our tidiness).
/// Each *NPC* window registers
/// [`close_npc_session_out_of_range::<T>`] ahead of its feed so the clear turns into the window's
/// `*_CLOSED` event the same frame — trade does **not** (its cancel is server-driven, decision 0592).
/// [`feed_interact_npc`] collapses the portrait-bound sessions into the [`InteractNpc`] the
/// portrait booth reads for its `"npc"` slot; the mailbox is deliberately excluded from that
/// collapse (its window icon is art, not a unit-model bake — decision 0544). Trade's "npc" is a
/// live *player* (the partner), baked exactly like a vendor (decision 0592).
pub(crate) trait NpcSession: Resource {
    /// The NPC this session is bound to; `None` = no window open.
    fn npc(&self) -> Option<u64>;
    /// The window's client-side close (no packet) — the same clear its close button does.
    fn close(&mut self);
}

/// Did an NPC-bound window switch to a **different** NPC of the same kind while it was already open
/// (a `Some(a) → Some(b)`, `a != b`)? The real client can't just repaint: its `ShowUIPanel` early-
/// returns when the frame is already visible, so the only way MERCHANT_SHOW (etc.) re-plays the open
/// sound is if the frame is HIDDEN first. So the client fires `*_CLOSED` then `*_SHOW` on a
/// vendor→vendor (or gossip→gossip, quest→quest) change — a real close+open, both sounds. Each feed
/// reproduces that, then **consumes the close intent** its own `*_CLOSED`→OnHide→`CloseX` queued, so
/// the session it just re-opened to `b` is not wiped by the drain (decision 0096).
pub(crate) fn npc_switched(prev: Option<u64>, now: Option<u64>) -> bool {
    matches!((prev, now), (Some(a), Some(b)) if a != b)
}

/// Close `T`'s open session when the player leaves the NPC-service range or the NPC despawns.
pub(crate) fn close_npc_session_out_of_range<T: NpcSession>(
    mut session: ResMut<T>,
    index: Res<GuidIndex>,
    self_q: Query<&Transform, With<SelfPlayer>>,
    transforms: Query<&Transform>,
) {
    let Some(npc) = session.npc() else { return };
    let Some(self_tf) = self_q.iter().next() else {
        return;
    };
    let out = match index.0.get(&npc).and_then(|e| transforms.get(*e).ok()) {
        Some(tf) => tf.translation.distance_squared(self_tf.translation) > SERVICE_RANGE_SQ,
        None => true, // the NPC despawned out from under the window
    };
    if out {
        debug!("ui_session: NPC {npc:#x} out of range/gone — client-side close");
        session.close();
    }
}

/// The one unit an NPC-interaction window points its portrait at: whichever [`NpcSession`] is
/// currently open (gossip / quest / merchant / trainer / taxi / trade / bank / auction — only one is
/// ever open in play),
/// resolved through the [`GuidIndex`] to its live world entity. The portrait booth reads this for
/// the `"npc"` token exactly as it reads [`crate::target::Selection`] for `"target"` — the
/// decision-0105 face bake, wired to the interaction arc's NPC (decision 0081). `None` = no NPC
/// window open, so the booth empties; the ring is hidden with its window then, so the dark disc
/// never shows.
#[derive(Resource, Default)]
pub(crate) struct InteractNpc(pub(crate) Option<Entity>, pub(crate) Option<u64>);

/// Collapse the portrait-bound sessions into [`InteractNpc`] each frame: the open one's guid,
/// resolved to its entity. One deterministic writer (not one publish-system per session), so there is
/// no cross-system race over who owns the `"npc"` token — the sessions are mutually exclusive, and a
/// bare `.or` chain is the whole rule. `Option<Res<_>>` keeps it safe in a headless test that mounts
/// the portrait plugin without every window plugin.
#[allow(clippy::too_many_arguments)]
pub(crate) fn feed_interact_npc(
    gossip: Option<Res<crate::ui_gossip::GossipState>>,
    quest: Option<Res<crate::ui_quest::QuestGiver>>,
    merchant: Option<Res<crate::ui_merchant::MerchantOpen>>,
    trainer: Option<Res<crate::ui_trainer::TrainerOpen>>,
    taxi: Option<Res<crate::ui_taxi::TaxiState>>,
    // Trade points the "npc" portrait at the partner (a live player) while its window is open — the
    // same booth path, one more mutually-exclusive session in the chain (decision 0592 P1).
    trade: Option<Res<crate::ui_trade::TradeSession>>,
    // The banker's portrait while the vault is open (decision 0604) — same booth path.
    bank: Option<Res<crate::ui_bank::BankOpen>>,
    // The auctioneer's, while the auction house is open — the same booth path again. Its absence
    // here is what left `AuctionPortraitTexture` a black disc: the window asks for `"npc"` in its
    // OnShow like every other NPC window, and nothing was answering.
    auction: Option<Res<crate::ui_auction::AuctionOpen>>,
    // The guild registrar's, while the charter window is open (decision 1672) — same booth path,
    // and it is NOT covered by the gossip arm above: the server closes the gossip menu before it
    // sends `SMSG_PETITION_SHOWLIST` (`Player.cpp:12428-12431` — `CloseGossip()` then
    // `SendPetitionShowList`), so by the time `GuildRegistrar_OnShow` asks for `"npc"` the gossip
    // session is already gone. Without this arm the window's portrait is the auctioneer's black
    // disc all over again and its name banner is blank.
    registrar: Option<Res<crate::ui_petition::GuildRegistrarState>>,
    // The stable master's, while the pet stable is open (decision 1684) — the black disc a THIRD
    // time, and this one could not have leaned on the gossip arm even by accident: benilla asks a
    // menuless stable master for its pet list DIRECTLY on the interact leg (decision 1680, the
    // client's own `0x5f05bc` path), so there is no gossip session behind it at all. The registrar
    // arm above is the same shape for the same reason.
    stable: Option<Res<crate::ui_stable::StableOpen>>,
    index: Option<Res<GuidIndex>>,
    mut out: ResMut<InteractNpc>,
) {
    let guid = gossip
        .and_then(|s| s.npc())
        .or_else(|| quest.and_then(|s| s.npc()))
        .or_else(|| merchant.and_then(|s| s.npc()))
        .or_else(|| trainer.and_then(|s| s.npc()))
        .or_else(|| taxi.and_then(|s| s.npc()))
        .or_else(|| trade.and_then(|s| s.npc()))
        .or_else(|| bank.and_then(|s| s.npc()))
        .or_else(|| auction.and_then(|s| s.npc()))
        .or_else(|| registrar.and_then(|s| s.npc()))
        .or_else(|| stable.and_then(|s| s.npc()));
    // Field 0 is the entity the portrait booth bakes and the facing chain steers by; field 1 is
    // the same NPC's **guid**, which `crate::ui_unit`'s feed needs to resolve the `"npc"` unit
    // token's name (a name lives in the `NameCache`, keyed by guid — there is no way back to one
    // from an entity). Both are set together and cleared together; a guid whose entity is not
    // streamed still names the unit, which is why they are two fields rather than one lookup.
    out.0 = guid.and_then(|g| index.and_then(|i| i.0.get(&g).copied()));
    out.1 = guid;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_merchant::MerchantOpen;

    /// Drive the guard in a minimal Bevy app over the merchant session (the trait's first
    /// consumer): in range keeps the window, stepping past the service gate closes it, and a
    /// despawned NPC closes it too.
    #[test]
    fn out_of_range_or_despawned_npc_closes_the_session() {
        let mut app = App::new();
        app.init_resource::<MerchantOpen>();
        app.init_resource::<GuidIndex>();
        app.add_systems(Update, close_npc_session_out_of_range::<MerchantOpen>);
        app.world_mut()
            .spawn((SelfPlayer, Transform::from_xyz(0.0, 0.0, 0.0)));
        let vendor = app
            .world_mut()
            .spawn(Transform::from_xyz(5.0, 0.0, 0.0))
            .id();
        app.world_mut()
            .resource_mut::<GuidIndex>()
            .0
            .insert(0x42, vendor);

        // 5.0 yd < the 5.5556 yd service gate: stays open.
        app.world_mut()
            .resource_mut::<MerchantOpen>()
            .open(0x42, vec![]);
        app.update();
        assert!(app.world().resource::<MerchantOpen>().is_open());

        // Step to 6 yd: past the gate — client-side close.
        *app.world_mut()
            .entity_mut(vendor)
            .get_mut::<Transform>()
            .unwrap() = Transform::from_xyz(6.0, 0.0, 0.0);
        app.update();
        assert!(!app.world().resource::<MerchantOpen>().is_open());

        // Re-open, then the NPC despawns out from under the window: closes too.
        app.world_mut()
            .resource_mut::<MerchantOpen>()
            .open(0x42, vec![]);
        app.world_mut().resource_mut::<GuidIndex>().0.remove(&0x42);
        app.update();
        assert!(!app.world().resource::<MerchantOpen>().is_open());
    }

    /// The `"npc"` portrait token's resolver ([`feed_interact_npc`]): the open NPC session's guid,
    /// mapped through the [`GuidIndex`] to its world entity. No window → None; an unindexed
    /// (not-yet-streamed) NPC → None, never a stale entity; the gossip session wins the `.or` chain
    /// when more than one carries a guid (the same NPC on a browse-goods hop).
    #[test]
    fn interact_npc_resolves_the_open_session_to_its_entity() {
        use crate::ui_gossip::GossipState;

        let mut app = App::new();
        app.init_resource::<GossipState>();
        app.init_resource::<MerchantOpen>();
        app.init_resource::<GuidIndex>();
        app.init_resource::<InteractNpc>();
        app.add_systems(Update, feed_interact_npc);

        let npc = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<GuidIndex>()
            .0
            .insert(0x42, npc);

        // No window open → no npc.
        app.update();
        assert_eq!(app.world().resource::<InteractNpc>().0, None);

        // A merchant opens on the indexed NPC → resolves to its entity.
        app.world_mut()
            .resource_mut::<MerchantOpen>()
            .open(0x42, vec![]);
        app.update();
        assert_eq!(app.world().resource::<InteractNpc>().0, Some(npc));

        // Gossip carries the same guid (a browse-goods hop) and wins the priority chain — still the
        // one NPC's entity.
        app.world_mut().resource_mut::<GossipState>().npc = Some(0x42);
        app.update();
        assert_eq!(app.world().resource::<InteractNpc>().0, Some(npc));

        // Bound to a guid with no index entry (NPC not streamed) → None, not the last entity.
        app.world_mut().resource_mut::<GossipState>().npc = Some(0xdead);
        app.update();
        assert_eq!(app.world().resource::<InteractNpc>().0, None);

        // Every window that draws a portrait must be IN this chain — a session missing from it
        // does not fail loudly, it just renders a black disc where the NPC's face goes, which is
        // exactly how the auction house shipped (director's report, 2026-08-22). Asserted per
        // session rather than in the abstract, because the omission is a missing `.or_else` and
        // nothing but a test can notice one.
        app.world_mut().resource_mut::<GossipState>().npc = None;
        app.world_mut().resource_mut::<MerchantOpen>().close();
        app.init_resource::<crate::ui_auction::AuctionOpen>();
        app.world_mut()
            .resource_mut::<crate::ui_auction::AuctionOpen>()
            .open(0x42, 1);
        app.update();
        assert_eq!(
            app.world().resource::<InteractNpc>().0,
            Some(npc),
            "the auctioneer's own portrait"
        );

        app.world_mut()
            .resource_mut::<crate::ui_auction::AuctionOpen>()
            .clear();

        // The stable master's (decision 1684) — the same black disc, reported by the director on
        // 2026-08-28. Note WHERE it shipped from: this very test already existed, with the
        // paragraph above it saying every portrait window must be in the chain — and the stable
        // still went out black, because the guard is **opt-in per session**. A window nobody
        // remembers to add here is a window nobody's test covers. Worse for this one than for the
        // auction house: benilla asks a menuless stable master for its list directly on the
        // interact leg (decision 1680), so there is no gossip session to accidentally carry it.
        app.init_resource::<crate::ui_stable::StableOpen>();
        app.world_mut()
            .resource_mut::<crate::ui_stable::StableOpen>()
            .open(0x42, 2, vec![]);
        app.update();
        assert_eq!(
            app.world().resource::<InteractNpc>().0,
            Some(npc),
            "the stable master's own portrait"
        );

        // Every session closes → None again.
        app.world_mut().resource_mut::<GossipState>().close();
        app.world_mut().resource_mut::<MerchantOpen>().close();
        app.world_mut()
            .resource_mut::<crate::ui_stable::StableOpen>()
            .clear();
        app.update();
        assert_eq!(app.world().resource::<InteractNpc>().0, None);
    }

    /// **The structural tripwire the three black discs earned** (decision 1684).
    ///
    /// The assertions above are opt-in: each names one session, and a window nobody remembers to
    /// add is a window nobody covers. That is not hypothetical — the auction house shipped a black
    /// portrait (0x-2026-08-22), the guild registrar was caught only because someone reasoned about
    /// the gossip close, and the stable master shipped black on 2026-08-28 with this very test
    /// already in the file telling everyone to add their session to it.
    ///
    /// So this one is exhaustive by construction: it reads the app's own sources, finds every
    /// `impl NpcSession for X`, and requires each `X` to be either **in the `.or` chain** or on the
    /// **explicit exclusion list** below with a stated reason. A new NPC window cannot be silent —
    /// it either wires its portrait or it says why it has none.
    #[test]
    fn every_npc_session_is_portrait_bound_or_explicitly_excluded() {
        use std::path::Path;

        /// Sessions that deliberately do NOT own the `"npc"` portrait token, each with the reason
        /// it does not. Adding a name here is a decision; leaving one out is a black disc.
        const EXCLUDED: &[(&str, &str)] = &[
            // Decision 0544: the mail window's icon is authored art, not a unit-model bake, so it
            // has no portrait to point and must not steal the token from a window that does.
            ("MailOpen", "its window icon is art, not a unit bake (0544)"),
            // Reached only from inside an open gossip menu, which is still open behind it and is
            // the FIRST arm of the chain — so the trainer's own NPC is already the answer. Unlike
            // the registrar's case, the server does not close the menu first.
            ("TalentWipeState", "rides the still-open gossip session"),
            ("BinderState", "rides the still-open gossip session"),
        ];

        // The chain's own source is the authority on what is wired — not a hand-copied list here,
        // which would be the very drift this test exists to catch.
        let this_file = include_str!("ui_session.rs");
        let chain = this_file
            .split_once("pub(crate) fn feed_interact_npc")
            .expect("feed_interact_npc")
            .1
            .split_once("\n}\n")
            .expect("end of feed_interact_npc")
            .0;

        let mut missing = Vec::new();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read_dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                // This file itself: the trait lives here and no session implements it here, but
                // the prose above spells the pattern out — so scanning ourselves finds the doc
                // comment and nothing real.
                if path.file_name().is_some_and(|f| f == "ui_session.rs") {
                    continue;
                }
                let src = std::fs::read_to_string(&path).expect("read source");
                for (_, rest) in src
                    .match_indices("impl NpcSession for ")
                    .map(|(i, _)| (i, &src[i + "impl NpcSession for ".len()..]))
                {
                    let ty: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if ty.is_empty() || EXCLUDED.iter().any(|(name, _)| *name == ty) {
                        continue;
                    }
                    // Wired = the chain mentions the type. The chain names each session by its
                    // resource type in a `Res<...>` parameter, so a substring test is exact enough
                    // and cannot be fooled by a comment (comments naming a type still mean someone
                    // thought about it, which is the point).
                    if !chain.contains(&ty) {
                        missing.push(format!("{ty} (in {})", path.display()));
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "these NpcSession windows own no `\"npc\"` portrait and are not on the exclusion \
             list, so each renders a BLACK DISC where the NPC's face goes — wire them into \
             `feed_interact_npc`'s chain, or add them to EXCLUDED with a reason: {missing:#?}"
        );
    }
}
