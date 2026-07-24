//! Vanilla (1.12) world-packet **header obfuscation**.
//!
//! After `CMSG_AUTH_SESSION`, every world-packet *header* (not the body) is run through a stateful
//! byte cipher keyed by the SRP session key. It is not a real cipher — each byte is XORed with a
//! rotating session-key byte and chained against the previous ciphertext byte:
//!
//! ```text
//! encrypt: c = (p ^ key[i]) + last_c ;  decrypt: p = (c - last_c) ^ key[i]
//! ```
//!
//! with `i` cycling over the 40-byte key and `last_c` the previous ciphertext byte (both start 0).
//! Client headers are 6 bytes (a `u16` BE size and `u32` LE opcode); server headers are **always 4**
//! (a `u16` BE size and `u16` LE opcode). There is no wider-size variant in 1.12: vmangos writes
//! every outgoing header as `ServerPktHeader { uint16 size; uint16 cmd; }` (WorldSocket.cpp — a
//! larger body is *truncated* into the u16, never given a wider field) and routes big object
//! updates through `SMSG_COMPRESSED_UPDATE_OBJECT` instead. The 3-byte large-packet size is a
//! later-expansion convention; nothing in this stack handles it, by design.
//!
//! The handshake: the server's `SMSG_AUTH_CHALLENGE` carries a random seed; the client picks its own
//! seed (a [`ProofSeed`]), proves it knows the session key via
//! [`ProofSeed::into_client_header_crypto`], and from then on encrypts/decrypts headers.

use std::io::{Read, Write};

use rand::{thread_rng, RngCore};
use sha1::{Digest, Sha1};

use crate::{NormalizedString, SESSION_KEY_LENGTH};

/// Client world-header length: `u16` size + `u32` opcode.
pub const CLIENT_HEADER_LENGTH: usize = 6;
/// Server world-header length: `u16` size + `u16` opcode.
pub const SERVER_HEADER_LENGTH: usize = 4;

/// `world_server_proof = SHA1( user | 0u32 | client_seed | server_seed | K )`.
fn world_server_proof(
    username: &NormalizedString,
    session_key: &[u8; SESSION_KEY_LENGTH],
    server_seed: u32,
    client_seed: u32,
) -> [u8; 20] {
    let mut h = Sha1::new();
    h.update(username.as_ref().as_bytes());
    h.update(0u32.to_le_bytes());
    h.update(client_seed.to_le_bytes());
    h.update(server_seed.to_le_bytes());
    h.update(session_key);
    h.finalize().into()
}

/// Encryption half of the header cipher — kept with the write half of a split connection.
#[derive(Debug, Clone)]
pub struct EncrypterHalf {
    session_key: [u8; SESSION_KEY_LENGTH],
    index: u8,
    previous: u8,
}

impl EncrypterHalf {
    /// Encrypt `data` in place, advancing the cipher state.
    pub fn encrypt(&mut self, data: &mut [u8]) {
        for byte in data {
            let encrypted =
                (*byte ^ self.session_key[self.index as usize]).wrapping_add(self.previous);
            self.index = (self.index + 1) % SESSION_KEY_LENGTH as u8;
            *byte = encrypted;
            self.previous = encrypted;
        }
    }

    /// Encrypt a client header (`u16` BE size + `u32` LE opcode).
    pub fn encrypt_client_header(&mut self, size: u16, opcode: u32) -> [u8; CLIENT_HEADER_LENGTH] {
        let size = size.to_be_bytes();
        let opcode = opcode.to_le_bytes();
        let mut header = [size[0], size[1], opcode[0], opcode[1], opcode[2], opcode[3]];
        self.encrypt(&mut header);
        header
    }

    /// Write an encrypted client header to `w`.
    pub fn write_encrypted_client_header<W: Write>(
        &mut self,
        mut w: W,
        size: u16,
        opcode: u32,
    ) -> std::io::Result<()> {
        let header = self.encrypt_client_header(size, opcode);
        w.write_all(&header)
    }
}

/// Decryption half of the header cipher — kept with the read half of a split connection.
#[derive(Debug, Clone)]
pub struct DecrypterHalf {
    session_key: [u8; SESSION_KEY_LENGTH],
    index: u8,
    previous: u8,
}

