//! The Changes sheet: the session folder's diff against HEAD.
//!
//! The folder comes from a transcript, so it is untrusted: it must be an
//! absolute existing directory, and git runs with the repo-config ways to
//! execute code switched off: fsmonitor, hooks, pager, external diff, textconv,
//! every repo-local `filter.*.clean`/`process` (emptied per call), and
//! submodules (never entered). Global and system config is left alone.

use crate::collect::git::GIT_TIMEOUT;
use crate::host::cmd::{Capped, Runner};
use serde_json::{json, Value};

/// Ceiling for list and count outputs.
const SMALL_MAX: usize = 16 * 1024 * 1024;
pub const DIFF_MAX: usize = 2 * 1024 * 1024;
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// Prefix of every git call. Repo config must never run code.
const SAFE: [&str; 6] = ["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-c", "core.pager=cat"];
/// Prefix of every diff.
const D: [&str; 4] = ["diff", "--no-color", "--no-ext-diff", "--no-textconv"];

fn d<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    D.iter().copied().chain(["--no-renames", "--relative", "--ignore-submodules=all"]).chain(extra.iter().copied()).collect()
}

fn none(folder: &str, repo: bool) -> Value {
    json!({ "folder": folder, "repo": repo, "files": [], "truncated": false })
}

/// One git call in `folder`. `over` are the `-c filter.<n>.…=` overrides. The
/// log key names the call kind, so one failing kind does not hide another.
async fn run(runner: &Runner, folder: &str, over: &[String], rest: &[&str], cap: usize) -> Result<Capped, String> {
    let args: Vec<&str> = ["-C", folder].into_iter().chain(SAFE).chain(over.iter().map(String::as_str)).chain(rest.iter().copied()).collect();
    runner.run_capped("git", &args, &format!("git {} {folder}", rest[0]), GIT_TIMEOUT, cap).await
}

/// Small outputs only (lists and counts): stdout of a call that exited 0.
async fn small(runner: &Runner, folder: &str, over: &[String], rest: &[&str]) -> Result<String, String> {
    match run(runner, folder, over, rest, SMALL_MAX).await? {
        Capped { out, truncated: false, code: Some(0) } => Ok(out),
        c => Err(format!("git {} exited {:?}", rest[0], c.code)),
    }
}

/// Overrides that empty every repo-local filter command (`clean`, `process`
/// run code during a diff), or `Err(())` when one cannot be overridden
/// (`-c` splits at the first `=`, so a name containing `=` is out of reach).
/// Global and system filters stay: they belong to the owner (git-lfs).
async fn filter_overrides(runner: &Runner, folder: &str) -> Result<Vec<String>, ()> {
    let c = run(runner, folder, &[], &["config", "-z", "--show-scope", "--get-regexp", r"^filter\..*\.(clean|process)$"], SMALL_MAX)
        .await
        .map_err(|_| ())?;
    if c.truncated || !matches!(c.code, Some(0 | 1)) {
        return Err(());
    }
    let mut names = std::collections::BTreeSet::new();
    // With -z the entries alternate: "<scope>", then "<key>\n<value>".
    let mut it = c.out.split('\0');
    while let (Some(scope), Some(entry)) = (it.next(), it.next()) {
        let key = entry.split_once('\n').map_or(entry, |(k, _)| k);
        if scope != "local" && scope != "worktree" {
            continue;
        }
        let name = key.strip_prefix("filter.").and_then(|k| k.rsplit_once('.')).map(|(n, _)| n).ok_or(())?;
        if name.contains('=') {
            return Err(());
        }
        names.insert(name.to_string());
    }
    Ok(names.iter().flat_map(|n| ["-c".to_string(), format!("filter.{n}.clean="), "-c".to_string(), format!("filter.{n}.process=")]).collect())
}

