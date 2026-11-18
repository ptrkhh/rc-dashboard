//! Web Push: the VAPID identity (RFC 8292) and the subscription store.

use super::ece::b64;
use super::notify::{Note, NoteKind};
use crate::collect::ctx::Ctx;
use crate::collect::fsio::write_atomic_private;
use serde::{Deserialize, Serialize};
use std::io;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use ring::rand::SystemRandom;
use sha2::{Digest, Sha256};
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

/// One browser or app registration (`PushSubscription.toJSON()` fields).
#[derive(Clone, Serialize, Deserialize)]
pub struct Sub {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub added: u64,
}

#[derive(Serialize, Deserialize)]
struct Store {
    vapid: String,
    subs: Vec<Sub>,
}

/// Held across every read-modify-write of `cdash-push.json`.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn file(ctx: &Ctx) -> std::path::PathBuf {
    ctx.claude_dir.join("cdash-push.json")
}

/// The stored state, or a fresh one (new key, no subscriptions) when the file
/// is missing. An unreadable or unparsable file is an error, never replaced:
/// a new key would orphan every subscribed device. Caller holds `LOCK`.
async fn load(ctx: &Ctx) -> io::Result<(Store, Vapid)> {
    let text = match tokio::fs::read_to_string(file(ctx)).await {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let v = Vapid::generate().map_err(io::Error::other)?;
            let store = Store { vapid: v.pkcs8_b64(), subs: vec![] };
            save(ctx, &store).await?;
            return Ok((store, v));
        }
        Err(e) => return Err(e),
    };
    let store: Store = serde_json::from_str(&text).map_err(io::Error::other)?;
    let v = Vapid::from_pkcs8_b64(&store.vapid).map_err(io::Error::other)?;
    Ok((store, v))
}

async fn save(ctx: &Ctx, store: &Store) -> io::Result<()> {
    write_atomic_private(&file(ctx), &serde_json::to_string(store).map_err(io::Error::other)?).await
}

/// The signing key, made on first use. Shared by `key` and `send`.
pub async fn vapid(ctx: &Ctx) -> io::Result<Vapid> {
    let _g = LOCK.lock().await;
    Ok(load(ctx).await?.1)
}

/// The public key a client subscribes with.
pub async fn key(ctx: &Ctx) -> io::Result<String> {
    Ok(vapid(ctx).await?.public_b64())
}

/// Upsert by endpoint: one entry per device.
pub async fn add(ctx: &Ctx, sub: Sub) -> io::Result<()> {
    let _g = LOCK.lock().await;
    let (mut store, _) = load(ctx).await?;
    store.subs.retain(|s| s.endpoint != sub.endpoint);
    store.subs.push(sub);
    save(ctx, &store).await
}

pub async fn remove(ctx: &Ctx, endpoint: &str) -> io::Result<()> {
    let _g = LOCK.lock().await;
    let (mut store, _) = load(ctx).await?;
    store.subs.retain(|s| s.endpoint != endpoint);
    save(ctx, &store).await
}

/// Empty when the file is missing or unreadable.
pub async fn subscriptions(ctx: &Ctx) -> Vec<Sub> {
    let _g = LOCK.lock().await;
    match tokio::fs::read_to_string(file(ctx)).await {
        Ok(t) => serde_json::from_str::<Store>(&t).map(|s| s.subs).unwrap_or_default(),
        Err(_) => vec![],
    }
}

/// The one HTTP client for push services: 10 s timeout, no redirects.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_default()
}

/// One push to every subscription, concurrently: a hanging device delays no
/// other. 404 and 410 drop the subscription. Logs name only the host.
pub async fn send(ctx: &Ctx, http: &reqwest::Client, note: &Note) {
    let subs = subscriptions(ctx).await;
    if subs.is_empty() {
        return;
    }
    let Ok(vapid) = vapid(ctx).await else {
        ctx.host.log.push("push failed: no signing key");
        return;
    };
    let vapid = std::sync::Arc::new(vapid);
    let payload = note.payload();
    let topic = b64(&Sha256::digest(note.session.as_bytes()))[..32].to_string();
    let urgency = if note.kind == NoteKind::Done { "normal" } else { "high" };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut set = tokio::task::JoinSet::new();
    for sub in subs {
        let (http, vapid, payload, topic) = (http.clone(), vapid.clone(), payload.clone(), topic.clone());
        set.spawn(async move {
            let host = reqwest::Url::parse(&sub.endpoint).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
            let sent = async {
                let ua = super::ece::unb64(&sub.p256dh).ok_or("bad p256dh")?;
                let auth = super::ece::unb64(&sub.auth).ok_or("bad auth")?;
                let body = super::ece::encrypt(&ua, &auth, payload.as_bytes())?;
                let res = http
                    .post(&sub.endpoint)
                    .header("Authorization", vapid.authorization(&sub.endpoint, now)?)
                    .header("Content-Encoding", "aes128gcm")
                    .header("TTL", "86400")
                    .header("Urgency", urgency)
                    .header("Topic", &topic)
                    .body(body)
                    .send()
                    .await
                    .map_err(|_| "unreachable".to_string())?;
                Ok::<_, String>(res.status())
            }
            .await;
            (sub.endpoint, host, sent)
        });
    }
    while let Some(done) = set.join_next().await {
        let Ok((endpoint, host, sent)) = done else { continue };
        match sent {
            Ok(s) if s.is_success() => {}
            Ok(s) => {
                if s.as_u16() == 404 || s.as_u16() == 410 {
                    let _ = remove(ctx, &endpoint).await;
                }
                ctx.host.log.push(format!("push failed {} {host}", s.as_u16()));
            }
            Err(e) => ctx.host.log.push(format!("push failed {e} {host}")),
        }
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
