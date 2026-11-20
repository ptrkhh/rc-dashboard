use super::ctx::Ctx;
use super::fsio::read_tail;
use super::lookup::session_file_for;
use crate::host::proc::ProcRow;
use crate::parse::git::{parse_git_status, GitStatus};
use crate::parse::paths::project_dir_name;
use crate::parse::transcript::parse_transcript;
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

/// One entry of `/api/sessions` `running[]`. Field names and shapes are the
/// parity gate's contract — see the exemption list in the spec before changing
/// any of them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Session {
    pub name: String,
    #[serde(rename = "displayName", skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub dir: String,
    pub pid: i32,
    #[serde(rename = "uptimeSec")]
    pub uptime_sec: i64,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(rename = "nativeTerminal", skip_serializing_if = "Option::is_none")]
    pub native_terminal: Option<bool>,
    /// While this pane's folder maps to a live browser CDP port.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub browser: bool,
    #[serde(rename = "rcLink")]
    pub rc_link: Option<String>,
    #[serde(rename = "rcUnavailable", skip_serializing_if = "std::ops::Not::not")]
    pub rc_unavailable: bool,
    pub git: Option<GitStatus>,
    pub working: bool,
    #[serde(rename = "lastMessage")]
    pub last_message: Option<String>,
    pub sid: Option<String>,
    pub cpu: Option<f32>,
    #[serde(rename = "rssKb")]
    pub rss_kb: u64,
    /// Rust-only; Node has no such field. Exempted by name in the parity gate.
    #[serde(rename = "cpuSampleAgeMs")]
    pub cpu_sample_age_ms: u128,
    /// Node omitted this key entirely for pane sessions and set it to `true`
    /// for external ones. `skip_serializing_if` reproduces that exactly.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub external: bool,
    /// The agent can serve `/api/chat` for this card. Omitted when false.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub chat: bool,
}

/// The card's Working dot. Claude's own `status` when the session file has
/// one: `busy` in a turn, `shell` while a background shell outlives it. Unlike
/// chat's composer (`busy` only), anything but idle or waiting counts here.
/// Versions without `status` get the guess: a fresh transcript or background work.
pub(super) async fn claude_working(status: Option<&str>, transcript: Option<(&Path, f64)>, rows: &[ProcRow], pid: i32, now_ms: f64) -> bool {
    if let Some(s) = status {
        return !matches!(s, "idle" | "waiting");
    }
    let Some((file, mtime)) = transcript else { return false };
    now_ms - mtime < 10_000.0 || background_busy(file, rows, pid, now_ms).await
}

/// Claude's turn may be over while a subagent or a background shell still
/// works for it. Subagents log to `<sid>/subagents/*.jsonl` beside the main
/// transcript; background shells write stdout to `.../tasks/<id>.output`.
/// shortcut: shells are found through `/proc`, so macOS misses them; read `lsof` there if needed.
async fn background_busy(transcript: &Path, rows: &[ProcRow], pid: i32, now_ms: f64) -> bool {
    let mut tree = vec![pid];
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        tree.extend(rows.iter().filter(|r| r.ppid == parent).map(|r| r.pid));
        i += 1;
    }
    let task_output = |child: &i32| {
        std::fs::read_link(format!("/proc/{child}/fd/1")).is_ok_and(|out| {
            out.extension().is_some_and(|e| e == "output") && out.parent().is_some_and(|d| d.ends_with("tasks"))
        })
    };
    if tree[1..].iter().any(task_output) {
        return true;
    }
    let Ok(mut subagents) = tokio::fs::read_dir(transcript.with_extension("").join("subagents")).await else {
        return false;
    };
    while let Ok(Some(e)) = subagents.next_entry().await {
        if e.metadata().await.is_ok_and(|m| now_ms - super::cache::mtime_ms(&m) < 10_000.0) {
            return true;
        }
    }
    false
}

