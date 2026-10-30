//! wenilla: the platform's half in a page, where there is no C runtime: the browser's own zone,
//! through `Date`. `getTimezoneOffset` is minutes *west* of UTC at the instant asked, so a zone's
//! standard offset is the larger of January's and July's, and an instant is daylight time when its
//! offset is less than that.

use js_sys::Date;

/// The page's offset west of UTC in minutes at `secs` past the epoch.
fn west_minutes(secs: i64) -> f64 {
    Date::new(&wasm_bindgen::JsValue::from_f64(secs as f64 * 1000.0)).get_timezone_offset()
}

/// The zone's standard offset west of UTC in minutes, off this year's January and July.
fn standard_west() -> f64 {
    let year = super::from_unix(super::unix_seconds()).year;
    let at = |month| west_minutes(super::days_from_civil(year, month, 1) * 86_400);
    at(1).max(at(7))
}

pub(super) fn tzset() {}

pub(super) fn localtime(secs: i64) -> Option<(i64, bool)> {
    let west = west_minutes(secs);
    west.is_finite()
        .then(|| ((-west * 60.0) as i64, west < standard_west()))
}

/// `_timezone` and `_dstbias`: the standard offset west of UTC in seconds, and what daylight time
/// adds to it (0 for a zone with none), off this year's January and July, as the unix half does.
pub(super) fn biases(_: &[super::Span]) -> (i32, i32) {
    let year = super::from_unix(super::unix_seconds()).year;
    let at = |month| west_minutes(super::days_from_civil(year, month, 1) * 86_400);
    let (jan, jul) = (at(1), at(7));
    let standard = jan.max(jul);
    let daylight = jan.min(jul) - standard;
    ((standard * 60.0) as i32, (daylight * 60.0) as i32)
}

/// No zone abbreviation reaches a page without `Intl`; the names read empty.
pub(super) fn name(_isdst: bool) -> Vec<u8> {
    Vec::new()
}
