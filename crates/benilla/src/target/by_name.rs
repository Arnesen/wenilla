//! Selection **by name** — the shared resolver behind `/target` and `/assist` (decision 0886).
//!
//! The reference has ONE resolver, `0x493aa0`, parameterised per caller; `/target`, `/assist` and
//! `/follow` are the same search with different arguments (wow-re
//! `object-layer/scratch/targeting-by-name.md`, §5-cross-checked):
//!
//! | binding | typemask | filter mode | exact-only |
//! |---|---|---|---|
//! | `TargetByName 0x489d60` | 8 = UNIT (creatures **and** players) | 0 | Lua arg #2, never supplied |
//! | `AssistByName 0x489c40` | 0x10 = **PLAYER only** | 0 | hardcoded 0 |
//! | `FollowByName 0x489ec0` | 0x10 = PLAYER only | 2 (`CanAssist` + alive) | Lua arg #2 |
//!
//! **Filter mode 0 is the whole point of the shape**: it underflows `0x493e40`'s jump table
//! (`dec eax; cmp eax,3; ja`) into the default arm `mov eax,1; ret 4` — *accept unconditionally*. So
//! by-name selection has **no** range limit, **no** facing cone, **no** dead/reaction/attackability
//! test, **no** scene-attach visibility gate, and **no** self-exclusion. It is a far wider net than
//! TAB ([`super::scan`]), which shares none of those defaults. `/target <your own name>` self-targets
//! in the real client, and so does it here.
//!
//! ## The comparison — two tiers, both case-insensitive
//!
//! 1. **Whole-string** (`0x64a4c0` → CRT `0x414310`, which folds both operands in its own bytes).
//! 2. **Longest common PREFIX** (`0x493cb6`-`0x493cf6`), anchored at index 0 — a prefix, never a
//!    substring. Live whenever the second Lua argument is absent, which is every shipped call site,
//!    so `/target Rag` really does select "Ragnaros". The running best length starts at **1**, so one
//!    folded character of overlap is enough to enter the ranking — and a query sharing no first
//!    letter with anything matches nothing.
//!
//! Within one prefix-length class the tie-break is the **strictly nearest** 3D centre-to-centre
//! dist² (the reference's `fcomp`/`jp` edge is a strict `<`, so ties keep the incumbent).
//!
//! ## The one deliberate deviation — and why
//!
//! In the reference a tier-1 whole-string hit **returns 0 from the walk callback, terminating the
//! enumeration** (`0x493ca2`, stopped at `0x4683b7`). Two consequences fall out, both
//! enumeration-order dependent: among several identically-named creatures the client takes
//! *whichever the ClntObjMgr table walk reaches first* — never the nearest — and a near "Bobby"
//! recorded before a far "Bob" beats it outright for the query `Bob`.
//!
//! **We do not reproduce the early-out**, and since 0886 was written the order it depends on has
//! been derived, so the choice is now made against a known reference rather than an unknown one.
//! Table #1 is an intrusive doubly-linked list (link field at `obj+0x38`, written by the ctor at
//! `0x4650c2`) whose insert is a **tail append** (`0x4646c3`-`0x4646d4`), so the walk runs
//! **oldest-linked first** — and "linked", not "spawned": a unit that streams out and back in is
//! re-appended to the tail. The reference's tier-1 pick is therefore *the same-named candidate that
//! has been continuously known longest*, which no client-side heuristic reproduces without modelling
//! the list itself.
//!
//! [`Rank::beats`] is instead a **total order** — exact beats any prefix, then longer prefix, then
//! strictly nearest — which is deterministic and picks the nearest of several same-named kobolds.
//! wow-re's note calls the reference behaviour here "the single behaviour most likely to look like a
//! bug in a reimplementation". Reproducing it is now *possible* (stamp each net entity with a
//! monotonic link sequence, re-stamped on stream-in, and order the walk by it); it is not done
//! because nearest is the better answer, not because the order is unknown. This is the one place to
//! change if that judgement is ever reversed.
//!
//! ## Not carried
//!
//! The reference pre-scans the **party** (`0x493b20`) and **raid** (`0x493b9d`) rosters before the
//! object walk, matching group members against the guid-keyed name cache at a sentinel distance of
//! 2.5e9 — so a groupmate with no streamed object is still selectable. benilla cannot hold a
//! guid-only selection at all yet ([`Selection::target`] is an `Entity`; the same gap already
//! no-ops `TargetUnit("partyN")` on an out-of-range member, see [`super::click`]), so the pre-scans
//! wait on that slice rather than being half-built here.

