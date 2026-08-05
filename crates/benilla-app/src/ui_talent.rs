//! The talent window feed (decision 0304) — the app half of the `benilla_ui::script::talent`
//! seam, the `ui_spellbook` shape: build the class's talent pages from the `Talent.dbc` ×
//! `TalentTab.dbc` catalog joined with the known-spell set (`PlayerActions.spells`) and the
//! self descriptor's `PLAYER_CHARACTER_POINTS1/2`, push the snapshot, fire the refresh events,
//! and drain `LearnTalent` clicks into `CMSG_LEARN_TALENT`.
//!
//! The resolves the engine trusts verbatim:
//! - **rank** = the highest rank whose spell is known (learn-up-to grants every lower rank —
//!   vmangos `Player::LearnTalent`).
//! - **name/icon** ride the display rank's spell (`Spell.dbc`), max(rank, 1) — an unlearned
//!   talent wears rank 1's face, the reference's own look.
//! - **`learnable`** (the tooltip's green "Click to learn" hint) = unspent points > 0 ∧
//!   `rank < maxRank` — the hint's own byte-verified gate (SetTalent `0x535170`, wow-re
//!   `talent-api.md`); the frame's gold/green/gray availability compose Lua-side from the
//!   transcribed reference (tier gate, prereq triplets, meetsPrereq). The learn SEND gates on
//!   not-at-max only (LearnTalent `0x4f36a0`) — the server enforces the rest.
//! - **tooltip req lines** (red, shown while locked): `TOOLTIP_TALENT_TIER_POINTS` ("Requires
//!   %d points in %s Talents") + `TOOLTIP_TALENT_PREREQ[_P1]` ("Requires %d point(s) in %s",
//!   the prereq talent's name) — GlobalStrings:4260-4263. A `required_spell` gap has no
//!   GlobalString and renders no line (the desaturation still communicates it).
//! - The tooltip's spell parts (display + next rank) ride `ui_tooltip`'s spell channel, which
//!   pre-feeds every rank spell of the class's pages at arrival — a `SetTalent` hover hits on
//!   the FIRST enter (the reference's all-local instancy); its ask-once miss path stays as the
//!   odd-case fallback.
//!
//! The free-professions chat line (`CHARACTER_POINTS_CHANGED` → "You now have %d free
//! profession(s)." — LEVEL_UP_SKILL_POINTS[_P1], the reference ChatFrame.lua:1326-1337) is
//! composed here on a cp2 rise: this feed owns the points diff, and the chat arc's own kinds
//! carry it as a SYSTEM line.

use std::collections::HashSet;

use bevy::prelude::*;

use benilla_formats::{Talent, TalentCatalog};
use benilla_ui::script::{TalentPrereqView, TalentTabView, TalentUiState, TalentView, UiScript};

use crate::assets::{AssetSet, LockRecover, WorldAssets};
use crate::net::{ClientCommand, NetCommands, ObjectStore, SelfPlayer};
use crate::ui_action::{PlayerActions, Spells};
use crate::ui_chat::{ChatEvent, ChatEventKind, ChatLog};
use crate::ui_script::UiInput;
use crate::ui_unit::UnitFeed;

/// The talent catalog (`Talent.dbc` × `TalentTab.dbc`) — absent when the client data failed to
/// load (every consumer tolerates that; the `SkillLines` precedent).
#[derive(Resource)]
pub(crate) struct Talents {
    pub(crate) catalog: TalentCatalog,
}

pub(crate) struct UiTalentPlugin;

impl Plugin for UiTalentPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_talents.after(AssetSet::Open))
            .add_systems(
                Update,
                (
                    // Feed before UiInput (an N-key open this frame sees a populated window);
                    // the learn drain after (a click's wire goes out the same frame).
                    feed_talents.in_set(UnitFeed).before(UiInput),
                    drain_talent_learns.after(UiInput),
                ),
            );
    }
}

fn load_talents(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_talent_catalog(&mut chain)
    };
    match loaded {
        Ok(catalog) => {
            info!("ui_talent: {} talent(s) in the catalog", catalog.len());
            commands.insert_resource(Talents { catalog });
        }
        Err(e) => warn!("ui_talent: Talent.dbc failed to load — no talent window: {e:#}"),
    }
}

