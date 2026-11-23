//! Interpreting server answers and mapping them to exit codes.

use serde_json::Value;

/// Result of interpreting one answer.
#[derive(Debug)]
pub struct Outcome {
    /// What goes to stdout.
    pub text: String,
    /// One-line detail without timing.
    pub line: String,
    /// (label, probability) sorted descending, for the bar chart.
    pub bars: Vec<(String, f64)>,
    pub confidence: f64,
    /// Set for noul answers only.
    pub verdict: Option<bool>,
}

impl Outcome {
    pub fn uncertain(&self, min_conf: f64) -> bool {
        self.confidence < min_conf
    }
}

/// Exit code for a finished request.
///
/// `uncertain` is the first answer's state for single questions and "any answer
/// uncertain" for `run`. `verdict` is the noul verdict of a single question.
/// `run` never exits 1.
pub fn exit_code(multi: bool, uncertain: bool, verdict: Option<bool>) -> u8 {
    if uncertain {
        2
    } else if !multi && verdict == Some(false) {
        1
    } else {
        0
    }
}

fn prob(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

pub fn sorted(mut v: Vec<(String, f64)>) -> Vec<(String, f64)> {
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v
}

pub fn join_bars(bars: &[(String, f64)]) -> String {
    bars.iter()
        .map(|(k, p)| format!("{k} {p:.2}"))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Turn one server answer into printable pieces. `question` is the request's
/// question object, used as a fallback source of score labels.
pub fn interpret(key: &str, ans: &Value, question: &Value) -> Result<Outcome, String> {
    let kind = ans.get("type").and_then(Value::as_str).unwrap_or("");
    let missing = |f: &str| format!("answer `{key}` has no `{f}`");
    match kind {
        "noul" => {
            let p = ans
                .get("noul")
                .and_then(Value::as_f64)
                .ok_or_else(|| missing("noul"))?;
            let confidence = (p - 0.5).abs() * 2.0;
            let yes = p >= 0.5;
            Ok(Outcome {
                text: yes.to_string(),
                line: format!("P(true)={p:.3}  conf {confidence:.2}"),
                bars: vec![("true".into(), p), ("false".into(), 1.0 - p)],
                confidence,
                verdict: Some(yes),
            })
        }
        "choice" => {
            let probs = ans
                .get("probabilities")
                .and_then(Value::as_object)
                .ok_or_else(|| missing("probabilities"))?;
            let chosen = ans
                .get("choice")
                .and_then(Value::as_str)
                .ok_or_else(|| missing("choice"))?;
            let bars = sorted(probs.iter().map(|(k, v)| (k.clone(), prob(v))).collect());
            let line = join_bars(&bars);
            let confidence = ans
                .get("confidence")
                .and_then(Value::as_f64)
                .ok_or_else(|| missing("confidence"))?;
            Ok(Outcome {
                text: chosen.to_string(),
                line,
                bars,
                confidence,
                verdict: None,
            })
        }
        "score" => {
            let probs = ans
                .get("probabilities")
                .and_then(Value::as_object)
                .ok_or_else(|| missing("probabilities"))?;
            let legend = ans.get("legend").and_then(Value::as_object);
            // Fall back to the question's own criteria list if the legend is absent.
            let label = |idx: &str| -> String {
                legend
                    .and_then(|l| l.get(idx))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        let i: usize = idx.parse().ok()?;
                        question
                            .get("criteria")?
                            .get(i)?
                            .as_str()
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| idx.to_string())
            };
            let bars = sorted(probs.iter().map(|(k, v)| (label(k), prob(v))).collect());
            let score = ans
                .get("score")
                .and_then(Value::as_f64)
                .ok_or_else(|| missing("score"))?;
            let line = format!("E={score:.2}  {}", join_bars(&bars));
            let confidence = ans
                .get("confidence")
                .and_then(Value::as_f64)
                .ok_or_else(|| missing("confidence"))?;
            let text = bars
                .first()
                .map(|b| b.0.clone())
                .ok_or_else(|| missing("probabilities"))?;
            Ok(Outcome {
                text,
                line,
                bars,
                confidence,
                verdict: None,
            })
        }
        other => Err(format!("answer `{key}` has unknown type `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exit_codes() {
        // single question
        assert_eq!(exit_code(false, false, Some(true)), 0);
        assert_eq!(exit_code(false, false, Some(false)), 1);
        assert_eq!(exit_code(false, false, None), 0);
        assert_eq!(exit_code(false, true, Some(true)), 2);
        // uncertain wins over a false verdict
        assert_eq!(exit_code(false, true, Some(false)), 2);
        // run never exits 1
        assert_eq!(exit_code(true, false, Some(false)), 0);
        assert_eq!(exit_code(true, true, None), 2);
    }

    #[test]
    fn noul_confidence() {
        let q = json!({});
        let o = interpret("a", &json!({"type":"noul","noul":0.95}), &q).unwrap();
        assert_eq!(o.text, "true");
        assert_eq!(o.verdict, Some(true));
        assert!((o.confidence - 0.9).abs() < 1e-9);
        assert!(!o.uncertain(0.8));
        assert!(o.uncertain(0.95));

        let o = interpret("a", &json!({"type":"noul","noul":0.1}), &q).unwrap();
        assert_eq!(o.text, "false");
        assert_eq!(o.verdict, Some(false));
        assert!((o.confidence - 0.8).abs() < 1e-9);

        // exactly 0.5 is "true" but has zero confidence
        let o = interpret("a", &json!({"type":"noul","noul":0.5}), &q).unwrap();
        assert_eq!(o.verdict, Some(true));
        assert!(o.uncertain(0.8));
    }

    #[test]
    fn choice_answer() {
        let ans = json!({"type":"choice","choice":"fix","confidence":0.97,
            "probabilities":{"feat":0.02,"fix":0.97,"chore":0.01}});
        let o = interpret("answer", &ans, &json!({})).unwrap();
        assert_eq!(o.text, "fix");
        assert_eq!(o.bars[0].0, "fix");
        assert!(o.line.starts_with("fix 0.97 | feat 0.02"));
        assert_eq!(o.verdict, None);
    }

    #[test]
    fn score_labels_from_legend_and_criteria() {
        let ans = json!({"type":"score","score":2.4,"confidence":0.7,
            "legend":{"0":"low","1":"high"},"probabilities":{"0":0.1,"1":0.9}});
        let o = interpret("a", &ans, &json!({})).unwrap();
        assert_eq!(o.text, "high");
        assert!(o.line.starts_with("E=2.40"));

        let ans = json!({"type":"score","score":0.1,"confidence":0.7,
            "probabilities":{"0":0.8,"1":0.2}});
        let q = json!({"criteria":["low","high"]});
        assert_eq!(interpret("a", &ans, &q).unwrap().text, "low");
    }

    #[test]
    fn malformed_answers() {
        let q = json!({});
        assert!(interpret("a", &json!({"type":"noul"}), &q).is_err());
        assert!(
            interpret("a", &json!({"type":"wat"}), &q)
                .unwrap_err()
                .contains("unknown type")
        );
        assert!(interpret("a", &json!({"type":"choice","probabilities":{}}), &q).is_err());
    }
}
