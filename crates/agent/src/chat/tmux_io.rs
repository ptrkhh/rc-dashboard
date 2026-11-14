//! Typing into a Claude pane through tmux. Every call goes through `Runner`.

use crate::host::cmd::Runner;
use std::path::Path;
use std::time::Duration;

/// Exact-match target, the form `codex::terminal_supported` uses.
fn target(name: &str) -> String {
    format!("={name}:")
}

/// The pause between a paste and its Enter.
// ponytail: 400 ms is the pause the spike used between paste and Enter; shorten once measured.
pub const PASTE_PAUSE: Duration = Duration::from_millis(400);

/// Paste `text` with bracketed paste, then press Enter.
pub async fn paste(runner: &Runner, name: &str, text: &str) -> Result<(), String> {
    paste_raw(runner, name, text).await?;
    tokio::time::sleep(PASTE_PAUSE).await;
    key(runner, name, "Enter").await
}

/// Paste `text` with bracketed paste, without Enter. The text goes through
/// a private file because `Runner` gives the child no stdin and tmux refuses
/// commands longer than about 16 KB. Assumes `wire::sanitize`d text.
pub async fn paste_raw(runner: &Runner, name: &str, text: &str) -> Result<(), String> {
    let buffer = format!("cdash-chat-{name}");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let file = std::env::temp_dir().join(format!("cdash-chat-{name}-{nanos}.txt"));
    write_private(&file, text).await.map_err(|e| format!("chat paste: {e}"))?;
    let path = file.to_string_lossy();
    let loaded = runner.run_checked("tmux", &["load-buffer", "-b", &buffer, &path], "tmux load-buffer").await;
    let _ = tokio::fs::remove_file(&file).await;
    loaded?;
    runner.run_checked("tmux", &["paste-buffer", "-p", "-d", "-b", &buffer, "-t", &target(name)], "tmux paste-buffer").await.map(drop)
}

async fn write_private(file: &std::path::Path, text: &str) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut f = tokio::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(file).await?;
    f.write_all(text.as_bytes()).await?;
    f.flush().await
}

pub async fn escape(runner: &Runner, name: &str) -> Result<(), String> {
    key(runner, name, "Escape").await
}

/// Press one tmux key (`Enter`, `Escape`, `BTab`).
pub async fn key(runner: &Runner, name: &str, key: &str) -> Result<(), String> {
    runner.run_checked("tmux", &["send-keys", "-t", &target(name), key], "tmux send-keys").await.map(drop)
}

/// The pane's visible text.
pub async fn capture(runner: &Runner, name: &str) -> Result<String, String> {
    runner.run_checked("tmux", &["capture-pane", "-p", "-t", &target(name)], "tmux capture-pane").await
}

/// How often `attach_image` looks for the new chip.
pub const CHIP_POLL: Duration = Duration::from_millis(50);

/// The `[Image #N]` chips in the input box: from the last line starting
/// with `❯` down (the conversation above also shows chips), or on the whole
/// screen when there is no such line.
pub fn input_chips(screen: &str) -> usize {
    let lines: Vec<&str> = screen.lines().collect();
    let from = lines.iter().rposition(|l| l.trim_start().starts_with('❯')).unwrap_or(0);
    let chip = |l: &str, i: usize| {
        let after = &l[i + "[Image #".len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        digits > 0 && after[digits..].starts_with(']')
    };
    lines[from..].iter().map(|l| l.match_indices("[Image #").filter(|&(i, _)| chip(l, i)).count()).sum()
}

/// Paste an image's path alone, without Enter, and wait until Claude shows
/// exactly one more chip for it. The screen is read before anything is typed.
pub async fn attach_image(runner: &Runner, name: &str, path: &Path, wait: Duration) -> Result<(), String> {
    let before = input_chips(&capture(runner, name).await?);
    paste_raw(runner, name, &path.to_string_lossy()).await?;
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        tokio::time::sleep(CHIP_POLL).await;
        if capture(runner, name).await.is_ok_and(|s| input_chips(&s) == before + 1) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("image did not attach".into());
        }
    }
}

/// Empty the input box after Claude handed a prompt back into it. Ctrl+U
/// clears to the start of a row and is harmless on an empty box.
pub async fn clear_input(runner: &Runner, name: &str, text: &str) -> Result<(), String> {
    // ponytail: over-presses on purpose; switch to ctrl+l (chat:clearInput) once it is verified on a real session.
    let presses = (2 * text.lines().count() + text.chars().count() / 20 + 4).min(500);
    let t = target(name);
    let mut args = vec!["send-keys", "-t", t.as_str()];
    args.extend(std::iter::repeat_n("C-u", presses));
    runner.run_checked("tmux", &args, "tmux send-keys").await.map(drop)
}

/// What chat needs to know about a pane.
pub struct Pane {
    pub pid: i32,
    /// A bare terminal: no tool to chat with.
    pub bare: bool,
    /// The token Claude's permission hook sends, when it was launched with one.
    pub hook: Option<String>,
}

