//! Driving Claude's rewind dialog (Esc Esc) from chat, checking the screen
//! before every key. Screens recorded on Claude Code 2.1.325 (`CLAUDE_FINDINGS.md`).

use super::picker::idle_gate;
use super::tap::Listed;
use std::future::Future;
use std::time::Duration;
use tokio::time::Instant;

const POLL: Duration = Duration::from_millis(50);
const ESC_GAP: Duration = Duration::from_millis(250);
const OPEN_WAIT: Duration = Duration::from_secs(2);
const STEP_WAIT: Duration = Duration::from_secs(1);
const IDLE_WAIT: Duration = Duration::from_secs(2);
const FAILED: &str = "rewind failed; use Terminal";
const SELECTOR_HEAD: &str = "Restore the code and/or conversation to the point before";
const CONFIRM_HEAD: &str = "Confirm you want to restore to the point before you sent this message:";

/// The pane chat drives. Named `Tui` because `tmux_io::Pane` already exists.
pub trait Tui: Sync {
    /// The pane's visible text.
    fn screen(&self) -> impl Future<Output = String> + Send;
    fn press(&self, key: &str) -> impl Future<Output = Result<(), String>> + Send;
    /// Empty the input box Claude put the rewound prompt back into.
    fn clear(&self, text: &str) -> impl Future<Output = ()> + Send;
    /// The status file's `status` and `waitingFor`.
    fn status(&self) -> impl Future<Output = (Option<String>, Option<String>)> + Send;
}

pub struct Confirm {
    pub quoted: String,
    /// (digit, label), top to bottom.
    pub options: Vec<(String, String)>,
}

/// The selector's rows (between its header and footer, joined) and the
/// highlighted entry. The list scrolls, so the rows tell a move apart even
/// when two neighbouring prompts read the same.
fn selector(screen: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = screen.lines().collect();
    let head = lines.iter().rposition(|l| l.contains(SELECTOR_HEAD))?;
    lines[..head].iter().any(|l| l.trim() == "Rewind").then_some(())?;
    let rows = &lines[head + 1..head + 1 + lines[head + 1..].iter().position(|l| l.contains("Enter to continue"))?];
    let entry = rows.iter().find_map(|l| l.trim().strip_prefix('❯'))?.trim().to_string();
    Some((rows.join("\n"), entry))
}

/// The highlighted entry when the screen shows the rewind selector.
pub fn selector_entry(screen: &str) -> Option<String> {
    selector(screen).map(|(_, entry)| entry)
}

/// The confirm screen: the quoted prompt and the numbered options in view.
pub fn confirm(screen: &str) -> Option<Confirm> {
    let lines: Vec<&str> = screen.lines().collect();
    let rest = &lines[lines.iter().rposition(|l| l.contains(CONFIRM_HEAD))? + 1..];
    let quoted = rest.iter().find_map(|l| l.trim().strip_prefix('│'))?.trim().to_string();
    let option = |l: &&str| {
        let l = l.trim();
        let (digit, label) = l.strip_prefix('❯').unwrap_or(l).trim_start().split_once(". ")?;
        let digit_only = !digit.is_empty() && digit.bytes().all(|b| b.is_ascii_digit());
        digit_only.then(|| (digit.to_string(), label.trim().to_string()))
    };
    Some(Confirm { quoted, options: rest.iter().filter_map(option).collect() })
}

/// A selector entry or quote shows prompt `p`: its first line (cut with `…`
/// when long), or the paste or image chip Claude shows instead.
pub fn entry_matches(entry: &str, p: &Listed) -> bool {
    let first = |s: &str| s.trim().lines().next().unwrap_or("").trim().to_string();
    let (raw, text) = (first(&p.raw), first(&p.text));
    let by_text = match entry.strip_suffix('…').map(str::trim_end) {
        Some(cut) => !cut.is_empty() && (raw.starts_with(cut) || text.starts_with(cut)),
        None => entry == raw || entry == text,
    };
    by_text || (entry.starts_with("[Pasted text #") && p.pasted) || (entry.starts_with("[Image #") && p.images)
}