use std::cmp::Ordering;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use benilla_protocol::guid;

use crate::names::NameCache;
use crate::net::{Guid, GuidIndex, NetCommands, ObjectStore, SelfPlayer};

use super::{scan, Selection};

/// Which guid families a search accepts — the reference's `edx` typemask argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameSearch {
    /// Typemask 8: creatures **and** players (a player object carries the UNIT bit). `/target`.
    AnyUnit,
    /// Typemask 0x10: players only. `/assist` — so `/assist <creature name>` cannot resolve, while
    /// assisting a *targeted* creature can (the no-argument form never goes through the resolver).
    PlayerOnly,
}

impl NameSearch {
    fn accepts(self, guid_val: u64) -> bool {
        match self {
            Self::AnyUnit => guid::is_player(guid_val) || guid::is_creature_or_pet(guid_val),
            Self::PlayerOnly => guid::is_player(guid_val),
        }
    }
}

/// `/target <name>` — resolve and select (`TargetByName`).
#[derive(bevy::ecs::message::Message, Clone, Debug)]
pub(crate) struct TargetByNameRequest {
    pub(crate) name: String,
}

/// `/assist [name]` — select the basis unit's own target (`AssistByName` / `AssistUnit("target")`).
/// `None` is the bare form, whose basis is the current selection.
#[derive(bevy::ecs::message::Message, Clone, Debug)]
pub(crate) struct AssistRequest {
    pub(crate) name: Option<String>,
}

/// How well one candidate answered the query. Ordered by [`Rank::beats`].
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rank {
    /// A tier-1 whole-string (case-insensitive) match.
    exact: bool,
    /// Folded common-prefix length, capped by both strings — the reference's
    /// `matchLen = searchCursor - ctx.name`.
    prefix: usize,
    /// 3D centre-to-centre squared distance from the active player.
    dist2: f32,
}

impl Rank {
    /// Strictly better than the incumbent? Exact beats any prefix; then the longer prefix; then the
    /// strictly nearer. Ties keep the incumbent, and a NaN distance never displaces — both edges the
    /// reference's `fcomp` + `test ah,5` + `jp` reject makes.
    fn beats(self, other: Rank) -> bool {
        match (self.exact, other.exact) {
            (true, false) => true,
            (false, true) => false,
            _ => match self.prefix.cmp(&other.prefix) {
                Ordering::Greater => true,
                Ordering::Less => false,
                Ordering::Equal => self.dist2 < other.dist2,
            },
        }
    }
}

/// The folded common-prefix length of `query` and `name` — the reference's lockstep walk
/// (`0x493cc0`-`0x493cee`) through the ASCII case fold `0x41089b`, stopping at whichever string ends
/// first or at the first folded mismatch.
fn common_prefix_len(query: &str, name: &str) -> usize {
    query
        .bytes()
        .zip(name.bytes())
        .take_while(|(q, n)| q.eq_ignore_ascii_case(n))
        .count()
}

/// Score one candidate against the query, or `None` when it does not match at all. The reference's
/// running best length seeds at **1**, so a zero-length overlap is not a match.
fn rank(query: &str, name: &str, dist2: f32) -> Option<Rank> {
    if query.eq_ignore_ascii_case(name) {
        return Some(Rank {
            exact: true,
            prefix: query.len(),
            dist2,
        });
    }
    let prefix = common_prefix_len(query, name);
    (prefix >= 1).then_some(Rank {
        exact: false,
        prefix,
        dist2,
    })
}

/// Everything a by-name search reads. Shared by the `/target` and `/assist` drains.
#[derive(SystemParam)]
#[allow(clippy::type_complexity)] // one bundled system param — the app's convention for big query sets
pub(crate) struct ByNameScan<'w, 's> {
    /// Every known unit INCLUDING our own avatar — the reference has no self-exclusion on this path.
    units: Query<
        'w,
        's,
        (
            Entity,
            &'static Guid,
            &'static Transform,
            Option<&'static ObjectStore>,
        ),
    >,
    self_q: Query<
        'w,
        's,
        (
            Entity,
            &'static Guid,
            &'static Transform,
            Option<&'static ObjectStore>,
        ),
        With<SelfPlayer>,
    >,
    /// Read-only: a by-name sweep must never fire a name query per candidate. A unit whose name has
    /// not been cached yet is simply not matchable — the same limit the reference has, whose own
    /// name getter reads an already-resolved string.
    names: Res<'w, NameCache>,
    /// Only [`Filter::AssistableAlive`] reads these — the reaction ladder behind `CanAssist`.
    factions: Option<Res<'w, super::Factions>>,
    reputations: Res<'w, crate::net::Reputations>,
}

