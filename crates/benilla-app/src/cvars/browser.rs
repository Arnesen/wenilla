//! Browser URL overrides applied to the central registry before the saved file loads.
use super::{CvarChanged, Cvars};

pub(super) fn apply_override(cvars: &mut Cvars, name: &str, value: &str) {
    let Some(row) = cvars.row(name) else { return };
    if row.numeric() && value.trim().parse::<f32>().is_err() {
        return;
    }
    let name = row.name.clone();
    let old = row.value.clone();
    cvars.own_for_session(&name, Some(value));
    // Unlike native env overrides, URL values have not already reached the knobs.
    // Boot applies latched settings immediately, just like loading the saved file.
    if old != value {
        cvars.events.push(CvarChanged {
            name,
            old,
            new: value.to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn url_override_updates_knobs_and_mirror_without_overwriting_saved_value() {
        let mut cvars = Cvars::default();
        apply_override(&mut cvars, "farclip", "150");
        cvars.load_file([("farclip".into(), "300".into())].into());
        assert_eq!(cvars.get("farclip"), Some("150"));
        assert_eq!(cvars.file.get("farclip").map(String::as_str), Some("300"));
        assert!(cvars
            .take_events()
            .iter()
            .any(|e| e.is("farclip") && e.new == "150"));
        assert!(cvars
            .vm_seed()
            .iter()
            .any(|r| r.name.eq_ignore_ascii_case("farclip") && r.value == "150"));
        assert!(!cvars.dirty);
    }
    #[test]
    fn a_boot_url_applies_a_latched_setting_immediately() {
        let mut cvars = Cvars::default();
        assert!(cvars.row("gxMultisample").unwrap().latched);
        apply_override(&mut cvars, "gxMultisample", "4");
        assert_eq!(cvars.get("gxMultisample"), Some("4"));
        assert!(cvars.row("gxMultisample").unwrap().pending.is_none());
        assert!(cvars
            .take_events()
            .iter()
            .any(|e| e.is("gxMultisample") && e.new == "4"));
    }

    #[test]
    fn invalid_url_value_does_not_block_the_saved_setting() {
        let mut cvars = Cvars::default();
        apply_override(&mut cvars, "farclip", "invalid");
        cvars.load_file([("farclip".into(), "300".into())].into());
        assert_eq!(cvars.get("farclip"), Some("300"));
        assert!(!cvars.is_session_owned("farclip"));
    }
}
