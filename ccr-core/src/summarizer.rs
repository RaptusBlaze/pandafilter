use once_cell::sync::OnceCell;
use regex::Regex;

static CRITICAL_PATTERN: OnceCell<Regex> = OnceCell::new();

fn critical_pattern() -> &'static Regex {
    CRITICAL_PATTERN.get_or_init(|| {
        Regex::new(r"(?i)(error|warning|warn|failed|failure|fatal|panic|exception|critical|FAILED|ERROR|WARNING)").unwrap()
    })
}

// ── Cached model ──────────────────────────────────────────────────────────────

static MODEL_CACHE: OnceCell<fastembed::TextEmbedding> = OnceCell::new();

fn get_model() -> anyhow::Result<&'static fastembed::TextEmbedding> {
    MODEL_CACHE.get_or_try_init(|| {
        use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
        TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::AllMiniLML6V2).with_show_download_progress(false),
        )
    })
}

// ── Math helpers ──────────────────────────────────────────────────────────────

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

fn compute_centroid(embeddings: &[Vec<f32>]) -> Vec<f32> {
    if embeddings.is_empty() {
        return vec![];
    }
    let dim = embeddings[0].len();
    let mut centroid = vec![0.0f32; dim];
    for emb in embeddings {
        for (i, v) in emb.iter().enumerate() {
            centroid[i] += v;
        }
    }
    let n = embeddings.len() as f32;
    centroid.iter_mut().for_each(|v| *v /= n);
    centroid
}

// ── Public result types ───────────────────────────────────────────────────────

pub struct SummarizeResult {
    pub output: String,
    pub lines_in: usize,
    pub lines_out: usize,
    pub omitted: usize,
}

// ── Line-level summarization (command output) ─────────────────────────────────

/// Standard anomaly-based summarization: keeps outlier lines (errors, unique events)
/// and suppresses repetitive noise that clusters near the centroid.
pub fn summarize(text: &str, budget_lines: usize) -> SummarizeResult {
    let lines: Vec<&str> = text.lines().collect();
    let lines_in = lines.len();

    let output = match summarize_semantic(&lines, budget_lines, None) {
        Ok(result) => result,
        Err(_) => summarize_headtail(&lines, budget_lines),
    };

    let lines_out = output.lines().count();
    let omitted = lines_in.saturating_sub(lines_out);
    SummarizeResult { output, lines_in, lines_out, omitted }
}

/// Query-biased summarization: combines anomaly scoring with relevance to `query`.
/// Lines that are both unusual AND relevant to the current task score highest.
pub fn summarize_with_query(text: &str, budget_lines: usize, query: &str) -> SummarizeResult {
    let lines: Vec<&str> = text.lines().collect();
    let lines_in = lines.len();

    let output = match summarize_semantic(&lines, budget_lines, Some(query)) {
        Ok(result) => result,
        Err(_) => summarize_headtail(&lines, budget_lines),
    };

    let lines_out = output.lines().count();
    let omitted = lines_in.saturating_sub(lines_out);
    SummarizeResult { output, lines_in, lines_out, omitted }
}

fn summarize_semantic(
    lines: &[&str],
    budget: usize,
    query: Option<&str>,
) -> anyhow::Result<String> {
    let total = lines.len();
    let budget = budget.min(total);

    let indexed_lines: Vec<(usize, &str)> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| (i, *l))
        .collect();

    if indexed_lines.is_empty() {
        return Ok(lines.join("\n"));
    }

    let model = get_model()?;

    // Embed lines + optional query in one batch
    let mut texts: Vec<&str> = indexed_lines.iter().map(|(_, l)| *l).collect();
    let has_query = query.is_some();
    if let Some(q) = query {
        texts.push(q);
    }

    let all_embeddings = model.embed(texts, None)?;
    let query_emb: Option<&Vec<f32>> = if has_query { all_embeddings.last() } else { None };
    let embeddings = if has_query {
        &all_embeddings[..all_embeddings.len() - 1]
    } else {
        &all_embeddings[..]
    };

    let centroid = compute_centroid(embeddings);

    // Score: anomaly component always present, query relevance blended in when available
    let scored: Vec<(usize, f32)> = indexed_lines
        .iter()
        .zip(embeddings.iter())
        .map(|((orig_idx, _), emb)| {
            let anomaly = 1.0 - cosine_similarity(emb, &centroid);
            let score = if let Some(q_emb) = query_emb {
                let relevance = cosine_similarity(emb, q_emb);
                0.5 * anomaly + 0.5 * relevance
            } else {
                anomaly
            };
            (*orig_idx, score)
        })
        .collect();

    // Hard-keep critical lines
    let mut selected: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (orig_idx, line) in lines.iter().enumerate() {
        if critical_pattern().is_match(line) {
            selected.insert(orig_idx);
        }
    }

    // Fill budget from highest-scoring lines above threshold
    let max_score = scored.iter().map(|(_, s)| *s).fold(0.0f32, f32::max);
    // Slightly lower threshold in query-biased mode since relevance can shift scores
    let threshold_factor = if has_query { 0.30 } else { 0.40 };
    let score_threshold = max_score * threshold_factor;

    let mut ranked = scored.clone();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    for (orig_idx, score) in &ranked {
        if selected.len() >= budget {
            break;
        }
        if *score < score_threshold {
            break;
        }
        selected.insert(*orig_idx);
    }

    // Restore order, insert omission markers between gaps
    let mut kept: Vec<usize> = selected.into_iter().collect();
    kept.sort();

    let mut result: Vec<String> = Vec::new();
    let mut prev_idx: Option<usize> = None;
    for idx in &kept {
        if let Some(prev) = prev_idx {
            let gap = idx - prev - 1;
            if gap > 0 {
                result.push(format!("[... {} lines omitted ...]", gap));
            }
        } else if *idx > 0 {
            result.push(format!("[... {} lines omitted ...]", idx));
        }
        result.push(lines[*idx].to_string());
        prev_idx = Some(*idx);
    }
    if let Some(last) = prev_idx {
        let trailing = total - last - 1;
        if trailing > 0 {
            result.push(format!("[... {} lines omitted ...]", trailing));
        }
    }

    Ok(result.join("\n"))
}

