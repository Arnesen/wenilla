//! Stamps the commit this binary was built from — see `benilla-buildstamp`, which owns the rule.

fn main() {
    benilla_buildstamp::emit();
}
