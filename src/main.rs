//! ouija: ask a System One decision model typed questions from the shell.
//!
//! Answer goes to stdout, details go to stderr, and the verdict is the exit code.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};

use ouija::answer::{Outcome, exit_code, interpret};
use ouija::config::{self, Flags, Settings};
use ouija::flavor::{self, Mood};
use ouija::spec;

const BAR_WIDTH: usize = 20;

const AFTER_HELP: &str = "\
EXAMPLES:
  ouija noul   \"Is anything broken?\"                         < log.txt
  ouija choice \"Commit type?\" feat fix \"refactor=Restructure without behavior change\" chore
  ouija score  \"Severity?\" cosmetic minor major critical      < incident.txt
  ouija run examples/triage.json                              < email.txt
  ouija config --check

OUTPUT:
  stdout  the answer only (true/false, chosen key, or chosen level)
  stderr  one-line detail with probabilities (-v for bars, -q for none)

EXIT CODES:
  0  noul true, or a confident choice/score (run: all answers confident)
  1  noul false (confident)
  2  uncertain: confidence below --min-conf
  3  error: server down, HTTP error, bad config or arguments, input too long

CONFIG:
  $OUIJA_CONFIG, else $XDG_CONFIG_HOME/ouija/config.toml, else ~/.config/ouija/config.toml
  Precedence: flag > env (OUIJA_URL, legacy KARPO_URL) > config file > default.
  Run `ouija config` to see each setting and where it came from.

Confidence is a ranking signal, not a calibrated probability.";

#[derive(Parser)]
#[command(
    name = "ouija",
    version,
    about = "Ask a System One decision model typed questions (yes/no, choice, score)",
    after_help = AFTER_HELP
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
    #[command(flatten)]
    common: Common,
}

#[derive(Args)]
struct Common {
    /// Server base URL
    /// (default: $OUIJA_URL, then $KARPO_URL, then config file, then http://localhost:18440)
    #[arg(long, global = true)]
    url: Option<String>,
    /// Confidence below this is reported as uncertain (exit 2) [default: 0.8]
    #[arg(long, global = true)]
    min_conf: Option<f64>,
    /// Request timeout in seconds, 1-86400 [default: 60]
    #[arg(long, global = true)]
    timeout: Option<u64>,
    /// Print the raw server response JSON to stdout instead of the answer
    #[arg(long, global = true)]
    json: bool,
    /// Print a per-option bar chart to stderr instead of the one-line detail
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Suppress stderr detail
    #[arg(short, long, global = true)]
    quiet: bool,
    /// Never print the flavor line (also: `flavor = false` in the config file)
    #[arg(long, global = true)]
    plain: bool,
    /// State text (or a JSON object) instead of reading stdin
    #[arg(long, global = true, conflicts_with = "state_file")]
    state: Option<String>,
    /// Read state from a file instead of stdin
    #[arg(long, global = true, value_name = "PATH")]
    state_file: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Yes/no question: prints true or false
    Noul {
        /// The question
        question: String,
    },
    /// Pick one option: prints the chosen key
    Choice {
        /// The question
        question: String,
        /// Options as `key` or `key=description` (at least 2)
        #[arg(required = true, num_args = 2..)]
        options: Vec<String>,
    },
    /// Rate on a scale: prints the chosen level label
    Score {
        /// The question
        question: String,
        /// Levels from lowest to highest (at least 2)
        #[arg(required = true, num_args = 2..)]
        levels: Vec<String>,
    },
    /// Ask many questions in one pass from a JSON spec (the `questions` object)
    Run {
        /// Path to the spec file
        spec: String,
    },
    /// Show the resolved settings and where each came from
    Config {
        /// Also contact the server's /healthz and exit 3 if it is unreachable
        #[arg(long)]
        check: bool,
    },
}

/// Write one line to stdout, reporting failures (closed pipe, full disk) as errors.
fn out_line(line: &str) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}")
        .and_then(|()| out.flush())
        .map_err(|e| format!("writing stdout: {e}"))
}