fn summarize_headtail(lines: &[&str], budget: usize) -> String {
    let total = lines.len();
    let head = budget / 2;
    let tail = budget - head;
    let omitted = total.saturating_sub(head + tail);

    let mut result: Vec<String> = Vec::new();
    result.extend(lines[..head.min(total)].iter().map(|l| l.to_string()));
    result.push(format!("[... {} lines omitted ...]", omitted));
    if tail > 0 && total > head {
        result.extend(lines[total.saturating_sub(tail)..].iter().map(|l| l.to_string()));
    }
    result.join("\n")
}

// ── Sentence-level summarization (conversation messages) ─────────────────────

pub struct MessageSummarizeResult {
    pub output: String,
    pub sentences_in: usize,
    pub sentences_out: usize,
}

pub fn summarize_message(text: &str, budget_ratio: f32) -> MessageSummarizeResult {
    let sentences = crate::sentence::split_sentences(text);
    let sentences_in = sentences.len();

    if sentences_in == 0 {
        return MessageSummarizeResult {
            output: text.to_string(),
            sentences_in: 0,
            sentences_out: 0,
        };
    }

    let budget = ((sentences_in as f32 * budget_ratio).ceil() as usize).max(1);
    if sentences_in <= budget {
        return MessageSummarizeResult {
            output: text.to_string(),
            sentences_in,
            sentences_out: sentences_in,
        };
    }

    let output = match summarize_sentences_semantic(&sentences, budget, is_hard_keep_sentence) {
        Ok(out) => out,
        Err(_) => summarize_sentences_headtail(&sentences, budget),
    };

    let sentences_out = crate::sentence::split_sentences(&output).len();
    MessageSummarizeResult { output, sentences_in, sentences_out }
}

pub fn summarize_assistant_message(text: &str, budget_ratio: f32) -> MessageSummarizeResult {
    let sentences = crate::sentence::split_sentences(text);
    let sentences_in = sentences.len();

    if sentences_in == 0 {
        return MessageSummarizeResult {
            output: text.to_string(),
            sentences_in: 0,
            sentences_out: 0,
        };
    }

    let budget = ((sentences_in as f32 * budget_ratio).ceil() as usize).max(1);
    if sentences_in <= budget {
        return MessageSummarizeResult {
            output: text.to_string(),
            sentences_in,
            sentences_out: sentences_in,
        };
    }

    let output = match summarize_sentences_semantic(&sentences, budget, is_hard_keep_assistant_sentence) {
        Ok(out) => out,
        Err(_) => summarize_sentences_headtail(&sentences, budget),
    };

    let sentences_out = crate::sentence::split_sentences(&output).len();
    MessageSummarizeResult { output, sentences_in, sentences_out }
}

fn is_hard_keep_sentence(s: &str) -> bool {
    let t = s.trim();
    if t.ends_with('?') { return true; }
    if t.contains('`') || t.contains("::") { return true; }
    if t.split_whitespace().any(|w| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        w.contains('_') && w.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false)
    }) { return true; }
    let lower = t.to_lowercase();
    ["must", "never", "always", "ensure", "make sure", "do not", "don't", "avoid", "required", "critical"]
        .iter()
        .any(|kw| lower.contains(kw))
}

