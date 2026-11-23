//! Optional one-line commentary printed to stderr on interactive terminals.
//!
//! Flavor never touches stdout or exit codes. The line is chosen from the
//! outcome, and varied deterministically by hashing a seed (the response id, or
//! the answer text), so no random-number dependency is needed.

/// Coarse outcome used to pick a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mood {
    /// Confident yes.
    Yes,
    /// Confident no.
    No,
    /// Confident choice or score; the line may mention the answer.
    Answer,
    /// Confidence below the threshold.
    Uncertain,
}

const YES: &[&str] = &[
    "\u{1f52e} the spirits say: yes",
    "\u{2728} the planchette glides to YES",
    "\u{1f56f}\u{fe0f} the candles burn steady: yes",
    "\u{1f52e} the veil parts, and it is a yes",
];

const NO: &[&str] = &[
    "\u{1f56f}\u{fe0f} the planchette drifts to NO",
    "\u{1f319} the spirits shake their heads: no",
    "\u{1f480} the candles gutter: no",
    "\u{1f311} the veil stays shut: no",
];

/// `{}` is replaced with the answer text.
const ANSWER: &[&str] = &[
    "\u{1f52e} the spirits say: {}",
    "\u{2728} the planchette spells out {}",
    "\u{1f56f}\u{fe0f} the candles lean toward {}",
    "\u{1f319} the board has spoken: {}",
];

const UNCERTAIN: &[&str] = &[
    "\u{1f47b} the spirits are murmuring\u{2026} (uncertain)",
    "\u{1f32b}\u{fe0f} the planchette trembles\u{2026} (uncertain)",
    "\u{1f56f}\u{fe0f} the candles flicker\u{2026} (uncertain)",
    "\u{1f9ff} the spirits are of two minds (uncertain)",
];

/// FNV-1a, stable across platforms and Rust versions.
fn hash(seed: &str) -> u64 {
    seed.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Choose the flavor line for an outcome.
pub fn line(mood: Mood, answer: &str, seed: &str) -> String {
    let pool = match mood {
        Mood::Yes => YES,
        Mood::No => NO,
        Mood::Answer => ANSWER,
        Mood::Uncertain => UNCERTAIN,
    };
    let pick = pool[(hash(seed) % pool.len() as u64) as usize];
    pick.replace("{}", answer)
}

/// Flavor is shown only in default human mode on an interactive stderr.
pub fn should_show(enabled: bool, quiet: bool, json: bool, stderr_is_tty: bool) -> bool {
    enabled && !quiet && !json && stderr_is_tty
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gating() {
        assert!(should_show(true, false, false, true));
        assert!(!should_show(false, false, false, true));
        assert!(!should_show(true, true, false, true));
        assert!(!should_show(true, false, true, true));
        assert!(!should_show(true, false, false, false));
    }

    #[test]
    fn deterministic_and_varied() {
        assert_eq!(line(Mood::Yes, "", "abc"), line(Mood::Yes, "", "abc"));
        let distinct: std::collections::HashSet<_> = (0..64)
            .map(|i| line(Mood::Uncertain, "", &format!("id-{i}")))
            .collect();
        assert!(distinct.len() > 1);
    }

    #[test]
    fn answer_is_substituted() {
        for i in 0..16 {
            let l = line(Mood::Answer, "fix", &i.to_string());
            assert!(l.contains("fix") && !l.contains("{}"), "{l}");
        }
    }

    #[test]
    fn single_line_per_mood() {
        for mood in [Mood::Yes, Mood::No, Mood::Answer, Mood::Uncertain] {
            for i in 0..16 {
                assert!(!line(mood, "x", &i.to_string()).contains('\n'));
            }
        }
    }
}
