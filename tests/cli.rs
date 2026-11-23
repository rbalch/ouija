//! End-to-end tests: run the real binary against an in-process mock server.
//!
//! No network beyond 127.0.0.1 is used, so these pass offline.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[derive(Clone)]
enum Mode {
    /// 200 with `{"answers": ...}` and a Server-Timing header.
    Answers(Value),
    /// Fixed status and body.
    Status(u16, &'static str),
    /// Read the request, then never answer.
    Hang,
}

struct Mock {
    url: String,
    bodies: Arc<Mutex<Vec<String>>>,
}

impl Mock {
    /// The last JSON request body received on /v1/systemone.
    fn last_request(&self) -> Value {
        let bodies = self.bodies.lock().unwrap();
        serde_json::from_str(bodies.last().expect("no request received")).unwrap()
    }
}

fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let path = head.split_whitespace().nth(1)?.to_string();
    let len = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")?
                .trim()
                .parse()
                .ok()
        })
        .unwrap_or(0usize);
    while buf.len() < header_end + len {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..]).to_string();
    Some((path, body))
}

fn respond(stream: &mut TcpStream, status: u16, extra: &str, body: &str) {
    let msg = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(msg.as_bytes());
}

fn mock(mode: Mode) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&bodies);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mode = mode.clone();
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || {
                let Some((path, body)) = read_request(&mut stream) else {
                    return;
                };
                if path == "/healthz" {
                    respond(
                        &mut stream,
                        200,
                        "",
                        r#"{"ok": true, "model": "mock-model"}"#,
                    );
                    return;
                }
                seen.lock().unwrap().push(body);
                match mode {
                    Mode::Answers(answers) => {
                        let body = json!({"id": "decision-test", "answers": answers}).to_string();
                        respond(
                            &mut stream,
                            200,
                            "Server-Timing: model;dur=123.4\r\n",
                            &body,
                        );
                    }
                    Mode::Status(code, body) => respond(&mut stream, code, "", body),
                    Mode::Hang => std::thread::sleep(Duration::from_secs(10)),
                }
            });
        }
    });
    Mock { url, bodies }
}

fn noul(p: f64) -> Mode {
    Mode::Answers(json!({"answer": {"type": "noul", "noul": p}}))
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fresh scratch directory for one test.
fn scratch() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ouija-cli-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Run<'a> {
    args: Vec<&'a str>,
    stdin: Vec<u8>,
    env: Vec<(&'a str, String)>,
}

fn ouija<'a>(args: &[&'a str]) -> Run<'a> {
    Run {
        args: args.to_vec(),
        stdin: b"some state".to_vec(),
        env: Vec::new(),
    }
}

