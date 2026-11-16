//! Desktop notifications. The page asks for them (`notify_set`); a watcher task
//! then reads the agent's `/api/notify` event stream and shows each note as a
//! system notification, whose tap opens that chat in the main window.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

pub const FIRST_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// A line is at most this long (the agent's own limit on a socket line).
const LINE_MAX: usize = 64 * 1024;

#[derive(Deserialize, PartialEq, Debug)]
pub struct WireNote {
    pub s: String,
    pub p: String,
    pub k: String,
}

/// Splits the event stream into notes: `data: {json}` lines, across chunk
/// boundaries; comments, blank lines and lines that do not parse are dropped.
#[derive(Default)]
pub struct Lines(Vec<u8>);

impl Lines {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<WireNote> {
        self.0.extend_from_slice(chunk);
        let mut notes = Vec::new();
        while let Some(i) = self.0.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.0.drain(..=i).collect();
            let line = String::from_utf8_lossy(&line);
            if let Some(json) = line.trim_end_matches(['\r', '\n']).strip_prefix("data:") {
                if let Ok(n) = serde_json::from_str(json.trim_start()) {
                    notes.push(n);
                }
            }
        }
        if self.0.len() > LINE_MAX {
            self.0.clear();
        }
        notes
    }
}

pub fn text_for(kind: &str) -> Option<&'static str> {
    match kind {
        "done" => Some("Finished"),
        "error" => Some("Stopped with an error"),
        "ask" => Some("Needs your answer"),
        _ => None,
    }
}

pub fn next_backoff(d: Duration) -> Duration {
    (d * 2).min(MAX_BACKOFF)
}

/// A stable positive id per session, so a new note replaces the old one.
pub fn note_id(session: &str) -> i32 {
    let h = Sha256::digest(session.as_bytes());
    (u32::from_be_bytes([h[0], h[1], h[2], h[3]]) & 0x7fff_ffff) as i32
}

/// The `reg.exe` calls that teach Windows the app's toast identity, or forget it.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn aumid_commands(on: bool, icon: &str) -> Vec<Vec<String>> {
    let key = r"HKCU\Software\Classes\AppUserModelId\dev.cdash.app";
    let args = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    if !on {
        return vec![args(&["delete", key, "/f"])];
    }
    let add = |name: &str, value: &str| args(&["add", key, "/v", name, "/t", "REG_SZ", "/d", value, "/f"]);
    vec![add("DisplayName", "RC Dashboard"), add("IconUri", icon)]
}

#[cfg(windows)]
#[allow(clippy::disallowed_types)] // reg.exe on the Windows client, which links no agent and so has no Runner
pub fn register_aumid(app: &AppHandle, on: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut icon = String::new();
    if on {
        let path = app.path().app_data_dir().map_err(|e| e.to_string())?.join("notify-icon.png");
        std::fs::create_dir_all(path.parent().unwrap_or(&path)).map_err(|e| e.to_string())?;
        std::fs::write(&path, include_bytes!("../icons/icon.png")).map_err(|e| e.to_string())?;
        icon = path.to_string_lossy().into_owned();
    }
    let reg = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
        .join(r"System32\reg.exe");
    for args in aumid_commands(on, &icon) {
        let ok = std::process::Command::new(&reg)
            .args(&args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| e.to_string())?
            .status
            .success();
        // ponytail: any failed delete counts as "key not found" (reg.exe's message is localised)
        if !ok && on {
            return Err("cannot register the app for notifications".into());
        }
    }
    Ok(())
}

#[derive(Default)]
struct Inner {
    watcher: Option<tauri::async_runtime::JoinHandle<()>>,
    shown: HashMap<i32, (String, String)>,
    latest: Option<(String, String)>,
}

#[derive(Default)]
pub struct NotifyState(Mutex<Inner>);

impl NotifyState {
    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Which chat a tap means: the notification's own extras, else its id, else the latest note.
    fn pick(&self, extra: Option<(String, String)>, id: Option<i32>) -> Option<(String, String)> {
        let g = self.inner();
        extra.or_else(|| id.and_then(|i| g.shown.get(&i).cloned())).or_else(|| g.latest.clone())
    }

    fn record(&self, id: i32, s: &str, p: &str) {
        let mut g = self.inner();
        g.shown.insert(id, (s.to_owned(), p.to_owned()));
        g.latest = Some((s.to_owned(), p.to_owned()));
    }

    pub fn clear(&self, id: i32) {
        self.inner().shown.remove(&id);
    }

    pub fn stop(&self) {
        if let Some(h) = self.inner().watcher.take() {
            h.abort();
        }
    }

    /// Restarts the watcher if one is running (the active profile changed).
    pub fn restart(&self, app: AppHandle) {
        if self.inner().watcher.is_some() {
            self.start(app);
        }
    }

    pub fn start(&self, app: AppHandle) {
        let mut g = self.inner();
        if let Some(h) = g.watcher.take() {
            h.abort();
        }
        g.watcher = Some(tauri::async_runtime::spawn(watch(app)));
    }
}

