use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CcrConfig {
    #[serde(default)]
    pub global: GlobalConfig,
    #[serde(default)]
    pub commands: HashMap<String, CommandConfig>,
    #[serde(default)]
    pub tee: TeeConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TeeConfig {
    pub enabled: bool,
    pub mode: TeeMode,
    pub max_files: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TeeMode {
    Aggressive,
    Always,
    Never,
}

impl Default for TeeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: TeeMode::Aggressive,
            max_files: 20,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GlobalConfig {
    pub summarize_threshold_lines: usize,
    pub head_lines: usize,
    pub tail_lines: usize,
    pub strip_ansi: bool,
    pub normalize_whitespace: bool,
    pub deduplicate_lines: bool,
    /// Additional regex patterns for lines that must never be dropped.
    /// Each entry is ORed with the built-in critical pattern
    /// (error|warning|failed|fatal|panic|exception|critical).
    /// Example: ["OOMKilled", "timeout", "deadline exceeded"]
    #[serde(default)]
    pub hard_keep_patterns: Vec<String>,
    /// BERT embedding model to use for semantic summarization.
    /// Options: "AllMiniLML6V2" (default, ~90MB), "AllMiniLML12V2" (~120MB).
    /// First call wins — changing this requires restarting the process.
    #[serde(default = "default_bert_model")]
    pub bert_model: String,
}

fn default_bert_model() -> String {
    "AllMiniLML6V2".to_string()
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            summarize_threshold_lines: 200,
            head_lines: 30,
            tail_lines: 30,
            strip_ansi: true,
            normalize_whitespace: true,
            deduplicate_lines: true,
            hard_keep_patterns: Vec::new(),
            bert_model: default_bert_model(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CommandConfig {
    #[serde(default)]
    pub patterns: Vec<FilterPattern>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FilterPattern {
    pub regex: String,
    pub action: FilterAction,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum FilterAction {
    Simple(SimpleAction),
    #[allow(non_snake_case)]
    ReplaceWith { ReplaceWith: String },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub enum SimpleAction {
    Remove,
    Collapse,
}
