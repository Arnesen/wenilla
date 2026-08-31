//! The charter session's **system lines** — the messages the engine composes, and *which of the
//! client's two message channels each one goes to*.
//!
//! The split is the part a re-implementation cannot guess, and we did guess it wrong first: by the
//! key names, `ERR_PETITION_SIGNED_S` and `ERR_PETITION_ALREADY_SIGNED` look like the same kind of
//! thing. They are not. wow-re
//! `system/object-layer/scratch/petition-wire-law.md` §4 read the message catalog at `0xb4b498`
//! (stride `0x14`, field `+4` = kind, routed through the four-entry table at `0x496888`) and swept
//! every row:
//!
//! - the **four** `_S`/success ids `0x141`-`0x144` carry kind `0` → the shared chat chokepoint
//!   `0x49a870` with the row's own `+0x10` chat type, which is `0xa` → **`CHAT_MSG_SYSTEM`**, the
//!   same channel every `ERR_GUILD_*` line uses;
//! - the **five** refusals `0x145`-`0x149`, and `ERR_NO_GUILD_CHARTER` (`0x7c`), carry kind `2` →
//!   `UI_ERROR_MESSAGE` (`0xe0`), the **red UIErrorsFrame line**.
//!
//! That sweep is no longer this module's to carry: since decision 1770 the whole registry ships as
//! [`benilla_ui::messages`], and each line below asks it by key. The paragraphs above stay because
//! they record *why* the split is not guessable — the reason a lookup was worth building.
//!
//! Two more facts from the same sweep that shape the tables below:
//!
//! - **`PETITION_SIGN_NEED_MORE` (4) has no user-visible message on the SIGN path at all.** It
//!   falls into the default arm with `>= 6` and reaches only the debug console
//!   (`0x63cb50("Petition error")`). It *does* have one on the turn-in path. Same code, two
//!   packets, two behaviours.
//! - **A successful turn-in emits no text whatsoever** (`0x5ef166`). It fires
//!   `GUILD_REGISTRAR_CLOSED` and says nothing. This file first shipped an `ERR_GUILD_FOUNDER_S`
//!   line there, reasoned from the fact that vmangos never sends the guild-command result that
//!   would produce one; the binary settles it — founding a guild really is silent, and the player
//!   learns of it from the guild pane lighting up.
//!
//! Every one of these rows also carries a named sound cue (`+0xc = 0x44`): `0x142`/`0x143` play
//! `igPlayerInviteAccept` and `0x144`-`0x149` play `igPlayerInviteDecline`, while `0x141` has none.
//! Recorded here; wiring the cue is [`super`]'s job, not this module's.

use benilla_protocol::messages::petition_result;

use crate::ui_action::MsgKind;

/// One message this module composes: the **catalog key**, which names both the row and — through
/// [`benilla_ui::messages`] — the surface it goes to, and the 1.12 text.
///
/// The key rides beside the text rather than the surface doing so, because the surface is not this
/// module's to know: it is the message record's `+0x04`, and reading it here from the catalog is
/// what retired the hand-swept table this file used to carry (decision 1770). The five
/// `ERR_GUILD_NAME_*` rows below were recorded as **INFERRED** for exactly that reason; the sweep
/// confirms all five are kind 2, so the inference is now a lookup.
#[derive(Debug, Clone, Copy)]
struct Msg {
    key: &'static str,
    text: &'static str,
}

const fn msg(key: &'static str, text: &'static str) -> Msg {
    Msg { key, text }
}