/// The feed's memory: the last pushed snapshot (the refresh diff) and the last points pair
/// (the CHARACTER_POINTS_CHANGED edge + the professions chat line).
#[derive(Default)]
struct FeedMemory {
    pushed: TalentUiState,
    /// `None` until the first descriptor read lands (a login's initial value must not chat).
    points: Option<(u32, u32)>,
}

fn feed_talents(
    script: Option<NonSendMut<UiScript>>,
    talents: Option<Res<Talents>>,
    actions: Res<PlayerActions>,
    spells: Option<Res<Spells>>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    mut chat: ResMut<ChatLog>,
    mut memory: Local<FeedMemory>,
) {
    let Some(mut script) = script else {
        return;
    };
    let (Some(talents), Some(spells)) = (talents.as_deref(), spells.as_deref()) else {
        return;
    };
    let Ok(store) = self_q.single() else {
        return;
    };
    let race = store.0.unit_race().unwrap_or(0);
    let class = store.0.unit_class().unwrap_or(0);
    let points = (
        store.0.player_talent_points().unwrap_or(0),
        store.0.player_free_professions().unwrap_or(0),
    );
    let fresh = build_pages(
        &talents.catalog,
        &actions.spells,
        spells,
        race,
        class,
        points,
    );
    if fresh != memory.pushed {
        debug!(
            "ui_talent: fed {} tab(s), {} points",
            fresh.tabs.len(),
            points.0
        );
        script.set_talents(fresh.clone());
        memory.pushed = fresh;
        script.fire_event("CHARACTER_POINTS_CHANGED", vec![]);
    }
    // The professions line rides the cp2 RISE only (a spend consumes, a rise frees — the
    // reference prints on arg2 > 0); the first observation seeds silently.
    if let Some((_, old_cp2)) = memory.points {
        if points.1 > old_cp2 {
            let text = if points.1 == 1 {
                "You now have 1 free profession.".to_string()
            } else {
                format!("You now have {} free professions.", points.1)
            };
            chat.push_event(ChatEvent::text_only(ChatEventKind::System, text));
        }
    }
    if memory.points != Some(points) {
        memory.points = Some(points);
    }
}

/// One talent's current rank: the highest rank whose spell is known (module doc).
fn rank_of(t: &Talent, known: &HashSet<u32>) -> u32 {
    t.ranks
        .iter()
        .enumerate()
        .filter(|(_, &s)| s != 0 && known.contains(&s))
        .map(|(i, _)| i as u32 + 1)
        .max()
        .unwrap_or(0)
}

/// Build the pushed snapshot — the app's whole resolve (module doc).
fn build_pages(
    catalog: &TalentCatalog,
    known: &HashSet<u32>,
    spells: &Spells,
    race: u8,
    class: u8,
    points: (u32, u32),
) -> TalentUiState {
    let mut tabs = Vec::new();
    let mut pages = Vec::new();
    for tab in catalog.tabs_for_class(race, class) {
        let list = catalog.talents_in_tab(tab.id);
        let spent: u32 = list.iter().map(|t| rank_of(t, known)).sum();
        let mut views = Vec::with_capacity(list.len());
        for t in list {
            let rank = rank_of(t, known);
            let max_rank = t.max_rank();
            let display_spell = t.ranks[rank.max(1) as usize - 1];
            let next_spell = if rank > 0 && rank < max_rank {
                t.ranks[rank as usize]
            } else {
                0
            };
            let d = spells.catalog.get(display_spell);
            // meetsPrereq is the requiredSpell known-check ONLY (byte-verified GetTalentInfo,
            // wow-re talent-api.md — the 0305 fold-back; talent prereqs live in the triplets).
            let meets_prereq = t.required_spell == 0 || known.contains(&t.required_spell);
            // The tier gate reads the tab's own spent sum; prereqs read the prereq's rank.
            let tier_unlocked = t.row * 5 <= spent;
            let mut req_lines = Vec::new();
            if !tier_unlocked {
                req_lines.push(format!(
                    "Requires {} points in {} Talents",
                    t.row * 5,
                    tab.name
                ));
            }
            // The requiredSpell line rides ITEM_REQ_SKILL "Requires %s" (byte-verified in the
            // SetTalent builder, wow-re talent-api.md).
            if !meets_prereq {
                if let Some(req) = spells.catalog.get(t.required_spell) {
                    req_lines.push(format!("Requires {}", req.name));
                }
            }
            let mut prereqs = Vec::new();
            if t.prereq_talent != 0 {
                if let Some(p) = catalog.talent(t.tab, t.prereq_talent) {
                    let need = t.prereq_rank + 1;
                    let learnable = rank_of(p, known) >= need;
                    prereqs.push(TalentPrereqView {
                        tier: p.row + 1,
                        column: p.col + 1,
                        learnable,
                    });
                    if !learnable {
                        let p_name = spells
                            .catalog
                            .get(p.ranks[0])
                            .map(|d| d.name.clone())
                            .unwrap_or_default();
                        req_lines.push(if need == 1 {
                            format!("Requires 1 point in {p_name}")
                        } else {
                            format!("Requires {need} points in {p_name}")
                        });
                    }
                }
            }
            // The green learn hint's own gate is points-available && not-maxed ONLY
            // (byte-verified SetTalent, wow-re talent-api.md) — the frame's gold/green/gray
            // availability law stays the transcribed Lua's (tier/prereq/meetsPrereq).
            let learnable = rank < max_rank && points.0 > 0;
            views.push(TalentView {
                name: d.map(|d| d.name.clone()).unwrap_or_default(),
                texture: d.and_then(|d| d.icon.clone()),
                tier: t.row + 1,
                column: t.col + 1,
                rank,
                max_rank,
                exceptional: t.exceptional,
                meets_prereq,
                prereqs,
                display_spell,
                next_spell,
                req_lines,
                learnable,
            });
        }
        tabs.push(TalentTabView {
            name: tab.name.clone(),
            background: tab.background.clone(),
            points_spent: spent,
        });
        pages.push(views);
    }
    TalentUiState {
        tabs,
        talents: pages,
        points,
    }
}

