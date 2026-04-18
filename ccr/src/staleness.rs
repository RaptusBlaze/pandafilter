//! Staleness detection for session entries.
//!
//! Identifies tool outputs that are no longer relevant to the current work:
//!   1. State commands (git, kubectl, ls, …) from more than 10 turns ago.
//!   2. Build / test outputs that predate the last code edit.
//!   3. File reads where the same file was subsequently edited.
//!
//! Staleness pressure (capped at 0.3) is added to the existing compression
//! pressure in hook.rs so stale context gets compressed harder without
//! dominating the pressure calculation.

use crate::session::SessionState;

/// Why a session entry was flagged as stale.
#[derive(Debug, Clone, PartialEq)]
pub enum StalenessReason {
    /// A state-inspection command (git status, ls, kubectl get, …) that was
    /// run more than `STALE_STATE_TURNS` turns ago.
    StateCommandOld,
    /// A build / test output recorded before the most recent Edit invocation.
    BuildBeforeEdit,
    /// A file read for a file that was subsequently modified via Edit.
    FileReadThenEdited,
}

#[derive(Debug, Clone)]
pub struct StalenessScore {
    pub reason: StalenessReason,
    /// Staleness intensity: 0.0 (fresh) → 1.0 (completely stale).
    pub score: f32,
    /// Session turn number of the stale entry.
    pub turn: usize,
}

/// Number of turns after which a state command is considered fully stale.
const STALE_STATE_TURNS: usize = 10;

/// Command prefixes treated as state-inspection commands.
/// Must match the two-token `cmd_key` format used by `hook.rs`.
const STATE_COMMANDS: &[&str] = &[
    "git status", "git log",   "git diff",  "git branch",
    "git stash",  "kubectl get", "kubectl describe",
    "docker ps",  "docker images",
    "ls",         "ls -",       "ll",
    "df ",        "ps ",        "ps aux",
];

/// Command prefixes treated as build / test commands.
const BUILD_COMMANDS: &[&str] = &[
    "cargo build", "cargo test", "cargo check", "cargo clippy",
    "pytest",      "python -m",  "npm test",    "npm run",
    "yarn test",   "yarn run",   "go test",     "go build",
    "make",        "tsc",        "jest",        "vitest",
    "rspec",       "bundle exec",
];

/// Analyse a session for stale entries and return scored results.
///
/// Operates entirely on existing session data — no BERT calls, no I/O.
pub fn detect_stale_entries(session: &SessionState) -> Vec<StalenessScore> {
    let current_turn = session.total_turns;
    let mut scores: Vec<StalenessScore> = Vec::new();

    // Find the turn number of the most recent Edit in the session.
    // Build outputs recorded before that turn are stale.
    let last_edit_turn = session
        .entries
        .iter()
        .rev()
        .find(|e| e.cmd.starts_with("Edit ") || e.cmd == "Edit")
        .map(|e| e.turn);

    for entry in &session.entries {
        let age_turns = current_turn.saturating_sub(entry.turn);

        // Rule 1: State commands older than STALE_STATE_TURNS
        if is_state_command(&entry.cmd) && age_turns > STALE_STATE_TURNS {
            let score = ((age_turns - STALE_STATE_TURNS) as f32
                / STALE_STATE_TURNS as f32)
                .min(1.0);
            scores.push(StalenessScore {
                reason: StalenessReason::StateCommandOld,
                score,
                turn: entry.turn,
            });
            continue;
        }

        // Rule 2: Build / test outputs from before the last edit
        if let Some(edit_turn) = last_edit_turn {
            if is_build_command(&entry.cmd) && entry.turn < edit_turn {
                // Fully stale — the build predates the most recent code change
                scores.push(StalenessScore {
                    reason: StalenessReason::BuildBeforeEdit,
                    score: 1.0,
                    turn: entry.turn,
                });
                continue;
            }
        }

        // Rule 3: File reads where the file was subsequently edited
        // cmd_key for Read uses the file path as cmd (set in process_read).
        // session.recent_edits maps file_path → edit locations.
        if !session.recent_edits.is_empty() {
            let cmd = entry.cmd.as_str();
            // File reads have cmd keys that look like absolute or relative paths
            if (cmd.starts_with('/') || cmd.contains('.'))
                && session.recent_edits.contains_key(cmd)
            {
                // The file was edited after this read was recorded
                scores.push(StalenessScore {
                    reason: StalenessReason::FileReadThenEdited,
                    score: 0.8,
                    turn: entry.turn,
                });
            }
        }
    }

    scores
}

fn is_state_command(cmd: &str) -> bool {
    STATE_COMMANDS.iter().any(|&s| cmd == s || cmd.starts_with(s))
}

fn is_build_command(cmd: &str) -> bool {
    BUILD_COMMANDS.iter().any(|&s| cmd == s || cmd.starts_with(s))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionEntry, SessionState};

    fn make_entry(turn: usize, cmd: &str, tokens: usize) -> SessionEntry {
        SessionEntry {
            turn,
            cmd: cmd.to_string(),
            ts: 0,
            tokens,
            embedding: vec![],
            content_preview: String::new(),
            state_content: None,
            centroid_delta: None,
            error_signatures: None,
        }
    }

    fn session_with_entries(entries: Vec<SessionEntry>, total_turns: usize) -> SessionState {
        SessionState {
            entries,
            total_turns,
            ..Default::default()
        }
    }

    #[test]
    fn state_command_old_is_stale() {
        let session = session_with_entries(
            vec![make_entry(1, "git status", 50)],
            20, // 19 turns later
        );
        let scores = detect_stale_entries(&session);
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].reason, StalenessReason::StateCommandOld);
        assert!(scores[0].score > 0.0);
    }

    #[test]
    fn state_command_recent_is_not_stale() {
        let session = session_with_entries(
            vec![make_entry(8, "git status", 50)],
            12, // only 4 turns later
        );
        let scores = detect_stale_entries(&session);
        assert_eq!(scores.len(), 0);
    }

    #[test]
    fn build_before_edit_is_stale() {
        let session = session_with_entries(
            vec![
                make_entry(1, "cargo build", 200),
                make_entry(5, "Edit src/main.rs", 10),
            ],
            10,
        );
        let scores = detect_stale_entries(&session);
        let build_stale = scores.iter().any(|s| s.reason == StalenessReason::BuildBeforeEdit);
        assert!(build_stale);
    }

    #[test]
    fn build_after_edit_is_not_stale() {
        let session = session_with_entries(
            vec![
                make_entry(1, "Edit src/main.rs", 10),
                make_entry(5, "cargo build", 200),
            ],
            10,
        );
        let scores = detect_stale_entries(&session);
        let build_stale = scores.iter().any(|s| s.reason == StalenessReason::BuildBeforeEdit);
        assert!(!build_stale);
    }

    #[test]
    fn fresh_session_has_zero_pressure() {
        let session = session_with_entries(vec![], 0);
        let scores = detect_stale_entries(&session);
        assert_eq!(scores.len(), 0);
    }

    #[test]
    fn pressure_capped_at_point_three() {
        // All entries stale → pressure cannot exceed 0.3
        let entries: Vec<SessionEntry> = (1..=15)
            .map(|i| make_entry(i, "git status", 500))
            .collect();
        let mut session = session_with_entries(entries, 30);
        session.total_tokens = 30 * 500;
        let p = session.staleness_pressure();
        assert!(p <= 0.3, "pressure was {}", p);
    }
}
