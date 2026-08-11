//! Stamps the commit this binary was built from — see `benilla-buildstamp`, which owns the rule
//! (and the reason it lives in this ~30-line shim rather than in `benilla-app`: decision 0993).

fn main() {
    benilla_buildstamp::emit();
}
