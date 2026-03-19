//! Per-session state: cross-turn output cache and compression tracking.
//!
//! Session identity uses the parent PID of the Claude Code process, injected
//! by the hook script as `CCR_SESSION_ID=$PPID`. Falls back to an hourly
//! timestamp window for `ccr run` invocations from a terminal.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_ENTRIES: usize = 30;
const SIMILARITY_THRESHOLD: f32 = 0.92;

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
pub struct SessionEntry {
    pub turn: usize,
    pub cmd: String,
    pub ts: u64,
    pub tokens: usize,
    /// BERT embedding of the filtered output (384-dim).
    pub embedding: Vec<f32>,
    /// First 4000 chars of filtered output — used by delta compression (Idea 3) and
    /// sentence-level dedup (C1). 4000 chars ≈ 30-50 lines, enough for meaningful
    /// line-level delta matching. (Was 600, which was too short for real outputs.)
    pub content_preview: String,
}

#[derive(Serialize, Deserialize, Default)]
pub struct SessionState {
    pub entries: Vec<SessionEntry>,
    /// Total tool-use turns seen in this session.
    pub total_turns: usize,
    /// Cumulative filtered tokens emitted in this session.
    pub total_tokens: usize,
    /// Per-command centroid: rolling mean of filtered-output embeddings for each command.
    /// Used by Idea 7 (historical centroid scoring) to measure what's genuinely new
    /// vs what a normal run of this command always produces.
    #[serde(default)]
    pub command_centroids: std::collections::HashMap<String, Vec<f32>>,
}

pub struct SessionHit {
    pub turn: usize,
    pub age_secs: u64,
    /// Tokens that were saved by not re-emitting the full output.
    pub tokens_saved: usize,
}

// ── Session identity ──────────────────────────────────────────────────────────

/// Returns the stable session identifier for this Claude Code session.
///
/// The hook script injects `CCR_SESSION_ID=$PPID` so that all hook invocations
/// within one Claude Code process share the same session file.
pub fn session_id() -> String {
    std::env::var("CCR_SESSION_ID").unwrap_or_else(|_| {
        // Fallback: group by 1-hour window (stable within a terminal work session)
        let secs = now_secs();
        format!("ts_{}", secs / 3600)
    })
}

// ── Persistence ───────────────────────────────────────────────────────────────

fn session_path(id: &str) -> Option<PathBuf> {
    Some(
        dirs::data_local_dir()?
            .join("ccr")
            .join("sessions")
            .join(format!("{}.json", id)),
    )
}

impl SessionState {
    pub fn load(id: &str) -> Self {
        session_path(id)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, id: &str) {
        if let Some(path) = session_path(id) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string(self) {
                let _ = std::fs::write(path, json);
            }
        }
    }
}

// ── Cross-turn similarity check (B3) ─────────────────────────────────────────

impl SessionState {
    /// Check if a recent run of the same command produced semantically identical output.
    /// Returns `Some(hit)` when cosine similarity exceeds the threshold.
    pub fn find_similar(&self, cmd: &str, embedding: &[f32]) -> Option<SessionHit> {
        let now = now_secs();
        self.entries
            .iter()
            .filter(|e| e.cmd == cmd && !e.embedding.is_empty())
            .rev()
            .find_map(|e| {
                let sim = cosine_sim(embedding, &e.embedding);
                if sim >= SIMILARITY_THRESHOLD {
                    Some(SessionHit {
                        turn: e.turn,
                        age_secs: now.saturating_sub(e.ts),
                        tokens_saved: e.tokens,
                    })
                } else {
                    None
                }
            })
    }

    /// Record a new output entry, evicting the oldest if over capacity.
    pub fn record(
        &mut self,
        cmd: &str,
        embedding: Vec<f32>,
        tokens: usize,
        content: &str,
    ) {
        self.total_turns += 1;
        self.total_tokens += tokens;

        let entry = SessionEntry {
            turn: self.total_turns,
            cmd: cmd.to_string(),
            ts: now_secs(),
            tokens,
            embedding,
            content_preview: content.chars().take(4000).collect(),
        };

        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
    }
}

// ── Per-command historical centroid (Idea 7) ─────────────────────────────────

impl SessionState {
    /// Returns the rolling-mean centroid for this command, if any runs have been recorded.
    pub fn command_centroid(&self, cmd: &str) -> Option<&Vec<f32>> {
        self.command_centroids.get(cmd)
    }

    /// Update the per-command centroid with a new observation.
    /// Uses a simple running mean: new_centroid = (old * n + new) / (n + 1),
    /// where n is the number of prior entries for this command.
    pub fn update_command_centroid(&mut self, cmd: &str, new_centroid: Vec<f32>) {
        let prior_count = self.entries.iter().filter(|e| e.cmd == cmd).count() as f32;

        let updated = match self.command_centroids.get(cmd) {
            None => new_centroid,
            Some(_) if prior_count <= 0.0 => new_centroid,
            Some(old) => {
                let n = prior_count;
                let np1 = n + 1.0;
                old.iter()
                    .zip(new_centroid.iter())
                    .map(|(o, nc)| (o * n + nc) / np1)
                    .collect()
            }
        };

        self.command_centroids.insert(cmd.to_string(), updated);
    }
}