// ── The templates, quoted verbatim from the reference's own patch chain; the `GlobalStrings.lua`
//    line is on each ──────────────────────────────────────────────────────────────────────────
const ERR_PETITION_ALREADY_SIGNED: Msg = msg(
    "ERR_PETITION_ALREADY_SIGNED",
    "You have already signed that guild charter.",
); // :1762
const ERR_PETITION_CREATOR: Msg = msg(
    "ERR_PETITION_CREATOR",
    "You can't sign your own guild charter.",
); // :1763
const ERR_PETITION_DECLINED_S: Msg = msg(
    "ERR_PETITION_DECLINED_S",
    "%s has declined your guild invitation.",
); // :1764
const ERR_PETITION_IN_GUILD: Msg = msg("ERR_PETITION_IN_GUILD", "You are already in a guild."); // :1765
const ERR_PETITION_NOT_ENOUGH_SIGNATURES: Msg = msg(
    "ERR_PETITION_NOT_ENOUGH_SIGNATURES",
    "You need more signatures.",
); // :1766
const ERR_PETITION_NOT_SAME_SERVER: Msg = msg(
    "ERR_PETITION_NOT_SAME_SERVER",
    "That player is not from your server",
); // :1767
const ERR_PETITION_OFFERED_S: Msg = msg(
    "ERR_PETITION_OFFERED_S",
    "You have requested %s's signature.",
); // :1768
const ERR_PETITION_SIGNED: Msg = msg("ERR_PETITION_SIGNED", "Guild charter signed."); // :1769
const ERR_PETITION_SIGNED_S: Msg =
    msg("ERR_PETITION_SIGNED_S", "%s has signed your guild charter."); // :1770
const ERR_NO_GUILD_CHARTER: Msg = msg("ERR_NO_GUILD_CHARTER", "You don't have a guild charter."); // :1745

// The guild-name validator's messages (`0x4f5160`'s seven arms). Only the three
// `benilla_ui::script::validate_guild_name` implements can be produced; the rest are here so the
// key→text map is complete and a later carve of `0x6c9b70` only has to add the branch.
const ERR_GUILD_ENTER_NAME: Msg = msg("ERR_GUILD_ENTER_NAME", "Enter a name for your guild."); // :1603
const ERR_GUILD_NAME_INVALID: Msg = msg(
    "ERR_GUILD_NAME_INVALID",
    "That name contains invalid characters,  Enter a new name.",
); // :1616
const ERR_GUILD_NAME_INVALID_SPACE: Msg = msg(
    "ERR_GUILD_NAME_INVALID_SPACE",
    "Guild names cannot start or end with a space.  Enter a new name.",
); // :1617
const ERR_GUILD_NAME_NAME_CONSECUTIVE_SPACES: Msg = msg(
    "ERR_GUILD_NAME_NAME_CONSECUTIVE_SPACES",
    "Consecutive spaces are not allowed.  Enter a new name.",
); // :1619
const ERR_GUILD_NAME_TOO_SHORT: Msg = msg(
    "ERR_GUILD_NAME_TOO_SHORT",
    "That name is too short.  Enter a new name.",
); // :1622

/// One composed line: the surface its message record names, and the text.
pub(super) type Line = (MsgKind, String);

/// A line with nothing to fill.
fn line(m: Msg) -> Line {
    (benilla_ui::messages::kind_of(m.key), m.text.to_string())
}

/// A line with one `%s` substitution.
fn fill(m: Msg, arg: &str) -> Line {
    (
        benilla_ui::messages::kind_of(m.key),
        m.text.replacen("%s", arg, 1),
    )
}

/// The line one `SMSG_PETITION_SIGN_RESULTS` prints **when the signer is us** — the switch at
/// `0x5eeff5`'s "I signed" leg.
///
/// The owner's copy of the same packet is a different path entirely: it appends the signer and
/// emits `ERR_PETITION_SIGNED_S`, and never inspects the result at all ([`signed_by_other`]).
///
/// `NEED_MORE` and anything `>= 6` return `None` — the default arm reaches only the debug console.
pub(super) fn my_sign_line(result: u32) -> Option<Line> {
    Some(match result {
        petition_result::OK => line(ERR_PETITION_SIGNED),
        petition_result::ALREADY_SIGNED => line(ERR_PETITION_ALREADY_SIGNED),
        petition_result::ALREADY_IN_GUILD => line(ERR_PETITION_IN_GUILD),
        petition_result::CANT_SIGN_OWN => line(ERR_PETITION_CREATOR),
        petition_result::NOT_SERVER => line(ERR_PETITION_NOT_SAME_SERVER),
        // `NEED_MORE` (4) and >= 6: the default arm, debug console only.
        _ => return None,
    })
}

/// The line the **owner** gets when somebody else signs — `ERR_PETITION_SIGNED_S`, a chat line, and
/// emitted **only when the signer's name is already cached** (`0x4f42f6`); on a cache miss the
/// client increments its pending-name counter instead and says nothing until the name lands.
pub(super) fn signed_by_other(name: &str) -> Line {
    fill(ERR_PETITION_SIGNED_S, name)
}