pub async fn diff(runner: &Runner, folder: &str) -> Value {
    if !std::path::Path::new(folder).is_absolute() || !std::path::Path::new(folder).is_dir() {
        return none(folder, false);
    }
    if small(runner, folder, &[], &["rev-parse", "--is-inside-work-tree"]).await.map(|o| o.trim() == "true") != Ok(true) {
        return none(folder, false);
    }
    let Ok(over) = filter_overrides(runner, folder).await else {
        // A filter we cannot switch off: list the files from the index and the
        // folder alone (no diff runs) and withhold every count and patch.
        let listed = small(runner, folder, &[], &["ls-files", "-z", "--cached", "--others", "--exclude-standard"]).await.unwrap_or_default();
        let others = small(runner, folder, &[], &["ls-files", "-z", "--others", "--exclude-standard"]).await.unwrap_or_default();
        let untracked: std::collections::HashSet<&str> = others.split('\0').collect();
        let files: Vec<Value> = listed.split('\0').filter(|p| !p.is_empty()).map(|p| {
            json!({ "path": p, "status": if untracked.contains(p) { "untracked" } else { "modified" }, "additions": null, "deletions": null, "patch": null })
        }).collect();
        return json!({ "folder": folder, "repo": true, "files": files, "truncated": true });
    };
    macro_rules! small {
        ($rest:expr) => {
            small(runner, folder, &over, $rest)
        };
    }
    let base = if small!(&["rev-parse", "--verify", "-q", "HEAD"]).await.is_ok() { "HEAD" } else { EMPTY_TREE };
    let with_base = |extra: &[&'static str]| -> Vec<&str> { d(extra).into_iter().chain([base]).collect() };

    let status = small!(&with_base(&["--name-status", "-z"])).await.unwrap_or_default();
    let counts = small!(&with_base(&["--numstat", "-z"])).await.unwrap_or_default();
    let mut st = status.split('\0');
    let mut cs = counts.split('\0');
    let mut files: Vec<Value> = Vec::new();
    while let (Some(s), Some(path)) = (st.next(), st.next()) {
        let (a, del) = match cs.next().map(|l| l.splitn(3, '\t').collect::<Vec<_>>()) {
            Some(p) if p.len() == 3 => (p[0].parse::<u64>().ok(), p[1].parse::<u64>().ok()),
            _ => (None, None),
        };
        let status = match s { "A" => "added", "D" => "deleted", _ => "modified" };
        files.push(json!({ "path": path, "status": status, "additions": a, "deletions": del, "patch": null, "typechange": s == "T" }));
    }

    let mut truncated = false;
    let mut used = 0;
    if !files.is_empty() {
        if let Ok(c) = run(runner, folder, &over, &with_base(&[]), DIFF_MAX).await {
            truncated = c.truncated;
            let mut sections = split_sections(&c.out).into_iter();
            let total = sections.len();
            let mut taken = 0;
            used = c.out.len();
            for f in files.iter_mut() {
                // A typechange is two sections (delete, then add): one patch.
                let n = if f["typechange"] == true { 2 } else { 1 };
                let mine: Vec<&str> = sections.by_ref().take(n).collect();
                taken += mine.len();
                // On truncation the last section is cut off, so it is dropped.
                if mine.len() == n && !(truncated && taken == total) {
                    f["patch"] = json!(mine.concat());
                }
            }
        } else {
            truncated = true;
        }
    }
    for f in files.iter_mut() {
        f.as_object_mut().map(|o| o.remove("typechange"));
    }

    let untracked = small!(&["ls-files", "--others", "--exclude-standard", "-z"]).await.unwrap_or_default();
    // ponytail: one git per untracked file; batch them if a folder with thousands of untracked files makes the sheet slow.
    for path in untracked.split('\0').filter(|p| !p.is_empty() && !p.ends_with('/')) {
        let mut row = json!({ "path": path, "status": "untracked", "additions": null, "deletions": null, "patch": null });
        // A link to a FIFO or device would block git; a dangling link is fine.
        let odd = std::fs::metadata(std::path::Path::new(folder).join(path)).is_ok_and(|m| !m.is_file() && !m.is_dir());
        let remaining = DIFF_MAX.saturating_sub(used);
        if odd {
            // keep the null row
        } else if remaining == 0 {
            truncated = true;
        } else {
            let args = d(&["--no-index", "--", "/dev/null", path]);
            if let Ok(c) = run(runner, folder, &over, &args, remaining).await {
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
        // A submodule whose own config has a filter.
        let sub = repo("hostile-sub-src");
        git(&d, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", sub.to_str().unwrap(), "sub"]);
        git(&d, &["commit", "-qm", "sub"]);
        let marker = d.join("PWNED");
        let sh = format!("touch {}", marker.display());
        git(&d, &["config", "core.fsmonitor", &sh]);
        git(&d, &["config", "diff.external", &sh]);
        git(&d, &["config", "diff.evil.textconv", &sh]);
        git(&d, &["config", "core.pager", &sh]);
        let hooks = d.join("hooks-evil");
        std::fs::create_dir_all(&hooks).unwrap();
        git(&d, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
        // Filters: a same-size edit and an untracked file, both attributed.
        git(&d, &["config", "filter.evil.clean", &sh]);
        git(&d, &["config", "filter.evil.process", &sh]);
        write(&d, ".gitattributes", "*.txt diff=evil filter=evil\n");
        write(&d, "a.txt", "one\nTWO\n");
        write(&d, "u.txt", "untracked\n");
        git(&d.join("sub"), &["config", "filter.s.clean", &sh]);
        write(&d.join("sub"), ".gitattributes", "*.txt filter=s\n");
        write(&d.join("sub"), "a.txt", "ONE\ntwo\n");
        let _ = std::fs::remove_file(&marker); // setup itself ran git with the config
        let v = diff(&r, d.to_str().unwrap()).await;
        assert!(v["files"].as_array().unwrap().len() >= 3, "{v}");
        assert!(!marker.exists(), "repo config ran code");
        assert!(v["files"][0]["patch"].as_str().unwrap().contains("-two\n+TWO"), "{v}");
    }

    #[tokio::test]
    async fn a_filter_name_git_cannot_override_withholds_every_diff() {
        let d = repo("eqname");
        let marker = d.join("PWNED");
        git(&d, &["config", "filter.a=b.clean", &format!("touch {}", marker.display())]);
        write(&d, ".gitattributes", "*.txt filter=a=b\n");
        write(&d, "a.txt", "one\nTWO\n");
        write(&d, "u.txt", "u\n");
        let v = diff(&runner(), d.to_str().unwrap()).await;
        assert!(!marker.exists());
        let files = v["files"].as_array().unwrap();
        assert!(files.iter().any(|f| f["path"] == "u.txt" && f["status"] == "untracked"), "{v}");
        assert!(files.iter().all(|f| f["patch"].is_null() && f["additions"].is_null()), "{v}");
        assert_eq!(v["truncated"], json!(true));
    }

    #[tokio::test]
    async fn a_typechange_keeps_the_patches_of_later_files_aligned() {
        let d = repo("typechange");
        std::fs::remove_file(d.join("a.txt")).unwrap();
        std::os::unix::fs::symlink("b.bin", d.join("a.txt")).unwrap();
        write(&d, "gone.txt", "bye\nmore\n");
        let v = diff(&runner(), d.to_str().unwrap()).await;
        let f = |p: &str| v["files"].as_array().unwrap().iter().find(|f| f["path"] == p).unwrap().clone();
        let a = f("a.txt")["patch"].as_str().unwrap().to_string();
        assert_eq!(a.matches("diff --git ").count(), 2, "{a}");
        let g = f("gone.txt");
        assert!(g["patch"].as_str().unwrap().starts_with("diff --git a/gone.txt"), "{v}");
        assert_eq!((g["additions"].clone(), g["deletions"].clone()), (json!(1), json!(0)), "{v}");
    }

    #[tokio::test]
    async fn an_untracked_link_to_a_fifo_is_skipped_not_waited_on() {
        let d = repo("fifo");
        #[allow(clippy::disallowed_types)] // test-only fixture setup
        let ok = std::process::Command::new("mkfifo").arg(d.join("pipe")).status().unwrap().success();
        assert!(ok);
        std::os::unix::fs::symlink(d.join("pipe"), d.join("l")).unwrap();
        std::os::unix::fs::symlink("nowhere", d.join("dangling")).unwrap();
        let started = std::time::Instant::now();
        let v = diff(&runner(), d.to_str().unwrap()).await;
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let f = |p: &str| v["files"].as_array().unwrap().iter().find(|f| f["path"] == p).unwrap().clone();
        assert!(f("l")["patch"].is_null());
        assert!(f("dangling")["patch"].is_string(), "{v}");
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
