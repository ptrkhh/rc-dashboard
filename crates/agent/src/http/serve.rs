use super::routes;
use crate::auth::config::{AuthConfig, GuardKind};
use crate::auth::layer::{guard_mw, GuardState};
use crate::collect::ctx::Ctx;
use crate::host;
use axum::body::Body;
use axum::extract::OriginalUri;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

/// CDASH on a phone keypad. Chosen to be unmemorable to everything else: it is
/// below the 32768 ephemeral range, so the kernel never hands it out to a
/// random client socket, and clear of 3000/5000/8000/8080/8888, where every
/// other dev server already lives. `PORT` overrides it.
pub const DEFAULT_PORT: u16 = 23274;
/// The next number up: where Claude's permission hook posts. Loopback only,
/// and fixed, because panes outlive the agent. `CDASH_HOOK_PORT` overrides it.
pub const DEFAULT_HOOK_PORT: u16 = 23275;

pub struct Config {
    pub bind: IpAddr,
    pub port: u16,
    pub claude_dir: PathBuf,
    pub disk_extra: Option<String>,
    pub auth: Arc<AuthConfig>,
    /// Present exactly when `CDASH_AUTH` includes `password`. Built at boot so
    /// a misconfiguration is refused before anything listens.
    pub password: Option<crate::auth::login::PasswordState>,
    /// Where the hook listener binds on 127.0.0.1; `None` runs none.
    pub hook_port: Option<u16>,
}

crate::guarded_routes! {
    get "/api/sessions" => routes::get_sessions,
    get "/api/logs" => routes::get_logs,
    get "/api/places" => routes::get_places,
    get "/api/browse" => routes::get_browse,
    get "/api/hostinfo" => routes::get_hostinfo,
    get "/api/term" => routes::get_term,
    get "/api/chat" => routes::get_chat,
    get "/api/chat/media/{ref}" => routes::get_chat_media,
    get "/api/models" => routes::get_models,
    get "/api/notify" => routes::get_notify,
    get "/api/push/key" => routes::get_push_key,
    post "/api/push/subscribe" => routes::post_push_subscribe,
    post "/api/push/unsubscribe" => routes::post_push_unsubscribe,
    post "/api/favorites" => routes::post_favorites,
    post "/api/pin" => routes::post_pin,
    post "/api/launch" => routes::post_launch,
    post "/api/resume" => routes::post_resume,
    post "/api/kill" => routes::post_kill,
    post "/api/purge" => routes::post_purge,
    post "/api/logout" => routes::post_logout,
}

