//! Stamps the commit this binary was built from — see `benilla-buildstamp`, which owns the rule
//! (and the reason it lives in this shim rather than in `benilla-app`: decision 0993). Runs on
//! the HOST, not the wasm32 target, same as any build script — the `git` calls it shells out to
//! never need to cross-compile.

fn main() {
    benilla_buildstamp::emit();
}
