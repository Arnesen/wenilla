//! The trainer tree's tests — the per-`trainerType` ordering laws (decisions 0247/1124) pinned
//! against wow-re's own emulated runs of the real finalizer, plus the filter/collapse/intent
//! surface. Split out of `mod.rs` when the four comparators pushed it past the file budget.

use super::*;
use crate::script::UiScript;

fn svc(
    spell_id: u32,
    name: &str,
    cat: TrainerServiceCategory,
    group_key: u32,
    group_name: &str,
) -> TrainerService {
    TrainerService {
        spell_id,
        // The fixture's default subject: the wire spell, no hop (the "no learn wrapper"
        // fallback); the tooltip tests set the arm they mean explicitly.
        tooltip: TrainerTooltip::Spell {
            spell_id,
            alt_caster: false,
        },
        name: Some(name.into()),
        subtext: Some("Rank 1".into()),
        texture: Some(format!("Interface\\Icons\\Spell_{spell_id}")),
        description: format!("Teaches {name}."),
        cost: 100,
        prof_first_rank: false,
        category: cat,
        level_req: 10,
        skill_req: None,
        ability_reqs: vec![],
        is_trade_skill: false,
        group_key,
        group_name: group_name.into(),
    }
}

/// A two-line warrior trainer (type 0). Groups sort by name (Arms < Fury); within Arms, the level
/// keys order Heroic Strike (level 1) before Cleave (level 20). The default-expanded tree is thus:
/// `[H:Arms, Heroic Strike(avail), Cleave(used), H:Fury, Bloodrage(unavail)]` — a 5-row list whose
/// service rows sit at indices 2, 3, 5.
fn trainer() -> TrainerState {
    let mut hs = svc(
        78,
        "Heroic Strike",
        TrainerServiceCategory::Available,
        26,
        "Arms",
    );
    hs.level_req = 1;
    let mut cl = svc(284, "Cleave", TrainerServiceCategory::Used, 26, "Arms");
    cl.level_req = 20;
    let br = svc(
        285,
        "Bloodrage",
        TrainerServiceCategory::Unavailable,
        256,
        "Fury",
    );
    TrainerState {
        greeting: "What would you like to learn?".into(),
        trainer_type: 0,
        groups: Vec::new(),
        services: vec![hs, cl, br],
    }
}

/// Every visible row as `(name, serviceType)` — the shape the ported XML actually renders.
fn visible(s: &mut UiScript) -> Vec<(String, String)> {
    let n = s.eval::<i64>("return GetNumTrainerServices()").unwrap();
    (1..=n)
        .map(|i| {
            s.eval::<(String, String)>(&format!(
                "local n,_,t = GetTrainerServiceInfo({i}) return n,t"
            ))
            .unwrap()
        })
        .collect()
}

#[test]
fn tree_interleaves_headers_and_ordered_services() {
    let mut s = UiScript::new().unwrap();
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 0);
    assert!(s
        .eval::<bool>("return GetTrainerServiceInfo(1) == nil")
        .unwrap());

    s.set_trainer(Some(trainer()));
    // 2 headers + 3 services = 5 visible rows.
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 5);

    // Row 1 is the "Arms" header: name, nil subtext, "header", isExpanded=1.
    let (hn, hsub, ht, hexp) = s
        .eval::<(String, Option<String>, String, Option<i64>)>(
            "local n,s,t,e = GetTrainerServiceInfo(1) return n,s,t,e",
        )
        .unwrap();
    assert_eq!((hn.as_str(), ht.as_str()), ("Arms", "header"));
    assert_eq!((hsub, hexp), (None, Some(1)));

    // Rows 2..5: services (name/state) then the Fury header then its service — the sorted tree.
    let info = |s: &mut UiScript, i: i64| {
        s.eval::<(String, String)>(&format!(
            "local n,_,t = GetTrainerServiceInfo({i}) return n,t"
        ))
        .unwrap()
    };
    assert_eq!(
        info(&mut s, 2),
        ("Heroic Strike".into(), "available".into())
    );
    assert_eq!(info(&mut s, 3), ("Cleave".into(), "used".into()));
    assert_eq!(info(&mut s, 4), ("Fury".into(), "header".into()));
    assert_eq!(info(&mut s, 5), ("Bloodrage".into(), "unavailable".into()));
}