/// Rewinds to `listed[target]` through the dialog. `listed` is what the
/// selector should list, oldest first; each step is checked against it.
pub async fn drive(tui: &impl Tui, listed: &[Listed], target: usize, files: bool) -> Result<(), String> {
    idle_gate(tui.status().await.0.as_deref())?;
    let want = listed.get(target).ok_or(FAILED)?;
    if choose(tui, listed, target, files).await.is_none() {
        back_out(tui).await;
        return Err(FAILED.into());
    }
    // The restore key is pressed: the rewind counts as done.
    let deadline = Instant::now() + IDLE_WAIT;
    while idle_gate(tui.status().await.0.as_deref()).is_err() && Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
    }
    tui.clear(&want.raw).await;
    Ok(())
}

/// Opens the selector, walks up to the target and presses its restore digit.
/// `None` as soon as a screen is not the one expected.
async fn choose(tui: &impl Tui, listed: &[Listed], target: usize, files: bool) -> Option<()> {
    tui.press("Escape").await.ok()?;
    tokio::time::sleep(ESC_GAP).await;
    idle_gate(tui.status().await.0.as_deref()).ok()?;
    tui.press("Escape").await.ok()?;
    let deadline = Instant::now() + OPEN_WAIT;
    let mut rows = loop {
        if let Some((rows, entry)) = selector(&tui.screen().await) {
            let (status, waiting_for) = tui.status().await;
            if entry == "(current)" && status.as_deref() == Some("waiting") && waiting_for.as_deref() == Some("dialog open") {
                break rows;
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(POLL).await;
    };
    for j in 1..=listed.len() - target {
        tui.press("Up").await.ok()?;
        let (now, entry) = watch(tui, STEP_WAIT, |s| selector(s).filter(|(r, _)| *r != rows)).await?;
        if !entry_matches(&entry, &listed[listed.len() - j]) {
            return None;
        }
        rows = now;
    }
    tui.press("Enter").await.ok()?;
    let c = watch(tui, STEP_WAIT, confirm).await?;
    if !entry_matches(&c.quoted, &listed[target]) {
        return None;
    }
    // The digits shift when the prompt changed code, so the option is found by its label.
    let digit = |label: &str| c.options.iter().find(|(_, l)| l == label).map(|(d, _)| d.as_str());
    let digit = files.then(|| digit("Restore code and conversation")).flatten().or_else(|| digit("Restore conversation"))?;
    // The digit alone confirms (2.1.325): no Enter.
    tui.press(digit).await.ok()
}

/// Polls the screen until `f` finds something, for up to `within`.
async fn watch<T>(tui: &impl Tui, within: Duration, f: impl Fn(&str) -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + within;
    loop {
        if let Some(v) = f(&tui.screen().await) {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Escape out of whichever dialog screen shows, at most three times, and
/// only while one shows.
async fn back_out(tui: &impl Tui) {
    let open = |s: &str| if confirm(s).is_some() { 2 } else { u8::from(selector(s).is_some()) };
    for _ in 0..3 {
        let was = open(&tui.screen().await);
        if was == 0 || tui.press("Escape").await.is_err() {
            return;
        }
        watch(tui, STEP_WAIT, |s| (open(s) != was).then_some(())).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::tap::Listed;
    use std::sync::Mutex;

    /// The selector as 2.1.325 first shows it: scrolled to the bottom, cursor on `(current)`.
    const SELECTOR: &str = "\
   Rewind

   Restore the code and/or conversation to the point before…

     ↑ 3 more
     Run sleep 3 with run_in_background, then reply with only: started
     No code changes

     Run sleep 8 in the foreground, then reply ok
     No code changes

     Reply with only: queued
     ⚠ No code restore

   ❯ (current)

   Enter to continue · Esc to cancel
";
    /// The same list after eight presses of Up.
    const SELECTOR_TOP: &str = "\
   Rewind
   Restore the code and/or conversation to the point before…
   ❯ Reply with only: one
     No code changes
     ! echo two
     No code changes
     Append the line b to f.txt, then reply with only: done
     No code changes
     Run sleep 3 with run_in_background, then reply with only: started
     No code changes
     ↓ 3 more
   Enter to continue · Esc to cancel
";
    /// Enter on a prompt without code changes.
    const CONFIRM: &str = "\
   Rewind
   Confirm you want to restore to the point before you sent this message:
   │ Append the line b to f.txt, then reply with only: done
   │ (2m ago)
   The conversation goes back to before this message.
   The code will be unchanged.
   ❯ 1. Restore conversation
     2. Summarize from here
     3. Summarize up to here
     4. Never mind
";
    /// Enter on a prompt whose Edit changed a file: the restore options shift.
    const CONFIRM_CODE: &str = "\
   Rewind
   Confirm you want to restore to the point before you sent this message:
   │ Use the Edit tool (not bash) to change the line a to c in f.txt, then reply with only: edited
   │ (26s ago)
   The conversation goes back to before this message.
   The code will be restored +1 -1 in f.txt.
   ❯ 1. Restore code and conversation
     2. Restore conversation
     3. Restore code
     4. Summarize from here
   ↓ 5. Summarize up to here
   ⚠ Restored files go back whole, losing later changes by anyone. Bash changes to other files are not undone.
";
    const SPIKE: [&str; 6] = [
        "Reply with only: one",
        "! echo two",
        "Append the line b to f.txt, then reply with only: done",
        "Run sleep 3 with run_in_background, then reply with only: started",
        "Run sleep 8 in the foreground, then reply ok",
        "Reply with only: queued",
    ];
    const NO_CODE: [(&str, &str); 4] = [("1", "Restore conversation"), ("2", "Summarize from here"), ("3", "Summarize up to here"), ("4", "Never mind")];
    const WITH_CODE: [(&str, &str); 4] = [("1", "Restore code and conversation"), ("2", "Restore conversation"), ("3", "Restore code"), ("4", "Summarize from here")];
    /// Rows the selector shows at once, `(current)` included.
    const ROWS: usize = 4;

    fn owned(o: &[(&str, &str)]) -> Vec<(String, String)> {
        o.iter().map(|(d, l)| (d.to_string(), l.to_string())).collect()
    }

    fn listed(lines: &[&str]) -> Vec<Listed> {
        let p = |(i, l): (usize, &&str)| Listed { uuid: format!("p{i}"), text: l.to_string(), raw: l.to_string(), pasted: false, images: false };
        lines.iter().enumerate().map(p).collect()
    }

    #[derive(Clone, Copy, PartialEq)]
    enum View {
        Closed,
        Selector,
        Confirm,
    }

    struct State {
        view: View,
        cursor: usize,
        top: usize,
        /// The last press was an Escape on the closed dialog.
        escaped: bool,
        status: String,
        waiting_for: Option<String>,
        presses: Vec<String>,
    }

    /// A TUI that knows only the rewind dialog, drawn as 2.1.325 draws it.
    struct FakeTui {
        entries: Vec<String>,
        options: Vec<(String, String)>,
        quote: Option<String>,
        s: Mutex<State>,
    }

    impl FakeTui {
        fn new(entries: &[&str]) -> Self {
            let s = State { view: View::Closed, cursor: 0, top: 0, escaped: false, status: "idle".into(), waiting_for: None, presses: Vec::new() };
            Self { entries: entries.iter().map(|e| e.to_string()).collect(), options: owned(&NO_CODE), quote: None, s: Mutex::new(s) }
        }
        fn options(mut self, o: &[(&str, &str)]) -> Self {
            self.options = owned(o);
            self
        }
        fn quoting(mut self, q: &str) -> Self {
            self.quote = Some(q.into());
            self
        }
        fn status(self, st: &str) -> Self {
            self.s.lock().unwrap().status = st.into();
            self
        }
        fn presses(&self) -> Vec<String> {
            self.s.lock().unwrap().presses.clone()
        }
        fn closed(&self) -> bool {
            self.s.lock().unwrap().view == View::Closed
        }
        fn render(&self, s: &State) -> String {
            let mut out = String::from("   Rewind\n\n");
            match s.view {
                View::Closed => return "❯ \n  ⏵⏵ bypass permissions on\n".into(),
                View::Selector => {
                    out += "   Restore the code and/or conversation to the point before…\n\n";
                    let rows = self.entries.len() + 1;
                    if s.top > 0 {
                        out += &format!("     ↑ {} more\n", s.top);
                    }
                    for i in s.top..rows.min(s.top + ROWS) {
                        let mark = if i == s.cursor { "   ❯ " } else { "     " };
                        match self.entries.get(i) {
                            Some(e) => out += &format!("{mark}{e}\n     No code changes\n\n"),
                            None => out += &format!("{mark}(current)\n\n"),
                        }
                    }
                    if s.top + ROWS < rows {
                        out += &format!("     ↓ {} more\n", rows - s.top - ROWS);
                    }
                    out += "   Enter to continue · Esc to cancel\n";
                }
                View::Confirm => {
                    let quote = self.quote.clone().unwrap_or_else(|| self.entries[s.cursor].clone());
                    out += &format!("   Confirm you want to restore to the point before you sent this message:\n   │ {quote}\n   │ (2m ago)\n");
                    out += "   The conversation goes back to before this message.\n   The code will be unchanged.\n";
                    for (i, (d, l)) in self.options.iter().enumerate() {
                        out += &format!("{}{d}. {l}\n", if i == 0 { "   ❯ " } else { "     " });
                    }
                }
            }
            out
        }
    }

    impl Tui for FakeTui {
        async fn screen(&self) -> String {
            let s = self.s.lock().unwrap();
            self.render(&s)
        }
        async fn press(&self, key: &str) -> Result<(), String> {
            let mut s = self.s.lock().unwrap();
            s.presses.push(key.to_string());
            let escaped = std::mem::take(&mut s.escaped);
            match (s.view, key) {
                (View::Closed, "Escape") if escaped => {
                    let rows = self.entries.len() + 1;
                    (s.view, s.cursor, s.top) = (View::Selector, rows - 1, rows.saturating_sub(ROWS));
                    (s.status, s.waiting_for) = ("waiting".into(), Some("dialog open".into()));
                }
                (View::Closed, "Escape") => s.escaped = true,
                (View::Selector, "Escape") => {
                    s.view = View::Closed;
                    (s.status, s.waiting_for) = ("idle".into(), None);
                }
                (View::Selector, "Up") if s.cursor > 0 => {
                    s.cursor -= 1;
                    s.top = s.top.min(s.cursor);
                }
                (View::Selector, "Enter") if s.cursor < self.entries.len() => s.view = View::Confirm,
                (View::Confirm, "Escape") => s.view = View::Selector,
                (View::Confirm, k) if self.options.iter().any(|(d, _)| d == k) => {
                    s.view = View::Closed;
                    (s.status, s.waiting_for) = ("idle".into(), None);
                }
                _ => {}
            }
            Ok(())
        }
        async fn clear(&self, _text: &str) {
            self.s.lock().unwrap().presses.push("clear".into());
        }
        async fn status(&self) -> (Option<String>, Option<String>) {
            let s = self.s.lock().unwrap();
            (Some(s.status.clone()), s.waiting_for.clone())
        }
    }

    #[test]
    fn the_selector_and_confirm_screens_parse() {
        assert_eq!(selector_entry(SELECTOR).as_deref(), Some("(current)"));
        let moved = SELECTOR.replace("   ❯ (current)", "     (current)").replace("     Reply with only: queued", "   ❯ Reply with only: queued");
        assert_eq!(selector_entry(&moved).as_deref(), Some("Reply with only: queued"));
        assert_eq!(selector_entry(SELECTOR_TOP).as_deref(), Some("Reply with only: one"), "scrolled to the top");
        let c = confirm(CONFIRM).unwrap();
        assert_eq!(c.quoted, "Append the line b to f.txt, then reply with only: done");
        assert_eq!(c.options, owned(&NO_CODE));
        let code = confirm(CONFIRM_CODE).unwrap();
        assert_eq!(code.options, owned(&WITH_CODE), "the scrolled `↓ 5.` row is not an option to press");
        assert!(selector_entry("❯ hello").is_none() && confirm(SELECTOR).is_none() && selector_entry(CONFIRM).is_none(), "an input box or the other screen is not mistaken");
    }

    #[test]
    fn entries_match_by_first_line_paste_chip_or_image_token() {
        let p = |raw: &str| Listed { uuid: "u".into(), text: raw.into(), raw: raw.into(), pasted: raw.contains("<pasted_content"), images: raw.contains("[Image #") };
        assert!(entry_matches("yes", &p("yes")) && !entry_matches("yes", &p("yes please")));
        assert!(entry_matches("fix the parser so it…", &p("fix the parser so it stops at the end of the line\nand more")));
        assert!(entry_matches("[Pasted text #2 +40 lines]", &p("<pasted_content id=\"2\">\nlog\n</pasted_content id=\"2\">")) && !entry_matches("[Pasted text #2 +40 lines]", &p("log")));
        assert!(entry_matches("[Image #3] what is this", &p("[Image #3] what is this")) && !entry_matches("[Image #3]", &p("no image")));
        let shell = Listed { uuid: "b".into(), text: "! echo two".into(), raw: "<bash-input>echo two</bash-input>".into(), pasted: false, images: false };
        assert!(entry_matches("! echo two", &shell));
    }

    #[tokio::test]
    async fn rewinding_two_back_checks_every_step_restores_the_conversation_and_clears_the_box() {
        let tui = FakeTui::new(&SPIKE);
        drive(&tui, &listed(&SPIKE), 4, false).await.unwrap();
        assert_eq!(tui.presses(), ["Escape", "Escape", "Up", "Up", "Enter", "1", "clear"]);
        assert!(tui.closed());
    }

    #[tokio::test]
    async fn a_prompt_scrolled_out_of_view_is_reached_one_up_at_a_time() {
        let tui = FakeTui::new(&SPIKE);
        drive(&tui, &listed(&SPIKE), 0, false).await.unwrap();
        assert_eq!(tui.presses(), ["Escape", "Escape", "Up", "Up", "Up", "Up", "Up", "Up", "Enter", "1", "clear"]);
    }

    #[tokio::test]
    async fn repeated_prompts_are_told_apart_by_position() {
        let tui = FakeTui::new(&["yes", "no", "yes"]);
        drive(&tui, &listed(&["yes", "no", "yes"]), 0, false).await.unwrap();
        assert_eq!(tui.presses()[..6], ["Escape", "Escape", "Up", "Up", "Up", "Enter"]);
        let twice = FakeTui::new(&["go on", "go on"]);
        drive(&twice, &listed(&["go on", "go on"]), 0, false).await.unwrap();
        assert_eq!(twice.presses()[..5], ["Escape", "Escape", "Up", "Up", "Enter"], "the same text twice in a row still counts as a move");
    }

    #[tokio::test]
    async fn a_dialog_that_disagrees_with_the_transcript_is_escaped_without_a_restore_key() {
        let tui = FakeTui::new(&["one", "two"]); // rewound in Terminal: the TUI lists fewer prompts
        assert_eq!(drive(&tui, &listed(&["one", "two", "three"]), 0, false).await.unwrap_err(), "rewind failed; use Terminal");
        assert!(!tui.presses().iter().any(|k| k == "clear" || k.chars().all(|c| c.is_ascii_digit())) && tui.closed());
        let quoting = FakeTui::new(&["one", "two"]).quoting("something else");
        assert!(drive(&quoting, &listed(&["one", "two"]), 1, false).await.is_err());
        assert_eq!(quoting.presses().iter().rev().take(2).collect::<Vec<_>>(), ["Escape", "Escape"], "confirm, then selector");
        assert!(quoting.closed());
        let top = FakeTui::new(&["one"]); // the transcript has more prompts than the list
        assert!(drive(&top, &listed(&["zero", "one"]), 0, false).await.is_err());
        assert!(top.closed());
    }

    #[tokio::test]
    async fn files_pick_restore_code_and_conversation_only_when_offered() {
        let both = || FakeTui::new(&["edit it"]).options(&WITH_CODE);
        let (a, b) = (both(), both());
        drive(&a, &listed(&["edit it"]), 0, true).await.unwrap();
        drive(&b, &listed(&["edit it"]), 0, false).await.unwrap();
        assert_eq!((a.presses()[4].as_str(), b.presses()[4].as_str()), ("1", "2"));
        let plain = FakeTui::new(&["no edits"]); // no code changes: conversation only
        drive(&plain, &listed(&["no edits"]), 0, true).await.unwrap();
        assert_eq!(plain.presses()[4], "1");
        assert_eq!(plain.presses().len(), 6, "the digit alone confirms: no Enter after it");
        let none = FakeTui::new(&["odd"]).options(&[("1", "Summarize from here"), ("2", "Never mind")]);
        assert!(drive(&none, &listed(&["odd"]), 0, true).await.is_err());
        assert!(none.closed() && !none.presses().iter().any(|k| k.chars().all(|c| c.is_ascii_digit())));
    }

    #[tokio::test]
    async fn a_busy_or_waiting_session_gets_no_key() {
        let busy = FakeTui::new(&["one"]).status("busy");
        assert_eq!(drive(&busy, &listed(&["one"]), 0, false).await.unwrap_err(), "Stop the turn or wait for it to finish");
        let asking = FakeTui::new(&["one"]).status("waiting");
        assert_eq!(drive(&asking, &listed(&["one"]), 0, false).await.unwrap_err(), "Answer the open question first");
        assert!(busy.presses().is_empty() && asking.presses().is_empty());
        drive(&FakeTui::new(&["one"]).status("shell"), &listed(&["one"]), 0, false).await.expect("a background shell is idle");
    }
}
