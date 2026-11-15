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
/// How long after `idle` Claude may take to put the rewound prompt back in the box.
const BOX_WAIT: Duration = Duration::from_secs(1);
const FAILED: &str = "rewind failed; use Terminal";
const DRAFT: &str = "Clear the Terminal input first";
const SELECTOR_HEAD: &str = "Restore the code and/or conversation to the point before";
const CONFIRM_HEAD: &str = "Confirm you want to restore to the point before you sent this message:";

/// The pane chat drives. Named `Tui` because `tmux_io::Pane` already exists.
pub trait Tui: Sync {
    /// The pane's visible text.
    fn screen(&self) -> impl Future<Output = String> + Send;
    fn press(&self, key: &str) -> impl Future<Output = Result<(), String>> + Send;
    /// Empty the input box Claude put the rewound prompt back into.
    fn clear(&self, text: &str) -> impl Future<Output = ()> + Send;
    /// The box should hold `text` but does not show it yet: clear it before the next send.
    fn leave(&self, text: &str);
    /// The status file's `status` and `waitingFor`.
    fn status(&self) -> impl Future<Output = (Option<String>, Option<String>)> + Send;
}

pub struct Confirm {
    pub quoted: String,
    /// (digit, label), top to bottom.
    pub options: Vec<(String, String)>,
}

/// The highlighted entry's position from the top of the whole list, and its
/// text. The list scrolls: the position is the `↑ N more` count plus the rows
/// shown above the cursor, each a prompt line and its changes line, so a move
/// is proven even when two neighbouring prompts read the same.
fn selector(screen: &str) -> Option<(usize, String)> {
    let lines: Vec<&str> = screen.lines().collect();
    let head = lines.iter().rposition(|l| l.contains(SELECTOR_HEAD))?;
    lines[..head].iter().any(|l| l.trim() == "Rewind").then_some(())?;
    let rows = &lines[head + 1..head + 1 + lines[head + 1..].iter().position(|l| l.contains("Enter to continue"))?];
    let (mut above, mut shown) = (0, 0);
    for l in rows.iter().map(|l| l.trim()) {
        if let Some(entry) = l.strip_prefix('❯') {
            // Anything but whole two-line rows above the cursor is a layout not understood.
            return (shown % 2 == 0).then(|| (above + shown / 2, entry.trim().to_string()));
        }
        match l.strip_prefix("↑ ").and_then(|n| n.strip_suffix(" more")) {
            Some(n) => above = n.parse().ok()?,
            None => shown += usize::from(!l.is_empty()),
        }
    }
    None
}

/// The highlighted entry when the screen shows the rewind selector.
pub fn selector_entry(screen: &str) -> Option<String> {
    selector(screen).map(|(_, entry)| entry)
}

/// The input box's first line: the last line starting with `❯`.
fn input_line(screen: &str) -> Option<&str> {
    screen.lines().rev().find_map(|l| l.trim_start().strip_prefix('❯')).map(str::trim)
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
    // Esc on a Terminal draft would wipe it; a box chat cannot see gets no key.
    match input_line(&tui.screen().await) {
        Some("") => {}
        Some(_) => return Err(DRAFT.into()),
        None => return Err(FAILED.into()),
    }
    tui.press("Escape").await.map_err(|_| FAILED)?;
    tokio::time::sleep(ESC_GAP).await;
    // A turn or question that started meanwhile gets no second Escape.
    idle_gate(tui.status().await.0.as_deref())?;
    if choose(tui, listed, target, files).await.is_none() {
        back_out(tui).await;
        return Err(FAILED.into());
    }
    // The restore key is pressed: the rewind counts as done.
    let deadline = Instant::now() + IDLE_WAIT;
    while idle_gate(tui.status().await.0.as_deref()).is_err() && Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
    }
    // Claude can report idle before it refills the box, and Ctrl+U on the
    // still-empty box would be lost. The box may show only the start of the
    // prompt, so it is matched as a cut entry.
    let refilled = |s: &str| input_line(s).filter(|l| !l.is_empty() && entry_matches(&format!("{l}…"), want)).map(drop);
    match watch(tui, BOX_WAIT, refilled).await {
        Some(()) => tui.clear(&want.raw).await,
        None => tui.leave(&want.raw),
    }
    Ok(())
}

