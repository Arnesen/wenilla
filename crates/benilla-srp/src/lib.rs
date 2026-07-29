//! SRP6 client + vanilla world-header crypto for **WoW 1.12.1 (build 5875)** — in-repo, replacing
//! `wow_srp` (decision 0021).
//!
//! WoW uses a lightly customised SRP6 for logon: a fixed 32-byte safe prime `N`, generator `g = 7`,
//! multiplier `k = 3`, and a bespoke "interleave" that folds the shared secret `S` into the 40-byte
//! session key. We implement only the **client** side (the realmd handshake) plus the world server's
//! header obfuscation — the only pieces benilla needs.
//!
//! All byte arrays are **little endian** on the wire (as the client sends them).
//!
//! Proven by a full SRP6 round-trip against `wow_srp`'s server + a byte-exact header-cipher diff
//! during the decision-0021 migration (oracle test in git history); ongoing regression coverage is
//! the oracle-free known-answer + golden tests in this crate.

use num_bigint::BigInt;
use rand::{thread_rng, RngCore};
use sha1::{Digest, Sha1};

pub mod vanilla_header;

pub use vanilla_header::{DecrypterHalf, EncrypterHalf, HeaderCrypto, ProofSeed};

/// Session-key length in bytes — always 40 (two concatenated SHA-1 hashes).
pub const SESSION_KEY_LENGTH: usize = 40;
/// Proof (`M1`/`M2`) length in bytes — a SHA-1 hash.
pub const PROOF_LENGTH: usize = 20;
/// Public-key (`A`/`B`) length in bytes.
pub const PUBLIC_KEY_LENGTH: usize = 32;
/// Salt length in bytes.
pub const SALT_LENGTH: usize = 32;
/// Generator `g` — statically 7 for WoW.
pub const GENERATOR: u8 = 7;
/// The WoW safe prime `N`, little endian (as sent in `CMD_AUTH_LOGON_CHALLENGE_Server`).
pub const LARGE_SAFE_PRIME_LITTLE_ENDIAN: [u8; 32] = [
    0xb7, 0x9b, 0x3e, 0x2a, 0x87, 0x82, 0x3c, 0xab, 0x8f, 0x5e, 0xbf, 0xbf, 0x8e, 0xb1, 0x1, 0x8,
    0x53, 0x50, 0x6, 0x29, 0x8b, 0x5b, 0xad, 0xbd, 0x5b, 0x53, 0xe1, 0x89, 0x5e, 0x64, 0x4b, 0x89,
];
/// The SRP multiplier `k` — statically 3 in this (pre-SRP6a) flavour.
const K_VALUE: u8 = 3;

// --- bigint helpers (little-endian, unsigned magnitude) -------------------------------------------

fn from_le(bytes: &[u8]) -> BigInt {
    BigInt::from_bytes_le(num_bigint::Sign::Plus, bytes)
}

/// Magnitude of `v` as a zero-padded 32-byte little-endian array (`v` is always `< N < 2^256`).
fn to_padded_32_le(v: &BigInt) -> [u8; 32] {
    let (_, bytes) = v.to_bytes_le();
    let mut out = [0u8; 32];
    out[..bytes.len()].copy_from_slice(&bytes);
    out
}

fn sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut h = Sha1::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

// --- normalized string ----------------------------------------------------------------------------

/// A username/password normalised the way the 1.12 client does it: ASCII only (no control chars),
/// uppercased, 1..=16 bytes. The SRP6 hashes are computed over this form, and the uppercased account
/// name is what's sent on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedString {
    s: String,
}

/// Error from [`NormalizedString::new`].
#[derive(Debug)]
pub enum NormalizedStringError {
    /// Empty or longer than 16 bytes.
    InvalidLength,
    /// Contained a non-ASCII or ASCII-control character.
    CharacterNotAllowed(char),
}

impl std::fmt::Display for NormalizedStringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidLength => write!(f, "string must be 1..=16 bytes"),
            Self::CharacterNotAllowed(c) => write!(f, "character not allowed: {c:?}"),
        }
    }
}
impl std::error::Error for NormalizedStringError {}