/// The per-candidate filter the reference hands `0x493e40` as its mode argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    /// **Mode 0** — and the shape is the point: `0-1` underflows the `cmp eax,3; ja` bound, so the
    /// jump table is skipped entirely for `mov eax,1; ret 4`. Accept unconditionally. `/target`,
    /// `/assist`.
    AcceptAll,
    /// **Mode 2** (`0x493eca`) — `CanAssist 0x6066f0` **and** `UNIT_FIELD_HEALTH > 0`. `/follow`
    /// resolves only to living, assistable candidates, so `/follow <an enemy player>` finds nothing.
    AssistableAlive,
}

impl ByNameScan<'_, '_> {
    /// The active player's position — the distance origin (the reference returns `(0,0)` outright
    /// when the active player object does not resolve, `0x493ae0`).
    fn origin(&self) -> Option<Vec3> {
        self.self_q
            .single()
            .ok()
            .map(|(_, _, tf, _)| tf.translation)
    }

    /// The reference's mode-2 arm (`0x493eca`): `CanAssist` — the reaction ladder at **≥ 4**
    /// (friendly; wow-re `w2d1` `CanAssist_Unit 0x6066f0`), the mirror of [`scan::can_attack`]'s
    /// `≤ 3` — **and** alive.
    fn assistable_alive(&self, store: Option<&ObjectStore>) -> bool {
        if store.is_some_and(|s| s.0.unit_is_dead()) {
            return false;
        }
        let self_store = self.self_q.single().ok().and_then(|(_, _, _, s)| s);
        super::ring_reaction(
            self.factions.as_deref(),
            &self.reputations,
            store,
            self_store,
        ) >= 4
    }

    /// Resolve a name to a unit, or `None`. See the module header for the full law.
    ///
    /// Every call logs its verdict under the `by-name:` prefix — one line per typed command, so a
    /// "`/target` won't take that mob" report is diagnosable from the log rather than re-guessed
    /// (the `WOW_TAB_TRACE` instrument's shape, ungated because the cost is per-command, not
    /// per-frame). The line names how many candidates were eligible, which won, and on what.
    fn resolve(&self, query: &str, search: NameSearch, filter: Filter) -> Option<(Entity, u64)> {
        let query = query.trim();
        if query.is_empty() {
            return None;
        }
        let Some(origin) = self.origin() else {
            info!("by-name: \"{query}\" — no active player object; nothing resolves");
            return None;
        };
        let mut considered = 0usize;
        let mut nameless = 0usize;
        let mut best: Option<(Entity, u64, Rank, String)> = None;
        for (entity, guid, tf, store) in &self.units {
            if !search.accepts(guid.0) {
                continue;
            }
            if filter == Filter::AssistableAlive && !self.assistable_alive(store) {
                continue;
            }
            let Some(name) = self.names.peek(guid.0) else {
                nameless += 1;
                continue;
            };
            considered += 1;
            let dist2 = tf.translation.distance_squared(origin);
            let Some(r) = rank(query, name, dist2) else {
                continue;
            };
            if best.as_ref().is_none_or(|(_, _, b, _)| r.beats(*b)) {
                best = Some((entity, guid.0, r, name.to_string()));
            }
        }
        match &best {
            Some((_, guid, r, name)) => info!(
                "by-name: \"{query}\" ({search:?}) -> \"{name}\" guid {guid:#x} \
                 ({}, prefix {}, {:.1} yd) over {considered} named candidates ({nameless} unnamed)",
                if r.exact { "exact" } else { "prefix" },
                r.prefix,
                r.dist2.sqrt(),
            ),
            None => info!(
                "by-name: \"{query}\" ({search:?}) -> NO MATCH over {considered} named candidates \
                 ({nameless} unnamed); target left untouched"
            ),
        }
        best.map(|(e, g, _, _)| (e, g))
    }
}

/// Drain `/target <name>`: resolve, then commit through the shared SetSelection path so a by-name
/// switch obeys the same stop → select → re-swing law a click does ([`scan::commit`]).
///
/// A miss leaves the current target **untouched** — verified: neither failure edge in `0x489db4`
/// calls `SetSelection`, so `/target nosuchname` never clears what you had. The reference does emit
/// a game message there (ids `0x127` name-not-found / `0xb8` empty name), but the id→string table is
/// runtime-populated BSS and wow-re could not statically recover the text, so we say nothing rather
/// than invent a line. That silence is the known deviation on this path.
pub(super) fn target_by_name_requests(
    mut requests: MessageReader<TargetByNameRequest>,
    scan_params: ByNameScan,
    mut commit: SelectCommit,
) {
    for request in requests.read() {
        let Some((entity, guid)) =
            scan_params.resolve(&request.name, NameSearch::AnyUnit, Filter::AcceptAll)
        else {
            continue;
        };
        commit.commit(entity, guid);
    }
}

