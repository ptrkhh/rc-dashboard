use super::cache::TranscriptCache;
use super::git::GitCache;
use super::usage::UsageCache;
use crate::host::cmd::Runner;
use crate::host::init::Host;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// What the dashboard knows about a session it launched itself. Mirrors the
/// values Node stored in `ctx.meta` (`lib/collect.js:137,149,234`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub rc_link: Option<String>,
}

/// The shared context every collect entry point takes. Mirrors the `ctx`
/// object built in `server.js:20-26`, plus the two caches and the `Runner`,
/// which in Node were module-level globals.
pub struct Ctx {
    pub host: Host,
    pub runner: Arc<Runner>,
    pub claude_dir: PathBuf,
    /// A second mount to report alongside `/`, e.g. `/mnt/d` (`server.js:22`).
    pub disk_extra: Option<String>,
    pub places_file: PathBuf,
    pub meta: Mutex<HashMap<String, Meta>>,
    pub purged: Mutex<HashSet<String>>,
    pub pin_lock: tokio::sync::Mutex<()>,
    pub transcripts: TranscriptCache,
    pub summaries: super::summaries::SummaryCache,
    pub git: Arc<GitCache>,
    /// Claude subscription limits, refreshed off the poll path.
    pub usage: Arc<UsageCache>,
    pub codex_usage: Arc<UsageCache>,
    pub codex_models: tokio::sync::Mutex<Option<(std::time::Instant, Vec<super::codex::ModelChoice>)>>,
    pub opencode_models: tokio::sync::Mutex<HashMap<String, (std::time::Instant, Vec<super::opencode::ModelChoice>)>>,
    pub opencode_threads: tokio::sync::Mutex<Option<(std::time::Instant, Vec<super::opencode::Thread>)>>,
    /// Codex's thread list as of the last read; see `codex::history`.
    pub codex_threads: tokio::sync::Mutex<Option<(std::time::Instant, Vec<super::codex::Thread>)>>,
    /// Set once at boot when `CDASH_AUTH` includes `password`. A `OnceLock`
    /// because `Ctx` is shared behind an `Arc` by the time the policy exists.
    pub password: std::sync::OnceLock<crate::auth::login::PasswordState>,
    /// One hub per chatted pane, started by the first `/api/chat` for it.
    pub chats: tokio::sync::Mutex<HashMap<String, crate::chat::hub::Hub>>,
    /// Sockets per session whose device is looking at it; see `FocusGuard`.
    pub focus: Mutex<HashMap<String, usize>>,
    /// Finished, failed and needs-an-answer notes from every hub.
    pub notes: tokio::sync::broadcast::Sender<crate::chat::notify::Note>,
    /// The port Claude's permission hook posts to, while that listener runs.
    pub hook_port: Option<u16>,
    /// Whether tmux has `new-session -e` (3.0+), once `tmux -V` answered; see `spawn::launch_hook_port`.
    pub tmux_env: tokio::sync::OnceCell<bool>,
}

impl Ctx {
    pub fn new(host: Host, claude_dir: PathBuf, disk_extra: Option<String>) -> Self {
        // `Host` owns a `Runner` too, but not behind an `Arc`, and the git
        // cache's background task needs one. Same resolved PATH, same log
        // buffer; only the log-once set is per-runner.
        let runner = Arc::new(Runner::new(host.path.clone(), Arc::clone(&host.log)));
        Self {
            places_file: claude_dir.join("cdash-places.json"),
            host,
            runner,
            claude_dir,
            disk_extra,
            meta: Mutex::new(HashMap::new()),
            purged: Mutex::new(HashSet::new()),
            pin_lock: tokio::sync::Mutex::new(()),
            transcripts: TranscriptCache::new(),
            summaries: super::summaries::SummaryCache::default(),
            git: Arc::new(GitCache::new()),
            usage: Arc::new(UsageCache::new()),
            codex_usage: Arc::new(UsageCache::new()),
            codex_models: tokio::sync::Mutex::new(None),
            opencode_models: tokio::sync::Mutex::new(HashMap::new()),
            opencode_threads: tokio::sync::Mutex::new(None),
            codex_threads: tokio::sync::Mutex::new(None),
            password: std::sync::OnceLock::new(),
            chats: tokio::sync::Mutex::new(HashMap::new()),
            focus: Mutex::new(HashMap::new()),
            notes: tokio::sync::broadcast::channel(64).0,
            hook_port: None,
            tmux_env: tokio::sync::OnceCell::new(),
        }
    }

    pub fn meta_get(&self, name: &str) -> Option<Meta> {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    /// Read-modify-write under a single lock. `meta_get` + `meta_set` is not a
    /// guard on a multi-threaded runtime: a `POST /api/kill` landing in the gap
    /// is re-inserted by the write.
    pub fn meta_update(&self, name: &str, f: impl FnOnce(&mut Meta)) -> bool {
        let mut g = self.meta.lock().unwrap_or_else(|e| e.into_inner());
        match g.get_mut(name) {
            Some(m) => {
                f(m);
                true
            }
            None => false,
        }
    }

    pub fn meta_set(&self, name: &str, m: Meta) {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string(), m);
    }

    pub fn meta_has(&self, name: &str) -> bool {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).contains_key(name)
    }

    pub fn meta_delete(&self, name: &str) {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).remove(name);
    }

    /// Is any connected device looking at this session right now?
    pub fn focused(&self, name: &str) -> bool {
        self.focus.lock().unwrap_or_else(|e| e.into_inner()).contains_key(name)
    }
}

/// One socket's vote in `Ctx::focus`. Counts at most once however often it is
/// set, and a dropped guard (closed or vanished socket) counts as off.
pub struct FocusGuard<'a> {
    ctx: &'a Ctx,
    name: &'a str,
    on: bool,
}

impl<'a> FocusGuard<'a> {
    pub fn new(ctx: &'a Ctx, name: &'a str) -> Self {
        Self { ctx, name, on: false }
    }

    pub fn set(&mut self, on: bool) {
        if on == self.on {
            return;
        }
        self.on = on;
        let mut m = self.ctx.focus.lock().unwrap_or_else(|e| e.into_inner());
        if on {
            *m.entry(self.name.to_string()).or_insert(0) += 1;
        } else if let Some(n) = m.get_mut(self.name) {
            *n -= 1;
            if *n == 0 {
                m.remove(self.name);
            }
        }
    }
}

impl Drop for FocusGuard<'_> {
    fn drop(&mut self) {
        self.set(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::tmux_io::tests::tempfile_path;

    #[test]
    fn focus_counts_each_connection_once_and_a_dropped_one_counts_as_off() {
        let ctx = crate::chat::claude::tests::ctx_with_path(tempfile_path("focus"), String::new());
        let (mut a, mut b) = (FocusGuard::new(&ctx, "cdash-a"), FocusGuard::new(&ctx, "cdash-a"));
        a.set(true);
        a.set(true);
        b.set(true);
        assert_eq!(ctx.focus.lock().unwrap().get("cdash-a"), Some(&2));
        a.set(false);
        assert!(ctx.focused("cdash-a"), "the other device still looks");
        drop(b);
        assert!(!ctx.focused("cdash-a") && !ctx.focus.lock().unwrap().contains_key("cdash-a"));
    }
}