fn is_hard_keep_assistant_sentence(s: &str) -> bool {
    let t = s.trim();
    if t.contains('`') || t.contains("::") { return true; }
    let first = t.chars().next().unwrap_or(' ');
    if first == '-' || first == '*' { return true; }
    if first.is_ascii_digit() && t.chars().nth(1).map(|c| c == '.' || c == ')').unwrap_or(false) {
        return true;
    }
    if t.contains('$') || t.contains('€') || t.contains('£') || t.contains('%') { return true; }
    if t.split_whitespace().any(|w| w.chars().any(|c| c.is_ascii_digit())) { return true; }
    let lower = t.to_lowercase();
    ["must", "never", "always", "ensure", "required", "critical"]
        .iter()
        .any(|kw| lower.contains(kw))
}

fn summarize_sentences_semantic(
    sentences: &[String],
    budget: usize,
    hard_keep: impl Fn(&str) -> bool,
) -> anyhow::Result<String> {
    let model = get_model()?;
    let texts: Vec<&str> = sentences.iter().map(|s| s.as_str()).collect();
    let embeddings = model.embed(texts, None)?;

    let centroid = compute_centroid(&embeddings);

    let scored: Vec<(usize, f32)> = embeddings
        .iter()
        .enumerate()
        .map(|(i, emb)| (i, 1.0 - cosine_similarity(emb, &centroid)))
        .collect();

    let mut selected: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (i, s) in sentences.iter().enumerate() {
        if hard_keep(s) {
            selected.insert(i);
        }
    }

    let max_score = scored.iter().map(|(_, s)| *s).fold(0.0f32, f32::max);
    let threshold = max_score * 0.40;

    let mut ranked = scored.clone();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    for (idx, score) in &ranked {
        if selected.len() >= budget { break; }
        if *score < threshold { break; }
        selected.insert(*idx);
    }

    let mut kept: Vec<usize> = selected.into_iter().collect();
    kept.sort();
    Ok(kept.iter().map(|&i| sentences[i].clone()).collect::<Vec<_>>().join(" "))
}

fn summarize_sentences_headtail(sentences: &[String], budget: usize) -> String {
    let total = sentences.len();
    let head = budget / 2;
    let tail = budget - head;
    let mut result: Vec<String> = Vec::new();
    result.extend_from_slice(&sentences[..head.min(total)]);
    if total > head {
        let tail_start = total.saturating_sub(tail);
        if tail_start > head {
            result.extend_from_slice(&sentences[tail_start..]);
        }
    }
    result.join(" ")
}

// ── Batch embedding (public) ──────────────────────────────────────────────────

pub fn embed_batch(texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
    let model = get_model()?;
    Ok(model.embed(texts.to_vec(), None)?)
}

/// Compute semantic similarity between two texts. Used as a quality gate on generative output.
pub fn semantic_similarity(a: &str, b: &str) -> anyhow::Result<f32> {
    let model = get_model()?;
    let embeddings = model.embed(vec![a, b], None)?;
    Ok(cosine_similarity(&embeddings[0], &embeddings[1]))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_input_not_summarized() {
        let lines: Vec<String> = (0..50).map(|i| format!("line {}", i)).collect();
        let input = lines.join("\n");
        let result = summarize(&input, 60);
        assert!(result.lines_out <= 50 + 1);
    }

    #[test]
    fn long_input_summarized() {
        let lines: Vec<String> = (0..500).map(|i| format!("line {}", i)).collect();
        let input = lines.join("\n");
        let result = summarize(&input, 60);
        assert!(result.output.contains("lines omitted"));
        assert!(result.output.lines().count() < 500);
    }

    #[test]
    fn error_lines_always_kept() {
        let mut lines: Vec<String> = (0..250).map(|i| format!("noise line {}", i)).collect();
        lines[125] = "error[E0308]: mismatched types".to_string();
        let input = lines.join("\n");
        let result = summarize(&input, 60);
        assert!(result.output.contains("error[E0308]: mismatched types"));
    }

    #[test]
    fn warning_lines_always_kept() {
        let mut lines: Vec<String> = (0..250).map(|i| format!("noise line {}", i)).collect();
        lines[200] = "warning: unused variable `x`".to_string();
        let input = lines.join("\n");
        let result = summarize(&input, 60);
        assert!(result.output.contains("warning: unused variable `x`"));
    }

    #[test]
    fn single_line_input() {
        let result = summarize("just one line", 60);
        assert!(result.output.contains("just one line"));
    }

    #[test]
    fn omission_line_counts_correctly() {
        let lines: Vec<String> = (0..500).map(|i| format!("line {}", i)).collect();
        let input = lines.join("\n");
        let result = summarize(&input, 60);
        assert!(result.output.contains("lines omitted"));
    }

    #[test]
    fn configurable_budget() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {}", i)).collect();
        let input = lines.join("\n");
        let result = summarize(&input, 10);
        assert!(result.output.lines().count() <= 100);
    }
}