impl Config {
    /// The bind default is `127.0.0.1` — a breaking change from the Node agent,
    /// which bound every interface. Exposing the dangerous topology now takes a
    /// deliberate `CDASH_BIND=0.0.0.0`.
    /// A bad `CDASH_AUTH` is a boot error, surfaced by the caller. Returning it
    /// here rather than defaulting is the point: a typo must not open the
    /// origin.
    pub fn from_env() -> Result<Self, String> {
        let auth = crate::auth::config::config_from_env()?;
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        // Same principle as CDASH_AUTH below: a typo is refused, not defaulted.
        // `CDASH_BIND=0.0.0..1` silently becoming loopback also flips the
        // cookie policy `boot::decide` derives from this value.
        let bind: IpAddr = match std::env::var("CDASH_BIND") {
            Err(_) => "127.0.0.1".parse().expect("literal is a valid IP"),
            Ok(b) => b.parse().map_err(|_| format!("CDASH_BIND: not an IP address: {b:?}"))?,
        };

        let password = if auth.guards.contains(&crate::auth::config::GuardKind::Password) {
            let policy = crate::auth::boot::decide(
                std::env::var("CDASH_PASSWORD_HASH").ok().as_deref(),
                bind,
                std::env::var("CDASH_PUBLIC_URL").ok().as_deref(),
                std::env::var("CDASH_ALLOW_INSECURE_COOKIE").as_deref() == Ok("1"),
            )?;
            // A bound of 0 admits nothing: every login would 503 forever
            // while the page advises trying again shortly.
            let pending_max = match std::env::var("CDASH_LOGIN_PENDING_MAX") {
                Err(_) => crate::auth::throttle::DEFAULT_PENDING_MAX,
                Ok(v) => match v.parse::<usize>() {
                    Ok(n) if n > 0 => n,
                    _ => return Err(format!("CDASH_LOGIN_PENDING_MAX: not a positive integer: {v:?}")),
                },
            };
            Some(crate::auth::login::PasswordState::new(policy, pending_max))
        } else {
            None
        };

        Ok(Self {
            password,
            bind,
            port: match std::env::var("PORT") {
                Err(_) => DEFAULT_PORT,
                Ok(p) => p.parse().map_err(|_| format!("PORT: not a port number: {p:?}"))?,
            },
            claude_dir: std::env::var("CLAUDE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(&home).join(".claude")),
            disk_extra: std::env::var("DISK_EXTRA").ok().filter(|s| !s.is_empty()),
            auth: Arc::new(auth),
            hook_port: match std::env::var("CDASH_HOOK_PORT") {
                Err(_) => Some(DEFAULT_HOOK_PORT),
                Ok(p) => Some(p.parse().map_err(|_| format!("CDASH_HOOK_PORT: not a port number: {p:?}"))?),
            },
        })
    }
}

/// Serve an asset included in the agent binary. `no-cache` plus a content
/// ETag makes every cache in between (browser, Cloudflare) ask before reusing
/// a copy, so a page from a new build never runs with an old stylesheet, and
/// an unchanged asset costs a 304.
async fn embedded_asset(uri: OriginalUri, req: axum::http::HeaderMap) -> Response {
    let path = uri.0.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    // ponytail: fixed asset list must track public/; generate it in build.rs if it grows.
    let (content_type, body): (&'static str, &'static [u8]) = match path {
        "index.html" => (
            "text/html; charset=utf-8",
            include_bytes!("../../../../public/index.html"),
        ),
        "app.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../../../public/app.js"),
        ),
        "style.css" => (
            "text/css; charset=utf-8",
            include_bytes!("../../../../public/style.css"),
        ),
        "manifest.json" => (
            "application/manifest+json",
            include_bytes!("../../../../public/manifest.json"),
        ),
        "sw.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../../../public/sw.js"),
        ),
        "icon.svg" => (
            "image/svg+xml",
            include_bytes!("../../../../public/icon.svg"),
        ),
        "icon-light.svg" => (
            "image/svg+xml",
            include_bytes!("../../../../public/icon-light.svg"),
        ),
        "icon-32.png" => (
            "image/png",
            include_bytes!("../../../../public/icon-32.png"),
        ),
        "icon-light-32.png" => (
            "image/png",
            include_bytes!("../../../../public/icon-light-32.png"),
        ),
        "icon-192.png" => (
            "image/png",
            include_bytes!("../../../../public/icon-192.png"),
        ),
        "icon-512.png" => (
            "image/png",
            include_bytes!("../../../../public/icon-512.png"),
        ),
        "icon-maskable.png" => (
            "image/png",
            include_bytes!("../../../../public/icon-maskable.png"),
        ),
        "transport/backoff.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../../../public/transport/backoff.js"),
        ),
        "term/keys.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../../../public/term/keys.js"),
        ),
        "chat/md.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../../../public/chat/md.js"),
        ),
        "chat/store.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../../../public/chat/store.js"),
        ),
        "chat/cards.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../../../public/chat/cards.js"),
        ),
        "chat/menu.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../../../public/chat/menu.js"),
        ),
        "chat/view.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../../../public/chat/view.js"),
        ),
        "vendor/xterm.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../../../public/vendor/xterm.js"),
        ),
        "vendor/addon-fit.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../../../public/vendor/addon-fit.js"),
        ),
        "vendor/xterm.css" => (
            "text/css; charset=utf-8",
            include_bytes!("../../../../public/vendor/xterm.css"),
        ),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    use sha2::{Digest, Sha256};
    let etag = format!("\"{}\"", &format!("{:x}", Sha256::digest(body))[..32]);
    if req.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
    }
    let headers = [(header::CONTENT_TYPE, content_type.to_string()), (header::CACHE_CONTROL, "no-cache".into()), (header::ETAG, etag)];
    (headers, Body::from(body)).into_response()
}

/// What `serve` hands back. The address is the readiness signal — an in-process
/// caller awaits this future instead of polling `/api/health`.
pub struct Bound {
    pub addr: SocketAddr,
    pub ctx: Arc<Ctx>,
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    /// The hook listener's serving task, while it runs.
    hook: Option<tokio::task::JoinHandle<()>>,
}

