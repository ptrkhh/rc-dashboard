//! Checking an answer against the ask it claims to answer.

use super::hub::Answer;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// Whether `a` is a complete, allowed answer to `ask`.
pub fn check_answer(ask: &Value, a: &Answer) -> Result<(), &'static str> {
    // The answer travels on the 64 KiB socket line.
    if serde_json::to_string(a).map_or(0, |s| s.len()) > super::MAX_TEXT {
        return Err("answer too long");
    }
    const BAD: &str = "not a valid answer";
    match ask["kind"].as_str() {
        Some("plan") => match a.choice.as_deref() {
            Some("yes" | "no") => Ok(()),
            _ => Err(BAD),
        },
        Some("question") => {
            let given = a.answers.as_ref().ok_or(BAD)?;
            for q in ask["questions"].as_array().ok_or(BAD)? {
                let r = &given[q["id"].as_str().ok_or(BAD)?];
                let choices = r["choices"].as_array().map_or(&[][..], |v| v.as_slice());
                let text = r["text"].as_str().unwrap_or("").trim();
                let listed = |c: &Value| q["options"].as_array().is_some_and(|o| o.iter().any(|o| o["label"] == *c));
                if !choices.iter().all(listed)
                    || (choices.len() > 1 && q["multi"] != true)
                    || (!text.is_empty() && q["other"] != true)
                {
                    return Err(BAD);
                }
                if choices.is_empty() && text.is_empty() {
                    return Err("answer every question");
                }
            }
            Ok(())
        }
        _ => Err(BAD),
    }
}

/// How long a card outlives the last "waiting" status before it counts as settled elsewhere.
pub const SETTLE_NET: Duration = Duration::from_secs(2);

const TOOLS: [&str; 2] = ["AskUserQuestion", "ExitPlanMode"];

struct Card {
    ask: Value,
    tool: String,
    input: Value,
    prompt: String,
    tx: oneshot::Sender<Value>,
    last_waiting: Instant,
}

/// A transcript `tool_use` that has no result yet.
struct Use {
    id: String,
    prompt: String,
    name: String,
    input: Value,
}

#[derive(Deserialize)]
struct Rec {
    #[serde(rename = "promptId", default)]
    prompt_id: String,
    #[serde(rename = "isSidechain", default)]
    sidechain: bool,
    message: Option<Msg>,
}

#[derive(Deserialize)]
struct Msg {
    #[serde(default)]
    content: Vec<Value>,
}

/// Claude's permission-hook requests waiting on the user, and the transcript uses they settle against.
#[derive(Default)]
pub struct Cards {
    cards: Vec<Card>,
    uses: Vec<Use>,
}

impl Cards {
    /// Turns a hook request into a card; the receiver yields the decision for the hook.
    pub fn add(&mut self, req: &Value, now: Instant) -> Option<oneshot::Receiver<Value>> {
        let tool = req["tool_name"].as_str()?;
        let input = &req["tool_input"];
        let mut ask = json!({"id": new_id(), "tool": "claude"});
        match tool {
            "AskUserQuestion" => {
                let qs = input["questions"].as_array().filter(|q| !q.is_empty())?;
                let mut out = Vec::new();
                for (i, q) in qs.iter().enumerate() {
                    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
                    let opts: Vec<Value> = q["options"].as_array().map_or(vec![], |o| {
                        o.iter().map(|o| json!({"label": s(&o["label"]), "description": s(&o["description"])})).collect()
                    });
                    out.push(json!({"id": format!("q{i}"), "header": s(&q["header"]), "question": q["question"].as_str()?,
                        "multi": q["multiSelect"] == true, "other": true, "secret": false, "options": opts}));
                }
                ask["kind"] = "question".into();
                ask["questions"] = out.into();
            }
            "ExitPlanMode" => {
                ask["kind"] = "plan".into();
                ask["plan"] = input["plan"].as_str()?.into();
            }
            _ => return None,
        }
        let (tx, rx) = oneshot::channel();
        self.cards.push(Card {
            ask,
            tool: tool.into(),
            input: input.clone(),
            prompt: req["prompt_id"].as_str().unwrap_or("").into(),
            tx,
            last_waiting: now,
        });
        Some(rx)
    }

