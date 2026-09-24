//! `ChrClasses.dbc`, loaded once and read by everything that asks the client a question about a
//! class.
//!
//! Three consumers today and they are unrelated to each other — [`crate::ui_pet_book`] wants field
//! 4's pet name token, `UnitHasRelicSlot` wants field 16's relic flag, and
//! `crate::spell::mods` wants field 15's class spell-family — which is exactly why the
//! table does not live inside any of them. It used to live in the pet book, back when the pet
//! token was the only column anyone read.
//!
//! The resource is **absent**, not empty, when the load fails. Every reader then falls to the
//! reference's own degraded answer (`"PET"` for the token, no relic slot for any class, family 0
//! — which the modifier gate's first conjunct refuses), so a missing table costs a warlock the
//! word "Demon", a paladin their relic branches and everyone their talent modifiers rather than
//! taking the client down.

use bevy::prelude::*;

use benilla_assets::{AssetSet, LockRecover, WorldAssets};
use benilla_formats::ChrClasses;

/// The parsed table — see [`benilla_formats::ChrClasses`] for what each column is and which bytes
/// say so.
#[derive(Resource)]
pub(crate) struct ChrClassTable(pub(crate) ChrClasses);

pub(crate) struct ChrClassesPlugin;

impl Plugin for ChrClassesPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_chr_classes.after(AssetSet::Open));
    }
}

fn load_chr_classes(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_chr_classes(&mut chain)
    };
    match loaded {
        Ok(table) => commands.insert_resource(ChrClassTable(table)),
        Err(e) => warn!(
            "chr_classes: ChrClasses.dbc failed to load — every pet book tab reads the client's \
             own \"PET\" fallback, so a warlock's says Pet rather than Demon, no class reads \
             as having a relic slot, and no talent spell modifier can apply (the gate has no \
             class family to match): {e:#}"
        ),
    }
}