/// The line one `SMSG_TURN_IN_PETITION_RESULTS` prints. **Success prints nothing** — see the module
/// doc; it is not an omission here.
pub(super) fn turn_in_line(result: u32) -> Option<Line> {
    Some(match result {
        petition_result::ALREADY_IN_GUILD => line(ERR_PETITION_IN_GUILD),
        petition_result::NEED_MORE => line(ERR_PETITION_NOT_ENOUGH_SIGNATURES),
        _ => return None,
    })
}

/// The line an inbound `MSG_PETITION_DECLINE` prints to the charter's owner — a chat line, and
/// **only if the declining player's name is already cached**. There is no query and no retry: an
/// uncached name means the owner is told nothing at all (`0x5ef12a`/`0x5ef139`).
pub(super) fn declined_line(name: &str) -> Line {
    fill(ERR_PETITION_DECLINED_S, name)
}

/// The local echo for a charter we just offered — emitted **optimistically on the send**, with no
/// server confirmation (`0x4f48fa`). Nothing comes back on success; the *target* is the one the
/// server answers.
pub(super) fn offered_line(name: &str) -> Line {
    fill(ERR_PETITION_OFFERED_S, name)
}

/// Offering a charter to yourself — guard 6 of `OfferPetition`'s eight (`0x4f4839`), which emits
/// `ERR_PETITION_CREATOR`, the same red line signing your own charter gets.
pub(super) fn self_offer_line() -> Line {
    line(ERR_PETITION_CREATOR)
}

/// The local refusal when `TurnInGuildCharter()`'s bag scan finds no charter — a **red** line
/// (`0x5ef49a` emits id `0x7c`, kind 2), and no packet is built.
pub(super) fn no_charter_line() -> Line {
    line(ERR_NO_GUILD_CHARTER)
}