impl NormalizedString {
    /// Validate + uppercase `s`. See [`NormalizedString`].
    pub fn new(s: impl AsRef<str>) -> Result<Self, NormalizedStringError> {
        let s = s.as_ref();
        if s.is_empty() || s.len() > 16 {
            return Err(NormalizedStringError::InvalidLength);
        }
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            if !c.is_ascii() || c.is_ascii_control() {
                return Err(NormalizedStringError::CharacterNotAllowed(c));
            }
            out.push(c.to_ascii_uppercase());
        }
        Ok(Self { s: out })
    }
}

impl AsRef<str> for NormalizedString {
    fn as_ref(&self) -> &str {
        &self.s
    }
}

// --- public key -----------------------------------------------------------------------------------

/// A validated SRP public key (`A` or `B`), stored little endian. Rejected if it is exactly zero or
/// exactly the safe prime `N` (the only 32-byte values that are `0 mod N`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicKey {
    key: [u8; 32],
}

/// Error from [`PublicKey::from_le_bytes`].
#[derive(Debug)]
pub enum InvalidPublicKeyError {
    /// The key is all zeros.
    IsZero,
    /// The key is `0 mod N` (equal to the safe prime).
    ModLargeSafePrimeIsZero,
}

impl std::fmt::Display for InvalidPublicKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IsZero => write!(f, "public key is zero"),
            Self::ModLargeSafePrimeIsZero => write!(f, "public key is 0 mod N"),
        }
    }
}
impl std::error::Error for InvalidPublicKeyError {}

impl PublicKey {
    /// Build from little-endian bytes, validating the key (see [`PublicKey`]).
    pub fn from_le_bytes(key: [u8; 32]) -> Result<Self, InvalidPublicKeyError> {
        // Valid unless every byte is either 0 or matches N[i] — i.e. the key is exactly 0 or exactly
        // N. (A multiple of N ≥ 2N needs 33 bytes, so those are the only two zero residues.)
        let only_zero_or_prime = key
            .iter()
            .zip(LARGE_SAFE_PRIME_LITTLE_ENDIAN.iter())
            .all(|(&k, &n)| k == 0 || k == n);
        if only_zero_or_prime {
            return Err(if key[0] == 0 {
                InvalidPublicKeyError::IsZero
            } else {
                InvalidPublicKeyError::ModLargeSafePrimeIsZero
            });
        }
        Ok(Self { key })
    }

    /// The key as little-endian bytes.
    pub const fn as_le_bytes(&self) -> &[u8; 32] {
        &self.key
    }

    fn as_bigint(&self) -> BigInt {
        from_le(&self.key)
    }
}

// --- SRP6 client ----------------------------------------------------------------------------------

/// `H( SHA1(N) XOR SHA1(g) )` — folded into the client proof `M1`. Computed from the server-supplied
/// `g`/`N` (they're fixed for WoW, but we don't assume it).
fn xor_hash(generator: u8, large_safe_prime: &[u8; 32]) -> [u8; 20] {
    let n_hash = sha1(&[large_safe_prime]);
    let g_hash = sha1(&[&[generator]]);
    let mut out = [0u8; 20];
    for (o, (n, g)) in out.iter_mut().zip(n_hash.iter().zip(g_hash.iter())) {
        *o = n ^ g;
    }
    out
}

/// `x = SHA1( salt | SHA1( UPPER(user) ":" UPPER(pass) ) )`.
fn calculate_x(
    username: &NormalizedString,
    password: &NormalizedString,
    salt: &[u8; 32],
) -> [u8; 20] {
    let inner = sha1(&[
        username.as_ref().as_bytes(),
        b":",
        password.as_ref().as_bytes(),
    ]);
    sha1(&[salt, &inner])
}

/// `u = SHA1( A | B )` (both little endian).
fn calculate_u(client_public_key: &PublicKey, server_public_key: &PublicKey) -> [u8; 20] {
    sha1(&[
        client_public_key.as_le_bytes(),
        server_public_key.as_le_bytes(),
    ])
}