#[test]
fn service_getters_read_the_row_at_a_visible_index() {
    let mut s = UiScript::new().unwrap();
    let mut t = trainer();
    // Heroic Strike (services[0], the row at index 2) carries a full gate set.
    t.services[0].skill_req = Some(TrainerSkillReq {
        name: "Blacksmithing".into(),
        rank: 100,
        met: true,
    });
    t.services[0].ability_reqs = vec![TrainerAbilityReq {
        name: "Apprentice".into(),
        met: false,
    }];
    s.set_trainer(Some(t));

    // Cost/level/desc/reqs all resolve at the SERVICE index 2 (not the header at 1).
    assert_eq!(
        s.eval::<(i64, i64, i64)>("return GetTrainerServiceCost(2)")
            .unwrap(),
        (100, 0, 0)
    );
    assert_eq!(
        s.eval::<i64>("return GetTrainerServiceLevelReq(2)")
            .unwrap(),
        1
    );
    assert_eq!(
        s.eval::<String>("return GetTrainerServiceDescription(2)")
            .unwrap(),
        "Teaches Heroic Strike."
    );
    let (skill, rank, has) = s
        .eval::<(String, i64, i64)>("return GetTrainerServiceSkillReq(2)")
        .unwrap();
    assert_eq!((skill.as_str(), rank, has), ("Blacksmithing", 100, 1));
    assert!(s
        .eval::<bool>(
            "local n,h = GetTrainerServiceAbilityReq(2,1) return n=='Apprentice' and h==nil"
        )
        .unwrap());
    assert!(s
        .eval::<bool>("local l,p = IsTrainerServiceLearnSpell(2) return l==1 and p==nil")
        .unwrap());

    // A HEADER row (index 1) has no service data — the getters no-op to defaults.
    assert_eq!(
        s.eval::<i64>("return GetTrainerServiceLevelReq(1)")
            .unwrap(),
        0
    );
    assert!(s
        .eval::<bool>("return GetTrainerServiceIcon(1) == nil")
        .unwrap());
    assert!(s
        .eval::<bool>("return GetTrainerServiceSkillReq(1) == nil")
        .unwrap());
}

/// The state filter takes a group's **header with it** once it hides the group's last service — the
/// finalizer's `[+0x1c]` hide has no header-row exemption (`0x4d8528`/`0x4d8535`, decision 1124).
/// This asserted the opposite until 1124: benilla rendered bare headers over empty groups, and an
/// all-boxes-off filter left a window full of headings and nothing else.
#[test]
fn state_filter_takes_a_groups_header_with_its_last_service() {
    let mut s = UiScript::new().unwrap();
    s.set_trainer(Some(trainer()));
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 5);

    // Hide "used": Cleave drops (5 → 4), and BOTH headers stay — Arms still has Heroic Strike.
    s.run("SetTrainerServiceTypeFilter('used', 0)").unwrap();
    assert_eq!(
        visible(&mut s)
            .into_iter()
            .map(|(_, t)| t)
            .collect::<Vec<_>>(),
        ["header", "available", "header", "unavailable"]
    );

    // Now hide "available" too: Arms loses its last row, so the ARMS HEADER GOES WITH IT — only
    // Fury's header and its unavailable service remain.
    s.run("SetTrainerServiceTypeFilter('available', 0)")
        .unwrap();
    assert_eq!(
        visible(&mut s),
        [
            ("Fury".to_string(), "header".to_string()),
            ("Bloodrage".to_string(), "unavailable".to_string()),
        ]
    );

    // Every box off renders NOTHING — not a list of bare headings.
    s.run("SetTrainerServiceTypeFilter('unavailable', 0)")
        .unwrap();
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 0);
    assert!(s
        .eval::<bool>("return GetTrainerServiceInfo(1) == nil")
        .unwrap());
}