impl<'a> Run<'a> {
    fn stdin(mut self, b: &[u8]) -> Self {
        self.stdin = b.to_vec();
        self
    }
    fn env(mut self, k: &'a str, v: impl Into<String>) -> Self {
        self.env.push((k, v.into()));
        self
    }
    fn go(self) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ouija"));
        cmd.args(&self.args)
            .env_clear()
            .env("HOME", scratch())
            .env("OUIJA_CONFIG", scratch().join("none.toml"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let data = self.stdin;
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
        let out = child.wait_with_output().unwrap();
        writer.join().unwrap();
        out
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}
fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

#[test]
fn noul_true_exit_0() {
    let m = mock(noul(0.97));
    let o = ouija(&["--url", &m.url, "noul", "Is it?"]).go();
    assert_eq!(code(&o), 0);
    assert_eq!(stdout(&o), "true\n");
    let e = stderr(&o);
    assert!(e.contains("P(true)=0.970"), "{e}");
    assert!(e.contains("(123 ms)"), "Server-Timing should be used: {e}");
}

#[test]
fn noul_false_exit_1() {
    let m = mock(noul(0.02));
    let o = ouija(&["--url", &m.url, "noul", "Is it?"]).go();
    assert_eq!(code(&o), 1);
    assert_eq!(stdout(&o), "false\n");
}

#[test]
fn uncertain_exit_2() {
    let m = mock(noul(0.6));
    let o = ouija(&["--url", &m.url, "noul", "Is it?"]).go();
    assert_eq!(code(&o), 2);
    assert_eq!(stdout(&o), "true\n");
    assert!(stderr(&o).contains("uncertain"));
    // Lowering the threshold makes the same answer confident.
    let o = ouija(&["--url", &m.url, "--min-conf", "0.1", "noul", "Is it?"]).go();
    assert_eq!(code(&o), 0);
}

#[test]
fn quiet_and_json() {
    let m = mock(noul(0.97));
    let o = ouija(&["--url", &m.url, "-q", "noul", "Is it?"]).go();
    assert_eq!(code(&o), 0);
    assert_eq!(stdout(&o), "true\n");
    assert_eq!(stderr(&o), "");

    let o = ouija(&["--url", &m.url, "--json", "noul", "Is it?"]).go();
    let v: Value = serde_json::from_str(stdout(&o).trim()).unwrap();
    assert_eq!(v["answers"]["answer"]["noul"], 0.97);
    assert!(!stdout(&o).contains("P(true)"));
}

#[test]
fn choice_and_score() {
    let m = mock(Mode::Answers(json!({"answer": {
        "type": "choice", "choice": "fix", "confidence": 0.99,
        "probabilities": {"feat": 0.01, "fix": 0.99}}})));
    let o = ouija(&["--url", &m.url, "choice", "Type?", "feat", "fix=Repair"]).go();
    assert_eq!((code(&o), stdout(&o).as_str()), (0, "fix\n"));
    let req = m.last_request();
    assert_eq!(req["questions"]["answer"]["criteria"]["fix"], "Repair");

    let m = mock(Mode::Answers(json!({"answer": {
        "type": "score", "score": 1.9, "confidence": 0.9,
        "legend": {"0": "low", "1": "high"}, "probabilities": {"0": 0.1, "1": 0.9}}})));
    let o = ouija(&["--url", &m.url, "-v", "score", "How?", "low", "high"]).go();
    assert_eq!((code(&o), stdout(&o).as_str()), (0, "high\n"));
    assert!(stderr(&o).contains('\u{2588}'));
}

#[test]
fn run_output_is_eval_safe() {
    let nasty = "a b'c $(echo pwned) `x`";
    let m = mock(Mode::Answers(json!({
        "bucket": {"type": "choice", "choice": nasty, "confidence": 0.99,
                   "probabilities": {nasty: 0.99, "z": 0.01}},
        "page_now": {"type": "noul", "noul": 0.01}
    })));
    let dir = scratch();
    let spec = dir.join("spec.json");
    std::fs::write(
        &spec,
        r#"{"bucket":{"type":"choice","instructions":"?","criteria":{"a":"a","z":"z"}},
            "page_now":{"type":"noul","instructions":"?"}}"#,
    )
    .unwrap();
    let o = ouija(&["--url", &m.url, "-q", "run", spec.to_str().unwrap()]).go();
    assert_eq!(code(&o), 0, "run never exits 1");
    let out = stdout(&o);
    assert_eq!(
        out,
        "bucket='a b'\\''c $(echo pwned) `x`'\npage_now=false\n"
    );
    // Evaluating the output reproduces the original value without executing it.
    let sh = Command::new("sh")
        .args([
            "-c",
            r#"eval "$1"; printf %s "$bucket|$page_now""#,
            "sh",
            &out,
        ])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&sh.stdout),
        format!("{nasty}|false")
    );
}