/// Fold the shared secret `S` (32 LE bytes) into the 40-byte session key: split the even / odd bytes,
/// SHA-1 each half, then interleave the two digests. This is WoW's specific `SHA1_Interleave`.
///
/// The SRP-6 RFC strips low-order zero bytes of `S` before the split; WoW's does **not** — it hashes
/// all 32 bytes unconditionally (`SRP6::HashSessionKey`, vmangos `src/shared/Crypto/Authentication/
/// SRP6.cpp:127`). Trimming derives a different `K` — and so a different `M1` — for the ~1-in-256
/// handshake whose `S` happens to end in a zero low byte, which the server answers with
/// `WOW_FAIL_UNKNOWN_ACCOUNT` (0x04): an intermittent "wrong password" on a correct one.
fn calculate_interleaved(s: &[u8; 32]) -> [u8; 40] {
    let mut e = [0u8; 16];
    for (i, b) in s.iter().step_by(2).enumerate() {
        e[i] = *b;
    }
    let g = sha1(&[&e]);

    let mut f = [0u8; 16];
    for (i, b) in s.iter().skip(1).step_by(2).enumerate() {
        f[i] = *b;
    }
    let h = sha1(&[&f]);

    let mut out = [0u8; 40];
    for (i, (gi, hi)) in g.iter().zip(h.iter()).enumerate() {
        out[i * 2] = *gi;
        out[i * 2 + 1] = *hi;
    }
    out
}

/// `M2 = SHA1( A | M1 | K )` — what the server proves back and we verify.
fn calculate_server_proof(
    client_public_key: &PublicKey,
    client_proof: &[u8; 20],
    session_key: &[u8; 40],
) -> [u8; 20] {
    sha1(&[client_public_key.as_le_bytes(), client_proof, session_key])
}

/// First step of the client logon: given the server's challenge values, computes our public key `A`,
/// the proof `M1`, and the session key. Send `A` + `M1` in `CMD_AUTH_LOGON_PROOF_Client`, then call
/// [`SrpClientChallenge::verify_server_proof`] with the server's `M2`.
#[derive(Debug, Clone)]
pub struct SrpClientChallenge {
    username: NormalizedString,
    client_proof: [u8; 20],
    client_public_key: [u8; 32],
    session_key: [u8; 40],
}

impl SrpClientChallenge {
    /// Compute `A`, `M1`, and the session key from the server challenge. Mirrors the real client:
    /// random 32-byte private key `a`, `A = g^a mod N`, `S = (B - k·g^x)^(a + u·x) mod N`, session
    /// key = interleave(S), `M1 = SHA1( H(N)^H(g) | SHA1(user) | salt | A | B | K )`.
    pub fn new(
        username: NormalizedString,
        password: NormalizedString,
        generator: u8,
        large_safe_prime: [u8; 32],
        server_public_key: PublicKey,
        salt: [u8; 32],
    ) -> SrpClientChallenge {
        let n = from_le(&large_safe_prime);
        let g = BigInt::from(generator);
        let k = BigInt::from(K_VALUE);

        let mut private_key = [0u8; 32];
        thread_rng().fill_bytes(&mut private_key);
        let a = from_le(&private_key);

        // A = g^a mod N
        let client_public_key = to_padded_32_le(&g.modpow(&a, &n));

        let x = from_le(&calculate_x(&username, &password, &salt));
        let client_pk = PublicKey::from_le_bytes(client_public_key)
            .expect("generated client public key is valid");
        let u = from_le(&calculate_u(&client_pk, &server_public_key));

        // S = (B - k·(g^x mod N))^(a + u·x) mod N
        let s_int =
            (server_public_key.as_bigint() - &k * g.modpow(&x, &n)).modpow(&(&a + &u * &x), &n);
        let session_key = calculate_interleaved(&to_padded_32_le(&s_int));

        // M1 = SHA1( xor_hash | SHA1(username) | salt | A | B | K )
        let username_hash = sha1(&[username.as_ref().as_bytes()]);
        let client_proof = sha1(&[
            &xor_hash(generator, &large_safe_prime),
            &username_hash,
            &salt,
            &client_public_key,
            server_public_key.as_le_bytes(),
            &session_key,
        ]);

        SrpClientChallenge {
            username,
            client_proof,
            client_public_key,
            session_key,
        }
    }

    /// Our proof `M1` (little endian) — send in `CMD_AUTH_LOGON_PROOF_Client`.
    pub const fn client_proof(&self) -> &[u8; 20] {
        &self.client_proof
    }

    /// Our public key `A` (little endian) — send in `CMD_AUTH_LOGON_PROOF_Client`.
    pub const fn client_public_key(&self) -> &[u8; 32] {
        &self.client_public_key
    }