/// Collapse is the *asymmetric* case, and deliberately so: it hides a group's services but keeps the
/// header — a different field (`hdr[+0x20]`) tested three instructions after the filter's, with a
/// header-row exemption the filter's lacks (`0x4d853d`). Without it a collapsed group could never be
/// re-expanded, having no header left to click.
#[test]
fn collapse_keeps_the_header_the_filter_would_remove() {
    let mut s = UiScript::new().unwrap();
    s.set_trainer(Some(trainer()));
    s.run("CollapseTrainerSkillLine(1)").unwrap(); // fold Arms — both its services hide
    assert_eq!(
        visible(&mut s),
        [
            ("Arms".to_string(), "header".to_string()),
            ("Fury".to_string(), "header".to_string()),
            ("Bloodrage".to_string(), "unavailable".to_string()),
        ]
    );
    // Collapse-all keeps every header; the filter's empty case keeps none.
    s.run("CollapseTrainerSkillLine(0)").unwrap();
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 2);
}

#[test]
fn collapse_by_header_index_and_collapse_all() {
    let mut s = UiScript::new().unwrap();
    s.set_trainer(Some(trainer()));

    // Collapse the Arms group by its header's display index (1): its two services vanish, the
    // header stays and now reports isExpanded=nil. 5 → 3 (H:Arms, H:Fury, Bloodrage).
    s.run("CollapseTrainerSkillLine(1)").unwrap();
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 3);
    assert!(s
        .eval::<bool>("local _,_,t,e = GetTrainerServiceInfo(1) return t=='header' and e==nil")
        .unwrap());
    assert_eq!(
        s.eval::<String>("local n = GetTrainerServiceInfo(2) return n")
            .unwrap(),
        "Fury",
        "Arms' services are folded; Fury's header is now row 2"
    );

    // Expand it back by the same header index.
    s.run("ExpandTrainerSkillLine(1)").unwrap();
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 5);

    // Collapse-all (id 0): both groups fold → just the two headers.
    s.run("CollapseTrainerSkillLine(0)").unwrap();
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 2);
    // Expand-all (id 0): back to the full tree.
    s.run("ExpandTrainerSkillLine(0)").unwrap();
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 5);
}

#[test]
fn collapse_survives_a_content_update_and_resets_on_close() {
    let mut s = UiScript::new().unwrap();
    s.set_trainer(Some(trainer()));
    s.run("CollapseTrainerSkillLine(1)").unwrap(); // fold Arms
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 3);

    // A re-list (same skill lines) keeps the fold — a buy re-lists and the tree shouldn't jump open.
    s.set_trainer(Some(trainer()));
    assert_eq!(
        s.eval::<i64>("return GetNumTrainerServices()").unwrap(),
        3,
        "Arms stays collapsed across a content update"
    );

    // Close → the collapse set resets; a re-open is fully expanded.
    s.set_trainer(None);
    s.set_trainer(Some(trainer()));
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 5);
}

#[test]
fn buy_queues_the_selected_services_spell_id_headers_no_op() {
    let mut s = UiScript::new().unwrap();
    s.set_trainer(Some(trainer()));
    // Row 5 is Bloodrage (spell 285). Buying it queues its spell id, not its index.
    s.run("BuyTrainerService(5)").unwrap();
    assert_eq!(s.take_trainer_buys(), vec![285]);
    assert!(s.take_trainer_buys().is_empty(), "drained");

    // Buying a HEADER row (index 1) queues nothing.
    s.run("BuyTrainerService(1)").unwrap();
    assert!(s.take_trainer_buys().is_empty(), "a header is not buyable");
}