fn main() -> ExitCode {
    // Never let a panic surface as an abort (exit 134).
    std::panic::set_hook(Box::new(|info| {
        eprintln!("ouija: internal error: {info}");
        std::process::exit(3);
    }));
    // clap exits with 2 on usage errors, which collides with "uncertain". Map to 3.
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let code = if e.use_stderr() { 3 } else { 0 };
            let _ = e.print();
            return ExitCode::from(code);
        }
    };
    match run(cli) {
        Ok(code) => ExitCode::from(code),
        Err(msg) => {
            eprintln!("ouija: {msg}");
            ExitCode::from(3)
        }
    }
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Load the config file (if any) and resolve all settings.
/// Returns the settings, the config path, and whether the file exists.
fn load_settings(c: &Common) -> Result<(Settings, Option<PathBuf>, bool), String> {
    let path = config::config_path(&env_var);
    let file = match &path {
        Some(p) => config::load_file(p)?,
        None => None,
    };
    let flags = Flags {
        url: c.url.clone(),
        min_conf: c.min_conf,
        timeout: c.timeout,
        plain: c.plain,
    };
    let settings = config::resolve(&flags, &env_var, path.as_deref().zip(file.as_ref()))?;
    let exists = file.is_some();
    Ok((settings, path, exists))
}

fn run(cli: Cli) -> Result<u8, String> {
    let c = &cli.common;
    let (settings, path, exists) = load_settings(c)?;
    if let Cmd::Config { check } = &cli.command {
        return show_config(&settings, path.as_deref(), exists, *check);
    }

    let questions = match &cli.command {
        Cmd::Noul { question } => spec::noul_questions(question)?,
        Cmd::Choice { question, options } => spec::choice_questions(question, options)?,
        Cmd::Score { question, levels } => spec::score_questions(question, levels)?,
        Cmd::Run { spec: path } => {
            let text = std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?;
            spec::parse_run_spec(&text, path)?
        }
        Cmd::Config { .. } => unreachable!("handled above"),
    };
    let state = read_state(c)?;
    let body = json!({ "state": state, "questions": questions }).to_string();
    let (resp, ms) = post(&settings, &body)?;

    if c.json {
        out_line(&resp.to_string())?;
    }
    let color = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let answers = resp
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| "response has no `answers` object".to_string())?;
    let multi = matches!(cli.command, Cmd::Run { .. });
    let min_conf = settings.min_conf.value;

    let mut any_uncertain = false;
    let mut first: Option<(Outcome, bool)> = None;
    for (key, q) in questions.as_object().into_iter().flatten() {
        let ans = answers
            .get(key)
            .ok_or_else(|| format!("response is missing answer `{key}`"))?;
        let out = interpret(key, ans, q)?;
        let uncertain = out.uncertain(min_conf);
        any_uncertain |= uncertain;
        if !c.json {
            if multi {
                out_line(&format!("{key}={}", spec::shell_word(&out.text)))?;
            } else {
                out_line(&out.text)?;
            }
        }
        if !c.quiet {
            let prefix = if multi {
                format!("{key}: ")
            } else {
                String::new()
            };
            report(&out, &prefix, ms, uncertain, min_conf, c.verbose, color);
        }
        if first.is_none() {
            first = Some((out, uncertain));
        }
    }

    let (first_out, first_uncertain) =
        first.ok_or_else(|| "request has no questions".to_string())?;
    let uncertain = if multi {
        any_uncertain
    } else {
        first_uncertain
    };
    let code = exit_code(multi, uncertain, first_out.verdict);

    let tty = std::io::stderr().is_terminal();
    if flavor::should_show(settings.flavor.value, c.quiet, c.json, tty) {
        let (mood, answer) = if uncertain {
            (Mood::Uncertain, "")
        } else if multi {
            (Mood::Answer, "the whole docket")
        } else {
            match first_out.verdict {
                Some(true) => (Mood::Yes, ""),
                Some(false) => (Mood::No, ""),
                None => (Mood::Answer, first_out.text.as_str()),
            }
        };
        let seed = resp
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| first_out.line.clone(), str::to_string);
        eprintln!("{}", flavor::line(mood, answer, &seed));
    }
    Ok(code)
}

/// `ouija config`: print each resolved setting and its source.
fn show_config(
    s: &Settings,
    path: Option<&std::path::Path>,
    exists: bool,
    check: bool,
) -> Result<u8, String> {
    let rows = [
        ("url", s.url.value.clone(), s.url.source.to_string()),
        (
            "min_conf",
            s.min_conf.value.to_string(),
            s.min_conf.source.to_string(),
        ),
        (
            "timeout",
            s.timeout.value.to_string(),
            s.timeout.source.to_string(),
        ),
        (
            "flavor",
            s.flavor.value.to_string(),
            s.flavor.source.to_string(),
        ),
    ];
    let width = rows.iter().map(|r| r.1.chars().count()).max().unwrap_or(0);
    for (name, value, source) in &rows {
        out_line(&format!("{name:<10}{value:<width$}   ({source})"))?;
    }
    match path {
        Some(p) => out_line(&format!(
            "\nconfig file: {} ({})",
            p.display(),
            if exists { "found" } else { "not found" }
        ))?,
        None => out_line("\nconfig file: none (set HOME, XDG_CONFIG_HOME or OUIJA_CONFIG)")?,
    }
    if check {
        let health = healthz(s)?;
        out_line(&format!("server: ok ({health})"))?;
    }
    Ok(0)
}

