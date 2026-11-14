//! RFC 8291 Web Push encryption (`aes128gcm`, RFC 8188), one record per message.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::{aead, agreement, hkdf, rand::{SecureRandom, SystemRandom}};

/// Record size announced in the header; one record always fits a push payload.
const RS: u32 = 4096;

/// Base64url without padding.
pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64(s: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(s).ok()
}

struct Len(usize);
impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn expand(prk: &hkdf::Prk, info: &[&[u8]], out: &mut [u8]) {
    // Lengths are fixed and far below the HKDF limit, so this cannot fail.
    prk.expand(info, Len(out.len())).and_then(|o| o.fill(out)).expect("hkdf expand");
}

/// The content-encryption key and nonce.
pub(crate) fn derive(
    ecdh_secret: &[u8],
    auth: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    salt: &[u8; 16],
) -> ([u8; 16], [u8; 12]) {
    let key = hkdf::Salt::new(hkdf::HKDF_SHA256, auth).extract(ecdh_secret);
    let mut ikm = [0u8; 32];
    expand(&key, &[b"WebPush: info\0", ua_public, as_public], &mut ikm);
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
    let (mut cek, mut nonce) = ([0u8; 16], [0u8; 12]);
    expand(&prk, &[b"Content-Encoding: aes128gcm\0"], &mut cek);
    expand(&prk, &[b"Content-Encoding: nonce\0"], &mut nonce);
    (cek, nonce)
}

pub(crate) fn encrypt_with(
    ecdh_secret: &[u8],
    auth: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    salt: &[u8; 16],
    plaintext: &[u8],
) -> Vec<u8> {
    let (cek, nonce) = derive(ecdh_secret, auth, ua_public, as_public, salt);
    let mut out = Vec::with_capacity(86 + plaintext.len() + 17);
    out.extend_from_slice(salt);
    out.extend_from_slice(&RS.to_be_bytes());
    out.push(as_public.len() as u8);
    out.extend_from_slice(as_public);
    let mut record = plaintext.to_vec();
    record.push(2); // last-record delimiter
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).expect("16-byte key"));
    key.seal_in_place_append_tag(aead::Nonce::assume_unique_for_key(nonce), aead::Aad::empty(), &mut record)
        .expect("seal");
    out.extend_from_slice(&record);
    out
}

/// Encrypt `plaintext` to a push subscription's `p256dh` and `auth`.
pub fn encrypt(ua_public: &[u8], auth: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    if plaintext.len() > (RS as usize) - 17 {
        return Err("payload too large".into());
    }
    let rng = SystemRandom::new();
    let mut salt = [0u8; 16];
    rng.fill(&mut salt).map_err(|_| "no random bytes")?;
    let private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).map_err(|_| "key generation failed")?;
    let as_public = private.compute_public_key().map_err(|_| "public key failed")?;
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, ua_public);
    let as_public = as_public.as_ref().to_vec();
    agreement::agree_ephemeral(private, &peer, |secret| {
        encrypt_with(secret, auth, ua_public, &as_public, &salt, plaintext)
    })
    .map_err(|_| "bad subscription key".to_string())
}

#[cfg(test)]
pub(crate) fn decrypt(ua_private: agreement::EphemeralPrivateKey, ua_public: &[u8], auth: &[u8], body: &[u8]) -> Vec<u8> {
    let salt: [u8; 16] = body[..16].try_into().unwrap();
    let as_public = &body[21..86];
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, as_public);
    let (cek, nonce) = agreement::agree_ephemeral(ua_private, &peer, |s| derive(s, auth, ua_public, as_public, &salt)).unwrap();
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).unwrap());
    let mut record = body[86..].to_vec();
    let plain = key.open_in_place(aead::Nonce::assume_unique_for_key(nonce), aead::Aad::empty(), &mut record).unwrap();
    let end = plain.iter().rposition(|&b| b != 0).unwrap();
    assert_eq!(plain[end], 2, "last-record delimiter");
    plain[..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::agreement::{EphemeralPrivateKey, ECDH_P256};

    #[test]
    fn rfc_8291_appendix_a() {
        let u = |s: &str| unb64(s).unwrap();
        let (ua, asp, auth) = (u("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4"),
            u("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8"), u("BTBZMqHH6r4Tts7J_aSIgg"));
        let salt: [u8; 16] = u("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let ecdh = u("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs");
        let (cek, nonce) = derive(&ecdh, &auth, &ua, &asp, &salt);
        assert_eq!((b64(&cek), b64(&nonce)), ("oIhVW04MRdy2XN9CiKLxTg".to_string(), "4h_95klXJ5E_qnoN".to_string()));
        let body = encrypt_with(&ecdh, &auth, &ua, &asp, &salt, b"When I grow up, I want to be a watermelon");
        assert_eq!(b64(&body), "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN");
    }

    #[test]
    fn a_message_encrypted_to_a_fresh_key_decrypts_with_it() {
        let rng = ring::rand::SystemRandom::new();
        let ua = EphemeralPrivateKey::generate(&ECDH_P256, &rng).unwrap();
        let ua_public = ua.compute_public_key().unwrap().as_ref().to_vec();
        let body = encrypt(&ua_public, &[7u8; 16], br#"{"s":"cdash-a"}"#).unwrap();
        assert_eq!(body.len(), 86 + 15 + 1 + 16, "one record: header, text, delimiter, tag");
        assert_eq!(decrypt(ua, &ua_public, &[7u8; 16], &body), br#"{"s":"cdash-a"}"#);
    }
}