#[test]
fn selection_and_close_intents() {
    let mut s = UiScript::new().unwrap();
    s.set_trainer(Some(trainer()));
    assert_eq!(
        s.eval::<i64>("return GetTrainerSelectionIndex()").unwrap(),
        0
    );
    s.run("SelectTrainerService(2)").unwrap();
    assert_eq!(
        s.eval::<i64>("return GetTrainerSelectionIndex()").unwrap(),
        2
    );
    s.run("SelectTrainerService(9)").unwrap(); // OOB (past the 5 rows) clears
    assert_eq!(
        s.eval::<i64>("return GetTrainerSelectionIndex()").unwrap(),
        0
    );

    assert!(!s.take_trainer_close());
    s.run("CloseTrainer()").unwrap();
    assert!(s.take_trainer_close());
    assert!(!s.take_trainer_close(), "drained");
}

/// The two whole-trainer predicates fork on the wire `trainer_type` — `IsTradeskillTrainer` on
/// `== 2` (`0x4d8ea0`), `IsTalentTrainer` on `== 1` (`0x4d8ed0`) — so at most one is ever true, and
/// at a class trainer neither is.
#[test]
fn tradeskill_and_talent_flags() {
    let mut s = UiScript::new().unwrap();
    assert!(s
        .eval::<bool>("return IsTradeskillTrainer() == nil")
        .unwrap());

    // A class trainer: neither predicate.
    s.set_trainer(Some(trainer()));
    assert!(s
        .eval::<bool>("return IsTradeskillTrainer() == nil and IsTalentTrainer() == nil")
        .unwrap());

    // A tradeskill trainer — one "Recipes" group, so its one service is the row at index 2.
    let mut recipe = svc(
        2743,
        "Copper Chain Pants",
        TrainerServiceCategory::Unavailable,
        2,
        "Recipes",
    );
    recipe.is_trade_skill = true;
    s.set_trainer(Some(TrainerState {
        greeting: String::new(),
        trainer_type: 2,
        groups: Vec::new(),
        services: vec![recipe],
    }));
    assert!(s
        .eval::<bool>("return IsTradeskillTrainer() == 1 and IsTalentTrainer() == nil")
        .unwrap());
    assert!(s
        .eval::<bool>("return IsTrainerServiceTradeSkill(2) == 1")
        .unwrap());

    // A mount trainer — the "talent" predicate, which used to be a hardcoded nil.
    s.set_trainer(Some(TrainerState {
        greeting: String::new(),
        trainer_type: 1,
        groups: Vec::new(),
        services: vec![svc(
            33,
            "Riding",
            TrainerServiceCategory::Available,
            762,
            "Riding",
        )],
    }));
    assert!(s
        .eval::<bool>("return IsTalentTrainer() == 1 and IsTradeskillTrainer() == nil")
        .unwrap());
}

#[test]
fn unresolved_skill_line_is_dropped() {
    let mut s = UiScript::new().unwrap();
    let mut t = trainer();
    // A service whose skill line didn't resolve (0) is dropped from the tree entirely.
    t.services.push(svc(
        999,
        "Orphan Spell",
        TrainerServiceCategory::Available,
        0,
        "",
    ));
    s.set_trainer(Some(t));
    // Still 5 rows — the orphan contributes neither a header nor a service row.
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 5);
}

#[test]
fn clearing_empties_and_resets_selection() {
    let mut s = UiScript::new().unwrap();
    s.set_trainer(Some(trainer()));
    s.run("SelectTrainerService(2)").unwrap();
    s.set_trainer(None);
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 0);
    assert_eq!(
        s.eval::<i64>("return GetTrainerSelectionIndex()").unwrap(),
        0
    );
}