#[test]
fn run_rejects_reserved_key_before_sending() {
    let m = mock(noul(0.97));
    let dir = scratch();
    let spec = dir.join("bad.json");
    std::fs::write(&spec, r#"{"path":{"type":"noul","instructions":"?"}}"#).unwrap();
    let o = ouija(&["--url", &m.url, "run", spec.to_str().unwrap()]).go();
    assert_eq!(code(&o), 3);
    assert!(stderr(&o).contains("`path`"), "{}", stderr(&o));
    assert!(m.bodies.lock().unwrap().is_empty());
}

#[test]
fn usage_error_is_3() {
    let o = ouija(&["bogus"]).go();
    assert_eq!(code(&o), 3);
    assert_eq!(stdout(&o), "");
    let o = ouija(&["choice", "only a question"]).go();
    assert_eq!(code(&o), 3);
}

#[test]
fn empty_inputs_are_rejected() {
    let m = mock(noul(0.97));
    let o = ouija(&["--url", &m.url, "noul", ""]).go();
    assert_eq!(code(&o), 3);
    let o = ouija(&["--url", &m.url, "choice", "Q?", "a=", "b"]).go();
    assert_eq!(code(&o), 3);
    assert!(stderr(&o).contains("empty description"));
    assert!(m.bodies.lock().unwrap().is_empty());
}

#[test]
fn http_422_is_3_with_message() {
    let m = mock(Mode::Status(422, r#"{"detail":"input too long"}"#));
    let o = ouija(&["--url", &m.url, "noul", "Is it?"]).go();
    assert_eq!(code(&o), 3);
    assert_eq!(stdout(&o), "");
    let e = stderr(&o);
    assert!(e.contains("422") && e.contains("input too long"), "{e}");
}

#[test]
fn timeout_is_3() {
    let m = mock(Mode::Hang);
    let start = Instant::now();
    let o = ouija(&["--url", &m.url, "--timeout", "1", "noul", "Is it?"]).go();
    assert_eq!(code(&o), 3);
    assert!(start.elapsed() < Duration::from_secs(8));
    assert!(stderr(&o).contains("failed"), "{}", stderr(&o));
}

#[test]
fn connection_refused_is_3() {
    let o = ouija(&["--url", "127.0.0.1:1", "noul", "Is it?"]).go();
    assert_eq!(code(&o), 3);
}

#[test]
fn state_object_vs_string() {
    let m = mock(noul(0.97));
    ouija(&["--url", &m.url, "-q", "--state", r#"{"a":1}"#, "noul", "Q?"]).go();
    assert_eq!(m.last_request()["state"], json!({"a": 1}));

    ouija(&["--url", &m.url, "-q", "--state", "plain text", "noul", "Q?"]).go();
    assert_eq!(m.last_request()["state"], json!("plain text"));

    // Broken JSON that merely looks like an object is a string.
    ouija(&["--url", &m.url, "-q", "--state", "{oops", "noul", "Q?"]).go();
    assert_eq!(m.last_request()["state"], json!("{oops"));

    // Piped stdin works the same way.
    ouija(&["--url", &m.url, "-q", "noul", "Q?"])
        .stdin(br#"{"b":2}"#)
        .go();
    assert_eq!(m.last_request()["state"], json!({"b": 2}));
}

#[test]
fn invalid_utf8_stdin_works() {
    let m = mock(noul(0.97));
    let o = ouija(&["--url", &m.url, "noul", "Q?"])
        .stdin(&[0xff, 0xfe, b'o', b'k', 0xc3])
        .go();
    assert_eq!(code(&o), 0);
    let state = m.last_request()["state"].as_str().unwrap().to_string();
    assert!(
        state.contains('\u{fffd}') && state.contains("ok"),
        "{state:?}"
    );
}

#[test]
fn empty_state_is_3() {
    let m = mock(noul(0.97));
    let o = ouija(&["--url", &m.url, "noul", "Q?"]).stdin(b"  \n").go();
    assert_eq!(code(&o), 3);
}

#[test]
fn config_precedence() {
    let m = mock(noul(0.97));
    let dead = "127.0.0.1:1";
    let dir = scratch();
    let cfg = dir.join("config.toml");
    let cfg_s = cfg.to_str().unwrap();

    // The file alone supplies the url.
    std::fs::write(&cfg, format!("url = \"{}\"\n", m.url)).unwrap();
    let o = ouija(&["noul", "Q?"]).env("OUIJA_CONFIG", cfg_s).go();
    assert_eq!(code(&o), 0, "{}", stderr(&o));

    // Env beats the file.
    std::fs::write(&cfg, format!("url = \"{dead}\"\n")).unwrap();
    let o = ouija(&["noul", "Q?"])
        .env("OUIJA_CONFIG", cfg_s)
        .env("OUIJA_URL", m.url.clone())
        .go();
    assert_eq!(code(&o), 0, "{}", stderr(&o));

    // The legacy alias works, and OUIJA_URL beats it.
    let o = ouija(&["noul", "Q?"]).env("KARPO_URL", m.url.clone()).go();
    assert_eq!(code(&o), 0);
    let o = ouija(&["noul", "Q?"])
        .env("OUIJA_URL", dead)
        .env("KARPO_URL", m.url.clone())
        .go();
    assert_eq!(code(&o), 3);

    // A whitespace-only env var is ignored.
    let o = ouija(&["noul", "Q?"])
        .env("OUIJA_CONFIG", cfg_s)
        .env("OUIJA_URL", "  ")
        .go();
    assert_eq!(code(&o), 3, "falls through to the dead url in the file");

    // The flag beats env.
    let o = ouija(&["--url", &m.url, "noul", "Q?"])
        .env("OUIJA_URL", dead)
        .go();
    assert_eq!(code(&o), 0);

    // min_conf from the file: conf is 0.94, so 0.99 is uncertain; the flag overrides it.
    std::fs::write(&cfg, format!("url = \"{}\"\nmin_conf = 0.99\n", m.url)).unwrap();
    let o = ouija(&["noul", "Q?"]).env("OUIJA_CONFIG", cfg_s).go();
    assert_eq!(code(&o), 2);
    let o = ouija(&["--min-conf", "0.5", "noul", "Q?"])
        .env("OUIJA_CONFIG", cfg_s)
        .go();
    assert_eq!(code(&o), 0);
}

#[test]
fn bad_config_is_3_and_names_the_file() {
    let dir = scratch();
    let cfg = dir.join("bad.toml");
    std::fs::write(&cfg, "bogus = 1\n").unwrap();
    let cfg_s = cfg.to_str().unwrap();
    let o = ouija(&["noul", "Q?"]).env("OUIJA_CONFIG", cfg_s).go();
    assert_eq!(code(&o), 3);
    let e = stderr(&o);
    assert!(e.contains("bad.toml") && e.contains("bogus"), "{e}");
    let o = ouija(&["config"]).env("OUIJA_CONFIG", cfg_s).go();
    assert_eq!(code(&o), 3);
}

#[test]
fn config_subcommand() {
    let m = mock(noul(0.97));
    let dir = scratch();
    let cfg = dir.join("c.toml");
    std::fs::write(&cfg, "timeout = 7\n").unwrap();
    let o = ouija(&["--url", &m.url, "config"])
        .env("OUIJA_CONFIG", cfg.to_str().unwrap())
        .go();
    assert_eq!(code(&o), 0);
    let out = stdout(&o);
    assert!(
        out.contains("(flag: --url)") && out.contains("(config:") && out.contains("(found)"),
        "{out}"
    );
    assert!(
        m.bodies.lock().unwrap().is_empty(),
        "config must not query the model"
    );

    let o = ouija(&["--url", &m.url, "config", "--check"]).go();
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("server: ok (mock-model)"));

    let o = ouija(&["--url", "127.0.0.1:1", "config", "--check"]).go();
    assert_eq!(code(&o), 3);
}

#[test]
fn flavor_never_appears_when_stderr_is_piped() {
    let words = [
        "spirits",
        "planchette",
        "candles",
        "veil",
        "board has spoken",
    ];
    for p in [0.97, 0.02, 0.6] {
        let m = mock(noul(p));
        let o = ouija(&["--url", &m.url, "noul", "Q?"]).go();
        let e = stderr(&o);
        assert!(words.iter().all(|w| !e.contains(w)), "{e}");
    }
}
