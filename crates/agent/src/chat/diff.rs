//! The Changes sheet: the session folder's diff against HEAD.
//!
//! The folder comes from a transcript, so it is untrusted: it must be an
//! absolute existing directory, and git runs with every repo-config hook that
//! could execute code switched off (fsmonitor, hooks, external diff, textconv).

use crate::collect::git::GIT_TIMEOUT;
use crate::host::cmd::Runner;
use serde_json::{json, Value};

pub const DIFF_MAX: usize = 2 * 1024 * 1024;
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// Prefix of every git call. Repo config must never run code.
const SAFE: [&str; 6] = ["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-c", "core.pager=cat"];
/// Prefix of every diff.
const D: [&str; 4] = ["diff", "--no-color", "--no-ext-diff", "--no-textconv"];

fn d<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    D.iter().copied().chain(["--no-renames", "--relative"]).chain(extra.iter().copied()).collect()
}

fn none(folder: &str, repo: bool) -> Value {
    json!({ "folder": folder, "repo": repo, "files": [], "truncated": false })
}

pub async fn diff(runner: &Runner, folder: &str) -> Value {
    if !std::path::Path::new(folder).is_absolute() || !std::path::Path::new(folder).is_dir() {
        return none(folder, false);
    }
    let key = format!("git {folder}");
    let git = |rest: &[&str]| -> Vec<String> {
        ["-C", folder].into_iter().chain(SAFE).chain(rest.iter().copied()).map(String::from).collect()
    };
    let small = |rest: &[&str]| {
        let args = git(rest);
        let key = key.clone();
        async move {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            runner.run_checked_with_timeout("git", &a, &key, GIT_TIMEOUT).await
        }
    };
    let capped = |rest: &[&str], cap: usize| {
        let args = git(rest);
        let key = key.clone();
        async move {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            runner.run_capped("git", &a, &key, GIT_TIMEOUT, cap).await
        }
    };
    if small(&["rev-parse", "--is-inside-work-tree"]).await.map(|o| o.trim() == "true") != Ok(true) {
        return none(folder, false);
    }
    let base = if small(&["rev-parse", "--verify", "-q", "HEAD"]).await.is_ok() { "HEAD" } else { EMPTY_TREE };
    let with_base = |extra: &[&'static str]| -> Vec<&str> { d(extra).into_iter().chain([base]).collect() };

    let status = small(&with_base(&["--name-status", "-z"])).await.unwrap_or_default();
    let counts = small(&with_base(&["--numstat", "-z"])).await.unwrap_or_default();
    let mut st = status.split('\0');
    let mut cs = counts.split('\0');
    let mut files: Vec<Value> = Vec::new();
    while let (Some(s), Some(path)) = (st.next(), st.next()) {
        let (a, del) = match cs.next().map(|l| l.splitn(3, '\t').collect::<Vec<_>>()) {
            Some(p) if p.len() == 3 => (p[0].parse::<u64>().ok(), p[1].parse::<u64>().ok()),
            _ => (None, None),
        };
        let status = match s { "A" => "added", "D" => "deleted", _ => "modified" };
        files.push(json!({ "path": path, "status": status, "additions": a, "deletions": del, "patch": null }));
    }

    let mut truncated = false;
    let mut used = 0;
    if !files.is_empty() {
        if let Ok(c) = capped(&with_base(&[]), DIFF_MAX).await {
            truncated = c.truncated;
            let mut sections = split_sections(&c.out);
            if truncated {
                sections.pop(); // the last one is cut off
            }
            used = c.out.len();
            for (f, s) in files.iter_mut().zip(sections) {
                f["patch"] = json!(s);
            }
        } else {
            truncated = true;
        }
    }

    let untracked = small(&["ls-files", "--others", "--exclude-standard", "-z"]).await.unwrap_or_default();
    // ponytail: one git per untracked file; batch them if a folder with thousands of untracked files makes the sheet slow.
    for path in untracked.split('\0').filter(|p| !p.is_empty() && !p.ends_with('/')) {
        let mut row = json!({ "path": path, "status": "untracked", "additions": null, "deletions": null, "patch": null });
        let remaining = DIFF_MAX.saturating_sub(used);
        if remaining == 0 {
            truncated = true;
        } else {
            let args = d(&["--no-index", "--", "/dev/null", path]);
            if let Ok(c) = capped(&args, remaining).await {
                if c.truncated {
                    used = DIFF_MAX;
                    truncated = true;
                } else if matches!(c.code, Some(0 | 1)) && !c.out.contains("\nBinary files ") {
                    used += c.out.len();
                    // Only lines after the first hunk header are content.
                    let body = c.out.split_once("\n@@").map_or("", |(_, b)| b);
                    let added = body.lines().skip(1).filter(|l| l.starts_with('+')).count();
                    row["additions"] = json!(added);
                    row["deletions"] = json!(0);
                    row["patch"] = json!(c.out);
                }
            }
        }
        files.push(row);
    }
    json!({ "folder": folder, "repo": true, "files": files, "truncated": truncated })
}

fn split_sections(out: &str) -> Vec<&str> {
    let starts: Vec<usize> = out.match_indices("diff --git ").map(|(i, _)| i).filter(|&i| i == 0 || out.as_bytes()[i - 1] == b'\n').collect();
    starts.iter().enumerate().map(|(k, &s)| &out[s..starts.get(k + 1).copied().unwrap_or(out.len())]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::tmux_io::tests::install_stub;
    use crate::host::cmd::Runner;
    use crate::host::log::LogBuffer;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn runner() -> Runner {
        Runner::new(std::env::var("PATH").unwrap_or_default(), Arc::new(LogBuffer::new()))
    }

    fn git(d: &Path, args: &[&str]) {
        #[allow(clippy::disallowed_types)] // test-only fixture setup
        let ok = std::process::Command::new("git")
            .arg("-C").arg(d)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output().unwrap().status.success();
        assert!(ok, "git {args:?}");
    }

    fn write(d: &Path, name: &str, body: &str) {
        std::fs::write(d.join(name), body).unwrap();
    }

    fn plain(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cdash-diff-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn repo(tag: &str) -> PathBuf {
        let d = plain(tag);
        git(&d, &["init", "-q"]);
        write(&d, "a.txt", "one\ntwo\n");
        write(&d, "b.bin", "\0bytes");
        write(&d, "gone.txt", "bye\n");
        git(&d, &["add", "."]);
        git(&d, &["commit", "-qm", "first"]);
        d
    }

    fn rows(v: &Value) -> Vec<(String, String, Value, Value)> {
        v["files"].as_array().unwrap().iter()
            .map(|f| (f["path"].as_str().unwrap().into(), f["status"].as_str().unwrap().into(), f["additions"].clone(), f["deletions"].clone()))
            .collect()
    }

    #[tokio::test]
    async fn changes_list_every_file_with_status_counts_and_patch() {
        let d = repo("changes");
        write(&d, "a.txt", "one\n2\n");
        write(&d, "b.bin", "\0changed");
        git(&d, &["rm", "-q", "gone.txt"]);
        write(&d, "new file.txt", "n\n");
        git(&d, &["add", "new file.txt"]);
        write(&d, "naïve \"q\".md", "x\ny\n");
        let v = diff(&runner(), d.to_str().unwrap()).await;
        let row = |p: &str, s: &str, a: Value, b: Value| (p.to_string(), s.to_string(), a, b);
        assert_eq!(rows(&v), [
            row("a.txt", "modified", json!(1), json!(1)),
            row("b.bin", "modified", Value::Null, Value::Null),
            row("gone.txt", "deleted", json!(0), json!(1)),
            row("new file.txt", "added", json!(1), json!(0)),
            row("naïve \"q\".md", "untracked", json!(2), json!(0)),
        ]);
        assert!(v["files"][0]["patch"].as_str().unwrap().contains("-two\n+2"));
        assert!(v["files"][4]["patch"].as_str().unwrap().contains("+x"));
        assert_eq!((v["repo"].as_bool(), v["truncated"].as_bool(), v["folder"].as_str()), (Some(true), Some(false), d.to_str()));
    }

    #[tokio::test]
    async fn outside_git_and_before_the_first_commit() {
        let d = plain("nogit");
        let v = diff(&runner(), d.to_str().unwrap()).await;
        assert_eq!(v, json!({ "folder": d.to_str().unwrap(), "repo": false, "files": [], "truncated": false }));

        let d = plain("nocommit");
        git(&d, &["init", "-q"]);
        write(&d, "x.txt", "x\n");
        git(&d, &["add", "x.txt"]);
        write(&d, "y.txt", "y\n");
        let v = diff(&runner(), d.to_str().unwrap()).await;
        let row = |p: &str, s: &str| (p.to_string(), s.to_string(), json!(1), json!(0));
        assert_eq!(rows(&v), [row("x.txt", "added"), row("y.txt", "untracked")]);
    }

    #[tokio::test]
    async fn over_two_megabytes_keeps_the_list_and_counts_but_drops_the_lines() {
        let d = repo("big");
        write(&d, "a.txt", &"line of text\n".repeat(3 * 1024 * 1024 / 13));
        write(&d, "small.txt", "s\n");
        let v = diff(&runner(), d.to_str().unwrap()).await;
        assert_eq!(v["truncated"], json!(true));
        let a = &v["files"][0];
        assert_eq!((a["path"].as_str(), a["patch"].is_null(), a["additions"].as_u64().unwrap() > 200_000), (Some("a.txt"), true, true));
        let s = &v["files"][1];
        assert_eq!(s["path"], "small.txt");
        assert!(s["patch"].is_null() && s["additions"].is_null() && s["deletions"].is_null());
        assert_eq!(v["files"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn every_git_call_has_fsmonitor_off_and_no_external_tools() {
        let d = plain("stub");
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{}/calls'\ncase \"$*\" in\n*rev-parse*--is-inside-work-tree*) echo true;;\n*name-status*) printf 'M\\0t.txt\\0';;\n*ls-files*) printf 'u.txt\\0';;\nesac\n",
            d.display());
        let bin = install_stub(&d, "git", &script);
        let r = Runner::new(bin, Arc::new(LogBuffer::new()));
        diff(&r, d.to_str().unwrap()).await;
        let calls = std::fs::read_to_string(d.join("calls")).unwrap();
        assert!(calls.lines().count() >= 6, "{calls}");
        for l in calls.lines() {
            assert!(l.contains("-c core.fsmonitor=false"), "{l}");
            assert!(l.contains("-c core.hooksPath=/dev/null"), "{l}");
            if l.contains(" diff ") {
                for f in ["--no-color", "--no-ext-diff", "--no-textconv"] {
                    assert!(l.contains(f), "{l}");
                }
            }
        }
        assert!(calls.contains("--no-index"), "{calls}");
    }

    #[tokio::test]
    async fn a_hostile_folder_or_repo_config_runs_nothing() {
        let r = runner();
        // Relative or missing folders never reach git.
        for f in ["relative/dir", "/nonexistent-cdash-folder"] {
            assert_eq!(diff(&r, f).await["repo"], json!(false));
        }
        let d = repo("hostile");
        let marker = d.join("PWNED");
        let sh = format!("touch {}", marker.display());
        git(&d, &["config", "core.fsmonitor", &sh]);
        git(&d, &["config", "diff.external", &sh]);
        git(&d, &["config", "diff.evil.textconv", &sh]);
        git(&d, &["config", "core.pager", &sh]);
        let hooks = d.join("hooks-evil");
        std::fs::create_dir_all(&hooks).unwrap();
        git(&d, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
        write(&d, ".gitattributes", "*.txt diff=evil\n");
        write(&d, "a.txt", "changed\n");
        write(&d, "u.txt", "untracked\n");
        let v = diff(&r, d.to_str().unwrap()).await;
        assert!(v["files"].as_array().unwrap().len() >= 3, "{v}");
        assert!(!marker.exists(), "repo config ran code");
    }

    #[tokio::test]
    async fn an_untracked_symlink_is_shown_as_a_link_and_never_followed() {
        let d = repo("link");
        let secret = plain("secret");
        write(&secret, "key", "TOP-SECRET-CONTENT\n");
        std::os::unix::fs::symlink(secret.join("key"), d.join("l")).unwrap();
        let v = diff(&runner(), d.to_str().unwrap()).await;
        assert_eq!(v["files"][0]["path"], "l");
        assert!(!v.to_string().contains("TOP-SECRET-CONTENT"), "{v}");
    }
}
