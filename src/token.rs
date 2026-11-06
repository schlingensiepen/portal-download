//! Random secrets, their hashes and their display strings.

use base64::Engine;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// A new random secret with a recognizable prefix, 256 bits of entropy.
pub fn generate(prefix: &str) -> String {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).expect("the operating system's random source is available");
    format!("{prefix}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}

/// The stored form of a secret: only its hash is kept.
pub fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// Eight lowercase characters derived from the hash; not secret, used to talk about a secret.
pub fn display_string(secret: &str) -> String {
    const ALPHA: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
    Sha256::digest(secret.as_bytes()).iter().take(8).map(|b| ALPHA[(*b as usize) % 26] as char).collect()
}

pub fn equal(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_secrets_differ_and_carry_the_prefix() {
        let a = generate("pdi_");
        let b = generate("pdi_");
        assert_ne!(a, b);
        assert!(a.starts_with("pdi_") && a.len() > 40);
        assert_eq!(display_string(&a).len(), 8);
        assert_ne!(hash(&a), a);
    }
}