/// Drain `/assist [name]`: find the **basis** unit, read its `UNIT_FIELD_TARGET` (+0x28 off the unit
/// block — wow-re proved the offset arithmetic against HEALTH and DYNAMIC_FLAGS on the same line),
/// and select whatever it is pointing at.
///
/// The bare form is `AssistUnit("target")` in the reference — assist whoever you have selected,
/// creature or player. The named form resolves **players only**. A basis with no target is a
/// completely silent no-op (verified: the shared tail bails before any send).
///
/// **The `assistAttack` leg is correctly absent.** With that CVar (`0xb4d8f8`) non-zero the
/// reference's tail also opens the swing on the newly-selected target (`0x5ecb70` →
/// `CMSG_ATTACKSWING`) — but its **registered default is `"0"`** (VERIFIED at the registration
/// bytes: `0x48fc50`, default-value slot `[ebp+0xc]` = `0x82e570` = `"0"`, int-parsed into
/// `[cvar+0x28]` by `0x63e127`). So stock `/assist` selects and does **not** swing, which is exactly
/// what this does. The leg becomes reachable only if benilla ever grows the CVar itself.
pub(super) fn assist_requests(
    mut requests: MessageReader<AssistRequest>,
    scan_params: ByNameScan,
    index: Res<GuidIndex>,
    mut commit: SelectCommit,
) {
    for request in requests.read() {
        // The basis: a named PLAYER, or — bare — whatever is currently selected.
        let basis = match &request.name {
            Some(name) => scan_params
                .resolve(name, NameSearch::PlayerOnly, Filter::AcceptAll)
                .map(|(e, _)| e),
            None => commit.selection.target,
        };
        let Some(basis) = basis else {
            info!("by-name: /assist — no basis unit; nothing to assist");
            continue;
        };
        // Its own target. A zero/absent guid is the silent no-op — the reference's shared tail
        // bails here before any send.
        let Some(guid) = commit
            .stores
            .get(basis)
            .ok()
            .and_then(|s| s.0.unit_target())
            .filter(|g| *g != 0)
        else {
            info!("by-name: /assist — the basis unit is targeting nothing; silent no-op");
            continue;
        };
        // "Select if it resolves" (`0x489a40`): only a streamed unit can be selected — the
        // reference's group-roster fallback for an unstreamed guid needs the guid-only selection
        // benilla does not have yet (see the module header).
        let Some(entity) = index.0.get(&guid).copied() else {
            info!(
                "by-name: /assist — the basis is targeting guid {guid:#x}, which is not streamed"
            );
            continue;
        };
        info!("by-name: /assist -> the basis unit's target, guid {guid:#x}");
        commit.commit(entity, guid);
    }
}

/// Drain `/follow [name]`: resolve the subject and hand it to [`crate::player`], which owns the
/// motion (decision 0890). Nothing goes on the wire — follow is client-side movement only.
///
/// The two forms differ in more than the argument. A **name** goes through the resolver with the
/// reference's filter mode 2 — players only, and only ones that are **alive and assistable** — so
/// `/follow <an enemy>` finds nothing. The **bare** form is `FollowUnit("target")`, a unit-token
/// resolve that never touches the by-name path at all, so it follows whatever you have selected,
/// creature or player, with no such filter.
pub(super) fn follow_requests(
    mut requests: MessageReader<crate::player::FollowRequest>,
    scan_params: ByNameScan,
    selection: Res<Selection>,
    mut follow: ResMut<crate::player::FollowState>,
) {
    for request in requests.read() {
        let resolved = match &request.name {
            Some(name) => scan_params
                .resolve(name, NameSearch::PlayerOnly, Filter::AssistableAlive)
                .map(|(_, guid)| guid),
            None => selection.guid,
        };
        match resolved {
            Some(guid) => {
                info!("follow: now following guid {guid:#x}");
                follow.start(guid);
            }
            None => {
                // The reference's `if GetSlashCmdTarget(msg)` guard and the resolver's own miss both
                // land here: nothing to follow, and whatever we were following is left alone.
                info!("follow: nothing to follow");
            }
        }
    }
}