    /// Delivers the user's decision to the hook.
    pub fn answer(&mut self, id: &str, a: &Answer) -> Result<(), String> {
        const GONE: &str = "already answered";
        let at = self.cards.iter().position(|c| c.ask["id"] == id).ok_or(GONE)?;
        let c = self.cards.remove(at);
        c.tx.send(decision(&c.tool, &c.input, a)).map_err(|_| GONE.to_string())
    }

    /// Reads one transcript line: opens uses of the card tools and settles cards whose use got a result.
    pub fn seen(&mut self, line: &str) {
        if line.contains("\"tool_use\"") && TOOLS.iter().any(|t| line.contains(t)) {
            if let Ok(Rec { prompt_id, sidechain: false, message: Some(m) }) = serde_json::from_str(line) {
                for b in m.content.iter().filter(|b| b["type"] == "tool_use") {
                    if let Some(name) = b["name"].as_str().filter(|n| TOOLS.contains(n)) {
                        self.uses.push(Use {
                            id: b["id"].as_str().unwrap_or("").into(),
                            prompt: prompt_id.clone(),
                            name: name.into(),
                            input: b["input"].clone(),
                        });
                    }
                }
            }
        }
        if !self.uses.is_empty() && line.contains("\"tool_result\"") {
            let Ok(Rec { message: Some(m), .. }) = serde_json::from_str(line) else { return };
            let done: Vec<&str> = m.content.iter().filter_map(|b| b["tool_use_id"].as_str()).collect();
            let closed = |u: &Use| done.contains(&u.id.as_str());
            let hit: Vec<usize> = self.matches().into_iter().enumerate()
                .filter(|(_, u)| u.is_some_and(|u| closed(&self.uses[u]))).map(|(c, _)| c).collect();
            for c in hit.into_iter().rev() {
                self.cards.remove(c);
            }
            self.uses.retain(|u| !closed(u));
        }
    }

    /// Settles cards the hook gave up on, or that nothing has been waiting for since `SETTLE_NET`.
    pub fn tick(&mut self, waiting: bool, now: Instant) {
        self.cards.retain(|c| !c.tx.is_closed());
        if waiting {
            self.cards.iter_mut().for_each(|c| c.last_waiting = now);
        } else {
            self.cards.retain(|c| now.saturating_duration_since(c.last_waiting) <= SETTLE_NET);
        }
    }

    pub fn list(&self) -> Vec<Value> {
        self.cards.iter().map(|c| c.ask.clone()).collect()
    }

    pub fn clear(&mut self) {
        self.cards.clear();
        self.uses.clear();
    }

    /// For each card, the open use it stands for: same prompt and tool, equal input first, else the oldest.
    fn matches(&self) -> Vec<Option<usize>> {
        let mut taken = Vec::new();
        self.cards.iter().map(|c| {
            let free = |i: &usize| {
                let u = &self.uses[*i];
                u.prompt == c.prompt && u.name == c.tool && !taken.contains(i)
            };
            // The record's input may lack keys the hook adds (planFilePath), so equal means "every key it has agrees".
            let same = |i: &usize| self.uses[*i].input.as_object().is_some_and(|o| o.iter().all(|(k, v)| c.input[k] == *v));
            let pick = (0..self.uses.len()).filter(free).find(same).or_else(|| (0..self.uses.len()).find(free));
            taken.extend(pick);
            pick
        }).collect()
    }
}