impl DecrypterHalf {
    /// Decrypt `data` in place, advancing the cipher state.
    pub fn decrypt(&mut self, data: &mut [u8]) {
        for byte in data {
            let encrypted = *byte;
            let decrypted =
                encrypted.wrapping_sub(self.previous) ^ self.session_key[self.index as usize];
            self.index = (self.index + 1) % SESSION_KEY_LENGTH as u8;
            self.previous = encrypted;
            *byte = decrypted;
        }
    }

    /// Decrypt a 4-byte server header into `(size, opcode)`. (1.12 server headers are always 4
    /// bytes — see the module doc; there is no wider-size variant to handle.)
    pub fn decrypt_server_header(&mut self, mut data: [u8; SERVER_HEADER_LENGTH]) -> ServerHeader {
        self.decrypt(&mut data);
        ServerHeader {
            size: u16::from_be_bytes([data[0], data[1]]),
            opcode: u16::from_le_bytes([data[2], data[3]]),
        }
    }

    /// Read + decrypt a 4-byte server header from `r`.
    pub fn read_and_decrypt_server_header<R: Read>(
        &mut self,
        mut r: R,
    ) -> std::io::Result<ServerHeader> {
        let mut buf = [0u8; SERVER_HEADER_LENGTH];
        r.read_exact(&mut buf)?;
        Ok(self.decrypt_server_header(buf))
    }
}

/// A decrypted server-packet header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerHeader {
    /// Body size in bytes, *including* the opcode field but not the size field.
    pub size: u16,
    /// Server opcode.
    pub opcode: u16,
}

/// Both halves of the header cipher. Split with [`Self::split`] for separate read/write paths.
#[derive(Debug, Clone)]
pub struct HeaderCrypto {
    encrypt: EncrypterHalf,
    decrypt: DecrypterHalf,
}

impl HeaderCrypto {
    /// Mutable access to the decryption half.
    pub fn decrypter(&mut self) -> &mut DecrypterHalf {
        &mut self.decrypt
    }

    /// Mutable access to the encryption half.
    pub fn encrypter(&mut self) -> &mut EncrypterHalf {
        &mut self.encrypt
    }

    /// Split into independent encrypt / decrypt halves (one per direction of a split socket).
    pub fn split(self) -> (EncrypterHalf, DecrypterHalf) {
        (self.encrypt, self.decrypt)
    }

    /// The cipher built straight from a session key, skipping the proof step — the *peer* side of
    /// [`ProofSeed::into_client_header_crypto`]. Both ends run the same state machine with inverse
    /// operations from (index 0, previous 0), so a server's [`EncrypterHalf`] is exactly what a
    /// client's [`DecrypterHalf`] inverts. Lets a test harness stand up a fake world server that
    /// speaks real encrypted headers.
    pub fn from_session_key(session_key: [u8; SESSION_KEY_LENGTH]) -> Self {
        Self::new(session_key)
    }

    fn new(session_key: [u8; SESSION_KEY_LENGTH]) -> Self {
        Self {
            encrypt: EncrypterHalf {
                session_key,
                index: 0,
                previous: 0,
            },
            decrypt: DecrypterHalf {
                session_key,
                index: 0,
                previous: 0,
            },
        }
    }
}

/// The client's random seed for the world handshake. Created before `CMSG_AUTH_SESSION`; turned into
/// the proof + [`HeaderCrypto`] once we know the server's seed.
#[derive(Debug, Clone, Copy)]
pub struct ProofSeed {
    seed: u32,
}

impl ProofSeed {
    /// A new random client seed.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            seed: thread_rng().next_u32(),
        }
    }

    /// The client seed to send in `CMSG_AUTH_SESSION`.
    pub const fn seed(&self) -> u32 {
        self.seed
    }

    /// Compute the client proof (binds the session key to both seeds) and the [`HeaderCrypto`] used
    /// for all subsequent headers. Valid once the server replies with a successful `SMSG_AUTH_RESPONSE`.
    pub fn into_client_header_crypto(
        self,
        username: &NormalizedString,
        session_key: [u8; SESSION_KEY_LENGTH],
        server_seed: u32,
    ) -> ([u8; 20], HeaderCrypto) {
        let proof = world_server_proof(username, &session_key, server_seed, self.seed);
        (proof, HeaderCrypto::new(session_key))
    }
}
