//! `GetChannelName` — the joined-channel lookup ([`crate::script::channel`]).
//!
//! The verb is small; the contract around it is not, and every test here is named after one claim.
//! The load-bearing one is [`a_channel_that_is_not_joined_answers_the_number_zero_never_nil`]: the
//! reference's own callers and the corpus both compare the first return numerically, so a nil there
//! does not degrade — it raises, at four call sites that work today.

use super::common::script;

/// Two channels joined in order, the way `ui_chat::feed` mirrors them after the server's
/// YOU_JOINED. Join ORDER is the whole numbering law, so the fixture is deliberately not sorted.
fn joined() -> crate::script::UiScript {
    let mut s = script();
    s.set_joined_channels(vec!["World".into(), "Trade - City".into()]);
    s
}

/// **A joined channel answers its 1-based slot, its name, and an instanceID.** The slot is the
/// position in JOIN order — not a DBC id, not an alphabetical rank.
#[test]
fn a_joined_channel_answers_its_slot_name_and_instance() {
    let s = joined();
    let (id, name, instance): (i64, String, i64) = s.eval("return GetChannelName(2)").unwrap();
    assert_eq!(id, 2, "the 1-based slot in join order");
    assert_eq!(name, "Trade - City");
    assert_eq!(
        instance, 0,
        "instanceID is 0 on every vanilla emulator, and a NUMBER so a caller can compare it"
    );
}

/// **The name form resolves, case-insensitively, and yields the same slot as the index form.**
/// Half the corpus sites look up by name (`GetChannelName("world")`, `GetChannelName("Trade -
/// City")`), and `_LazyPig` passes `"world"` for a channel the server names `"World"`.
#[test]
fn the_name_form_resolves_case_insensitively_to_the_same_slot() {
    let s = joined();
    let by_name: i64 = s.eval("return GetChannelName('world')").unwrap();
    let by_index: i64 = s.eval("return GetChannelName(1)").unwrap();
    assert_eq!(by_name, 1);
    assert_eq!(by_name, by_index, "both directions must agree on the slot");

    let name: String = s
        .eval("local _, n = GetChannelName('TRADE - CITY') return n")
        .unwrap();
    assert_eq!(
        name, "Trade - City",
        "the answer is the JOINED spelling, not the caller's"
    );
}

/// **Not joined ⇒ the NUMBER 0. Never nil.** This is the trap, and it is the only way this verb can
/// break working code rather than merely fail to help it.
///
/// Verified from both sides. The reference compares it numerically at `ChatFrame.lua:2114`
/// (`if ( channelNum > 0 )`) and `l.2232` (`if ( channelNum <= 0 ) then return end`); so does the
/// corpus at `_LazyPig/LazyPig.lua:1996` (`if id > 0 then`). A nil first return turns every one of
/// those into "attempt to compare nil with number" — a loud failure in code that works today.
///
/// Asserted by DOING the comparison, not by inspecting the type, because the comparison is the
/// thing that must not raise.
#[test]
fn a_channel_that_is_not_joined_answers_the_number_zero_never_nil() {
    let s = joined();

    let id: i64 = s.eval("return GetChannelName('NoSuchChannel')").unwrap();
    assert_eq!(id, 0);

    // The reference's own guard shape, run for real: it must evaluate, not raise.
    let guard: bool = s
        .eval("local id = GetChannelName('NoSuchChannel') return id > 0")
        .unwrap();
    assert!(
        !guard,
        "the reference's `if ( channelNum > 0 )` must run and be false"
    );

    let out_of_range: bool = s
        .eval("local id = GetChannelName(99) return id > 0")
        .unwrap();
    assert!(!out_of_range, "an out-of-range index takes the same branch");

    let zero: bool = s
        .eval("local id = GetChannelName(0) return id > 0")
        .unwrap();
    assert!(
        !zero,
        "the client bounds-checks 1 <= n <= count, so 0 is not a slot"
    );
}

/// **A numeric STRING resolves as a number**, because `ChatFrame.lua:2113` hands one straight in:
/// it `gsub`s `/1` down to `"1"` and calls `GetChannelName(channel)`. On the real client Lua's own
/// coercion makes that work; ours must not treat it as a channel literally named "1".
#[test]
fn a_numeric_string_resolves_as_an_index_not_as_a_name() {
    let s = joined();
    let id: i64 = s.eval("return GetChannelName('2')").unwrap();
    assert_eq!(id, 2, "the `/2` slash-command path depends on this");
}

/// **Empty until the server confirms a join.** `ui_chat::feed` appends on the server's YOU_JOINED
/// notice, never on the request, so a session that has asked and not been answered has no slots —
/// and the verb must still answer 0 rather than raise on an empty list.
#[test]
fn nothing_is_joined_before_the_server_confirms_it() {
    let s = script();
    let id: i64 = s.eval("return GetChannelName('World')").unwrap();
    assert_eq!(id, 0);
}
