//! Query module — rank files by relevance using embeddings and cochanges.

use anyhow::Result;
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct RankedFile {
    pub path: String,
    pub role: String,
    pub confidence: f64,
    pub cochange_count: i64,
    pub relevance_score: f64,
}

/// Query the focus graph for relevant files given a prompt embedding.
///
/// Returns files ranked by a combination of:
/// 1. Semantic similarity (embedding distance)  — weight 0.5
/// 2. Co-change frequency (log-normalized)      — weight 0.2
/// 3. Read history boost (if provided)          — weight 0.3
/// 4. Role classification multiplier
pub fn query(
    conn: &Connection,
    prompt_embedding: &[f32],
    top_k: usize,
) -> Result<Vec<RankedFile>> {
    query_with_read_boosts(conn, prompt_embedding, top_k, None)
}

/// Like `query` but accepts optional read-history boosts (file_path → normalized frequency).
pub fn query_with_read_boosts(
    conn: &Connection,
    prompt_embedding: &[f32],
    top_k: usize,
    read_boosts: Option<&std::collections::HashMap<String, f64>>,
) -> Result<Vec<RankedFile>> {
    // Pass 1: collect raw scores
    let mut stmt = conn.prepare(
        "SELECT path, role, role_confidence, embedding, commit_count FROM files"
    )?;

    struct RawCandidate {
        path: String,
        role: String,
        confidence: f64,
        similarity: f64,
        raw_cochange: i64,
    }

    let mut raw_candidates: Vec<RawCandidate> = Vec::new();
    let rows = stmt.query_map([], |row| {
        let path: String = row.get(0)?;
        let role: String = row.get(1)?;
        let confidence: f64 = row.get(2)?;
        let blob: Vec<u8> = row.get(3)?;
        Ok((path, role, confidence, blob))
    })?;

    for row_result in rows {
        let (path, role, confidence, blob) = row_result?;
        let file_embedding = blob_to_embedding(&blob);
        let similarity = cosine_similarity(prompt_embedding, &file_embedding);
        let raw_cochange = get_cochange_score(conn, &path)?;
        raw_candidates.push(RawCandidate {
            path,
            role,
            confidence,
            similarity,
            raw_cochange,
        });
    }

    // Pass 2: log-normalize co-change scores to [0, 1]
    let max_cochange = raw_candidates
        .iter()
        .map(|c| c.raw_cochange)
        .max()
        .unwrap_or(0);
    let log_max = (1.0 + max_cochange as f64).ln();

    let has_read_boosts = read_boosts.map_or(false, |rb| !rb.is_empty());

    // Weights: if read boosts available, use 0.5/0.2/0.3; otherwise 0.7/0.3/0
    let (w_sim, w_cochange, w_read) = if has_read_boosts {
        (0.5, 0.2, 0.3)
    } else {
        (0.7, 0.3, 0.0)
    };

    let mut candidates: Vec<(String, String, f64, i64, f64)> = raw_candidates
        .into_iter()
        .map(|c| {
            let norm_cochange = if log_max > 0.0 {
                (1.0 + c.raw_cochange as f64).ln() / log_max
            } else {
                0.0
            };

            let read_boost = if w_read > 0.0 {
                read_boosts
                    .and_then(|rb| rb.get(&c.path))
                    .copied()
                    .unwrap_or(0.0)
            } else {
                0.0
            };

            let relevance = c.similarity * w_sim + norm_cochange * w_cochange + read_boost * w_read;

            let role_boost = match c.role.as_str() {
                "entry_point" => 1.5,
                "persistence" => 1.2,
                "state_manager" => 1.1,
                _ => 1.0,
            };

            (c.path, c.role, c.confidence, c.raw_cochange, relevance * role_boost)
        })
        .collect();

    candidates.sort_by(|a, b| b.4.partial_cmp(&a.4).unwrap_or(std::cmp::Ordering::Equal));

    Ok(candidates
        .into_iter()
        .take(top_k)
        .map(|(path, role, confidence, cochange_count, relevance_score)| {
            RankedFile {
                path,
                role,
                confidence,
                cochange_count,
                relevance_score,
            }
        })
        .collect())
}

/// Get cochange score for a file (sum of all co-occurrence counts)
fn get_cochange_score(conn: &Connection, file_path: &str) -> Result<i64> {
    let score: i64 = conn.query_row(
        "SELECT COALESCE(SUM(change_count), 0) FROM cochanges
         WHERE file_a = ?1 OR file_b = ?1",
        [file_path],
        |row| row.get(0),
    )?;
    Ok(score)
}

/// Convert 4-byte blob to embedding vector
fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    blob.chunks(4)
        .map(|chunk| {
            if chunk.len() == 4 {
                f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
            } else {
                0.0
            }
        })
        .collect()
}

/// Compute cosine similarity between two embeddings
fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let min_len = a.len().min(b.len());
    let a = &a[..min_len];
    let b = &b[..min_len];

    let dot_product: f64 = a.iter().zip(b.iter()).map(|(x, y)| (*x as f64) * (*y as f64)).sum();

    let a_norm: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let b_norm: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();

    if a_norm == 0.0 || b_norm == 0.0 {
        return 0.0;
    }

    dot_product / (a_norm * b_norm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical() {
        let v = vec![1.0, 0.0, 0.0];
        let similarity = cosine_similarity(&v, &v);
        assert!((similarity - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let similarity = cosine_similarity(&a, &b);
        assert!(similarity.abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let similarity = cosine_similarity(&a, &b);
        assert!((similarity + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_blob_to_embedding() {
        let bytes = vec![0, 0, 128, 63]; // 1.0 in little-endian f32
        let embedding = blob_to_embedding(&bytes);
        assert_eq!(embedding.len(), 1);
        assert!((embedding[0] - 1.0).abs() < 1e-6);
    }
}
