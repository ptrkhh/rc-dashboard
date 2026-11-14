//! Web Push: the VAPID identity (RFC 8292).

use super::ece::b64;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

/// Contact claim: the project page, never a person.
pub const SUBJECT: &str = "https://github.com/ptrkhh/rc-dashboard";

/// The agent's P-256 signing key.
pub struct Vapid {
    pkcs8: Vec<u8>,
    public: Vec<u8>,
}

impl Vapid {
    pub fn generate() -> Result<Vapid, String> {
        let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
            .map_err(|_| "key generation failed")?;
        Self::load(doc.as_ref().to_vec())
    }

    pub fn from_pkcs8_b64(s: &str) -> Result<Vapid, String> {
        Self::load(super::ece::unb64(s).ok_or("bad base64")?)
    }

    fn load(pkcs8: Vec<u8>) -> Result<Vapid, String> {
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8, &SystemRandom::new())
            .map_err(|_| "bad VAPID key")?;
        Ok(Vapid { public: pair.public_key().as_ref().to_vec(), pkcs8 })
    }

    pub fn pkcs8_b64(&self) -> String {
        b64(&self.pkcs8)
    }

    /// The uncompressed 65-byte point, base64url.
    pub fn public_b64(&self) -> String {
        b64(&self.public)
    }

    pub fn jwt(&self, endpoint: &str, now: u64) -> Result<String, String> {
        let aud = reqwest::Url::parse(endpoint).map_err(|e| e.to_string())?.origin().ascii_serialization();
        let claims = serde_json::json!({"aud": aud, "exp": now + 12 * 3600, "sub": SUBJECT});
        jsonwebtoken::encode(&Header::new(Algorithm::ES256), &claims, &EncodingKey::from_ec_der(&self.pkcs8))
            .map_err(|e| e.to_string())
    }

    pub fn authorization(&self, endpoint: &str, now: u64) -> Result<String, String> {
        Ok(format!("vapid t={}, k={}", self.jwt(endpoint, now)?, self.public_b64()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ece::unb64;
    use serde_json::{json, Value};

    #[test]
    fn the_vapid_token_is_es256_for_the_push_origin_and_names_the_project() {
        let v = Vapid::generate().unwrap();
        assert_eq!(Vapid::from_pkcs8_b64(&v.pkcs8_b64()).unwrap().public_b64(), v.public_b64(), "the stored key reloads");
        let jwt = v.jwt("https://fcm.googleapis.com/fcm/send/abc:def", 1_800_000_000).unwrap();
        let part = |i: usize| -> Value { serde_json::from_slice(&unb64(jwt.split('.').nth(i).unwrap()).unwrap()).unwrap() };
        assert_eq!(part(0), json!({"typ":"JWT","alg":"ES256"}));
        assert_eq!(part(1), json!({"aud":"https://fcm.googleapis.com","exp":1_800_000_000u64 + 12 * 3600,"sub":SUBJECT}));
        let (signed, sig) = jwt.rsplit_once('.').unwrap();
        ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, unb64(&v.public_b64()).unwrap())
            .verify(signed.as_bytes(), &unb64(sig).unwrap()).expect("verifies against the advertised key");
        assert!(!jwt.contains('@'));
        let auth = v.authorization("https://fcm.googleapis.com/x", 1).unwrap();
        assert!(auth.starts_with("vapid t="));
        assert!(auth.ends_with(&format!(", k={}", v.public_b64())));
    }
}
