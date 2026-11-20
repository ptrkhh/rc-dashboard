//! Rename a session by writing only the per-sid name store each tool already
//! reads. Bridge state files (`<pane>.json`/`.native`) are rewritten by live
//! bridges, so the sidecar is the one place a rename can stick.
use super::ctx::Ctx;
use super::fsio::{read_if, write_atomic, write_atomic_private};
use super::validate::{assert_opencode_sid, assert_path, assert_valid_sid, Refused};
use crate::parse::paths::project_dir_name;
use tokio::io::AsyncWriteExt;

fn bad(msg: &str) -> Refused {
    Refused::BadRequest(msg.into())
}

fn failed(e: std::io::Error) -> Refused {
    Refused::Failed(format!("Could not save the session name: {e}"))
}

pub async fn rename(ctx: &Ctx, tool: &str, sid: &str, dir: &str, name: &str) -> Result<(), Refused> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control) {
        return Err(bad("Name must be 1-120 characters without control characters"));
    }
    assert_path(dir)?;
    match tool {
        "claude" => {
            assert_valid_sid(sid)?;
            let project = ctx.claude_dir.join("projects").join(project_dir_name(dir));
            let transcript = project.join(format!("{sid}.jsonl"));
            if !transcript.is_file() {
                return Err(bad("No Claude transcript for this session"));
            }
            // What Claude Code's own rename does: one appended `custom-title`
            // line (its /resume list prefers it), then the sidecar the dashboard reads.
            // O_APPEND + one write_all: Claude may be appending concurrently.
            let line = serde_json::json!({ "type": "custom-title", "customTitle": name, "sessionId": sid });
            let mut fh = tokio::fs::OpenOptions::new().append(true).open(&transcript).await.map_err(failed)?;
            fh.write_all(format!("{line}\n").as_bytes()).await.map_err(failed)?;
            tokio::fs::create_dir_all(project.join(sid)).await.map_err(failed)?;
            let title = serde_json::json!({ "customTitle": name }).to_string();
            write_atomic(&project.join(sid).join("custom-title.json"), &title, ".cdash.tmp").await.map_err(failed)
        }
        "codex" => {
            assert_valid_sid(sid)?;
            let root = super::codex::root(ctx);
            tokio::fs::create_dir_all(&root).await.map_err(failed)?;
            write_atomic_private(&root.join(format!("{sid}.name")), name).await.map_err(failed)?;
            *ctx.codex_threads.lock().await = None;
            Ok(())
        }
        "opencode" => {
            assert_opencode_sid(sid)?;
            let file = super::opencode::root(ctx).join(format!("{sid}.owned.json"));
            let mut owned: serde_json::Value = read_if(&file)
                .await
                .and_then(|t| serde_json::from_str(&t).ok())
                .filter(|v: &serde_json::Value| v["sid"] == sid)
                .ok_or_else(|| bad("Not a dashboard-owned OpenCode session"))?;
            owned["display_name"] = name.into();
            write_atomic_private(&file, &owned.to_string()).await.map_err(failed)?;
            *ctx.opencode_threads.lock().await = None;
            Ok(())
        }
        _ => Err(bad("tool must be claude, codex, or opencode")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::lookup::custom_title;
    use crate::host::cmd::Runner;
    use crate::host::log::LogBuffer;
    use std::path::PathBuf;
    use std::sync::Arc;

    const SID: &str = "0123abcd-0123-4567-89ab-0123456789ab";
    const DIR: &str = "/home/u/proj";

    fn ctx(tag: &str) -> Ctx {
        let dir = std::env::temp_dir().join(format!("cdash-rename-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = Arc::new(LogBuffer::new());
        let host = crate::host::init::Host {
            runner: Runner::new(String::new(), Arc::clone(&log)),
            log,
            path: String::new(),
            sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
        };
        Ctx::new(host, dir, None)
    }

    fn transcript(ctx: &Ctx) -> PathBuf {
        ctx.claude_dir.join("projects").join(project_dir_name(DIR)).join(format!("{SID}.jsonl"))
    }

    #[tokio::test]
    async fn claude_rename_lands_in_transcript_and_custom_title_json() {
        let ctx = ctx("claude");
        std::fs::create_dir_all(transcript(&ctx).parent().unwrap()).unwrap();
        std::fs::write(transcript(&ctx), "{\"type\":\"user\"}\n").unwrap();
        rename(&ctx, "claude", SID, DIR, " Brainstorming: Rename Session ").await.unwrap();
        assert_eq!(custom_title(&ctx.claude_dir, DIR, SID).await.as_deref(), Some("Brainstorming: Rename Session"));
        let text = std::fs::read_to_string(transcript(&ctx)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "{\"type\":\"user\"}");
        let last: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(last, serde_json::json!({"type":"custom-title","customTitle":"Brainstorming: Rename Session","sessionId":SID}));
    }

    #[tokio::test]
    async fn claude_rename_without_transcript_is_refused_and_writes_nothing() {
        let ctx = ctx("claude-none");
        assert!(matches!(rename(&ctx, "claude", SID, DIR, "X").await, Err(Refused::BadRequest(_))));
        assert!(!ctx.claude_dir.join("projects").exists());
    }

    #[tokio::test]
    async fn codex_rename_writes_the_bare_name_sidecar() {
        let ctx = ctx("codex");
        rename(&ctx, "codex", SID, DIR, "Renamed").await.unwrap();
        let file = ctx.claude_dir.join("cdash-codex").join(format!("{SID}.name"));
        assert_eq!(std::fs::read_to_string(file).unwrap(), "Renamed");
    }

    #[tokio::test]
    async fn opencode_rename_keeps_owned_sid_and_dir() {
        let ctx = ctx("opencode");
        let root = ctx.claude_dir.join("cdash-opencode");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("ses_x1.owned.json"), r#"{"sid":"ses_x1","dir":"/p","display_name":"Old"}"#).unwrap();
        rename(&ctx, "opencode", "ses_x1", DIR, "Renamed").await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("ses_x1.owned.json")).unwrap()).unwrap();
        assert_eq!(v, serde_json::json!({"sid":"ses_x1","dir":"/p","display_name":"Renamed"}));
    }

    #[tokio::test]
    async fn opencode_rename_refuses_missing_or_mismatched_owned_json() {
        let ctx = ctx("opencode-bad");
        let root = ctx.claude_dir.join("cdash-opencode");
        assert!(matches!(rename(&ctx, "opencode", "ses_x1", DIR, "R").await, Err(Refused::BadRequest(_))));
        assert!(!root.join("ses_x1.owned.json").exists());
        std::fs::create_dir_all(&root).unwrap();
        let other = r#"{"sid":"ses_other","dir":"/p"}"#;
        std::fs::write(root.join("ses_x1.owned.json"), other).unwrap();
        assert!(matches!(rename(&ctx, "opencode", "ses_x1", DIR, "R").await, Err(Refused::BadRequest(_))));
        assert_eq!(std::fs::read_to_string(root.join("ses_x1.owned.json")).unwrap(), other);
    }

    #[tokio::test]
    async fn rename_rejects_blank_too_long_control_and_bad_sid() {
        let ctx = ctx("reject");
        for (tool, sid, dir, name) in [
            ("codex", SID, DIR, "  "),
            ("codex", SID, DIR, &"x".repeat(121)),
            ("codex", SID, DIR, "ok\u{7}"),
            ("codex", SID, "rel/dir", "ok"),
            ("codex", "nope", DIR, "ok"),
            ("opencode", "ses_x!", DIR, "ok"),
            ("vim", SID, DIR, "ok"),
        ] {
            assert!(matches!(rename(&ctx, tool, sid, dir, name).await, Err(Refused::BadRequest(_))), "{tool} {sid} {name:?}");
        }
        assert!(!ctx.claude_dir.join("cdash-codex").exists());
        rename(&ctx, "codex", SID, DIR, &"é".repeat(120)).await.unwrap();
    }
}
