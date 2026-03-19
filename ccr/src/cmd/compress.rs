use anyhow::Result;
use ccr_sdk::{
    compressor::{compress, CompressionConfig},
    deduplicator::deduplicate,
    message::Message,
    ollama::OllamaConfig,
};
use std::io::Read;

pub fn run(
    input: &str,
    output: Option<&str>,
    recent_turns: usize,
    tier1_turns: usize,
    ollama_url: Option<&str>,
    ollama_model: &str,
    max_tokens: Option<usize>,
) -> Result<()> {
    // Read input from file path or stdin ("-")
    let raw = if input == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        std::fs::read_to_string(input)
            .map_err(|e| anyhow::anyhow!("cannot read '{}': {}", input, e))?
    };

    let messages = parse_conversation(&raw)?;

    if messages.is_empty() {
        let out = "[]";
        write_output(out, output)?;
        return Ok(());
    }

    let config = CompressionConfig {
        recent_n: recent_turns,
        tier1_n: tier1_turns,
        ollama: ollama_url.map(|url| OllamaConfig {
            base_url: url.to_string(),
            model: ollama_model.to_string(),
            similarity_threshold: 0.80,
        }),
        max_context_tokens: max_tokens,
        ..CompressionConfig::default()
    };

    // Deduplicate first, then compress (matches Optimizer logic)
    let deduped = deduplicate(messages);
    let result = compress(deduped, &config);

    let json = serde_json::to_string_pretty(&result.messages)?;
    write_output(&json, output)?;

    // Stats to stderr so they don't pollute piped output
    if result.tokens_in > 0 {
        let saved_pct =
            100.0 * (result.tokens_in - result.tokens_out.min(result.tokens_in)) as f64
                / result.tokens_in as f64;
        eprintln!(
            "[ccr compress] {} → {} tokens ({:.0}% saved)",
            result.tokens_in, result.tokens_out, saved_pct
        );
    }

    Ok(())
}

fn write_output(content: &str, path: Option<&str>) -> Result<()> {
    match path {
        Some(p) => std::fs::write(p, content)
            .map_err(|e| anyhow::anyhow!("cannot write to '{}': {}", p, e)),
        None => {
            println!("{}", content);
            Ok(())
        }
    }
}

/// Parse a conversation JSON.
/// Accepts two formats:
///   1. `[{"role": "...", "content": "..."}]`  — bare array
///   2. `{"messages": [{"role": "...", "content": "..."}]}`  — object with messages key
fn parse_conversation(raw: &str) -> Result<Vec<Message>> {
    // Try bare array first
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(raw) {
        return arr
            .iter()
            .map(|v| {
                Ok(Message {
                    role: v["role"].as_str().unwrap_or("user").to_string(),
                    content: v["content"].as_str().unwrap_or("").to_string(),
                })
            })
            .collect();
    }

    // Try {messages: [...]} object
    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(raw) {
        if let Some(msgs) = obj["messages"].as_array() {
            return msgs
                .iter()
                .map(|v| {
                    Ok(Message {
                        role: v["role"].as_str().unwrap_or("user").to_string(),
                        content: v["content"].as_str().unwrap_or("").to_string(),
                    })
                })
                .collect();
        }
    }

    anyhow::bail!(
        "input is not valid conversation JSON \
         (expected array or {{\"messages\": [...]}})"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_array() {
        let json = r#"[{"role":"user","content":"hello"},{"role":"assistant","content":"hi"}]"#;
        let msgs = parse_conversation(json).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].content, "hi");
    }

    #[test]
    fn parse_messages_object() {
        let json = r#"{"messages":[{"role":"user","content":"hello"}]}"#;
        let msgs = parse_conversation(json).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[test]
    fn parse_empty_array() {
        let json = "[]";
        let msgs = parse_conversation(json).unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn parse_invalid_json_errors() {
        let result = parse_conversation("not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn compression_reduces_tokens_for_long_history() {
        let long = "This is a long message with several sentences. It discusses project details. \
                    Make sure errors are never dropped. The config should be in TOML format. \
                    We want fast performance and low token usage.";
        let mut pairs: Vec<serde_json::Value> = Vec::new();
        for i in 0..10 {
            pairs.push(serde_json::json!({"role": "user", "content": long}));
            pairs.push(serde_json::json!({"role": "assistant", "content": format!("Response {}.", i)}));
        }
        let json = serde_json::to_string(&pairs).unwrap();
        let msgs = parse_conversation(&json).unwrap();
        let config = CompressionConfig::default();
        let deduped = deduplicate(msgs);
        let result = compress(deduped, &config);
        assert!(result.tokens_out <= result.tokens_in);
    }

    #[test]
    fn empty_input_returns_empty_json() {
        // Verify the empty path works end-to-end
        let msgs = parse_conversation("[]").unwrap();
        assert!(msgs.is_empty());
    }
}
