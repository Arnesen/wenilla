//! Shared fixtures for the script test modules.

use crate::script::UiScript;

pub(super) fn script() -> UiScript {
    UiScript::new().expect("construct UiScript")
}