fn basename(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

/// Sessions this dashboard did not launch: every `~/.claude/sessions/<pid>.json`
/// whose pid still belongs to Claude. Read-only — they live in terminals we do not own.
pub async fn external_sessions(
    ctx: &Arc<Ctx>,
    rows: &[ProcRow],
    pane_pids: &HashSet<i32>,
    now_ms: f64,
) -> Vec<Session> {
    let alive: HashSet<i32> = rows
        .iter()
        .filter(|r| r.name == "claude" || r.name == "claude.exe")
        .map(|r| r.pid)
        .collect();
    let dir = ctx.claude_dir.join("sessions");
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return Vec::new();
    };

    let mut out = Vec::new();
    while let Ok(Some(e)) = entries.next_entry().await {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(pid) = p
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        // Headless claude processes of our own: `sdk-cli` is filtered below as well.
        if pane_pids.contains(&pid) || !alive.contains(&pid) || ctx.own_pids.lock().unwrap_or_else(|e| e.into_inner()).contains(&pid) {
            continue;
        }
        let Some(sess) = session_file_for(&ctx.claude_dir, pid).await else { continue };
        let (Some(sid), Some(cwd)) = (sess.session_id.clone(), sess.cwd.clone()) else { continue };
        // E1: 'cli' is a session someone is sitting in front of; 'sdk-cli' is
        // programmatic and not ours to show.
        if sess.entrypoint.as_deref() != Some("cli") {
            continue;
        }

        let file = ctx
            .claude_dir
            .join("projects")
            .join(project_dir_name(&cwd))
            .join(format!("{sid}.jsonl"));
        let md = tokio::fs::metadata(&file).await.ok();
        let mut last_message = None;
        if md.is_some() {
            if let Some(txt) = read_tail(&file).await {
                last_message = parse_transcript(&txt).last_assistant_text;
            }
        }
        let written = md.as_ref().map(|m| (file.as_path(), super::cache::mtime_ms(m)));
        let working = claude_working(sess.status.as_deref(), written, rows, pid, now_ms).await;

        let usage = {
            let mut s = ctx.host.sampler.lock().unwrap_or_else(|e| e.into_inner());
            s.tree_usage(pid)
        };
        let git_out =
            Arc::clone(&ctx.git).status_for(Arc::clone(&ctx.runner), &cwd, now_ms as u64);

        out.push(Session {
            display_name: super::lookup::custom_title(&ctx.claude_dir, &cwd, &sid).await,
            name: sess
                .name
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| basename(&cwd)),
            dir: cwd,
            pid,
            uptime_sec: sess
                .started_at
                .map(|t| (((now_ms - t) / 1000.0).round() as i64).max(0))
                .unwrap_or(0),
            model: None,
            effort: None,
            variant: None,
            provider: None,
            native_terminal: None,
            browser: false,
            rc_unavailable: false,
            rc_link: sess
                .bridge_session_id
                .map(|id| format!("https://claude.ai/code/{id}")),
            git: git_out.as_deref().map(parse_git_status),
            working,
            last_message,
            sid: Some(sid),
            cpu: usage.cpu,
            rss_kb: usage.rss_kb,
            cpu_sample_age_ms: usage.cpu_sample_age_ms,
            external: true,
            chat: false,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::log::LogBuffer;
    use std::path::PathBuf;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-ext-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        dir
    }

    fn ctx_for(claude_dir: PathBuf) -> Arc<Ctx> {
        let log = Arc::new(LogBuffer::new());
        let path = std::env::var("PATH").unwrap_or_default();
        let host = crate::host::init::Host {
            runner: crate::host::cmd::Runner::new(path.clone(), Arc::clone(&log)),
            log,
            path,
            sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
        };
        Arc::new(Ctx::new(host, claude_dir, None))
    }

    fn write_session(dir: &Path, pid: i32, body: &str) {
        std::fs::write(dir.join("sessions").join(format!("{pid}.json")), body).unwrap();
    }

    fn rows(pids: &[i32]) -> Vec<ProcRow> {
        pids.iter()
            .map(|p| ProcRow {
                pid: *p,
                ppid: 1,
                name: "claude".into(),
                cpu: 1.0,
                rss_kb: 100,
            })
            .collect()
    }

    #[tokio::test]
    async fn claude_status_decides_working_and_older_versions_fall_back_to_the_guess() {
        let t = Path::new("/no-such/s-1.jsonl");
        for (status, working) in [("busy", true), ("shell", true), ("idle", false), ("waiting", false)] {
            // A transcript written just now must not override Claude's own word.
            assert_eq!(claude_working(Some(status), Some((t, 1_000.0)), &[], 1, 1_000.0).await, working, "{status}");
        }
        assert!(claude_working(None, Some((t, 1_000.0)), &[], 1, 5_000.0).await, "no status: a fresh transcript");
        assert!(!claude_working(None, Some((t, 1_000.0)), &[], 1, 60_000.0).await, "no status: a quiet transcript");
        assert!(!claude_working(None, None, &[], 1, 1_000.0).await, "no status, no transcript");
    }

    #[tokio::test]
    async fn a_running_subagent_or_background_shell_keeps_the_session_busy() {
        let d = tempdir("busy");
        let transcript = d.join("s-1.jsonl");
        let now = super::super::cache::mtime_ms(&std::fs::metadata(&d).unwrap()) + 1_000.0;
        assert!(!background_busy(&transcript, &[], 1, now).await, "nothing running");

        std::fs::create_dir_all(d.join("s-1/subagents")).unwrap();
        std::fs::write(d.join("s-1/subagents/agent-a.jsonl"), "").unwrap();
        let now = super::super::cache::mtime_ms(&std::fs::metadata(d.join("s-1/subagents/agent-a.jsonl")).unwrap());
        assert!(background_busy(&transcript, &[], 1, now + 1_000.0).await, "a subagent wrote just now");
        assert!(!background_busy(&transcript, &[], 1, now + 60_000.0).await, "a quiet subagent is done");

        std::fs::create_dir_all(d.join("tasks")).unwrap();
        let ctx = ctx_for(d.clone());
        let out = d.join("tasks/b1.output").to_string_lossy().into_owned();
        let pid = ctx.runner.run("sh", &["-c", "sleep 5 >\"$0\" 2>&1 & echo $!", &out], "test shell").await;
        let pid: i32 = pid.trim().parse().unwrap();
        let tree = [ProcRow { pid, ppid: 1, name: "sleep".into(), cpu: 0.0, rss_kb: 0 }];
        let busy = background_busy(&transcript, &tree, 1, now + 60_000.0).await;
        ctx.runner.run("kill", &[&pid.to_string()], "test kill").await;
        assert!(busy, "a child writing to tasks/*.output is a background shell");
    }

    const CLI: &str = r#"{"sessionId":"s-1","cwd":"/proj","entrypoint":"cli","startedAt":1000,"name":"api"}"#;

    #[tokio::test]
    async fn a_live_cli_session_is_reported() {
        let d = tempdir("live");
        write_session(&d, 500, CLI);
        let out = external_sessions(&ctx_for(d), &rows(&[500]), &HashSet::new(), 61_000.0).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "api");
        assert_eq!(out[0].dir, "/proj");
        assert_eq!(out[0].sid.as_deref(), Some("s-1"));
        assert!(out[0].external);
        assert_eq!(out[0].uptime_sec, 60, "(61000 - 1000) / 1000");
    }

    #[tokio::test]
    async fn a_name_less_session_falls_back_to_the_directory_basename() {
        let d = tempdir("noname");
        write_session(&d, 501, r#"{"sessionId":"s","cwd":"/a/proj","entrypoint":"cli"}"#);
        let out = external_sessions(&ctx_for(d), &rows(&[501]), &HashSet::new(), 0.0).await;
        assert_eq!(out[0].name, "proj");
        assert_eq!(out[0].uptime_sec, 0, "no startedAt means zero, not a negative age");
    }

    #[tokio::test]
    async fn a_custom_title_overrides_the_directory_fallback() {
        let d = tempdir("custom-title");
        write_session(&d, 508, CLI);
        let titled = d.join("projects").join(project_dir_name("/proj")).join("s-1");
        std::fs::create_dir_all(&titled).unwrap();
        std::fs::write(
            titled.join("custom-title.json"),
            r#"{"customTitle":"[Claude] proj · renamed"}"#,
        )
        .unwrap();

        let out = external_sessions(&ctx_for(d), &rows(&[508]), &HashSet::new(), 0.0).await;
        assert_eq!(out[0].display_name.as_deref(), Some("[Claude] proj · renamed"));
    }

    #[tokio::test]
    async fn an_sdk_cli_session_is_excluded() {
        // E1: claude-mem observers and SDK runs are not sessions anyone is
        // sitting in front of. Showing them makes the list untrustworthy.
        let d = tempdir("sdk");
        write_session(&d, 502, r#"{"sessionId":"s","cwd":"/p","entrypoint":"sdk-cli"}"#);
        let out = external_sessions(&ctx_for(d), &rows(&[502]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_headless_claude_the_agent_started_is_not_reported() {
        let d = tempdir("own");
        write_session(&d, 505, CLI);
        let ctx = ctx_for(d);
        assert_eq!(external_sessions(&ctx, &rows(&[505]), &HashSet::new(), 0.0).await.len(), 1);
        ctx.own_pids.lock().unwrap().insert(505);
        assert!(external_sessions(&ctx, &rows(&[505]), &HashSet::new(), 0.0).await.is_empty());
    }

    #[tokio::test]
    async fn a_session_missing_its_entrypoint_is_excluded() {
        let d = tempdir("noentry");
        write_session(&d, 503, r#"{"sessionId":"s","cwd":"/p"}"#);
        let out = external_sessions(&ctx_for(d), &rows(&[503]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_dead_pid_is_excluded() {
        // E4: the session file outlives the process that wrote it.
        let d = tempdir("dead");
        write_session(&d, 504, CLI);
        let out = external_sessions(&ctx_for(d), &rows(&[999]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_recycled_pid_belonging_to_another_process_is_excluded() {
        let d = tempdir("recycled");
        write_session(&d, 504, CLI);
        let mut live = rows(&[504]);
        live[0].name = "bash".into();

        let out = external_sessions(&ctx_for(d), &live, &HashSet::new(), 0.0).await;

        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_pid_already_shown_as_a_pane_is_excluded() {
        // E3: without this the same session appears twice, once per source.
        let d = tempdir("dupe");
        write_session(&d, 505, CLI);
        let panes: HashSet<i32> = [505].into_iter().collect();
        let out = external_sessions(&ctx_for(d), &rows(&[505]), &panes, 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_session_without_session_id_or_cwd_is_excluded() {
        let d = tempdir("partial");
        write_session(&d, 506, r#"{"cwd":"/p","entrypoint":"cli"}"#);
        write_session(&d, 507, r#"{"sessionId":"s","entrypoint":"cli"}"#);
        let out = external_sessions(&ctx_for(d), &rows(&[506, 507]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn non_json_files_and_non_numeric_names_are_skipped() {
        // E11: the sessions directory is not guaranteed to hold only pid files.
        let d = tempdir("junk");
        std::fs::write(d.join("sessions/notes.txt"), "x").unwrap();
        std::fs::write(d.join("sessions/abc.json"), CLI).unwrap();
        let out = external_sessions(&ctx_for(d), &rows(&[1]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_missing_sessions_directory_yields_an_empty_list() {
        let dir = std::env::temp_dir().join(format!("cdash-ext-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = external_sessions(&ctx_for(dir), &rows(&[1]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }
}