// ── Semantic delta compression (Idea 3) ──────────────────────────────────────

/// Threshold for delta matching: lower than B3 (0.92) so that iterative
/// workflows (cargo build N times) get delta treatment even when outputs are
/// only moderately similar.
const DELTA_THRESHOLD: f32 = 0.55;

/// Result of a semantic delta comparison between a new output and a prior run.
pub struct DeltaResult {
    /// Compressed output showing only new/changed lines plus a back-reference.
    pub output: String,
    /// Number of new output lines not semantically matched to the prior run.
    #[allow(dead_code)]
    pub new_count: usize,
    /// Number of new output lines matched (suppressed) by the prior run.
    #[allow(dead_code)]
    pub same_count: usize,
    /// Turn number of the prior run this delta references.
    #[allow(dead_code)]
    pub reference_turn: usize,
}

impl SessionState {
    /// Compare `new_lines` against recent entries for the same `cmd`.
    ///
    /// Returns `Some(DeltaResult)` when a prior run is found with overall
    /// embedding similarity ≥ DELTA_THRESHOLD.  Lines semantically matched
    /// to prior output are suppressed; genuinely new lines are surfaced.
    pub fn compute_delta(
        &self,
        cmd: &str,
        new_lines: &[&str],
        new_embedding: &[f32],
    ) -> Option<DeltaResult> {
        // Find the most recent entry for the same command within delta range
        let prior = self
            .entries
            .iter()
            .filter(|e| e.cmd == cmd && !e.embedding.is_empty())
            .rev()
            .find(|e| {
                let sim = cosine_sim(new_embedding, &e.embedding);
                sim >= DELTA_THRESHOLD
            })?;

        // Re-embed each new line and compare against the prior content preview.
        // Lines with high similarity to any sentence in the prior output are "same".
        let model = ccr_core::summarizer::embed_batch(new_lines).ok()?;

        // Embed lines from prior content for line-level comparison
        let prior_lines: Vec<&str> = prior.content_preview.lines().collect();
        if prior_lines.is_empty() {
            return None;
        }
        let prior_embs = ccr_core::summarizer::embed_batch(&prior_lines).ok()?;

        const LINE_MATCH_THRESHOLD: f32 = 0.88;
        let mut new_lines_out: Vec<String> = Vec::new();
        let mut same_count = 0usize;
        let mut new_count = 0usize;

        for (i, line) in new_lines.iter().enumerate() {
            let line_emb = &model[i];
            let best_sim = prior_embs
                .iter()
                .map(|pe| cosine_sim(line_emb, pe))
                .fold(0.0f32, f32::max);

            if best_sim >= LINE_MATCH_THRESHOLD {
                same_count += 1;
            } else {
                new_count += 1;
                new_lines_out.push((*line).to_string());
            }
        }

        let ref_marker = format!(
            "[{} lines same as turn {} — {} tokens saved]",
            same_count, prior.turn, prior.tokens
        );

        let mut output_parts: Vec<String> = Vec::new();
        if same_count > 0 {
            output_parts.push(ref_marker);
        }
        output_parts.extend(new_lines_out);

        Some(DeltaResult {
            output: output_parts.join("\n"),
            new_count,
            same_count,
            reference_turn: prior.turn,
        })
    }
}

// ── Session-aware compression budget (C2) ────────────────────────────────────

impl SessionState {
    /// Returns a compression factor in [0.5, 1.0].
    ///
    /// At 1.0 (fresh session): no extra compression beyond the handler's own filter.
    /// Decreases linearly toward 0.5 once the session exceeds 50k cumulative tokens,
    /// signalling that the context window is filling up and outputs should be shorter.
    pub fn compression_factor(&self) -> f32 {
        const THRESHOLD: usize = 50_000;
        if self.total_tokens < THRESHOLD {
            return 1.0;
        }
        let excess = (self.total_tokens - THRESHOLD) as f32 / THRESHOLD as f32;
        (1.0 - 0.5 * excess.min(1.0)).max(0.5)
    }

    /// Returns content previews of the N most recent entries, oldest first.
    /// Used by the sentence-level deduplicator (C1) as the "prior context" window.
    pub fn recent_content(&self, limit: usize) -> Vec<(usize, String)> {
        self.entries
            .iter()
            .rev()
            .take(limit)
            .rev()
            .map(|e| (e.turn, e.content_preview.clone()))
            .collect()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    crate::handlers::util::cosine_similarity(a, b)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Human-readable age string, e.g. "3s", "2m", "1h".
pub fn format_age(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}
