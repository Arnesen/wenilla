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

// ── The templates, quoted verbatim from the reference's own patch chain; the `GlobalStrings.lua`
//    line is on each ──────────────────────────────────────────────────────────────────────────
const ERR_PETITION_ALREADY_SIGNED: &str = "You have already signed that guild charter."; // :1762
const ERR_PETITION_CREATOR: &str = "You can't sign your own guild charter."; // :1763
const ERR_PETITION_DECLINED_S: &str = "%s has declined your guild invitation."; // :1764
const ERR_PETITION_IN_GUILD: &str = "You are already in a guild."; // :1765
const ERR_PETITION_NOT_ENOUGH_SIGNATURES: &str = "You need more signatures."; // :1766
const ERR_PETITION_NOT_SAME_SERVER: &str = "That player is not from your server"; // :1767
const ERR_PETITION_OFFERED_S: &str = "You have requested %s's signature."; // :1768
const ERR_PETITION_SIGNED: &str = "Guild charter signed."; // :1769
const ERR_PETITION_SIGNED_S: &str = "%s has signed your guild charter."; // :1770
const ERR_NO_GUILD_CHARTER: &str = "You don't have a guild charter."; // :1745

// The guild-name validator's messages (`0x4f5160`'s seven arms). Only the three
// `benilla_ui::script::validate_guild_name` implements can be produced; the rest are here so the
// key→text map is complete and a later carve of `0x6c9b70` only has to add the branch.
const ERR_GUILD_ENTER_NAME: &str = "Enter a name for your guild."; // :1603
const ERR_GUILD_NAME_INVALID: &str = "That name contains invalid characters,  Enter a new name."; // :1616
const ERR_GUILD_NAME_INVALID_SPACE: &str =
    "Guild names cannot start or end with a space.  Enter a new name."; // :1617
const ERR_GUILD_NAME_NAME_CONSECUTIVE_SPACES: &str =
    "Consecutive spaces are not allowed.  Enter a new name."; // :1619
const ERR_GUILD_NAME_TOO_SHORT: &str = "That name is too short.  Enter a new name."; // :1622

/// Which of the client's two channels a line goes to — the `+4 kind` field, as a type.
///
/// Modelled rather than left implicit because the split does not follow the key names, and a
/// `String` return would let a future edit put a refusal in the chat log without anything noticing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Line {
    /// Kind `0` → the `CHAT_MSG_SYSTEM` chat line.
    Chat(String),
    /// Kind `2` → the red `UI_ERROR_MESSAGE` frame.
    Error(String),
}

/// One `%s` substitution.
fn fill(template: &str, arg: &str) -> String {
    template.replacen("%s", arg, 1)
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
        petition_result::OK => Line::Chat(ERR_PETITION_SIGNED.to_string()),
        petition_result::ALREADY_SIGNED => Line::Error(ERR_PETITION_ALREADY_SIGNED.to_string()),
        petition_result::ALREADY_IN_GUILD => Line::Error(ERR_PETITION_IN_GUILD.to_string()),
        petition_result::CANT_SIGN_OWN => Line::Error(ERR_PETITION_CREATOR.to_string()),
        petition_result::NOT_SERVER => Line::Error(ERR_PETITION_NOT_SAME_SERVER.to_string()),
        // `NEED_MORE` (4) and >= 6: the default arm, debug console only.
        _ => return None,
    })
}

/// The line the **owner** gets when somebody else signs — `ERR_PETITION_SIGNED_S`, a chat line, and
/// emitted **only when the signer's name is already cached** (`0x4f42f6`); on a cache miss the
/// client increments its pending-name counter instead and says nothing until the name lands.
pub(super) fn signed_by_other(name: &str) -> Line {
    Line::Chat(fill(ERR_PETITION_SIGNED_S, name))
}

/// The line one `SMSG_TURN_IN_PETITION_RESULTS` prints. **Success prints nothing** — see the module
/// doc; it is not an omission here.
pub(super) fn turn_in_line(result: u32) -> Option<Line> {
    Some(match result {
        petition_result::ALREADY_IN_GUILD => Line::Error(ERR_PETITION_IN_GUILD.to_string()),
        petition_result::NEED_MORE => Line::Error(ERR_PETITION_NOT_ENOUGH_SIGNATURES.to_string()),
        _ => return None,
    })
}

/// The line an inbound `MSG_PETITION_DECLINE` prints to the charter's owner — a chat line, and
/// **only if the declining player's name is already cached**. There is no query and no retry: an
/// uncached name means the owner is told nothing at all (`0x5ef12a`/`0x5ef139`).
pub(super) fn declined_line(name: &str) -> Line {
    Line::Chat(fill(ERR_PETITION_DECLINED_S, name))
}

/// The local echo for a charter we just offered — emitted **optimistically on the send**, with no
/// server confirmation (`0x4f48fa`). Nothing comes back on success; the *target* is the one the
/// server answers.
pub(super) fn offered_line(name: &str) -> Line {
    Line::Chat(fill(ERR_PETITION_OFFERED_S, name))
}