impl Bound {
    /// Fire the graceful-shutdown trigger and wait for the serving tasks to
    /// drain and exit, after which both ports are released. Held hooks are
    /// answered empty on the trigger, so this stays prompt. The standalone
    /// binary never calls this; it exists so an in-process embedder can tear
    /// the server down.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
        if let Some(hook) = self.hook {
            let _ = hook.await;
        }
    }

    /// Whether the serving task has ended. An embedder that caches `addr` needs
    /// this: an accept loop that panicked leaves the address string valid-looking
    /// while every request to it is refused.
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

/// The unauthenticated exceptions, enumerated rather than implied — and the
/// list the router below is built from, so the two cannot disagree. A fourth
/// entry here without a matching arm registers nothing; a fourth route without
/// an entry here is unreachable.
pub const UNAUTH_PATHS: &[&str] = &["/api/health", "/login", "/api/login"];

/// Built in two halves: an unauthenticated router carrying exactly the
/// enumerated exceptions, and a guarded one carrying everything else — the
/// static file service included — with the guard applied as a layer over the
/// second before the two are merged. A route added to the guarded half cannot
/// escape the layer regardless of where it is written.
pub fn router(
    ctx: Arc<Ctx>,
    auth: Arc<AuthConfig>,
    password: Option<crate::auth::login::PasswordState>,
    cf: Option<Arc<crate::auth::cfaccess::CfState>>,
) -> Router {
    let st = GuardState { auth, log: Arc::clone(&ctx.host.log), password: password.clone(), cf };

    let guarded = guarded_router()
        .fallback_service(Router::new().fallback(get(embedded_asset)))
        .layer(axum::middleware::from_fn_with_state(st, guard_mw))
        .with_state(ctx);

    // Built by walking UNAUTH_PATHS rather than beside it: the list and the
    // router are one statement, so an exception cannot be added to the router
    // without appearing in the list the bypass test reads.
    let mut unauth = Router::new();
    for p in UNAUTH_PATHS {
        unauth = match (*p, password.clone()) {
            ("/api/health", _) => unauth
                .route(p, get(|| async { Json(serde_json::json!({ "ok": true })) })),
            // Exceptions 2 and 3 exist only under `password`: static HTML with
            // no host data, and a throttled login that carries none either.
            ("/login", Some(pw)) => {
                unauth.merge(Router::new().route(p, get(crate::auth::login::get_login)).with_state(pw))
            }
            ("/api/login", Some(pw)) => unauth.merge(
                Router::new()
                    .route(p, axum::routing::post(crate::auth::login::post_login))
                    .with_state(pw),
            ),
            _ => unauth,
        };
    }

    unauth.merge(guarded)
}