    /// Verify the server's proof `M2`. On success the SRP handshake is complete and the session key
    /// is held in the returned [`SrpClient`].
    pub fn verify_server_proof(
        self,
        server_proof: [u8; 20],
    ) -> Result<SrpClient, MatchProofsError> {
        let expected = calculate_server_proof(
            &PublicKey::from_le_bytes(self.client_public_key)
                .expect("our own client public key is valid"),
            &self.client_proof,
            &self.session_key,
        );
        if server_proof != expected {
            return Err(MatchProofsError {
                server_proof,
                expected,
            });
        }
        Ok(SrpClient {
            username: self.username,
            session_key: self.session_key,
        })
    }
}

/// The server's proof `M2` did not match what we computed (usually a wrong password).
#[derive(Debug)]
pub struct MatchProofsError {
    /// The proof the server sent.
    pub server_proof: [u8; 20],
    /// The proof we expected.
    pub expected: [u8; 20],
}
impl std::fmt::Display for MatchProofsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "server proof mismatch (wrong password?)")
    }
}
impl std::error::Error for MatchProofsError {}

/// A completed SRP6 logon: holds the session key carried into the world server.
#[derive(Debug, Clone)]
pub struct SrpClient {
    #[allow(dead_code)]
    username: NormalizedString,
    session_key: [u8; 40],
}

impl SrpClient {
    /// The SRP session key `K` (40 little-endian bytes).
    pub const fn session_key(&self) -> &[u8; 40] {
        &self.session_key
    }
}

// --- account creation (server-side verifier) ------------------------------------------------------

/// The SRP6 **password verifier** `v = g^x mod N` (little endian) for a given salt — the value a
/// server stores alongside the salt so it never holds the raw password. Deterministic in
/// `(username, password, salt)`. See [`generate_account`] for the new-account path.
pub fn password_verifier(
    username: &NormalizedString,
    password: &NormalizedString,
    salt: &[u8; 32],
) -> [u8; 32] {
    let n = from_le(&LARGE_SAFE_PRIME_LITTLE_ENDIAN);
    let g = BigInt::from(GENERATOR);
    let x = from_le(&calculate_x(username, password, salt));
    to_padded_32_le(&g.modpow(&x, &n))
}

/// Generate a fresh random salt and the matching [`password_verifier`] for a new account. Returns
/// `(salt, verifier)`, both little endian.
pub fn generate_account(
    username: &NormalizedString,
    password: &NormalizedString,
) -> ([u8; 32], [u8; 32]) {
    let mut salt = [0u8; 32];
    thread_rng().fill_bytes(&mut salt);
    let verifier = password_verifier(username, password, &salt);
    (salt, verifier)
}

#[cfg(test)]
mod tests {
    //! Oracle-free regression tests. The SRP6 *interleave* and `x` derivation are pinned to published
    //! WoW SRP6 test vectors (the gtker/wow_srp corpus, MIT — the same values used to byte-validate
    //! this crate against `wow_srp` during the decision-0021 migration). The header cipher + password
    //! verifier are pinned to golden outputs captured from that validated implementation.
    use super::*;

