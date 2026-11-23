//! Settings: config file, environment, and flag resolution.
//!
//! Precedence, highest first: command-line flag, environment variable, config
//! file, built-in default.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const DEFAULT_URL: &str = "http://localhost:18440";
pub const DEFAULT_MIN_CONF: f64 = 0.8;
pub const DEFAULT_TIMEOUT: u64 = 60;
pub const DEFAULT_FLAVOR: bool = true;

/// Environment variables that set the server URL, highest priority first.
/// `KARPO_URL` is a legacy alias.
pub const URL_ENV: &[&str] = &["OUIJA_URL", "KARPO_URL"];

/// Contents of `config.toml`. Every key is optional; unknown keys are an error.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub url: Option<String>,
    pub min_conf: Option<f64>,
    pub timeout: Option<u64>,
    pub flavor: Option<bool>,
}

/// Values given on the command line.
#[derive(Debug, Default, Clone)]
pub struct Flags {
    pub url: Option<String>,
    pub min_conf: Option<f64>,
    pub timeout: Option<u64>,
    /// `--plain` was given.
    pub plain: bool,
}

/// Where a resolved setting came from.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Flag(&'static str),
    Env(&'static str),
    File(PathBuf),
    Default,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Flag(name) => write!(f, "flag: {name}"),
            Source::Env(name) => write!(f, "env: {name}"),
            Source::File(p) => write!(f, "config: {}", p.display()),
            Source::Default => write!(f, "default"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Setting<T> {
    pub value: T,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub url: Setting<String>,
    pub min_conf: Setting<f64>,
    pub timeout: Setting<u64>,
    pub flavor: Setting<bool>,
}

/// Largest accepted timeout, in seconds (one day).
pub const MAX_TIMEOUT: u64 = 86_400;

// Shared validators. Each returns a wording that callers prefix with the name
// of the setting (`min_conf`, `--min-conf`, an env var, ...).

pub fn validate_min_conf(v: f64) -> Result<f64, String> {
    if (0.0..=1.0).contains(&v) {
        Ok(v)
    } else {
        Err("must be between 0 and 1".to_string())
    }
}

pub fn validate_timeout(v: u64) -> Result<u64, String> {
    if (1..=MAX_TIMEOUT).contains(&v) {
        Ok(v)
    } else {
        Err(format!("must be between 1 and {MAX_TIMEOUT} seconds"))
    }
}

pub fn validate_url(v: &str) -> Result<(), String> {
    let v = v.trim();
    if v.is_empty() {
        Err("must not be empty".to_string())
    } else if v.chars().any(char::is_whitespace) {
        Err("must not contain whitespace".to_string())
    } else {
        Ok(())
    }
}

/// Parse and validate config file text. `path` is used in error messages.
pub fn parse_file(text: &str, path: &Path) -> Result<FileConfig, String> {
    let at = |e: String| format!("{}: {e}", path.display());
    let key = |k: &str, e: String| at(format!("{k} {e}"));
    let cfg: FileConfig = toml::from_str(text).map_err(|e| at(e.to_string().trim().to_string()))?;
    if let Some(u) = &cfg.url {
        validate_url(u).map_err(|e| key("url", e))?;
    }
    if let Some(c) = cfg.min_conf {
        validate_min_conf(c).map_err(|e| key("min_conf", e))?;
    }
    if let Some(t) = cfg.timeout {
        validate_timeout(t).map_err(|e| key("timeout", e))?;
    }
    Ok(cfg)
}

/// Locate the config file. `env` looks up an environment variable.
///
/// Order: `OUIJA_CONFIG`, then `$XDG_CONFIG_HOME/ouija/config.toml`, then
/// `~/.config/ouija/config.toml`. Empty values are ignored.
pub fn config_path(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    if let Some(p) = get("OUIJA_CONFIG") {
        return Some(PathBuf::from(p));
    }
    if let Some(x) = get("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(x).join("ouija").join("config.toml"));
    }
    get("HOME").map(|h| PathBuf::from(h).join(".config/ouija/config.toml"))
}

/// Read the config file. A missing file is `Ok(None)`; any other problem is an error.
pub fn load_file(path: &Path) -> Result<Option<FileConfig>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_file(&text, path).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Merge flags, environment and the config file into final settings.
///
/// `file` is the config path with its parsed contents, if the file exists.
pub fn resolve(
    flags: &Flags,
    env: &dyn Fn(&str) -> Option<String>,
    file: Option<(&Path, &FileConfig)>,
) -> Result<Settings, String> {
    let from_file = |src: &Path| Source::File(src.to_path_buf());

    let url = if let Some(u) = &flags.url {
        validate_url(u).map_err(|e| format!("--url {e}"))?;
        Setting {
            value: u.trim().to_string(),
            source: Source::Flag("--url"),
        }
    } else if let Some((name, v)) = URL_ENV.iter().find_map(|n| {
        env(n)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .map(|v| (*n, v))
    }) {
        validate_url(&v).map_err(|e| format!("{name} {e}"))?;
        Setting {
            value: v,
            source: Source::Env(name),
        }
    } else if let Some((p, FileConfig { url: Some(u), .. })) = file {
        Setting {
            value: u.clone(),
            source: from_file(p),
        }
    } else {
        Setting {
            value: DEFAULT_URL.to_string(),
            source: Source::Default,
        }
    };

    let min_conf = if let Some(c) = flags.min_conf {
        let c = validate_min_conf(c).map_err(|e| format!("--min-conf {e}"))?;
        Setting {
            value: c,
            source: Source::Flag("--min-conf"),
        }
    } else if let Some((
        p,
        FileConfig {
            min_conf: Some(c), ..
        },
    )) = file
    {
        Setting {
            value: *c,
            source: from_file(p),
        }
    } else {
        Setting {
            value: DEFAULT_MIN_CONF,
            source: Source::Default,
        }
    };

    let timeout = if let Some(t) = flags.timeout {
        let t = validate_timeout(t).map_err(|e| format!("--timeout {e}"))?;
        Setting {
            value: t,
            source: Source::Flag("--timeout"),
        }
    } else if let Some((
        p,
        FileConfig {
            timeout: Some(t), ..
        },
    )) = file
    {
        Setting {
            value: *t,
            source: from_file(p),
        }
    } else {
        Setting {
            value: DEFAULT_TIMEOUT,
            source: Source::Default,
        }
    };

    let flavor = if flags.plain {
        Setting {
            value: false,
            source: Source::Flag("--plain"),
        }
    } else if let Some((
        p,
        FileConfig {
            flavor: Some(b), ..
        },
    )) = file
    {
        Setting {
            value: *b,
            source: from_file(p),
        }
    } else {
        Setting {
            value: DEFAULT_FLAVOR,
            source: Source::Default,
        }
    };

    Ok(Settings {
        url,
        min_conf,
        timeout,
        flavor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    fn file(text: &str) -> FileConfig {
        parse_file(text, Path::new("/c.toml")).unwrap()
    }

    #[test]
    fn defaults() {
        let s = resolve(&Flags::default(), &env_of(&[]), None).unwrap();
        assert_eq!(s.url.value, DEFAULT_URL);
        assert_eq!(s.url.source, Source::Default);
        assert_eq!(s.min_conf.value, 0.8);
        assert_eq!(s.timeout.value, 60);
        assert!(s.flavor.value);
    }

    #[test]
    fn parses_all_keys() {
        let c = file("url='http://spark:18440'\nmin_conf=0.9\ntimeout=5\nflavor=false\n");
        assert_eq!(c.url.as_deref(), Some("http://spark:18440"));
        assert_eq!(c.min_conf, Some(0.9));
        assert_eq!(c.timeout, Some(5));
        assert_eq!(c.flavor, Some(false));
        assert_eq!(file(""), FileConfig::default());
        // An integer is accepted where a float is expected.
        assert_eq!(file("min_conf = 1").min_conf, Some(1.0));
    }

    #[test]
    fn file_errors_name_the_file() {
        let p = Path::new("/etc/o.toml");
        for bad in [
            "url = ",
            "bogus = 1",
            "timeout = 0",
            "timeout = 86401",
            "timeout = -3",
            "timeout = 'x'",
            "min_conf = 1.5",
            "min_conf = -0.1",
            "url = ''",
            "flavor = 'yes'",
        ] {
            let e = parse_file(bad, p).unwrap_err();
            assert!(e.starts_with("/etc/o.toml:"), "{bad} -> {e}");
        }
        let e = parse_file("bogus = 1", p).unwrap_err();
        assert!(e.contains("bogus"), "{e}");
    }

    #[test]
    fn precedence_flag_env_file_default() {
        let c = file("url='http://file:1'\nmin_conf=0.5\ntimeout=7\nflavor=false");
        let p = Path::new("/c.toml");
        let none = Flags::default();

        // file beats default
        let s = resolve(&none, &env_of(&[]), Some((p, &c))).unwrap();
        assert_eq!(s.url.value, "http://file:1");
        assert_eq!(s.url.source, Source::File(p.into()));
        assert_eq!(s.min_conf.value, 0.5);
        assert_eq!(s.timeout.value, 7);
        assert!(!s.flavor.value);

        // env beats file
        let s = resolve(&none, &env_of(&[("OUIJA_URL", "env:2")]), Some((p, &c))).unwrap();
        assert_eq!(s.url.value, "env:2");
        assert_eq!(s.url.source, Source::Env("OUIJA_URL"));

        // flag beats env
        let flags = Flags {
            url: Some("flag:3".into()),
            min_conf: Some(0.1),
            timeout: Some(9),
            plain: false,
        };
        let s = resolve(&flags, &env_of(&[("OUIJA_URL", "env:2")]), Some((p, &c))).unwrap();
        assert_eq!(s.url.value, "flag:3");
        assert_eq!(s.min_conf.source, Source::Flag("--min-conf"));
        assert_eq!(s.timeout.value, 9);
    }

    #[test]
    fn legacy_env_alias_and_empty_values() {
        let none = Flags::default();
        let s = resolve(&none, &env_of(&[("KARPO_URL", "old:1")]), None).unwrap();
        assert_eq!(s.url.value, "old:1");
        assert_eq!(s.url.source, Source::Env("KARPO_URL"));

        // OUIJA_URL wins over the alias
        let s = resolve(
            &none,
            &env_of(&[("KARPO_URL", "old:1"), ("OUIJA_URL", "new:2")]),
            None,
        )
        .unwrap();
        assert_eq!(s.url.value, "new:2");

        // empty values are ignored and fall through
        let c = file("url='http://file:1'");
        let p = Path::new("/c.toml");
        let s = resolve(
            &none,
            &env_of(&[("OUIJA_URL", ""), ("KARPO_URL", "")]),
            Some((p, &c)),
        )
        .unwrap();
        assert_eq!(s.url.value, "http://file:1");
    }

    #[test]
    fn plain_flag_overrides_file_flavor() {
        let c = file("flavor = true");
        let flags = Flags {
            plain: true,
            ..Flags::default()
        };
        let s = resolve(&flags, &env_of(&[]), Some((Path::new("/c"), &c))).unwrap();
        assert!(!s.flavor.value);
        assert_eq!(s.flavor.source, Source::Flag("--plain"));
    }

    #[test]
    fn flag_validation() {
        let bad = |f: Flags| resolve(&f, &env_of(&[]), None).unwrap_err();
        assert!(
            bad(Flags {
                min_conf: Some(1.1),
                ..Flags::default()
            })
            .contains("--min-conf")
        );
        assert!(
            bad(Flags {
                min_conf: Some(f64::NAN),
                ..Flags::default()
            })
            .contains("--min-conf")
        );
        assert!(
            bad(Flags {
                timeout: Some(0),
                ..Flags::default()
            })
            .contains("--timeout")
        );
        assert!(
            bad(Flags {
                url: Some(" ".into()),
                ..Flags::default()
            })
            .contains("--url")
        );
    }

    #[test]
    fn timeout_cap_is_shared() {
        assert_eq!(validate_timeout(86_400), Ok(86_400));
        assert!(validate_timeout(86_401).is_err());
        assert!(validate_timeout(0).is_err());
        let f = Flags {
            timeout: Some(86_401),
            ..Flags::default()
        };
        assert!(
            resolve(&f, &env_of(&[]), None)
                .unwrap_err()
                .starts_with("--timeout")
        );
        assert_eq!(file("timeout = 86400").timeout, Some(86_400));
    }

    #[test]
    fn env_url_trimmed_and_validated() {
        let none = Flags::default();
        // whitespace-only is unset
        let s = resolve(
            &none,
            &env_of(&[("OUIJA_URL", "  "), ("KARPO_URL", "old:1")]),
            None,
        )
        .unwrap();
        assert_eq!(s.url.value, "old:1");
        let s = resolve(&none, &env_of(&[("OUIJA_URL", "  \t")]), None).unwrap();
        assert_eq!(s.url.source, Source::Default);
        // trimmed
        let s = resolve(&none, &env_of(&[("OUIJA_URL", " spark:1 \n")]), None).unwrap();
        assert_eq!(s.url.value, "spark:1");
        // invalid value names the variable
        let e = resolve(&none, &env_of(&[("KARPO_URL", "a b")]), None).unwrap_err();
        assert!(e.starts_with("KARPO_URL"), "{e}");
    }

    #[test]
    fn path_resolution() {
        let p = config_path(&env_of(&[
            ("OUIJA_CONFIG", "/x/y.toml"),
            ("XDG_CONFIG_HOME", "/xdg"),
        ]));
        assert_eq!(p, Some(PathBuf::from("/x/y.toml")));
        let p = config_path(&env_of(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")]));
        assert_eq!(p, Some(PathBuf::from("/xdg/ouija/config.toml")));
        let p = config_path(&env_of(&[("HOME", "/home/u")]));
        assert_eq!(p, Some(PathBuf::from("/home/u/.config/ouija/config.toml")));
        // empty XDG_CONFIG_HOME falls back to HOME
        let p = config_path(&env_of(&[("XDG_CONFIG_HOME", ""), ("HOME", "/home/u")]));
        assert_eq!(p, Some(PathBuf::from("/home/u/.config/ouija/config.toml")));
        assert_eq!(config_path(&env_of(&[])), None);
    }

    #[test]
    fn load_file_missing_is_ok() {
        assert_eq!(
            load_file(Path::new("/nonexistent/ouija/config.toml")),
            Ok(None)
        );
    }

    #[test]
    fn load_file_directory_is_error() {
        assert!(load_file(Path::new(env!("CARGO_MANIFEST_DIR"))).is_err());
    }
}
