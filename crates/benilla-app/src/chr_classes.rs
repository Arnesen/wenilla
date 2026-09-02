//! `ChrClasses.dbc`, loaded once and read by everything that asks the client a question about a
//! class.
//!
//! Two consumers today and they are unrelated to each other — [`crate::ui_pet_book`] wants field
//! 4's pet name token, `UnitHasRelicSlot` wants field 16's relic flag — which is exactly why the
//! table does not live inside either of them. It used to live in the pet book, back when the pet
//! token was the only column anyone read.
//!
//! The resource is **absent**, not empty, when the load fails. Both readers then fall to the
//! reference's own degraded answer (`"PET"` for the token, no relic slot for any class), so a
//! missing table costs a warlock the word "Demon" and a paladin their relic branches rather than
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
             own \"PET\" fallback, so a warlock's says Pet rather than Demon, and no class reads \
             as having a relic slot: {e:#}"
        ),
    }
}