/// **The tradeskill order, pinned against wow-re's emulated run of the real finalizer** (decision
/// 1124). The fixture is creature 957 "Dane Lindgren" — the blacksmithing trainer in the director's
/// report — with his 19 real `npc_trainer` rows: 18 recipes on skill line 164 with ascending
/// `reqSkillValue`, plus the profession-learn service 2020 ("Apprentice Blacksmith", `reqLevel 5`,
/// no skill gate). wow-re fed exactly this set through the builder `0x4d7560` + finalizer `0x4d8410`
/// with `0xb73a08 = 2` and the real `qsort`/collator, and got the 21 rows asserted below.
///
/// Two things this pins that benilla had wrong, and one it never had:
/// - the learn row is **first among services**, under its own `Development Skills` header — placed
///   by the `SKILL_STEP` group partition, *not* by any level key;
/// - `reqLevel` is **inert** at type 2 (the class comparator's first key, which used to sink 2020 to
///   the bottom of the list);
/// - recipes ascend by `reqSkillValue`, ties broken by name.
#[test]
fn tradeskill_trainer_matches_the_emulated_reference_order() {
    // (wire spell, name, reqSkillValue) — the recipes, in wire order (ascending here, but the
    // fixture is shuffled below to prove the order is the comparator's and not the input's).
    let recipes: &[(u32, &str, u32)] = &[
        (2743, "Copper Chain Pants", 1),
        (2754, "Copper Mace", 15),
        (2755, "Copper Axe", 20),
        (3340, "Copper Chain Boots", 20),
        (2756, "Copper Shortsword", 25),
        (3341, "Rough Grinding Stone", 25),
        (9984, "Copper Claymore", 30),
        (8881, "Copper Dagger", 30),
        (2744, "Copper Battle Axe", 35),
        (3299, "Copper Chain Belt", 35),
        (3342, "Runed Copper Gauntlets", 40),
        (3343, "Runed Copper Pants", 45),
        (2746, "Coarse Sharpening Stone", 65),
        (7409, "Coarse Weightstone", 65),
        (3118, "Heavy Copper Maul", 65),
        (3300, "Runed Copper Belt", 70),
        (2747, "Thick War Axe", 70),
        (3344, "Coarse Grinding Stone", 75),
    ];
    let mut services: Vec<TrainerService> = recipes
        .iter()
        .map(|&(id, name, req)| {
            // Group key 2 = TRADESKILL_SERVICE_LEARN: no SKILL_STEP effect on the wire spell.
            let mut s = svc(id, name, TrainerServiceCategory::Unavailable, 2, "Recipes");
            s.subtext = None;
            s.level_req = 0;
            s.skill_req = Some(TrainerSkillReq {
                name: "Blacksmithing".into(),
                rank: req,
                met: false,
            });
            s
        })
        .collect();
    services.reverse(); // wire order is not display order — and must not be
                        // Group key 1 = TRADESKILL_SERVICE_STEP: spell 2020 carries Effect 44 SKILL_STEP.
    let mut learn = svc(
        2020,
        "Apprentice Blacksmith",
        TrainerServiceCategory::Available,
        1,
        "Development Skills",
    );
    learn.subtext = None;
    learn.level_req = 5; // the key that used to decide everything, and decides nothing here
    learn.prof_first_rank = true;
    services.insert(services.len() / 2, learn);

    let mut s = UiScript::new().unwrap();
    s.set_trainer(Some(TrainerState {
        greeting: "Care to learn how to turn the ore that you find into weapons?".into(),
        trainer_type: 2,
        groups: Vec::new(),
        services,
    }));

    let rows = visible(&mut s);
    let expected: Vec<(&str, &str)> = vec![
        ("Development Skills", "header"),
        ("Apprentice Blacksmith", "available"),
        ("Recipes", "header"),
        ("Copper Chain Pants", "unavailable"),
        ("Copper Mace", "unavailable"),
        ("Copper Axe", "unavailable"),
        ("Copper Chain Boots", "unavailable"),
        ("Copper Shortsword", "unavailable"),
        ("Rough Grinding Stone", "unavailable"),
        ("Copper Claymore", "unavailable"),
        ("Copper Dagger", "unavailable"),
        ("Copper Battle Axe", "unavailable"),
        ("Copper Chain Belt", "unavailable"),
        ("Runed Copper Gauntlets", "unavailable"),
        ("Runed Copper Pants", "unavailable"),
        ("Coarse Sharpening Stone", "unavailable"),
        ("Coarse Weightstone", "unavailable"),
        ("Heavy Copper Maul", "unavailable"),
        ("Runed Copper Belt", "unavailable"),
        ("Thick War Axe", "unavailable"),
        ("Coarse Grinding Stone", "unavailable"),
    ];
    let got: Vec<(&str, &str)> = rows.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
    assert_eq!(got, expected);
}

