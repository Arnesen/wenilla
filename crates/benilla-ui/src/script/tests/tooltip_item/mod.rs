//! The engine item-tooltip renderer (decision 0274 P1, law per 0276): the verified line law
//! end-to-end, the red failed-requirement law against live player state, the sell-price money
//! protocol (engine fires `OnTooltipAddMoney` — merchant open + real instance + repair off),
//! and the in-flight-template fallback.

use super::common::script;
use crate::script::*;

/// A full weapon view exercising most line families.
fn axe() -> ItemTemplateView {
    ItemTemplateView {
        name: "Ravager".into(),
        quality: 3,
        class: 2,
        subclass: 1, // two-hand axe shares the "Axe" display name
        inventory_type: 17,
        bonding: 2,
        stats: vec![(7, 12), (4, 9)],
        damages: vec![(68.0, 103.0, 0), (2.0, 4.0, 5)],
        delay_ms: 3500,
        resistances: [0, 0, 0, 0, 10, 0],
        max_durability: 90,
        required_level: 37,
        allowable_class: (1 << 0) | (1 << 3), // Warrior, Rogue
        spell_triggers: vec![(2, 9633, "Ravager".into())],
        description: "A wicked axe of the Scarlet Crusade.".into(),
        sell_price: 15230,
        ..Default::default()
    }
}

/// The tooltip's left-column lines in order, each with its draw color (read from the extract —
/// the color a renderer would actually paint).
fn lines_of(s: &mut UiScript) -> Vec<(String, [f32; 4])> {
    s.resolve();
    let quads = s.extract();
    let count: i64 = s.eval("return TT:NumLines()").unwrap();
    (1..=count)
        .map(|i| {
            let t: String = s
                .eval(&format!(
                    "return getglobal('TTTextLeft{i}'):GetText() or ''"
                ))
                .unwrap();
            let color = quads
                .iter()
                .find_map(|q| match &q.content {
                    QuadContent::Text {
                        text: Some(txt),
                        color,
                        ..
                    } if *txt == t => *color,
                    _ => None,
                })
                .unwrap_or([0.0; 4]);
            (t, color)
        })
        .collect()
}

/// A right-cell color, matched by its unique text in the extract.
fn right_color(s: &mut UiScript, needle: &str) -> [f32; 4] {
    s.resolve();
    s.extract()
        .iter()
        .find_map(|q| match &q.content {
            QuadContent::Text {
                text: Some(t),
                color,
                ..
            } if t == needle => *color,
            _ => None,
        })
        .unwrap_or_else(|| panic!("no right cell {needle:?}"))
}

mod bindings;
mod law;
mod mechanics;
mod set;