pub async fn pane(runner: &Runner, name: &str) -> Option<Pane> {
    let out = runner
        .run_checked("tmux", &["display-message", "-p", "-t", &target(name), "#{pane_pid}|#{@cdash_kind}|#{@cdash_hook}"], "tmux chat pane")
        .await
        .ok()?;
    let mut f = out.trim().split('|');
    let pid = f.next()?.parse().ok()?;
    let bare = f.next()? == "terminal";
    let hook = f.next().filter(|h| !h.is_empty()).map(String::from);
    Some(Pane { pid, bare, hook })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::host::log::LogBuffer;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    pub(crate) fn tempfile_path(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("cdash-chat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        let _ = std::fs::remove_file(&p);
        p
    }

    /// A stub `tmux` that appends its arguments to `<dir>/tmux-args`, prints
    /// `answer` and exits with `exit`. Returns the directory holding it.
    pub(crate) fn stub_tmux(dir: &std::path::Path, answer: &str, exit: i32) -> String {
        let script = format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}/tmux-args'\necho '{answer}'\nexit {exit}\n", dir.display());
        install_stub(dir, "tmux", &script)
    }

    /// Install an executable `name` running `script` into `<dir>/bin` and
    /// return that directory. The file is written by a child and renamed into
    /// place: a write fd held in this process can leak into another test's
    /// fork and make exec fail with ETXTBSY, and a rename never disturbs a
    /// stub that is running (so re-stubbing in place is safe too).
    pub(crate) fn install_stub(dir: &std::path::Path, name: &str, script: &str) -> String {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let src = dir.join(format!("{name}.src"));
        std::fs::write(&src, script).unwrap();
        #[allow(clippy::disallowed_types)] // test-only fixture setup
        let ok = std::process::Command::new("sh")
            .args(["-c", "cp \"$1\" \"$2.new\" && chmod 755 \"$2.new\" && mv -f \"$2.new\" \"$2\"", "sh"])
            .arg(&src)
            .arg(bin.join(name))
            .status()
            .unwrap()
            .success();
        assert!(ok);
        bin.to_string_lossy().into_owned()
    }

    /// A stub `tmux` for image pastes: `capture-pane` prints `<dir>/screen`
    /// (`❯ ` at first), `load-buffer` appends the pasted file and `\n--\n` to
    /// `<dir>/pastes`, and `paste-buffer` moves `<dir>/after-paste` (written
    /// from `after_paste`) over the screen. Arguments go to `<dir>/tmux-args`.
    /// It sets its own PATH: callers' runners see only the stub.
    pub(crate) fn chip_stub(dir: &Path, after_paste: Option<&str>) -> String {
        std::fs::write(dir.join("screen"), "❯ ").unwrap();
        if let Some(a) = after_paste {
            std::fs::write(dir.join("after-paste"), a).unwrap();
        }
        let script = format!(
            "#!/bin/sh\nPATH=/usr/bin:/bin\ncd '{}' || exit 1\nprintf '%s\\n' \"$*\" >> tmux-args\ncase \"$1\" in\n\
             capture-pane) cat screen ;;\n\
             load-buffer) cat \"$4\" >> pastes; printf '\\n--\\n' >> pastes ;;\n\
             paste-buffer) [ -f after-paste ] && mv -f after-paste screen ;;\n\
             esac\nexit 0\n",
            dir.display()
        );
        install_stub(dir, "tmux", &script)
    }

    fn stub_runner(tag: &str) -> (Runner, PathBuf) {
        let dir = tempfile_path(tag);
        std::fs::create_dir_all(&dir).unwrap();
        let path = stub_tmux(&dir, "", 0);
        (Runner::new(path, Arc::new(LogBuffer::new())), dir)
    }

    fn real_runner() -> Runner {
        Runner::new(std::env::var("PATH").unwrap_or_default(), Arc::new(LogBuffer::new()))
    }

    fn have_tmux() -> bool {
        #[allow(clippy::disallowed_types)] // test-only probe, no Runner in scope
        let ok = std::process::Command::new("tmux").arg("-V").output().is_ok();
        if !ok {
            eprintln!("tmux not installed; skipping");
        }
        ok
    }

    fn tmux(args: &[&str]) -> std::process::Output {
        #[allow(clippy::disallowed_types)] // test-only fixture control
        std::process::Command::new("tmux").args(args).output().unwrap()
    }

    /// A detached tmux session running `cmd`, killed on drop.
    struct Session(String);
    impl Session {
        fn new(tag: &str, cmd: &str) -> Self {
            let name = format!("cdash-chattest-{}-{tag}", std::process::id());
            let _ = tmux(&["kill-session", "-t", &format!("={name}")]);
            assert!(tmux(&["new-session", "-d", "-s", &name, "-x", "80", "-y", "24", cmd]).status.success());
            Self(name)
        }
    }
    impl Drop for Session {
        fn drop(&mut self) {
            let _ = tmux(&["kill-session", "-t", &format!("={}", self.0)]);
        }
    }

    pub(crate) async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
        for _ in 0..300 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("never happened: {what}");
    }

    #[tokio::test]
    async fn paste_loads_a_private_file_pastes_bracketed_then_presses_enter() {
        let (runner, dir) = stub_runner("paste");
        paste(&runner, "cdash-a", "hello\nworld").await.unwrap();
        let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap();
        let lines: Vec<&str> = args.lines().collect();
        assert!(lines[0].starts_with("load-buffer -b cdash-chat-cdash-a "));
        assert_eq!(lines[1], "paste-buffer -p -d -b cdash-chat-cdash-a -t =cdash-a:");
        assert_eq!(lines[2], "send-keys -t =cdash-a: Enter");
        let file = lines[0].rsplit(' ').next().unwrap();
        assert!(!std::path::Path::new(file).exists(), "the temp file is removed");
    }
    #[tokio::test]
    async fn escape_and_clear_send_only_those_keys() {
        let (runner, dir) = stub_runner("keys");
        escape(&runner, "cdash-a").await.unwrap();
        clear_input(&runner, "cdash-a", "one\ntwo").await.unwrap();
        let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap();
        let lines: Vec<&str> = args.lines().collect();
        assert_eq!(lines[0], "send-keys -t =cdash-a: Escape");
        assert!(lines[1].starts_with("send-keys -t =cdash-a: C-u C-u"));
        assert!(lines[1].split(' ').skip(3).all(|k| k == "C-u"));
    }
    #[tokio::test]
    async fn pane_reads_the_pid_the_kind_and_the_hook_token() {
        for (i, (answer, bare, hook)) in [("4242|", false, None), ("4242|terminal|", true, None), ("4242||tok-1", false, Some("tok-1"))].into_iter().enumerate() {
            let dir = tempfile_path(&format!("pane-{i}"));
            std::fs::create_dir_all(&dir).unwrap();
            let runner = Runner::new(stub_tmux(&dir, answer, 0), Arc::new(LogBuffer::new()));
            let p = pane(&runner, "cdash-a").await.unwrap();
            assert_eq!((p.pid, p.bare, p.hook.as_deref()), (4242, bare, hook));
        }
    }
    #[test]
    fn chips_are_counted_in_the_input_box_only() {
        let screen = "❯ [Image #1] What color?\n  ⎿  [Image #1]\n● Red.\n────\n❯ [Image #2] [Image #3]\n────\n  ⏵⏵ bypass permissions on";
        assert_eq!(input_chips(screen), 2, "the conversation above the box does not count");
        assert_eq!(input_chips("❯ \n────\n  ⏵⏵ bypass permissions on"), 0);
        assert_eq!(input_chips("[Image #7] and no prompt line"), 1, "no box found: the whole screen counts");
        assert_eq!(input_chips("❯ [Image #] [Image #x] [Image #12"), 0, "only whole tokens count");
    }
    #[tokio::test]
    async fn an_image_path_is_pasted_alone_and_attach_waits_for_its_chip() {
        let dir = tempfile_path("chip-ok");
        std::fs::create_dir_all(&dir).unwrap();
        let runner = Runner::new(chip_stub(&dir, Some("❯ [Image #3]")), Arc::new(LogBuffer::new()));
        attach_image(&runner, "cdash-a", Path::new("/u/up/Shot 1.png"), Duration::from_secs(2)).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("pastes")).unwrap(), "/u/up/Shot 1.png\n--\n");
        let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap();
        assert!(args.starts_with("capture-pane -p -t =cdash-a:"));
        assert!(args.contains("paste-buffer -p -d -b cdash-chat-cdash-a -t =cdash-a:") && !args.contains("send-keys"), "no Enter");
    }
    #[tokio::test]
    async fn an_image_that_never_shows_a_chip_fails_within_the_wait() {
        let dir = tempfile_path("chip-none");
        std::fs::create_dir_all(&dir).unwrap();
        let runner = Runner::new(chip_stub(&dir, None), Arc::new(LogBuffer::new()));
        let started = std::time::Instant::now();
        assert_eq!(attach_image(&runner, "cdash-a", Path::new("/u/a.png"), Duration::from_millis(300)).await, Err("image did not attach".to_string()));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    #[tokio::test]
    async fn text_reaches_a_real_pane_as_typed_and_multiline_stays_one_paste() {
        if !have_tmux() {
            return;
        }
        let out = tempfile_path("real");
        let s = Session::new("io", &format!("cat > {}", out.display()));
        paste(&real_runner(), &s.0, "alpha\n-beta").await.unwrap();
        eventually("cat wrote the text", || std::fs::read_to_string(&out).is_ok_and(|t| t == "alpha\n-beta\n")).await;
    }
}
