//! Checking an answer against the ask it claims to answer.

use super::hub::Answer;
use serde_json::Value;

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
