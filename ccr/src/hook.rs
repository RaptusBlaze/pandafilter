use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::{self, Read};

#[derive(Debug, Deserialize)]
struct HookInput {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_input: serde_json::Value,
    #[serde(default)]
    tool_response: ToolResponse,
}

#[derive(Debug, Deserialize, Default)]
struct ToolResponse {
    #[serde(default)]
    output: String,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct HookOutput {
    output: String,
}

pub fn run() -> Result<()> {
    let mut input = String::new();
    if io::stdin().read_to_string(&mut input).is_err() {
        return Ok(());
    }

    let hook_input: HookInput = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };

    let command_hint = hook_input
        .tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .and_then(|cmd| cmd.split_whitespace().next())
        .map(|s| s.to_string());

    let output_text = if let Some(err) = &hook_input.tool_response.error {
        err.clone()
    } else {
        hook_input.tool_response.output.clone()
    };

    if output_text.is_empty() {
        return Ok(());
    }

    let config = match crate::config_loader::load_config() {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    // B2: use command string as query for biased BERT summarization
    let query = hook_input
        .tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let pipeline = ccr_core::pipeline::Pipeline::new(config);
    let result = match pipeline.process(
        &output_text,
        command_hint.as_deref(),
        query.as_deref(),
    ) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };

    // ── Session-aware passes ──────────────────────────────────────────────────

    let sid = crate::session::session_id();
    let mut session = crate::session::SessionState::load(&sid);
    let cmd_key = command_hint.as_deref().unwrap_or("unknown");

    // C1: Sentence-level deduplication against recent session content.
    // Marks sentences that repeat earlier tool outputs as [covered in turn N].
    let after_dedup = apply_sentence_dedup(&result.output, cmd_key, &session);

    // C2: Apply extra line compression when the session is token-heavy.
    let compression_factor = session.compression_factor();
    let final_output = if compression_factor < 0.90 {
        let line_count = after_dedup.lines().count();
        let reduced_budget = ((line_count as f32 * compression_factor) as usize).max(10);
        ccr_core::summarizer::summarize(&after_dedup, reduced_budget).output
    } else {
        after_dedup
    };

    // B3: Record in session cache for cross-turn dedup on future calls.
    if let Ok(mut embeddings) = ccr_core::summarizer::embed_batch(&[final_output.as_str()]) {
        if let Some(emb) = embeddings.pop() {
            let tokens = ccr_core::tokens::count_tokens(&final_output);
            session.record(cmd_key, emb, tokens, &final_output);
            session.save(&sid);
        }
    }

    let hook_output = HookOutput { output: final_output };
    println!("{}", serde_json::to_string(&hook_output)?);

    Ok(())
}

/// C1: Build a deduplication context from recent session entries and apply
/// the ccr-sdk sentence deduplicator to the current output.
fn apply_sentence_dedup(
    output: &str,
    _cmd: &str,
    session: &crate::session::SessionState,
) -> String {
    use ccr_sdk::deduplicator::deduplicate;
    use ccr_sdk::message::Message;

    // Use last 8 entries as prior context regardless of command —
    // repeated file content and error messages appear across different commands.
    let prior = session.recent_content(8);
    if prior.is_empty() {
        return output.to_string();
    }

    let mut messages: Vec<Message> = prior
        .into_iter()
        .map(|(_, content)| Message { role: "user".to_string(), content })
        .collect();

    messages.push(Message {
        role: "user".to_string(),
        content: output.to_string(),
    });

    let deduped = deduplicate(messages);

    deduped
        .into_iter()
        .last()
        .map(|m| m.content)
        .unwrap_or_else(|| output.to_string())
}