    fn hx(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
    fn rev(mut v: Vec<u8>) -> Vec<u8> {
        v.reverse();
        v
    }

    #[test]
    fn interleave_known_answer() {
        // (S little-endian, expected session key little-endian) from the WoW SRP6 vector corpus.
        let cases = [
            (
                "8F4CEBD60DFC34E5C007E51BD4F3A4FF2BC1D930E2D3EA770D8D3EEDFF2DCCFC",
                "EE144E1AE08DAC891AB63ABC42BF89738003343422E6B58131BEE4C3087A7027E55A7216D18D556C",
            ),
            (
                "CCC1BDE07FC4FA3182DDEAAB036A88F78AD605AB0D8BFBF6F5EE8ED65CDE4F09",
                "2AA23706C3FC3517A4293E2D4944F567E220CC1A227359D70154E5FD3CEE973673130C4AFBAD9E6D",
            ),
        ];
        for (s_hex, key_hex) in cases {
            let s: [u8; 32] = hx(s_hex).try_into().unwrap();
            assert_eq!(calculate_interleaved(&s).to_vec(), hx(key_hex), "S={s_hex}");
        }
    }

    /// A low-order zero byte in `S` must *not* shorten the split: WoW hashes all 32 bytes, so `S`
    /// and `S` with its low byte zeroed differ only in that byte, never in length. (Trimming — the
    /// SRP-6 RFC behaviour — is what made ~1 login in 256 come back as "wrong password".)
    #[test]
    fn interleave_does_not_trim_low_order_zero_bytes() {
        let mut s = [0u8; 32];
        s.copy_from_slice(&hx(
            "8F4CEBD60DFC34E5C007E51BD4F3A4FF2BC1D930E2D3EA770D8D3EEDFF2DCCFC",
        ));
        let mut zeroed = s;
        zeroed[0] = 0;
        zeroed[1] = 0;

        // Even halves: byte 0 is the only difference; odd halves: byte 1. Both must change, and the
        // trimming implementation would instead have re-split the remaining 30 bytes wholesale.
        let mut expect_even = [0u8; 16];
        let mut expect_odd = [0u8; 16];
        for i in 0..16 {
            expect_even[i] = zeroed[i * 2];
            expect_odd[i] = zeroed[i * 2 + 1];
        }
        let g = sha1(&[&expect_even]);
        let h = sha1(&[&expect_odd]);
        let mut expected = [0u8; 40];
        for (i, (gi, hi)) in g.iter().zip(h.iter()).enumerate() {
            expected[i * 2] = *gi;
            expected[i * 2 + 1] = *hi;
        }

        assert_eq!(calculate_interleaved(&zeroed), expected);
        assert_ne!(calculate_interleaved(&zeroed), calculate_interleaved(&s));
    }

    #[test]
    fn calculate_x_known_answer() {
        // Fixed salt (big-endian in the corpus); user/pass → x (big-endian in the corpus). We store
        // both little-endian, so reverse the corpus hex.
        let salt: [u8; 32] = rev(hx(
            "CAC94AF32D817BA64B13F18FDEDEF92AD4ED7EF7AB0E19E9F2AE13C828AEAF57",
        ))
        .try_into()
        .unwrap();
        let cases = [
            (
                "00XD0QOSA9L8KMXC",
                "43R4Z35TKBKFW8JI",
                "E2F9A0F1E824006C98DA753448E743F7DAA1EAA1",
            ),
            (
                "01GJDP3DSFHR56JQ",
                "9ZK1PFJ9LA0JSHPR",
                "553A6123ABCFD539F2E0B77F64860C64675BC0FD",
            ),
        ];
        for (user, pass, x_hex) in cases {
            let x = calculate_x(
                &NormalizedString::new(user).unwrap(),
                &NormalizedString::new(pass).unwrap(),
                &salt,
            );
            assert_eq!(x.to_vec(), rev(hx(x_hex)), "user={user}");
        }
    }

    #[test]
    fn password_verifier_golden() {
        let salt: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(11).wrapping_add(2));
        let v = password_verifier(
            &NormalizedString::new("alice").unwrap(),
            &NormalizedString::new("password1").unwrap(),
            &salt,
        );
        assert_eq!(
            v.to_vec(),
            hx("28b837075a12b82553921d9095fa3fdcb0151c4bfc860ab97a69d0fa86a3d213")
        );
    }

    #[test]
    fn header_cipher_golden() {
        let sk: [u8; 40] = std::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(3));
        let (_p, crypto) = ProofSeed::new().into_client_header_crypto(
            &NormalizedString::new("alice").unwrap(),
            sk,
            0xDEAD_BEEF,
        );
        let (mut enc, mut dec) = crypto.split();
        assert_eq!(
            enc.encrypt_client_header(12, 0x37F).to_vec(),
            hx("03097792b1d7")
        );
        assert_eq!(
            enc.encrypt_client_header(0x1FF, 0xC7).to_vec(),
            hx("03ceca0c55a5")
        );
        let mut buf: Vec<u8> = (0..32u16).map(|i| (i as u8).wrapping_mul(13)).collect();
        dec.decrypt(&mut buf);
        assert_eq!(
            buf,
            hx("03071c15122b2039364f445d5a5368617e778c85829b90a9a6bfb4cdcac3d8d1")
        );
    }
}
