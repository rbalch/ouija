//! Building requests: question objects, `run` spec validation, shell quoting, URLs.

use serde_json::{Map, Value, json};

/// Lowercase names that are special in zsh or bash. `run` keys are lowercase
/// only, so uppercase specials (`PATH`, `IFS`, `LD_PRELOAD`, ...) cannot occur.
/// Matched case-insensitively.
const RESERVED_KEYS: &[&str] = &[
    "path",
    "fpath",
    "cdpath",
    "manpath",
    "module_path",
    "mailpath",
    "status",
    "argv",
    "options",
    "pipestatus",
    "prompt",
    "psvar",
    "signals",
    "parameters",
    "commands",
    "functions",
    "aliases",
    "history",
    "histchars",
    "home",
    "term",
    "user",
    "pwd",
    "oldpwd",
    "ifs",
    "lang",
    "ps1",
    "ps2",
    "ps3",
    "ps4",
    "watch",
    "mail",
    "path_",
];

/// Prefixes of loader and locale variables.
const RESERVED_PREFIXES: &[&str] = &["ld_", "dyld_", "lc_"];

/// Keys become shell variable names in `run` output, so they must be lowercase
/// identifiers (`[a-z][a-z0-9_]*`) that do not collide with known shell specials.
pub fn valid_run_key(key: &str) -> Result<(), String> {
    let mut chars = key.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !ok {
        return Err(format!(
            "spec key `{key}` is not valid: use lowercase letters, digits and underscores, starting with a letter ([a-z][a-z0-9_]*)"
        ));
    }
    let lower = key.to_ascii_lowercase();
    if RESERVED_KEYS.contains(&lower.as_str())
        || RESERVED_PREFIXES.iter().any(|p| lower.starts_with(p))
    {
        return Err(format!(
            "spec key `{key}` is a reserved shell variable name; pick another"
        ));
    }
    Ok(())
}

/// Quote a value for safe use in a shell word. Plain tokens are left bare.
pub fn shell_word(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Split an option of the form `key` or `key=description`. A bare key is its own description.
pub fn parse_option(opt: &str) -> Result<(&str, &str), String> {
    let (k, d) = opt.split_once('=').unwrap_or((opt, opt));
    if k.is_empty() {
        return Err(format!("empty option key in `{opt}`"));
    }
    if d.is_empty() {
        return Err(format!("empty description for option `{k}`"));
    }
    Ok((k, d))
}

/// Reject empty or duplicate score levels.
pub fn check_levels(levels: &[String]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for l in levels {
        if l.is_empty() {
            return Err("empty score level".to_string());
        }
        if !seen.insert(l.as_str()) {
            return Err(format!("duplicate score level `{l}`"));
        }
    }
    Ok(())
}

/// The `questions` object for a single yes/no question.
pub fn noul_questions(question: &str) -> Result<Value, String> {
    check_question(question)?;
    Ok(json!({ "answer": { "type": "noul", "instructions": question } }))
}

/// Reject an empty or whitespace-only question.
pub fn check_question(question: &str) -> Result<(), String> {
    if question.trim().is_empty() {
        Err("the question must not be empty".to_string())
    } else {
        Ok(())
    }
}

/// The `questions` object for a single multiple-choice question.
pub fn choice_questions(question: &str, options: &[String]) -> Result<Value, String> {
    check_question(question)?;
    let mut criteria = Map::new();
    for opt in options {
        let (k, d) = parse_option(opt)?;
        if criteria.insert(k.to_string(), json!(d)).is_some() {
            return Err(format!("duplicate option `{k}`"));
        }
    }
    Ok(json!({ "answer": { "type": "choice", "instructions": question, "criteria": criteria } }))
}

/// The `questions` object for a single scored question.
pub fn score_questions(question: &str, levels: &[String]) -> Result<Value, String> {
    check_question(question)?;
    check_levels(levels)?;
    Ok(json!({ "answer": { "type": "score", "instructions": question, "criteria": levels } }))
}

/// Parse and validate the text of a `run` spec. `name` is used in error messages.
pub fn parse_run_spec(text: &str, name: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("parsing {name}: {e}"))?;
    match v.as_object() {
        Some(o) if !o.is_empty() => {
            o.keys().try_for_each(|k| valid_run_key(k))?;
            Ok(v)
        }
        _ => Err(format!(
            "{name}: spec must be a non-empty JSON object of questions"
        )),
    }
}

/// Accept a bare `host:port` by assuming plain http, and drop trailing slashes.
pub fn normalize_url(raw: &str) -> String {
    let raw = raw.trim();
    let base = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    };
    base.trim_end_matches('/').to_string()
}