/// GET /healthz and return a short description of the server.
fn healthz(s: &Settings) -> Result<String, String> {
    let url = format!("{}/healthz", spec::normalize_url(&s.url.value));
    let mut resp = agent(s.timeout.value)
        .get(&url)
        .call()
        .map_err(|e| format!("request to {url} failed: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("reading response: {e}"))?;
    if status != 200 {
        return Err(format!("server returned HTTP {status}: {}", text.trim()));
    }
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok(v.get("model")
        .and_then(Value::as_str)
        .unwrap_or("healthy")
        .to_string())
}

/// Read the state from --state, --state-file or stdin. A JSON object is sent as an object.
///
/// Run interactively with nothing piped in and no state flag, the question is asked on its
/// own (empty state), e.g. `ouija noul "is the sky blue?"`. Empty input that *was* provided
/// (an empty pipe or file) is still an error: in scripts that usually means something
/// upstream failed.
fn read_state(c: &Common) -> Result<Value, String> {
    let text = if let Some(s) = &c.state {
        s.clone()
    } else if let Some(p) = &c.state_file {
        let bytes = std::fs::read(p).map_err(|e| format!("reading {p}: {e}"))?;
        String::from_utf8_lossy(&bytes).into_owned()
    } else if std::io::stdin().is_terminal() {
        return Ok(Value::String(String::new()));
    } else {
        // Read bytes and convert lossily: truncated or Latin-1 input still works.
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .map_err(|e| format!("reading stdin: {e}"))?;
        String::from_utf8_lossy(&buf).into_owned()
    };
    if text.trim().is_empty() {
        return Err("state is empty".to_string());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(v) if v.is_object() => Ok(v),
        _ => Ok(Value::String(text)),
    }
}

fn agent(timeout_secs: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout_secs)))
        .http_status_as_error(false)
        .build()
        .into()
}

/// POST the request. Returns the parsed response and the model time in ms.
fn post(s: &Settings, body: &str) -> Result<(Value, f64), String> {
    let url = format!("{}/v1/systemone", spec::normalize_url(&s.url.value));
    let start = Instant::now();
    let mut resp = agent(s.timeout.value)
        .post(&url)
        .header("Content-Type", "application/json")
        .send(body)
        .map_err(|e| format!("request to {url} failed: {e}"))?;
    let wall = start.elapsed().as_secs_f64() * 1000.0;
    let status = resp.status().as_u16();
    let timing = resp
        .headers()
        .get("server-timing")
        .and_then(|v| v.to_str().ok())
        .and_then(spec::model_ms);
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("reading response: {e}"))?;
    if status != 200 {
        return Err(format!("server returned HTTP {status}: {}", text.trim()));
    }
    let v = serde_json::from_str(&text).map_err(|e| format!("invalid JSON response: {e}"))?;
    Ok((v, timing.unwrap_or(wall)))
}

/// Print the stderr detail for one answer.
fn report(
    out: &Outcome,
    prefix: &str,
    ms: f64,
    uncertain: bool,
    min_conf: f64,
    verbose: bool,
    color: bool,
) {
    let (dim, yellow, reset) = if color {
        ("\x1b[2m", "\x1b[33m", "\x1b[0m")
    } else {
        ("", "", "")
    };
    let warn = if uncertain {
        format!("  {yellow}\u{26a0} uncertain (< {min_conf:.2}){reset}")
    } else {
        String::new()
    };
    if verbose {
        eprintln!("{prefix}{dim}({ms:.0} ms){reset}{warn}");
        let width = out
            .bars
            .iter()
            .map(|b| b.0.chars().count())
            .max()
            .unwrap_or(0);
        for (label, p) in &out.bars {
            let n = (p.clamp(0.0, 1.0) * BAR_WIDTH as f64).round() as usize;
            eprintln!(
                "  {label:<width$}  {}{} {p:.2}",
                "\u{2588}".repeat(n),
                " ".repeat(BAR_WIDTH - n)
            );
        }
    } else {
        eprintln!("{prefix}{}  {dim}({ms:.0} ms){reset}{warn}", out.line);
    }
}
