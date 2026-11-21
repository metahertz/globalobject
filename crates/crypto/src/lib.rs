//! Signing identities and tokens. This crate only joins existing libraries together:
//! tokens are PASETO v4.public (`pasetors`), and keys come from Argon2id (`argon2`).
//! File encryption lives in `rustic_core`, and transport encryption in `iroh`.

use argon2::{Algorithm, Argon2, Params, Version};
use ed25519_dalek::SigningKey;
use pasetors::{
    keys::{AsymmetricPublicKey, AsymmetricSecretKey},
    token::UntrustedToken,
    version4::{PublicToken, V4},
    Public,
};
use rand_core::{OsRng, RngCore};
use serde::{de::DeserializeOwned, Serialize};

pub type Key32 = [u8; 32];

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CryptoError {
    #[error("bad token")]
    BadToken,
    #[error("malformed key")]
    Malformed,
}

pub fn random_key() -> Key32 {
    let mut k = [0u8; 32];
    OsRng.fill_bytes(&mut k);
    k
}

pub fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

/// BLAKE3, the hash used across the system. Same function `iroh` identifies blobs with.
pub fn hash(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

/// The account's signing seed, from its recovery phrase. Argon2id, with the salt bound to the
/// phrase, so the phrase alone recreates the account on a new machine.
pub fn seed_from_phrase(phrase: &str, passphrase: &str) -> Key32 {
    let mut salt = [0u8; 16];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::new(8 * 1024, 1, 1, Some(16)).expect("params"))
        .hash_password_into(phrase.as_bytes(), b"globalobject salt v1", &mut salt)
        .expect("argon2 salt");
    let mut out = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::new(64 * 1024, 3, 1, Some(32)).expect("params"))
        .hash_password_into(format!("{phrase}\0{passphrase}").as_bytes(), &salt, &mut out)
        .expect("argon2");
    out
}

pub struct Identity {
    seed: Key32,
    secret: AsymmetricSecretKey<V4>,
    public: Vec<u8>,
}

impl Identity {
    pub fn generate() -> Self {
        Identity::from_seed(&random_key())
    }
    pub fn from_seed(seed: &Key32) -> Self {
        let sk = SigningKey::from_bytes(seed);
        let secret = AsymmetricSecretKey::<V4>::from(&sk.to_keypair_bytes()).expect("valid key");
        Identity { seed: *seed, secret, public: sk.verifying_key().to_bytes().to_vec() }
    }
    pub fn seed(&self) -> Key32 {
        self.seed
    }
    pub fn public_hex(&self) -> String {
        hex::encode(&self.public)
    }
    /// A PASETO v4.public token over `value`. `purpose` is bound in as the implicit assertion,
    /// so a token issued for one purpose can't be replayed as another.
    pub fn token<T: Serialize>(&self, purpose: &str, value: &T) -> String {
        let payload = serde_json::to_vec(value).expect("serialize");
        PublicToken::sign(&self.secret, &payload, None, Some(purpose.as_bytes())).expect("sign")
    }
}

/// Verify a token and return its contents.
pub fn open_token<T: DeserializeOwned>(public_hex: &str, purpose: &str, token: &str) -> Result<T, CryptoError> {
    let raw = hex::decode(public_hex).map_err(|_| CryptoError::Malformed)?;
    let key = AsymmetricPublicKey::<V4>::from(&raw).map_err(|_| CryptoError::Malformed)?;
    let untrusted = UntrustedToken::<Public, V4>::try_from(token).map_err(|_| CryptoError::BadToken)?;
    let trusted = PublicToken::verify(&key, &untrusted, None, Some(purpose.as_bytes())).map_err(|_| CryptoError::BadToken)?;
    serde_json::from_str(trusted.payload()).map_err(|_| CryptoError::BadToken)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_verify_and_carry_their_contents() {
        let id = Identity::generate();
        let t = id.token("grant", &vec![1, 2, 3]);
        assert!(t.starts_with("v4.public."));
        assert_eq!(open_token::<Vec<i32>>(&id.public_hex(), "grant", &t).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn tokens_reject_other_keys_purposes_and_tampering() {
        let (id, other) = (Identity::generate(), Identity::generate());
        let t = id.token("grant", &7);
        assert_eq!(open_token::<i32>(&other.public_hex(), "grant", &t), Err(CryptoError::BadToken));
        assert_eq!(open_token::<i32>(&id.public_hex(), "receipt", &t), Err(CryptoError::BadToken));
        let mut bad = t.into_bytes();
        let i = bad.len() - 5;
        bad[i] = if bad[i] == b'A' { b'B' } else { b'A' };
        assert_eq!(open_token::<i32>(&id.public_hex(), "grant", &String::from_utf8(bad).unwrap()), Err(CryptoError::BadToken));
    }

    #[test]
    fn seeds_are_deterministic_and_distinct() {
        assert_eq!(seed_from_phrase("a", "p"), seed_from_phrase("a", "p"));
        assert_ne!(seed_from_phrase("a", "p"), seed_from_phrase("a", "q"));
        assert_ne!(seed_from_phrase("a", "p"), seed_from_phrase("b", "p"));
        assert_eq!(Identity::from_seed(&[9; 32]).public_hex(), Identity::from_seed(&[9; 32]).public_hex());
    }
}