/// **The mount (type 1, the client's "talent") order, pinned against wow-re's emulated run**
/// (decision 1124): the already-known services fold into the `-1` "My Talents" group, which the
/// header comparator puts **first**, ahead of the name-ordered skill-line headers; within a group
/// the state byte sorts available → unavailable → used, then the name.
#[test]
fn mount_trainer_folds_known_services_into_my_talents() {
    let known = |id: u32, name: &str| {
        svc(
            id,
            name,
            TrainerServiceCategory::Used,
            TRAINER_GROUP_KNOWN,
            "My Talents",
        )
    };
    let services = vec![
        svc(
            33,
            "Riding",
            TrainerServiceCategory::Available,
            762,
            "Riding",
        ),
        known(6648, "Tiger Riding"),
        svc(
            824,
            "Horse Riding",
            TrainerServiceCategory::Unavailable,
            148,
            "Horse Riding",
        ),
        svc(
            8394,
            "Ram Riding",
            TrainerServiceCategory::Available,
            152,
            "Ram Riding",
        ),
    ];
    let mut s = UiScript::new().unwrap();
    s.set_trainer(Some(TrainerState {
        greeting: "Ready to ride?".into(),
        trainer_type: 1,
        groups: Vec::new(),
        services,
    }));

    let got: Vec<String> = visible(&mut s).into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        got,
        [
            "My Talents",
            "Tiger Riding",
            "Horse Riding",
            "Horse Riding",
            "Ram Riding",
            "Ram Riding",
            "Riding",
            "Riding",
        ],
        "the -1 group leads; every other header is name-ordered"
    );
    // And the type predicates fork on the same dword the sort does.
    assert!(s.eval::<bool>("return IsTalentTrainer() == 1").unwrap());
    assert!(s
        .eval::<bool>("return IsTradeskillTrainer() == nil")
        .unwrap());
}

/// The **state byte is a sort key at type 1 and only there** — the same three services, ordered by
/// state (available → unavailable → used) inside one group, where the class cascade would order
/// them by level and the tradeskill one by skill value.
#[test]
fn talent_order_sorts_on_state_within_a_group() {
    let one = |id: u32, name: &str, cat: TrainerServiceCategory| svc(id, name, cat, 762, "Riding");
    let services = vec![
        one(3, "Cee", TrainerServiceCategory::Available),
        one(1, "Aye", TrainerServiceCategory::Used),
        one(2, "Bee", TrainerServiceCategory::Unavailable),
    ];
    let mut s = UiScript::new().unwrap();
    s.set_trainer(Some(TrainerState {
        greeting: String::new(),
        trainer_type: 1,
        groups: Vec::new(),
        services: services.clone(),
    }));
    let got: Vec<String> = visible(&mut s).into_iter().map(|(n, _)| n).collect();
    assert_eq!(got, ["Riding", "Cee", "Bee", "Aye"]);

    // The same three at a CLASS trainer sort by name instead (equal levels, no skill gates) — the
    // proof that the comparator really is selected by the type and not shared.
    s.set_trainer(Some(TrainerState {
        greeting: String::new(),
        trainer_type: 0,
        groups: Vec::new(),
        services,
    }));
    let got: Vec<String> = visible(&mut s).into_iter().map(|(n, _)| n).collect();
    assert_eq!(got, ["Riding", "Aye", "Bee", "Cee"]);
}
