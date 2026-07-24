//! Shared fixtures for the world message layer's oracle-free regression tests (split by domain
//! across `tests/*.rs`). Client packet bodies are pinned to golden hex captured from the validated
//! implementation (byte-validated against `wow_world_messages` during the decision-0021 migration);
//! `SMSG_UPDATE_OBJECT` fixtures are real serialized packet bodies from that same corpus, parsed +
//! decoded here. Simple server bodies are hand-built (their layout is trivial: a few little-endian
//! scalars).

pub fn hx(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
