//! Sealing for secrets Nova keeps at rest (stardelt-nova#103): the tokens of a
//! persisted session.
//!
//! AES-256-GCM from `ring` (already in the build through rustls) with a fresh random
//! 96-bit nonce per value. The key is derived with HKDF-SHA256 from
//! `NOVA_SESSION_SECRET` under its own label, so it is never the cookie-signing key.
//!
//! Layout of a sealed value: `[VERSION][nonce, 12 bytes][ciphertext][tag, 16 bytes]`.
//!
//! `aad` binds a value to the place it is stored (a session row's id hash): a sealed
//! value copied to another row does not open.
//!
//! A changed secret cannot open old values. That is the rotation story: everyone signs
//! in again, exactly as when the cookie key changes today.

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::hkdf::{HKDF_SHA256, Salt};
use ring::rand::{SecureRandom, SystemRandom};

/// First byte of every sealed value. A new layout or algorithm gets a new number.
const VERSION: u8 = 1;
/// HKDF salt and label: a key for this one purpose.
const SALT: &[u8] = b"stardelt-nova";
const INFO: &[u8] = b"session-seal-v1";
/// The AES-256-GCM tag length.
const TAG_LEN: usize = 16;

/// Why a value could not be sealed or opened. It never carries the value.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SealError {
    #[error("the system random source failed")]
    Random,
    #[error("the value could not be sealed")]
    Seal,
    #[error("the value is not sealed with this key for this place")]
    Open,
}

/// The key for sealed values. No `Debug` on purpose: it holds a key.
pub struct Sealer {
    key: LessSafeKey,
    rng: SystemRandom,
}

impl Sealer {
    /// The sealing key for `secret` (`NOVA_SESSION_SECRET`, at least 32 bytes).
    pub fn from_secret(secret: &[u8]) -> Self {
        let prk = Salt::new(HKDF_SHA256, SALT).extract(secret);
        let info = [INFO];
        let okm = prk
            .expand(&info, &AES_256_GCM)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        Self {
            key: LessSafeKey::new(UnboundKey::from(okm)),
            rng: SystemRandom::new(),
        }
    }

    /// `plaintext`, sealed and bound to `aad`.
    pub fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, SealError> {
        let mut nonce = [0u8; NONCE_LEN];
        self.rng.fill(&mut nonce).map_err(|_| SealError::Random)?;
        let mut body = plaintext.to_vec();
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad),
                &mut body,
            )
            .map_err(|_| SealError::Seal)?;
        let mut sealed = Vec::with_capacity(1 + NONCE_LEN + body.len());
        sealed.push(VERSION);
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&body);
        Ok(sealed)
    }

    /// The plaintext of a value `seal` made with the same key and the same `aad`.
    pub fn open(&self, sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>, SealError> {
        if sealed.len() < 1 + NONCE_LEN + TAG_LEN || sealed[0] != VERSION {
            return Err(SealError::Open);
        }
        let nonce = Nonce::try_assume_unique_for_key(&sealed[1..1 + NONCE_LEN])
            .map_err(|_| SealError::Open)?;
        let mut body = sealed[1 + NONCE_LEN..].to_vec();
        let plaintext = self
            .key
            .open_in_place(nonce, Aad::from(aad), &mut body)
            .map_err(|_| SealError::Open)?;
        Ok(plaintext.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"a-test-session-secret-of-32-bytes!";
    const ROW: &[u8] = b"row-1";

    #[test]
    fn a_sealed_value_opens_with_the_same_key_and_place() {
        let sealer = Sealer::from_secret(SECRET);
        let sealed = sealer.seal(b"refresh-token-value", ROW).unwrap();
        assert_eq!(sealer.open(&sealed, ROW).unwrap(), b"refresh-token-value");
    }

    #[test]
    fn a_sealed_value_does_not_contain_the_plaintext() {
        let sealer = Sealer::from_secret(SECRET);
        let sealed = sealer.seal(b"refresh-token-value", ROW).unwrap();
        assert!(!sealed.windows(7).any(|w| w == b"refresh"));
        assert_eq!(sealed[0], VERSION);
        assert_eq!(
            sealed.len(),
            1 + NONCE_LEN + b"refresh-token-value".len() + TAG_LEN
        );
    }

    #[test]
    fn sealing_the_same_value_twice_gives_two_different_values() {
        let sealer = Sealer::from_secret(SECRET);
        let a = sealer.seal(b"same", ROW).unwrap();
        let b = sealer.seal(b"same", ROW).unwrap();
        assert_ne!(a, b, "every seal draws a fresh nonce");
    }

    #[test]
    fn a_changed_byte_does_not_open() {
        let sealer = Sealer::from_secret(SECRET);
        let mut sealed = sealer.seal(b"refresh-token-value", ROW).unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert_eq!(sealer.open(&sealed, ROW), Err(SealError::Open));
    }

    #[test]
    fn a_value_moved_to_another_place_does_not_open() {
        let sealer = Sealer::from_secret(SECRET);
        let sealed = sealer.seal(b"refresh-token-value", ROW).unwrap();
        assert_eq!(sealer.open(&sealed, b"row-2"), Err(SealError::Open));
    }

    #[test]
    fn another_secret_does_not_open() {
        let sealed = Sealer::from_secret(SECRET)
            .seal(b"refresh-token-value", ROW)
            .unwrap();
        let other = Sealer::from_secret(b"a-completely-different-secret-of-32b!");
        assert_eq!(other.open(&sealed, ROW), Err(SealError::Open));
    }

    #[test]
    fn short_or_unknown_values_do_not_open() {
        let sealer = Sealer::from_secret(SECRET);
        assert_eq!(sealer.open(b"", ROW), Err(SealError::Open));
        assert_eq!(sealer.open(&[VERSION; 20], ROW), Err(SealError::Open));
        let mut sealed = sealer.seal(b"x", ROW).unwrap();
        sealed[0] = 2;
        assert_eq!(sealer.open(&sealed, ROW), Err(SealError::Open));
    }
}