/// Offering a charter to yourself — guard 6 of `OfferPetition`'s eight (`0x4f4839`), which emits
/// `ERR_PETITION_CREATOR`, the same red line signing your own charter gets.
pub(super) fn self_offer_line() -> Line {
    Line::Error(ERR_PETITION_CREATOR.to_string())
}

/// The local refusal when `TurnInGuildCharter()`'s bag scan finds no charter — a **red** line
/// (`0x5ef49a` emits id `0x7c`, kind 2), and no packet is built.
pub(super) fn no_charter_line() -> Line {
    Line::Error(ERR_NO_GUILD_CHARTER.to_string())
}

/// The line for a name the client's own validator refused
/// ([`benilla_ui::script::validate_guild_name`]) — a red line, by the class every other
/// `ERR_GUILD_NAME_*` row in the catalog belongs to.
///
/// **The kind of these five rows is INFERRED**, unlike the nine petition rows above: that round
/// swept the petition ids and `0x7c`, not the `0x75`-`0x7b` band. They are refusals, and every
/// refusal in the swept set is kind 2. An unknown key falls back to the generic invalid-name text
/// rather than showing a raw key.
pub(super) fn name_refused_line(key: &str) -> Line {
    Line::Error(
        match key {
            "ERR_GUILD_ENTER_NAME" => ERR_GUILD_ENTER_NAME,
            "ERR_GUILD_NAME_INVALID_SPACE" => ERR_GUILD_NAME_INVALID_SPACE,
            "ERR_GUILD_NAME_NAME_CONSECUTIVE_SPACES" => ERR_GUILD_NAME_NAME_CONSECUTIVE_SPACES,
            "ERR_GUILD_NAME_TOO_SHORT" => ERR_GUILD_NAME_TOO_SHORT,
            _ => ERR_GUILD_NAME_INVALID,
        }
        .to_string(),
    )
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
            Some(Line::Chat("Guild charter signed.".into()))
        );
        assert_eq!(
            signed_by_other("Bob"),
            Line::Chat("Bob has signed your guild charter.".into())
        );
        assert_eq!(
            declined_line("Bob"),
            Line::Chat("Bob has declined your guild invitation.".into())
        );
        assert_eq!(
            offered_line("Bob"),
            Line::Chat("You have requested Bob's signature.".into())
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
                Some(Line::Error(text.into())),
                "code {code} is a RED line"
            );
        }
        assert_eq!(
            no_charter_line(),
            Line::Error("You don't have a guild charter.".into())
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
            Some(Line::Error("You need more signatures.".into()))
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

        for (key, ours) in [
            ("ERR_PETITION_ALREADY_SIGNED", ERR_PETITION_ALREADY_SIGNED),
            ("ERR_PETITION_CREATOR", ERR_PETITION_CREATOR),
            ("ERR_PETITION_DECLINED_S", ERR_PETITION_DECLINED_S),
            ("ERR_PETITION_IN_GUILD", ERR_PETITION_IN_GUILD),
            (
                "ERR_PETITION_NOT_ENOUGH_SIGNATURES",
                ERR_PETITION_NOT_ENOUGH_SIGNATURES,
            ),
            ("ERR_PETITION_NOT_SAME_SERVER", ERR_PETITION_NOT_SAME_SERVER),
            ("ERR_PETITION_OFFERED_S", ERR_PETITION_OFFERED_S),
            ("ERR_PETITION_SIGNED", ERR_PETITION_SIGNED),
            ("ERR_PETITION_SIGNED_S", ERR_PETITION_SIGNED_S),
            ("ERR_NO_GUILD_CHARTER", ERR_NO_GUILD_CHARTER),
            ("ERR_GUILD_ENTER_NAME", ERR_GUILD_ENTER_NAME),
            ("ERR_GUILD_NAME_INVALID", ERR_GUILD_NAME_INVALID),
            ("ERR_GUILD_NAME_INVALID_SPACE", ERR_GUILD_NAME_INVALID_SPACE),
            (
                "ERR_GUILD_NAME_NAME_CONSECUTIVE_SPACES",
                ERR_GUILD_NAME_NAME_CONSECUTIVE_SPACES,
            ),
            ("ERR_GUILD_NAME_TOO_SHORT", ERR_GUILD_NAME_TOO_SHORT),
        ] {
            assert_eq!(g(key), ours, "{key} is not what the client says");
        }
    }

    /// The validator's keys resolve to their own text, and an unknown key degrades to the generic
    /// invalid-name line rather than surfacing a raw key to the player.
    #[test]
    fn refused_names_resolve_their_key_and_degrade_safely() {
        assert_eq!(
            name_refused_line("ERR_GUILD_ENTER_NAME"),
            Line::Error("Enter a name for your guild.".into())
        );
        assert_eq!(
            name_refused_line("ERR_GUILD_NAME_INVALID_SPACE"),
            Line::Error("Guild names cannot start or end with a space.  Enter a new name.".into())
        );
        assert_eq!(
            name_refused_line("ERR_SOMETHING_UNCARVED"),
            Line::Error("That name contains invalid characters,  Enter a new name.".into()),
            "never a raw key on screen"
        );
    }
}