/// Bind, start serving on a background task, and return the bound address.
/// Errors rather than logging-and-aborting because the in-process caller has no
/// stderr to scrape and no child to have exited.
pub async fn serve(cfg: Config) -> std::io::Result<Bound> {
    // Fetched before binding: a cf-access origin that cannot verify anything
    // must fail as a service that did not start, with a named reason, rather
    // than as a successful SSO followed by 401 on every request.
    // Gated on the guard chain, not on the config being present. Leftover
    // CDASH_CF_* variables in a unit file must not couple boot to Cloudflare's
    // availability for a guard that is not in the chain — that turned a switch
    // to `CDASH_AUTH=none` behind a tunnel into an agent that exits 2 whenever
    // Cloudflare is unreachable.
    let cf = match cfg.auth.cf.clone() {
        Some(cfcfg) if cfg.auth.guards.contains(&GuardKind::CfAccess) => {
            let url = crate::auth::cfaccess::certs_url(&cfcfg.team_domain);
            let jwks = crate::auth::cfaccess::fetch_jwks(&url).await.map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("cf-access: could not fetch JWKS: {e}"),
                )
            })?;
            let cache = crate::auth::cfaccess::JwksCache::new();
            cache.install(jwks);
            Some(Arc::new(crate::auth::cfaccess::CfState { cfg: cfcfg, jwks: cache }))
        }
        _ => None,
    };

    let listener = tokio::net::TcpListener::bind((cfg.bind, cfg.port)).await?;
    let addr = listener.local_addr()?;

    // A taken hook port is not a boot error: Claude then launches without the hook.
    let hook_listener = match cfg.hook_port {
        Some(port) => Some(tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await.map_err(|e| (port, e))),
        None => None,
    };

    let h = host::init::init().await;
    let mut ctx = Ctx::new(h, cfg.claude_dir, cfg.disk_extra);
    let hook_listener = match hook_listener {
        Some(Ok(l)) => {
            ctx.hook_port = Some(l.local_addr()?.port());
            Some(l)
        }
        Some(Err((port, e))) => {
            ctx.host.log.push(format!("hook listener off: 127.0.0.1:{port}: {e}; Claude sessions launch without the hook"));
            None
        }
        None => None,
    };
    let ctx = Arc::new(ctx);
    if let Some(pw) = cfg.password.clone() {
        let _ = ctx.password.set(pw);
    }
    if let Some(cf) = cf.clone() {
        crate::auth::cfaccess::spawn_refresh(cf, Arc::clone(&ctx.host.log));
    }
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    // The notify streams end on this signal; see `routes::get_notify`.
    let app = router(Arc::clone(&ctx), Arc::clone(&cfg.auth), cfg.password.clone(), cf)
        .layer(axum::Extension(stop_rx.clone()));
    crate::chat::notify::spawn(Arc::clone(&ctx), stop_rx.clone());
    // ponytail: one sweeper per serve() call; an embedder that restarts the server in-process starts a second one, which is harmless.
    tokio::spawn(crate::chat::media::sweep_daily());
    let hook = hook_listener.map(|l| {
        let app = crate::chat::hook::router(Arc::clone(&ctx)).layer(axum::Extension(stop_rx.clone()));
        let mut stop_rx = stop_rx.clone();
        tokio::spawn(async move {
            // `ConnectInfo`: the hook route serves loopback peers only.
            let _ = axum::serve(l, app.into_make_service_with_connect_info::<SocketAddr>())
                .with_graceful_shutdown(async move {
                    let _ = stop_rx.changed().await;
                })
                .await;
        })
    });
    let task = tokio::spawn(async move {
        // `ConnectInfo` needs the peer address, which the trusted-proxy guard
        // checks against its allowlist.
        let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async move {
                let _ = stop_rx.changed().await;
            })
            .await;
    });

    Ok(Bound { addr, ctx, stop: stop_tx, task, hook })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-http-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        dir
    }

    /// An unreachable team domain: if the fetch were attempted, `serve` would
    /// return InvalidData and this would fail. Booting proves it was skipped.
    #[tokio::test]
    async fn cf_config_without_the_cf_access_guard_does_not_fetch_jwks() {
        let mut cfg = cfg_for(tempdir("cf-unused"));
        cfg.auth = Arc::new(
            AuthConfig::build_with_cf(
                vec![GuardKind::None],
                None,
                "X-Forwarded-Email".into(),
                vec![],
                Some(crate::auth::cfaccess::CfConfig {
                    team_domain: "https://team.cloudflareaccess.invalid".into(),
                    aud: "tag".into(),
                }),
            )
            .expect("none is always buildable"),
        );
        let b = serve(cfg).await.expect("a guard chain without cf-access must not need Cloudflare");
        b.stop().await;
    }

    pub(crate) fn cfg_for(dir: PathBuf) -> Config {
        Config {
            bind: "127.0.0.1".parse().unwrap(),
            port: 0, // let the OS choose, so tests never collide
            claude_dir: dir,
            disk_extra: None,
            auth: Arc::new(
                AuthConfig::build(
                    vec![crate::auth::config::GuardKind::None],
                    None,
                    "X-Forwarded-Email".into(),
                    vec![],
                )
                .expect("none is always buildable"),
            ),
            password: None,
            hook_port: None,
        }
    }

    /// Serve `app` on an ephemeral loopback port, with connect info as `serve` does.
    pub(crate) async fn serve_router(app: Router) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await;
        });
        addr
    }

    #[tokio::test]
    async fn stop_returns_while_a_notify_stream_is_open() {
        let b = serve(cfg_for(tempdir("stop-notify"))).await.unwrap();
        let res = reqwest::get(format!("http://{}/api/notify", b.addr)).await.unwrap();
        assert_eq!(res.status(), 200);
        tokio::time::timeout(std::time::Duration::from_secs(2), b.stop())
            .await
            .expect("stop returns within 2 s of an open /api/notify stream");
        drop(res);
    }

    #[tokio::test]
    async fn serve_returns_the_bound_address_rather_than_logging_it() {
        // This is what `server.js` could not do: it discarded the returned
        // Server and logged the `port` variable, so no test could find it.
        let b = serve(cfg_for(tempdir("bound"))).await.unwrap();
        assert_ne!(b.addr.port(), 0, "port 0 must resolve to a real port");
        assert!(b.addr.ip().is_loopback());
    }

    #[tokio::test]
    async fn health_answers_without_leaking_host_details() {
        let b = serve(cfg_for(tempdir("health"))).await.unwrap();
        let body = reqwest_get(&format!("http://{}/api/health", b.addr)).await;
        assert_eq!(body, "{\"ok\":true}");
    }

    #[tokio::test]
    async fn a_held_port_is_an_error_rather_than_a_panic() {
        let held = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = held.local_addr().unwrap().port();
        let mut cfg = cfg_for(tempdir("held"));
        cfg.port = port;
        let Err(e) = serve(cfg).await else {
            panic!("binding a held port must fail");
        };
        assert_eq!(e.kind(), std::io::ErrorKind::AddrInUse);
    }

    #[tokio::test]
    async fn the_hook_listener_has_its_own_port_and_a_taken_port_is_logged_not_fatal() {
        let mut cfg = cfg_for(tempdir("hook-on"));
        cfg.hook_port = Some(0);
        let b = serve(cfg).await.unwrap();
        let port = b.ctx.hook_port.expect("bound");
        assert_eq!(http_post(&format!("http://127.0.0.1:{port}/hook/claude?pane=bad"), "{}").await.0, 400);
        assert_ne!(http_post(&format!("http://{}/hook/claude?pane=bad", b.addr), "{}").await.0, 400, "not on the dashboard's port");
        b.stop().await;
        // What a desktop restart does right after stop.
        tokio::net::TcpListener::bind(("127.0.0.1", port)).await.expect("stop released the hook port");
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut cfg = cfg_for(tempdir("hook-taken"));
        cfg.hook_port = Some(held.local_addr().unwrap().port());
        let b = serve(cfg).await.expect("a taken hook port is not a boot error");
        assert_eq!(b.ctx.hook_port, None);
        assert!(b.ctx.host.log.lines().iter().any(|l| l.contains("hook listener off")));
    }

    #[tokio::test]
    async fn stop_releases_the_port_for_rebinding() {
        let b = serve(cfg_for(tempdir("stop"))).await.unwrap();
        let addr = b.addr;
        b.stop().await;
        // The old listener must be gone, not just the accept loop parked.
        // Rebinding the port would race parallel tests, which draw ephemeral
        // ports too; a refused connect cannot be taken from us. A connect that
        // succeeds (a self-connect, or a neighbour's listener) is retried.
        for _ in 0..300 {
            match tokio::net::TcpStream::connect(addr).await {
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => return,
                _ => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
        panic!("{addr} still accepts connections after stop");
    }

    /// `reqwest` used to be absent from this crate, which is why these helpers
    /// spoke HTTP down a raw `TcpStream`. cf-access's JWKS fetch made it a
    /// production dependency; the hand-rolled parser outlived its reason.
    pub(crate) async fn reqwest_get(url: &str) -> String {
        let (status, body) = http_get(url).await;
        assert_eq!(status, 200, "GET {url} returned {status}: {body}");
        body
    }

    /// Returns (status, body).
    pub(crate) async fn http_get(url: &str) -> (u16, String) {
        raw_request("GET", url, None).await
    }

    pub(crate) async fn http_post(url: &str, json: &str) -> (u16, String) {
        raw_request("POST", url, Some(json)).await
    }

    async fn raw_request(method: &str, url: &str, json: Option<&str>) -> (u16, String) {
        // No redirects: the guard answers navigations with 302 -> /login, and
        // following it would report 200 for a request that was refused.
        let c = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client builds");
        let m = reqwest::Method::from_bytes(method.as_bytes()).expect("test method is a token");
        let mut req = c.request(m, url);
        if let Some(b) = json {
            req = req.header("content-type", "application/json").body(b.to_string());
        }
        let res = req.send().await.expect("request reaches the test server");
        (res.status().as_u16(), res.text().await.unwrap_or_default())
    }
}