/// The hook decision for the user's answer.
pub fn decision(tool: &str, input: &Value, a: &Answer) -> Value {
    match tool {
        "ExitPlanMode" if a.choice.as_deref() == Some("yes") => json!({"behavior":"allow","updatedInput":{"plan":input["plan"]},
            "updatedPermissions":[{"type":"setMode","mode":"bypassPermissions","destination":"session"}]}),
        "ExitPlanMode" => {
            let t = a.text.as_deref().unwrap_or("").trim();
            json!({"behavior":"deny","message": if t.is_empty() { "Keep planning." } else { t }})
        }
        "AskUserQuestion" => {
            let mut answers = serde_json::Map::new();
            for (i, q) in input["questions"].as_array().into_iter().flatten().enumerate() {
                let r = a.answers.as_ref().map_or(&Value::Null, |m| &m[format!("q{i}")]);
                let mut vals: Vec<&str> = r["choices"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                let text = r["text"].as_str().unwrap_or("").trim();
                let v = if q["multiSelect"] == true {
                    vals.extend((!text.is_empty()).then_some(text));
                    json!(vals)
                } else {
                    json!(if text.is_empty() { vals.first().copied().unwrap_or("") } else { text })
                };
                answers.insert(q["question"].as_str().unwrap_or("").into(), v);
            }
            json!({"behavior":"allow","updatedInput":{"questions":input["questions"],"answers":answers}})
        }
        _ => json!({"behavior":"allow"}),
    }
}

fn new_id() -> String {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("the OS CSPRNG must be available");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn ask_request(prompt: &str) -> Value {
        json!({"session_id":"s","prompt_id":prompt,"permission_mode":"bypassPermissions","hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion",
            "tool_input":{"questions":[
                {"question":"Pick a color","header":"Color","options":[{"label":"Red","description":"Red"},{"label":"Blue","description":"Blue"}],"multiSelect":false},
                {"question":"Pick fruits","header":"Fruits","options":[{"label":"Apple","description":"Apple"},{"label":"Pear","description":"Pear"}],"multiSelect":true}]}})
    }
    pub(crate) fn plan_request(prompt: &str, plan: &str) -> Value {
        json!({"session_id":"s","prompt_id":prompt,"permission_mode":"plan","hook_event_name":"PermissionRequest","tool_name":"ExitPlanMode",
            "tool_input":{"plan":plan,"planFilePath":"/home/u/.claude/plans/p.md"}})
    }
    pub(crate) fn use_line(prompt: &str, id: &str, name: &str, input: &Value) -> String {
        json!({"type":"assistant","uuid":format!("u-{id}"),"promptId":prompt,"isSidechain":false,
            "message":{"id":"m","role":"assistant","content":[{"type":"tool_use","id":id,"name":name,"input":input}]}}).to_string()
    }
    pub(crate) fn result_line(id: &str) -> String {
        json!({"type":"user","uuid":format!("r-{id}"),"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":"done"}]}}).to_string()
    }


    fn question() -> Value {
        json!({"id":"a1","kind":"question","tool":"codex","questions":[
            {"id":"q0","header":"Color","question":"Pick a color","multi":false,"other":true,"secret":false,"options":[{"label":"Red","description":""},{"label":"Blue","description":""}]},
            {"id":"q1","header":"Fruits","question":"Pick fruits","multi":true,"other":false,"secret":false,"options":[{"label":"Apple","description":""},{"label":"Pear","description":""}]}]})
    }
    fn with(answers: Value) -> Answer {
        Answer { answers: Some(answers), ..Default::default() }
    }
    #[test]
    fn answers_must_cover_every_question_with_listed_labels() {
        let q = question();
        assert_eq!(check_answer(&q, &with(json!({"q0":{"choices":["Red"]},"q1":{"choices":["Apple","Pear"]}}))), Ok(()));
        assert_eq!(check_answer(&q, &with(json!({"q0":{"choices":[],"text":"Green"},"q1":{"choices":["Pear"]}}))), Ok(()));
        assert_eq!(check_answer(&q, &with(json!({"q0":{"choices":["Red"]}}))), Err("answer every question"));
        assert_eq!(check_answer(&q, &with(json!({"q0":{"choices":["Red"]},"q1":{"choices":[],"text":" "}}))), Err("answer every question"));
        for bad in [json!({"q0":{"choices":["Green"]},"q1":{"choices":["Pear"]}}),      // not an option
                    json!({"q0":{"choices":["Red","Blue"]},"q1":{"choices":["Pear"]}}), // two picks on a single choice
                    json!({"q0":{"choices":["Red"]},"q1":{"choices":["Pear"],"text":"Kiwi"}})] { // free text where none is allowed
            assert_eq!(check_answer(&q, &with(bad)), Err("not a valid answer"));
        }
        let long = "x".repeat(crate::chat::MAX_TEXT);
        assert_eq!(check_answer(&q, &with(json!({"q0":{"choices":[],"text":long},"q1":{"choices":["Pear"]}}))), Err("answer too long"));
    }
    #[test]
    fn plan_answers_are_yes_or_no() {
        let p = json!({"id":"a2","kind":"plan","tool":"claude","plan":"# P"});
        let c = |choice: &str, text: Option<&str>| Answer { choice: Some(choice.into()), text: text.map(Into::into), answers: None };
        assert_eq!(check_answer(&p, &c("yes", None)), Ok(()));
        assert_eq!(check_answer(&p, &c("no", Some("Add a step"))), Ok(()));
        assert_eq!(check_answer(&p, &c("approve", None)), Err("not a valid answer"));
    }
    #[tokio::test]
    async fn a_question_becomes_a_card_and_its_answer_is_the_exact_decision() {
        let mut cards = Cards::default();
        let rx = cards.add(&ask_request("p1"), Instant::now()).unwrap();
        let ask = cards.list().remove(0);
        let id = ask["id"].as_str().unwrap().to_string();
        assert!(id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(ask, json!({"id":id,"kind":"question","tool":"claude","questions":[
            {"id":"q0","header":"Color","question":"Pick a color","multi":false,"other":true,"secret":false,"options":[{"label":"Red","description":"Red"},{"label":"Blue","description":"Blue"}]},
            {"id":"q1","header":"Fruits","question":"Pick fruits","multi":true,"other":true,"secret":false,"options":[{"label":"Apple","description":"Apple"},{"label":"Pear","description":"Pear"}]}]}));
        let a = Answer { answers: Some(json!({"q0":{"choices":["Red"]},"q1":{"choices":["Apple","Pear"]}})), ..Default::default() };
        cards.answer(&id, &a).unwrap();
        assert_eq!(rx.await.unwrap(), json!({"behavior":"allow","updatedInput":{"questions":ask_request("p1")["tool_input"]["questions"],"answers":{"Pick a color":"Red","Pick fruits":["Apple","Pear"]}}}));
        assert!(cards.list().is_empty());
        assert_eq!(cards.answer(&id, &a), Err("already answered".into()));
    }
    #[test]
    fn every_decision_row_is_exactly_the_spec_s() {
        let input = json!({"plan":"# Plan\n1. a","planFilePath":"/p.md"});
        let c = |choice: &str, text: Option<&str>| Answer { choice: Some(choice.into()), text: text.map(Into::into), answers: None };
        assert_eq!(decision("ExitPlanMode", &input, &c("yes", None)), json!({"behavior":"allow","updatedInput":{"plan":"# Plan\n1. a"},"updatedPermissions":[{"type":"setMode","mode":"bypassPermissions","destination":"session"}]}));
        assert_eq!(decision("ExitPlanMode", &input, &c("no", Some("Add a third step"))), json!({"behavior":"deny","message":"Add a third step"}));
        assert_eq!(decision("ExitPlanMode", &input, &c("no", Some("  "))), json!({"behavior":"deny","message":"Keep planning."}));
        let free = Answer { answers: Some(json!({"q0":{"choices":["Red"],"text":"Teal"},"q1":{"choices":["Pear"],"text":"Kiwi"}})), ..Default::default() };
        assert_eq!(decision("AskUserQuestion", &ask_request("p")["tool_input"], &free)["updatedInput"]["answers"],
                   json!({"Pick a color":"Teal","Pick fruits":["Pear","Kiwi"]}), "free text wins a single choice and joins a multiple one");
    }
    #[tokio::test]
    async fn a_card_settles_when_its_tool_use_gets_a_result_even_if_the_record_came_first() {
        let mut cards = Cards::default();
        let input = ask_request("p1")["tool_input"].clone();
        cards.seen(&use_line("p1", "toolu_1", "AskUserQuestion", &input)); // the record raced ahead of the hook
        let rx = cards.add(&ask_request("p1"), Instant::now()).unwrap();
        cards.seen(&result_line("toolu_other"));
        assert_eq!(cards.list().len(), 1);
        cards.seen(&result_line("toolu_1"));
        assert!(cards.list().is_empty());
        assert!(rx.await.is_err(), "settled elsewhere: the hook gets no decision");
    }
    #[tokio::test]
    async fn matching_takes_equal_input_then_the_oldest_so_a_stale_plan_record_still_settles() {
        let mut cards = Cards::default();
        let rx = cards.add(&plan_request("p2", "# New plan"), Instant::now()).unwrap();
        cards.seen(&use_line("p2", "toolu_old", "ExitPlanMode", &json!({"plan":"# Previous plan"}))); // E1c
        cards.seen(&use_line("p9", "toolu_x", "ExitPlanMode", &json!({"plan":"# New plan"})));
        cards.seen(&result_line("toolu_x"));
        assert_eq!(cards.list().len(), 1, "another prompt_id never matches");
        cards.seen(&result_line("toolu_old"));
        assert!(cards.list().is_empty() && rx.await.is_err());
        let rx = cards.add(&plan_request("p3", "# B"), Instant::now()).unwrap();
        cards.seen(&use_line("p3", "toolu_a", "ExitPlanMode", &json!({"plan":"# A"})));
        cards.seen(&use_line("p3", "toolu_b", "ExitPlanMode", &json!({"plan":"# B"})));
        cards.seen(&result_line("toolu_a"));
        assert_eq!(cards.list().len(), 1, "equal input beats age");
        cards.seen(&result_line("toolu_b"));
        assert!(cards.list().is_empty() && rx.await.is_err());
    }
    #[tokio::test]
    async fn the_status_net_and_a_closed_connection_settle_cards() {
        let t0 = Instant::now();
        let mut cards = Cards::default();
        let rx = cards.add(&ask_request("p1"), t0).unwrap();
        cards.tick(true, t0 + Duration::from_secs(5));
        cards.tick(false, t0 + Duration::from_secs(6));
        assert_eq!(cards.list().len(), 1, "2 s after the last waiting, not after arrival");
        cards.tick(false, t0 + Duration::from_millis(7100));
        assert!(cards.list().is_empty() && rx.await.is_err());
        drop(cards.add(&ask_request("p2"), t0).unwrap()); // Claude closed the hook connection
        cards.tick(true, t0);
        assert!(cards.list().is_empty());
    }
    #[tokio::test]
    async fn two_pending_cards_are_answered_and_settled_independently() {
        let mut cards = Cards::default();
        let now = Instant::now();
        let rx1 = cards.add(&ask_request("p1"), now).unwrap();
        let rx2 = cards.add(&plan_request("p1", "# P"), now).unwrap();
        let ids: Vec<String> = cards.list().iter().map(|a| a["id"].as_str().unwrap().to_string()).collect();
        assert_ne!(ids[0], ids[1]);
        cards.seen(&use_line("p1", "toolu_q", "AskUserQuestion", &ask_request("p1")["tool_input"]));
        cards.answer(&ids[1], &Answer { choice: Some("yes".into()), ..Default::default() }).unwrap();
        assert_eq!(cards.list().len(), 1);
        cards.seen(&result_line("toolu_q"));
        assert!(cards.list().is_empty());
        assert!(rx1.await.is_err());
        assert_eq!(rx2.await.unwrap()["behavior"], "allow");
    }
    #[test]
    fn requests_that_cannot_be_cards_are_refused() {
        let mut cards = Cards::default();
        for req in [json!({"tool_name":"Bash","tool_input":{"command":"ls"}}),
                    json!({"tool_name":"AskUserQuestion","tool_input":{"questions":[]}}),
                    json!({"tool_name":"ExitPlanMode","tool_input":{}})] {
            assert!(cards.add(&req, Instant::now()).is_none());
        }
        assert!(cards.list().is_empty());
    }
}
