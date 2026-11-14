//! What a hub tells the notifier: a session finished, failed or wants an answer.

use crate::collect::ctx::Ctx;
use serde_json::json;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NoteKind {
    Done,
    Error,
    Ask,
}

impl NoteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NoteKind::Done => "done",
            NoteKind::Error => "error",
            NoteKind::Ask => "ask",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub session: String,
    pub project: String,
    pub kind: NoteKind,
    pub at: u64,
}

impl Note {
    /// Exactly what a push carries: no message text, tool or file names.
    pub fn payload(&self) -> String {
        json!({"s":self.session,"p":self.project,"k":self.kind.as_str(),"t":self.at}).to_string()
    }
}

/// A hub's way to publish notes for its session.
pub struct Sink {
    pub ctx: Arc<Ctx>,
    pub project: String,
}

impl Sink {
    pub fn note(&self, session: &str, kind: NoteKind) {
        if self.ctx.focused(session) {
            return;
        }
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let _ = self.ctx.notes.send(Note { session: session.into(), project: self.project.clone(), kind, at });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn the_payload_carries_session_project_kind_and_time_only() {
        let n = Note { session: "cdash-a".into(), project: "my \"proj\"".into(), kind: NoteKind::Ask, at: 1_789_000_000 };
        assert_eq!(
            serde_json::from_str::<Value>(&n.payload()).unwrap(),
            json!({"s":"cdash-a","p":"my \"proj\"","k":"ask","t":1_789_000_000u64})
        );
    }
}