/// The line for a name the client's own validator refused
/// ([`benilla_ui::script::validate_guild_name`]) — a red line.
///
/// **These five rows were recorded here as INFERRED** — the sweep that settled the nine petition
/// ids and `0x7c` never covered the `0x75`-`0x7b` band, so the kind was reasoned from the fact
/// that every refusal in the swept set is kind 2. The full catalog (decision 1770) confirms all
/// five: the inference was right, and it is a lookup now rather than a reason.
///
/// An unknown key falls back to the generic invalid-name text rather than showing a raw key.
pub(super) fn name_refused_line(key: &str) -> Line {
    line(match key {
        "ERR_GUILD_ENTER_NAME" => ERR_GUILD_ENTER_NAME,
        "ERR_GUILD_NAME_INVALID_SPACE" => ERR_GUILD_NAME_INVALID_SPACE,
        "ERR_GUILD_NAME_NAME_CONSECUTIVE_SPACES" => ERR_GUILD_NAME_NAME_CONSECUTIVE_SPACES,
        "ERR_GUILD_NAME_TOO_SHORT" => ERR_GUILD_NAME_TOO_SHORT,
        _ => ERR_GUILD_NAME_INVALID,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The channel split, which does not follow the key names.** Success to chat, refusal to the
    /// red frame — swept out of the message catalog, not guessed. This test is the one that would
    /// fail if somebody "tidied" the two into a single `String` return.
    #[test]
    fn successes_go_to_chat_and_refusals_go_to_the_red_line() {
        assert_eq!(
            my_sign_line(petition_result::OK),
            Some((MsgKind::Chat, "Guild charter signed.".into()))
        );
        assert_eq!(
            signed_by_other("Bob"),
            (MsgKind::Chat, "Bob has signed your guild charter.".into())
        );
        assert_eq!(
            declined_line("Bob"),
            (
                MsgKind::Chat,
                "Bob has declined your guild invitation.".into()
            )
        );
        assert_eq!(
            offered_line("Bob"),
            (MsgKind::Chat, "You have requested Bob's signature.".into())
        );

        for (code, text) in [
            (
                petition_result::ALREADY_SIGNED,
                "You have already signed that guild charter.",
            ),
            (
                petition_result::ALREADY_IN_GUILD,
                "You are already in a guild.",
            ),
            (
                petition_result::CANT_SIGN_OWN,
                "You can't sign your own guild charter.",
            ),
            (
                petition_result::NOT_SERVER,
                "That player is not from your server",
            ),
        ] {
            assert_eq!(
                my_sign_line(code),
                Some((MsgKind::Error, text.into())),
                "code {code} is a RED line"
            );
        }
        assert_eq!(
            no_charter_line(),
            (MsgKind::Error, "You don't have a guild charter.".into())
        );
    }

    /// **`NEED_MORE` is silent on the sign path and loud on the turn-in path** — one result code,
    /// two packets, two behaviours. Reading the enum and giving both packets the same table is the
    /// mistake this pins, and it is the shape the first version of this file had.
    #[test]
    fn need_more_is_silent_when_signing_and_loud_when_turning_in() {
        assert_eq!(
            my_sign_line(petition_result::NEED_MORE),
            None,
            "the sign switch's default arm reaches only the debug console"
        );
        assert_eq!(
            turn_in_line(petition_result::NEED_MORE),
            Some((MsgKind::Error, "You need more signatures.".into()))
        );
    }

    /// A successful turn-in says **nothing**. The window closing is the whole of the feedback.
    #[test]
    fn a_successful_turn_in_prints_no_line_at_all() {
        assert_eq!(turn_in_line(petition_result::OK), None);
        // …and the codes the turn-in packet cannot carry stay silent rather than borrowing the
        // sign path's wording.
        assert_eq!(turn_in_line(petition_result::CANT_SIGN_OWN), None);
        assert_eq!(turn_in_line(petition_result::ALREADY_SIGNED), None);
    }

    /// **Every string in this file, checked against the shipped `GlobalStrings.lua`.**
    ///
    /// Three of the five validator lines shipped here as *paraphrases* — "Names may not begin or
    /// end with a space." where the real client says "Guild names cannot start or end with a
    /// space.  Enter a new name." — because they were written from the key name instead of copied.
    /// Nothing else could catch that: the text is never compared to anything at runtime, so a
    /// paraphrase renders, reads plausibly, and is simply not what the game says. This test is the
    /// oracle, and it is `ui_items::equip_error`'s, applied one table over.
    #[test]
    fn every_line_matches_the_real_global_strings() {
        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let src = chain
            .read_file("Interface\\FrameXML\\GlobalStrings.lua")
            .expect("GlobalStrings.lua in the chain");
        let s = benilla_ui::script::UiScript::new().expect("VM");
        s.run(&String::from_utf8_lossy(&src)).expect("runs clean");
        let g = |key: &str| {
            s.lua()
                .globals()
                .get::<String>(key)
                .unwrap_or_else(|e| panic!("{key} missing from GlobalStrings: {e}"))
        };

        for m in [
            ERR_PETITION_ALREADY_SIGNED,
            ERR_PETITION_CREATOR,
            ERR_PETITION_DECLINED_S,
            ERR_PETITION_IN_GUILD,
            ERR_PETITION_NOT_ENOUGH_SIGNATURES,
            ERR_PETITION_NOT_SAME_SERVER,
            ERR_PETITION_OFFERED_S,
            ERR_PETITION_SIGNED,
            ERR_PETITION_SIGNED_S,
            ERR_NO_GUILD_CHARTER,
            ERR_GUILD_ENTER_NAME,
            ERR_GUILD_NAME_INVALID,
            ERR_GUILD_NAME_INVALID_SPACE,
            ERR_GUILD_NAME_NAME_CONSECUTIVE_SPACES,
            ERR_GUILD_NAME_TOO_SHORT,
        ] {
            assert_eq!(g(m.key), m.text, "{} is not what the client says", m.key);
        }
    }

    /// The validator's keys resolve to their own text, and an unknown key degrades to the generic
    /// invalid-name line rather than surfacing a raw key to the player.
    #[test]
    fn refused_names_resolve_their_key_and_degrade_safely() {
        assert_eq!(
            name_refused_line("ERR_GUILD_ENTER_NAME"),
            (MsgKind::Error, "Enter a name for your guild.".into())
        );
        assert_eq!(
            name_refused_line("ERR_GUILD_NAME_INVALID_SPACE"),
            (
                MsgKind::Error,
                "Guild names cannot start or end with a space.  Enter a new name.".into()
            )
        );
        assert_eq!(
            name_refused_line("ERR_SOMETHING_UNCARVED"),
            (
                MsgKind::Error,
                "That name contains invalid characters,  Enter a new name.".into()
            ),
            "never a raw key on screen"
        );
    }
}