/// The shared commit tail — everything [`scan::commit`] needs, bundled, so both drains route a
/// resolved unit through the one SetSelection path (dedup + the stop → select → re-swing law).
#[derive(SystemParam)]
#[allow(clippy::type_complexity)] // one bundled system param — the app's convention for big query sets
pub(crate) struct SelectCommit<'w, 's> {
    selection: ResMut<'w, Selection>,
    net: Res<'w, NetCommands>,
    me: Query<
        'w,
        's,
        (
            &'static Guid,
            Option<&'static ObjectStore>,
            Has<crate::creature_anim::Engaged>,
        ),
        With<SelfPlayer>,
    >,
    stores: Query<'w, 's, &'static ObjectStore>,
    factions: Option<Res<'w, super::Factions>>,
    reputations: Res<'w, crate::net::Reputations>,
}

impl SelectCommit<'_, '_> {
    fn commit(&mut self, entity: Entity, guid: u64) {
        let me = self.me.single().ok();
        // The new-target classification `scan::commit` expects from its callers (it does not
        // re-derive one) — the same `can_attack` the cursor and the TAB scan pass.
        let attackable = scan::can_attack(
            self.stores.get(entity).ok(),
            self.factions.as_deref(),
            &self.reputations,
            me.and_then(|(_, store, _)| store),
        );
        scan::commit(
            &mut self.selection,
            &self.net,
            entity,
            guid,
            me.is_some_and(|(_, _, engaged)| engaged),
            me.map(|(g, _, _)| g.0),
            attackable,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact(dist2: f32) -> Rank {
        Rank {
            exact: true,
            prefix: 3,
            dist2,
        }
    }
    fn prefix(prefix: usize, dist2: f32) -> Rank {
        Rank {
            exact: false,
            prefix,
            dist2,
        }
    }

    #[test]
    fn prefix_length_folds_case_and_stops_at_the_shorter_string() {
        assert_eq!(common_prefix_len("rag", "Ragnaros"), 3);
        assert_eq!(common_prefix_len("RAG", "ragnaros"), 3);
        assert_eq!(
            common_prefix_len("Ragnarosx", "Ragnaros"),
            8,
            "capped by the candidate"
        );
        assert_eq!(
            common_prefix_len("zzz", "Ragnaros"),
            0,
            "no shared first letter"
        );
    }

    #[test]
    fn a_match_needs_one_folded_character() {
        // The reference seeds its best length at 1, so zero overlap is not a match at all.
        assert!(rank("zzz", "Ragnaros", 1.0).is_none());
        assert_eq!(rank("rag", "Ragnaros", 1.0).map(|r| r.prefix), Some(3));
    }

    #[test]
    fn whole_string_match_is_case_insensitive_and_ranks_exact() {
        let r = rank("kobold vermin", "Kobold Vermin", 9.0).expect("matches");
        assert!(r.exact, "tier 1 is case-insensitive whole-string");
        // A multi-word creature name is the case that made the whole-argument trim load-bearing.
    }

    #[test]
    fn exact_beats_any_prefix_and_nearest_breaks_the_tie() {
        // Our deliberate deviation: the reference's enumeration-order early-out would let a nearer
        // "Bobby" beat a farther "Bob"; we rank exact first, unconditionally.
        assert!(exact(100.0).beats(prefix(3, 1.0)));
        assert!(!prefix(3, 1.0).beats(exact(100.0)));
        // Among exact matches — several identically-named kobolds — the nearest wins.
        assert!(exact(4.0).beats(exact(9.0)));
        assert!(!exact(9.0).beats(exact(4.0)));
    }

    #[test]
    fn longer_prefix_beats_nearer_shorter_one() {
        // The reference's `jg -> accept` edge: a longer prefix wins outright, distance irrelevant.
        assert!(prefix(5, 900.0).beats(prefix(3, 1.0)));
        assert!(!prefix(3, 1.0).beats(prefix(5, 900.0)));
        // Equal prefix length falls through to strictly-nearest.
        assert!(prefix(3, 4.0).beats(prefix(3, 9.0)));
    }

    #[test]
    fn ties_keep_the_incumbent_and_nan_never_displaces() {
        // The reference's `jp` reject edge makes the accept a strict `<`.
        assert!(!prefix(3, 4.0).beats(prefix(3, 4.0)));
        assert!(!exact(4.0).beats(exact(4.0)));
        assert!(!prefix(3, f32::NAN).beats(prefix(3, 4.0)));
    }
}