/// Extract `model;dur=N` from a Server-Timing header value.
pub fn model_ms(header: &str) -> Option<f64> {
    header
        .split(',')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("model;dur=")?.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_keys() {
        for ok in ["a", "x_", "page_now", "a1_b2", "pathx", "ld"] {
            assert!(valid_run_key(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "1a", "_x", "a-b", "a b", "a;b", "$x", "a.b", "\u{e9}", "Page", "A", "PATH", "IFS",
        ] {
            let e = valid_run_key(bad).unwrap_err();
            assert!(e.contains(&format!("`{bad}`")), "{e}");
        }
        for reserved in [
            "path",
            "fpath",
            "cdpath",
            "status",
            "argv",
            "options",
            "pipestatus",
            "home",
            "ifs",
            "ps1",
            "ps4",
            "pwd",
            "term",
            "user",
            "lang",
            "path_",
            "ld_preload",
            "ld_library_path",
            "dyld_insert_libraries",
            "lc_all",
        ] {
            let e = valid_run_key(reserved).unwrap_err();
            assert!(e.contains("reserved") && e.contains(reserved), "{e}");
        }
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(shell_word("fix"), "fix");
        assert_eq!(shell_word("a_b-c.d"), "a_b-c.d");
        assert_eq!(shell_word(""), "''");
        assert_eq!(shell_word("within an hour"), "'within an hour'");
        assert_eq!(shell_word("it's"), r"'it'\''s'");
        assert_eq!(shell_word("$(rm -rf /)"), "'$(rm -rf /)'");
        assert_eq!(shell_word("a\nb"), "'a\nb'");
    }

    #[test]
    fn option_parsing() {
        assert_eq!(parse_option("fix"), Ok(("fix", "fix")));
        assert_eq!(
            parse_option("fix=Repair a bug"),
            Ok(("fix", "Repair a bug"))
        );
        // Only the first `=` splits.
        assert_eq!(parse_option("k=a=b"), Ok(("k", "a=b")));
        assert!(parse_option("=desc").is_err());
        assert!(
            parse_option("fix=")
                .unwrap_err()
                .contains("empty description")
        );
        assert!(parse_option("").is_err());
    }

    #[test]
    fn choice_building() {
        let opts: Vec<String> = ["feat", "fix=Repair"].map(String::from).into();
        let q = choice_questions("Type?", &opts).unwrap();
        assert_eq!(q["answer"]["type"], "choice");
        assert_eq!(q["answer"]["criteria"]["feat"], "feat");
        assert_eq!(q["answer"]["criteria"]["fix"], "Repair");

        let dup: Vec<String> = ["a", "a=again"].map(String::from).into();
        assert!(
            choice_questions("Q?", &dup)
                .unwrap_err()
                .contains("duplicate")
        );
    }

    #[test]
    fn score_building() {
        let lv: Vec<String> = ["low", "high"].map(String::from).into();
        assert!(score_questions("  ", &lv).is_err());
        let q = score_questions("How?", &lv).unwrap();
        assert_eq!(q["answer"]["criteria"], json!(["low", "high"]));
        assert!(score_questions("How?", &["a".into(), "a".into()]).is_err());
        assert!(score_questions("How?", &["a".into(), "".into()]).is_err());
    }

    #[test]
    fn run_spec() {
        let ok = r#"{"a":{"type":"noul","instructions":"?"}}"#;
        assert!(parse_run_spec(ok, "s.json").is_ok());
        assert!(
            parse_run_spec("{}", "s.json")
                .unwrap_err()
                .contains("non-empty")
        );
        assert!(parse_run_spec("[]", "s.json").is_err());
        assert!(
            parse_run_spec("nope", "s.json")
                .unwrap_err()
                .starts_with("parsing s.json")
        );
        assert!(parse_run_spec(r#"{"bad-key":{}}"#, "s.json").is_err());
        assert!(parse_run_spec(r#"{"PATH":{}}"#, "s.json").is_err());
        assert!(parse_run_spec(r#"{"path":{}}"#, "s.json").is_err());
    }

    #[test]
    fn url_normalization() {
        assert_eq!(normalize_url("spark:18440"), "http://spark:18440");
        assert_eq!(normalize_url("http://spark:18440/"), "http://spark:18440");
        assert_eq!(
            normalize_url("https://x.example/base//"),
            "https://x.example/base"
        );
        assert_eq!(normalize_url("  localhost:1 "), "http://localhost:1");
    }

    #[test]
    fn server_timing() {
        assert_eq!(model_ms("model;dur=312.5"), Some(312.5));
        assert_eq!(model_ms("queue;dur=1, model;dur=40"), Some(40.0));
        assert_eq!(model_ms("queue;dur=1"), None);
        assert_eq!(model_ms("model;dur=abc"), None);
    }
}