/// Raises the main window and hands the chat to the page.
fn open_chat(app: &AppHandle, s: &str, p: &str) {
    let Some(w) = app.get_webview_window("main") else { return };
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
    let _ = w.eval(format!("window.cdashOpenChat&&window.cdashOpenChat({},{})", serde_json::json!(s), serde_json::json!(p)));
}

/// Call once from `setup`: a tap on a notification opens its chat.
pub fn on_tap(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.notification().on_action(move |a| {
        if a.action_id() != "tap" {
            return;
        }
        let n = a.notification();
        let text = |k: &str| n.and_then(|n| n.extra().get(k)).and_then(|v| v.as_str()).map(str::to_owned);
        let extra = text("s").map(|s| (s, text("p").unwrap_or_default()));
        if let Some((s, p)) = handle.state::<NotifyState>().pick(extra, n.map(|n| n.id())) {
            open_chat(&handle, &s, &p);
        }
    });
}

fn show(app: &AppHandle, n: &WireNote) {
    let Some(body) = text_for(&n.k) else { return };
    let id = note_id(&n.s);
    app.state::<NotifyState>().record(id, &n.s, &n.p);
    let nt = app.notification();
    let _ = nt.remove_active(vec![id]); // replaces the old note on Linux; a no-op elsewhere
    let _ = nt.builder().id(id).title(&n.p).body(body).extra("s", &n.s).extra("p", &n.p).show();
}

/// Reads one `/api/notify` stream to its end. `Ok(true)` once the agent answered 200.
async fn read(app: &AppHandle, client: &reqwest::Client, ok: &mut bool) -> Result<(), String> {
    let base = crate::current_base(app, &app.state::<crate::ServerState>())?;
    let mut res = client.get(format!("{base}/api/notify")).send().await.map_err(|e| e.to_string())?;
    if res.status() != 200 {
        return Err(format!("notify stream: {}", res.status()));
    }
    *ok = true;
    let mut lines = Lines::default();
    while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
        for n in lines.push(&chunk) {
            show(app, &n);
        }
    }
    Ok(())
}

async fn watch(app: AppHandle) {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .read_timeout(Duration::from_secs(60))
        .build()
        .expect("a client with no TLS roots to load always builds");
    let mut backoff = FIRST_BACKOFF;
    loop {
        let mut ok = false;
        let _ = read(&app, &client, &mut ok).await;
        if ok {
            backoff = FIRST_BACKOFF;
        }
        tokio::time::sleep(backoff).await;
        backoff = next_backoff(backoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_lines_become_notes_across_chunk_boundaries() {
        let mut l = Lines::default();
        let a = l.push(b"data: {\"s\":\"cdash-a\",\"p\":\"proj\",\"k\":\"done\",\"t\":1}\n\n: keep\n\ndata: {\"s\":\"cd");
        assert_eq!(a, [WireNote { s: "cdash-a".into(), p: "proj".into(), k: "done".into() }]);
        let b = l.push(b"ash-b\",\"p\":\"q\",\"k\":\"ask\",\"t\":2}\r\n\r\ndata: not json\n\n");
        assert_eq!(b.iter().map(|n| n.s.as_str()).collect::<Vec<_>>(), ["cdash-b"]);
    }

    #[test]
    fn texts_backoff_and_ids() {
        assert_eq!(
            ["done", "error", "ask", "later"].map(text_for),
            [Some("Finished"), Some("Stopped with an error"), Some("Needs your answer"), None]
        );
        assert_eq!(
            (FIRST_BACKOFF, next_backoff(FIRST_BACKOFF), next_backoff(Duration::from_secs(200))),
            (Duration::from_secs(5), Duration::from_secs(10), Duration::from_secs(300))
        );
        assert_eq!(note_id("cdash-a"), note_id("cdash-a"));
        assert_ne!(note_id("cdash-a"), note_id("cdash-b"));
        assert!(note_id("cdash-a") >= 0);
    }

    #[test]
    fn windows_learns_the_app_id_when_on_and_forgets_it_when_off() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let key = r"HKCU\Software\Classes\AppUserModelId\dev.cdash.app";
        let icon = r"C:\Users\o\AppData\Roaming\dev.cdash.app\notify-icon.png";
        assert_eq!(
            aumid_commands(true, icon),
            [
                s(&["add", key, "/v", "DisplayName", "/t", "REG_SZ", "/d", "RC Dashboard", "/f"]),
                s(&["add", key, "/v", "IconUri", "/t", "REG_SZ", "/d", icon, "/f"])
            ]
        );
        assert_eq!(aumid_commands(false, ""), [s(&["delete", key, "/f"])]);
    }

    #[test]
    fn a_tap_prefers_extras_then_id_then_latest() {
        let st = NotifyState::default();
        let mut g = st.inner();
        g.shown.insert(7, ("cdash-a".into(), "pa".into()));
        g.latest = Some(("cdash-z".into(), "pz".into()));
        drop(g);
        let pair = |s: &str, p: &str| Some((s.to_string(), p.to_string()));
        assert_eq!(st.pick(pair("x", "y"), Some(7)), pair("x", "y"));
        assert_eq!(st.pick(None, Some(7)), pair("cdash-a", "pa"));
        assert_eq!(st.pick(None, Some(8)), pair("cdash-z", "pz"));
    }
}