/// Drain the window's `LearnTalent(tab, index)` clicks into `CMSG_LEARN_TALENT` — requested
/// rank = the current rank count (the 0-based next rank, vmangos's learn-up-to semantics). The
/// gate mirrors the pushed `learnable` (the server re-validates regardless).
fn drain_talent_learns(
    script: Option<NonSendMut<UiScript>>,
    talents: Option<Res<Talents>>,
    actions: Res<PlayerActions>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    commands: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    let clicks = script.take_talent_learns();
    if clicks.is_empty() {
        return;
    }
    let Some(talents) = talents.as_deref() else {
        return;
    };
    let Ok(store) = self_q.single() else {
        return;
    };
    let race = store.0.unit_race().unwrap_or(0);
    let class = store.0.unit_class().unwrap_or(0);
    let tabs = talents.catalog.tabs_for_class(race, class);
    for (tab, index) in clicks {
        // 1-based Lua-facing pair; a 0 from a stray script is a miss, never an underflow.
        let Some(t) = (tab as usize)
            .checked_sub(1)
            .and_then(|i| tabs.get(i))
            .and_then(|tab| {
                (index as usize)
                    .checked_sub(1)
                    .and_then(|i| talents.catalog.talents_in_tab(tab.id).get(i))
            })
        else {
            continue;
        };
        let rank = rank_of(t, &actions.spells);
        // The client's ONLY send gate is not-at-max (byte-verified LearnTalent 0x4f36a0 — the
        // 0305 fold-back); points/prereqs are the server's to enforce.
        if rank >= t.max_rank() {
            continue;
        }
        debug!("ui_talent: learn talent {} rank {}", t.id, rank);
        let _ = commands.0.send(ClientCommand::LearnTalent {
            talent_id: t.id,
            rank,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_formats::MAX_TALENT_RANK;

    fn talent(id: u32, tab: u32, row: u32, col: u32, ranks: &[u32]) -> Talent {
        let mut r = [0u32; MAX_TALENT_RANK];
        r[..ranks.len()].copy_from_slice(ranks);
        Talent {
            id,
            tab,
            row,
            col,
            ranks: r,
            prereq_talent: 0,
            prereq_rank: 0,
            required_spell: 0,
            exceptional: false,
        }
    }

    #[test]
    fn rank_is_the_highest_known_rank_spell() {
        let t = talent(1, 81, 0, 0, &[100, 101, 102]);
        let known: HashSet<u32> = [100, 101].into_iter().collect();
        assert_eq!(rank_of(&t, &known), 2);
        assert_eq!(rank_of(&t, &HashSet::new()), 0);
        // A gap never happens on the live wire (learn-up-to), but the max() read stays honest.
        let holey: HashSet<u32> = [102].into_iter().collect();
        assert_eq!(rank_of(&t, &holey), 3);
    }
}