/// The status file says the dialog chat opened is still up.
async fn dialog_open(tui: &impl Tui) -> bool {
    let (status, waiting_for) = tui.status().await;
    status.as_deref() == Some("waiting") && waiting_for.as_deref() == Some("dialog open")
}

/// Presses the second Escape, walks up to the target and presses its restore
/// digit. `None` as soon as a screen or the status is not the one expected.
async fn choose(tui: &impl Tui, listed: &[Listed], target: usize, files: bool) -> Option<()> {
    tui.press("Escape").await.ok()?;
    let deadline = Instant::now() + OPEN_WAIT;
    let mut pos = loop {
        if let Some((pos, entry)) = selector(&tui.screen().await) {
            if entry == "(current)" && dialog_open(tui).await {
                break pos;
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(POLL).await;
    };
    // `(current)` sits below every listed prompt: a count that drifted from the transcript gets no Up.
    (pos == listed.len()).then_some(())?;
    for j in 1..=listed.len() - target {
        tui.press("Up").await.ok()?;
        let (now, entry) = watch(tui, STEP_WAIT, |s| selector(s).filter(|(p, _)| *p != pos)).await?;
        // Exactly one row up, or the count is off.
        if now + 1 != pos || !entry_matches(&entry, &listed[listed.len() - j]) {
            return None;
        }
        pos = now;
    }
    dialog_open(tui).await.then_some(())?;
    tui.press("Enter").await.ok()?;
    let c = watch(tui, STEP_WAIT, |s| confirm(s).filter(|_| selector(s).is_none())).await?;
    if !entry_matches(&c.quoted, &listed[target]) {
        return None;
    }
    // The digits shift when the prompt changed code, so the option is found by its label.
    let digit = |label: &str| c.options.iter().find(|(_, l)| l == label).map(|(d, _)| d.as_str());
    let digit = files.then(|| digit("Restore code and conversation")).flatten().or_else(|| digit("Restore conversation"))?;
    dialog_open(tui).await.then_some(())?;
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
        /// The input box's text.
        input: String,
        /// Screen reads after the restore digit until the box shows the prompt; `None`: never.
        fill_in: Option<usize>,
        ups: usize,
        /// The changes lines were redrawn: the rows differ, the cursor did not move.
        restat: bool,
        presses: Vec<String>,
    }

    /// A TUI that knows only the rewind dialog, drawn as 2.1.325 draws it.
    struct FakeTui {
        entries: Vec<String>,
        options: Vec<(String, String)>,
        quote: Option<String>,
        /// Esc Esc opens nothing.
        no_dialog: bool,
        /// The status turns `busy` after the first Escape.
        busy_between: bool,
        /// This Up (counting from 1) redraws the rows without moving.
        drop_up: Option<usize>,
        /// The status leaves `dialog open` after this many Ups (stale or closed).
        lose_status_after: Option<usize>,
        /// The status leaves `dialog open` when Enter opens the confirm screen.
        lose_status_on_enter: bool,
        s: Mutex<State>,
    }

    impl FakeTui {
        fn new(entries: &[&str]) -> Self {
            let s = State {
                view: View::Closed,
                cursor: 0,
                top: 0,
                escaped: false,
                status: "idle".into(),
                waiting_for: None,
                input: String::new(),
                fill_in: Some(0),
                ups: 0,
                restat: false,
                presses: Vec::new(),
            };
            Self {
                entries: entries.iter().map(|e| e.to_string()).collect(),
                options: owned(&NO_CODE),
                quote: None,
                no_dialog: false,
                busy_between: false,
                drop_up: None,
                lose_status_after: None,
                lose_status_on_enter: false,
                s: Mutex::new(s),
            }
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
        fn draft(self, text: &str) -> Self {
            self.s.lock().unwrap().input = text.into();
            self
        }
        fn fill_in(self, reads: Option<usize>) -> Self {
            self.s.lock().unwrap().fill_in = reads;
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
                View::Closed => return format!("❯ {}\n  ⏵⏵ bypass permissions on\n", s.input),
                View::Selector => {
                    out += "   Restore the code and/or conversation to the point before…\n\n";
                    let rows = self.entries.len() + 1;
                    if s.top > 0 {
                        out += &format!("     ↑ {} more\n", s.top);
                    }
                    let changes = if s.restat { "f.txt +1 -1" } else { "No code changes" };
                    for i in s.top..rows.min(s.top + ROWS) {
                        let mark = if i == s.cursor { "   ❯ " } else { "     " };
                        match self.entries.get(i) {
                            Some(e) => out += &format!("{mark}{e}\n     {changes}\n\n"),
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
            let mut s = self.s.lock().unwrap();
            // After the digit, Claude puts the prompt back in the box, maybe late.
            if s.view == View::Closed && s.presses.iter().any(|k| k.bytes().all(|b| b.is_ascii_digit())) {
                match s.fill_in {
                    Some(0) => {
                        s.input = self.entries[s.cursor].clone();
                        s.fill_in = None;
                    }
                    Some(n) => s.fill_in = Some(n - 1),
                    None => {}
                }
            }
            self.render(&s)
        }
        async fn press(&self, key: &str) -> Result<(), String> {
            let mut s = self.s.lock().unwrap();
            s.presses.push(key.to_string());
            let escaped = std::mem::take(&mut s.escaped);
            match (s.view, key) {
                (View::Closed, "Escape") if escaped && !self.no_dialog => {
                    let rows = self.entries.len() + 1;
                    (s.view, s.cursor, s.top) = (View::Selector, rows - 1, rows.saturating_sub(ROWS));
                    (s.status, s.waiting_for) = ("waiting".into(), Some("dialog open".into()));
                }
                (View::Closed, "Escape") => {
                    s.escaped = true;
                    if self.busy_between {
                        s.status = "busy".into();
                    }
                }
                (View::Selector, "Escape") => {
                    s.view = View::Closed;
                    (s.status, s.waiting_for) = ("idle".into(), None);
                }
                (View::Selector, "Up") => {
                    s.ups += 1;
                    if self.drop_up == Some(s.ups) {
                        s.restat = true;
                    } else if s.cursor > 0 {
                        s.cursor -= 1;
                        s.top = s.top.min(s.cursor);
                    }
                    if self.lose_status_after == Some(s.ups) {
                        (s.status, s.waiting_for) = ("idle".into(), None);
                    }
                }
                (View::Selector, "Enter") if s.cursor < self.entries.len() => {
                    s.view = View::Confirm;
                    if self.lose_status_on_enter {
                        (s.status, s.waiting_for) = ("idle".into(), None);
                    }
                }
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
            let mut s = self.s.lock().unwrap();
            // Ctrl+U on a box not yet refilled is lost.
            let what = if s.input.is_empty() { "clear an empty box" } else { "clear" };
            s.presses.push(what.into());
            s.input.clear();
        }
        fn leave(&self, _text: &str) {
            self.s.lock().unwrap().presses.push("leave".into());
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
        assert_eq!((selector(SELECTOR).unwrap().0, selector(&moved).unwrap().0, selector(SELECTOR_TOP).unwrap().0), (6, 5, 0), "positions count the rows scrolled away");
        assert!(selector(&SELECTOR.replace("     No code changes\n\n     Reply", "\n     Reply")).is_none(), "a row without its changes line is not understood");
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

    #[tokio::test]
    async fn an_up_that_did_not_move_the_cursor_fails_even_when_the_rows_were_redrawn() {
        // The second Up is lost while the changes lines redraw; the two "yes" rows read the same.
        let tui = FakeTui { drop_up: Some(2), ..FakeTui::new(&["a", "yes", "yes"]) };
        assert_eq!(drive(&tui, &listed(&["a", "yes", "yes"]), 1, false).await.unwrap_err(), "rewind failed; use Terminal");
        assert_eq!(tui.presses(), ["Escape", "Escape", "Up", "Up", "Escape"], "no Enter on the wrong yes");
        assert!(tui.closed());
    }

    #[tokio::test]
    async fn a_list_longer_than_the_transcript_gets_no_up() {
        let tui = FakeTui::new(&["one", "extra", "two"]);
        assert_eq!(drive(&tui, &listed(&["one", "two"]), 1, false).await.unwrap_err(), "rewind failed; use Terminal");
        assert_eq!(tui.presses(), ["Escape", "Escape", "Escape"], "opened, counted, closed");
        assert!(tui.closed());
    }

    #[tokio::test]
    async fn a_terminal_draft_is_never_escaped_away() {
        let tui = FakeTui::new(&["one"]).draft("half a thought");
        assert_eq!(drive(&tui, &listed(&["one"]), 0, false).await.unwrap_err(), "Clear the Terminal input first");
        assert!(tui.presses().is_empty());
    }

    #[tokio::test]
    async fn a_dialog_that_never_opens_gets_no_more_keys() {
        let tui = FakeTui { no_dialog: true, ..FakeTui::new(&["one"]) };
        assert_eq!(drive(&tui, &listed(&["one"]), 0, false).await.unwrap_err(), "rewind failed; use Terminal");
        assert_eq!(tui.presses(), ["Escape", "Escape"]);
    }

    #[tokio::test]
    async fn a_turn_that_starts_between_the_escapes_gets_the_gate_wording_and_no_second_escape() {
        let tui = FakeTui { busy_between: true, ..FakeTui::new(&["one"]) };
        assert_eq!(drive(&tui, &listed(&["one"]), 0, false).await.unwrap_err(), "Stop the turn or wait for it to finish");
        assert_eq!(tui.presses(), ["Escape"]);
    }

    #[tokio::test]
    async fn enter_and_the_digit_wait_for_the_status_to_say_the_dialog_is_open() {
        let tui = FakeTui { lose_status_after: Some(2), ..FakeTui::new(&SPIKE) };
        assert!(drive(&tui, &listed(&SPIKE), 4, false).await.is_err());
        assert_eq!(tui.presses(), ["Escape", "Escape", "Up", "Up", "Escape"]);
        let at_confirm = FakeTui { lose_status_on_enter: true, ..FakeTui::new(&SPIKE) };
        assert_eq!(drive(&at_confirm, &listed(&SPIKE), 4, false).await.unwrap_err(), "rewind failed; use Terminal");
        assert_eq!(at_confirm.presses(), ["Escape", "Escape", "Up", "Up", "Enter", "Escape", "Escape"], "no digit: confirm, then selector, backed out");
        assert!(at_confirm.closed());
    }

    #[tokio::test]
    async fn the_box_is_cleared_once_it_shows_the_prompt_and_otherwise_left_for_the_next_send() {
        let late = FakeTui::new(&["one", "two"]).fill_in(Some(3));
        drive(&late, &listed(&["one", "two"]), 1, false).await.unwrap();
        assert_eq!(late.presses().last().unwrap(), "clear", "not Ctrl+U on a box still empty");
        let never = FakeTui::new(&["one", "two"]).fill_in(None);
        drive(&never, &listed(&["one", "two"]), 1, false).await.unwrap();
        assert_eq!(never.presses().last().unwrap(), "leave");
    }
}
