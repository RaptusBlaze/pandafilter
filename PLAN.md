# CCR Optimization Implementation Plan

9 improvements ordered by dependency. Each item includes exact file locations, implementation steps, test strategy, and edge cases.

---

## Recommended Implementation Order

```
P3 → P4 → P8 → P1 → P5 → P7 → P2 → P9 → P6
```

Rationale: config/pattern changes first (no API impact), then BERT initialization before anything that depends on embeddings, then pipeline API changes last since they have the most dependents.

---

## P3 — Expand TOML Patterns

**Why first:** Zero risk, pure config, immediate token savings in the hook path (PostToolUse doesn't call handlers — only the pipeline, so patterns are the only filtering layer there).

### Background

TOML patterns apply in `pipeline.rs:52` via `PatternFilter` — they are applied before BERT, in the hook/PostToolUse path where handlers are not invoked. Currently only `git`, `cargo`, `npm`, `docker` have entries.

### File to Modify

`config/default_filters.toml`

### Patterns to Add

```toml
[commands.kubectl]
patterns = [
  { regex = "^[WIE]\\d{4}\\s+", action = "Remove" },
  { regex = "^Warning:.*deprecated.*", action = "Remove" },
  { regex = "^resource mapping not found.*", action = "Remove" },
]

[commands.terraform]
patterns = [
  { regex = "^\\s*Refreshing state\\.\\.\\.", action = "Collapse" },
  { regex = "^\\s*Reading\\.\\.\\.", action = "Collapse" },
  { regex = "^\\s*[╷╵│].*", action = "Remove" },
]

[commands.brew]
patterns = [
  { regex = "^==> Downloading https://", action = "Collapse" },
  { regex = "^==> Fetching dependencies", action = "Remove" },
  { regex = "^Already downloaded:", action = "Remove" },
]

[commands.go]
patterns = [
  { regex = "^go: downloading ", action = "Collapse" },
  { regex = "^go: finding module ", action = "Remove" },
  { regex = "^go: extracting ", action = "Remove" },
]

[commands.helm]
patterns = [
  { regex = "^coalesce\\.go:\\d+", action = "Remove" },
]

[commands.pip]
patterns = [
  { regex = "^Collecting \\S+", action = "Collapse" },
  { regex = "^  Downloading ", action = "Remove" },
  { regex = "^  Using cached ", action = "Remove" },
  { regex = "^Installing collected packages:", action = "Remove" },
]

[commands.make]
patterns = [
  { regex = "^make\\[\\d+\\]: Entering directory", action = "Remove" },
  { regex = "^make\\[\\d+\\]: Leaving directory", action = "Remove" },
  { regex = "^make\\[\\d+\\]: Nothing to be done", action = "Remove" },
]

[commands.tsc]
patterns = [
  { regex = "^TS\\d+: Build info file ", action = "Remove" },
]

[commands.gh]
patterns = [
  { regex = "^✓ ", action = "Remove" },
]
```

### Test Strategy

- Add `ccr-core/tests/fixtures/` directory with representative raw outputs per command.
- For each new command, add a fixture file (e.g. `kubectl_get_pods.txt`, `terraform_plan.txt`).
- Unit test for each: run `PatternFilter::apply()` on fixture, assert noise lines are gone.
- **Critical property test**: no pattern should match a line containing `error`, `warning`, or `failed` — verify by scanning all new regexes against a set of representative critical lines.
- Snapshot test: save expected filtered output as `*.expected.txt`, compare in CI.

### Edge Cases

- Terraform color codes: patterns target box-drawing chars, not ANSI codes (ANSI is stripped in stage 1 of pipeline before patterns apply — safe).
- Kubectl klog prefixes: `[WIE]\d{4}` is the klog format — must not match lines like `WARNING: some actual warning` (different format — safe, the klog format is `W0101 12:34:56...`).

---

## P4 — Configurable Hard-Keep Patterns

**Why second:** Pure config struct + summarizer change, no pipeline API changes.

### Background

`summarizer.rs` has a static `OnceCell<Regex>` called `CRITICAL_PATTERN` used in 5 internal functions. It's hardcoded to `error|warning|warn|failed|failure|fatal|panic|exception|critical`. Users running domain-specific workloads (k8s, mobile, embedded) have different critical keywords.

### Files to Modify

1. `ccr-core/src/config.rs` — add field to `GlobalConfig`
2. `ccr-core/src/summarizer.rs` — make pattern buildable from config
3. `ccr-core/src/pipeline.rs` — build pattern once and pass to summarizer calls

### Implementation

**`ccr-core/src/config.rs`** — add to `GlobalConfig`:

```rust
/// Additional regex patterns for lines that must never be dropped.
/// ORed with the built-in critical pattern (error|warning|failed|...).
#[serde(default)]
pub hard_keep_patterns: Vec<String>,
```

**`ccr-core/src/summarizer.rs`** — add public builder function:

```rust
pub fn build_critical_pattern(extra_patterns: &[String]) -> Regex {
    if extra_patterns.is_empty() {
        return critical_pattern().clone();  // reuse static singleton
    }
    let base = r"(?i)(error|warning|warn|failed|failure|fatal|panic|exception|critical)";
    let extras = extra_patterns
        .iter()
        .map(|p| format!("(?:{})", p))
        .collect::<Vec<_>>()
        .join("|");
    Regex::new(&format!("{}|{}", base, extras))
        .expect("hard_keep_patterns contains invalid regex")
}
```

**`ccr-core/src/pipeline.rs`** — in `Pipeline::process()`, build the pattern once:

```rust
let critical = ccr_core::summarizer::build_critical_pattern(
    &self.config.global.hard_keep_patterns
);
// pass `&critical` to all summarizer dispatch calls
```

The 5 internal summarizer functions (`summarize_semantic`, `summarize_semantic_intent`, `summarize_semantic_anchored`, `do_cluster_summarize`, `summarize_against_centroid_inner`) each get a new `critical: &Regex` parameter.

**User config** (`~/.config/ccr/config.toml`):

```toml
[global]
hard_keep_patterns = ["OOMKilled", "timeout", "segfault", "deadline exceeded"]
```

### Test Strategy

- Unit: build pattern with extras `["OOMKilled"]`; verify `OOMKilled` lines survive `summarize()` that would otherwise drop them.
- Unit: build pattern with empty extras; verify behavior is identical to current static pattern.
- Config deserialization: load TOML with `hard_keep_patterns = ["foo"]`, assert field parses correctly and reaches `build_critical_pattern`.
- Regex validation: if a user provides an invalid regex, `build_critical_pattern` should return a descriptive error (not panic in production). Add `anyhow::bail!` instead of `expect`.

### Edge Cases

- Invalid user-provided regex: catch at config-load time and log a warning, fall back to default pattern.
- Empty string in `hard_keep_patterns`: `(?:)` matches everything — validate non-empty entries.

---

## P8 — Configurable BERT Model

**Why third:** Must happen before P2 (historical centroid) and any embedding-heavy work, since it controls model quality for all subsequent items.

### Background

`summarizer.rs` initializes `AllMiniLML6V2` in a `OnceCell` via `get_model()`. The `fastembed` crate supports `AllMiniLML12V2` and `ParaphraseMLMiniLML12V2` among others. The model is initialized lazily on first use.

### Files to Modify

1. `ccr-core/src/config.rs` — add `bert_model` field to `GlobalConfig`
2. `ccr-core/src/summarizer.rs` — add `init_model(name)` public function, refactor `get_model()`
3. `ccr/src/main.rs` — call `init_model` at startup after config load

### Implementation

**`ccr-core/src/config.rs`** — add to `GlobalConfig`:

```rust
/// BERT embedding model. Options:
///   "AllMiniLML6V2"              (~90MB, default, fastest)
///   "AllMiniLML12V2"             (~120MB, better quality)
///   "ParaphraseMLMiniLML12V2"    (~120MB, paraphrase-optimized)
#[serde(default = "default_bert_model")]
pub bert_model: String,

fn default_bert_model() -> String {
    "AllMiniLML6V2".to_string()
}
```

**`ccr-core/src/summarizer.rs`** — refactor model initialization:

```rust
static MODEL_CACHE: OnceCell<fastembed::TextEmbedding> = OnceCell::new();

fn load_model(name: &str) -> anyhow::Result<fastembed::TextEmbedding> {
    use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
    let embedding_model = match name {
        "AllMiniLML12V2"          => EmbeddingModel::AllMiniLML12V2,
        "ParaphraseMLMiniLML12V2" => EmbeddingModel::ParaphraseMLMiniLML12V2,
        _                         => EmbeddingModel::AllMiniLML6V2,
    };
    if !bert_is_cached() {
        eprintln!("[ccr] downloading BERT model ({})...", name);
    }
    let model = TextEmbedding::try_new(
        InitOptions::new(embedding_model).with_show_download_progress(false),
    )?;
    mark_bert_cached();
    Ok(model)
}

/// Initialize the BERT model with a specific model name.
/// Must be called before any summarization. First call wins.
pub fn init_model(model_name: &str) -> anyhow::Result<()> {
    MODEL_CACHE.get_or_try_init(|| load_model(model_name))?;
    Ok(())
}

fn get_model() -> anyhow::Result<&'static fastembed::TextEmbedding> {
    // Lazy default if init_model() was never called
    MODEL_CACHE.get_or_try_init(|| load_model("AllMiniLML6V2"))
}
```

**`ccr/src/main.rs`** — after config load, before any subcommand dispatch:

```rust
// Early BERT model initialization based on config
if let Ok(config) = config_loader::load_config() {
    let _ = ccr_core::summarizer::init_model(&config.global.bert_model);
}
```

**Note on sentinel file:** `~/.local/share/ccr/.bert_ready` doesn't encode which model was downloaded. Switching models will still trigger a download (the new model hasn't been downloaded), but the sentinel message won't appear. Document this behavior.

### Test Strategy

- Unit: `init_model("AllMiniLML6V2")` — no error, returns `Ok(())`.
- Unit: `init_model("Unknown")` — falls back to default, no panic.
- Unit: call `init_model()` twice — second call is no-op, `OnceCell` guarantees.
- Config deserialization: load TOML with `bert_model = "AllMiniLML12V2"`, assert field parses.

---

## P1 — PreToolUse Flag Injection

### Background

`cmd/rewrite.rs` currently wraps commands as `ccr run <cmd>`. Flag injection happens inside each handler's `rewrite_args()` when `ccr run` executes. The gap: the command string that Claude Code sees in `tool_input` shows the original command, not the flag-injected form. Additionally, handlers missing useful flags in `rewrite_args()` should be updated.

### Files to Modify

1. `ccr/src/cmd/rewrite.rs` — call handler's `rewrite_args()` to build the full rewritten string
2. Handler files that are missing useful flags in `rewrite_args()`

### Implementation

**`ccr/src/cmd/rewrite.rs`** — in `rewrite_single()`:

```rust
fn rewrite_single(command: &str) -> Option<String> {
    let trimmed = command.trim();
    let args: Vec<String> = trimmed
        .split_whitespace()
        .map(String::from)
        .collect();
    let first = args.first()?.as_str();

    let handler = crate::handlers::get_handler(first)?;
    let rewritten_args = handler.rewrite_args(&args);

    // Format as: ccr run <rewritten args>
    Some(format!("ccr run {}", rewritten_args.join(" ")))
}
```

**Handlers to add/fix `rewrite_args()` implementations:**

| Handler | Subcommand | Flag | Guard condition |
|---|---|---|---|
| `npm` | `install` | `--no-progress` | not already present |
| `terraform` | `plan`, `apply` | `-no-color` | not already present |
| `docker` | `build` | `--progress=plain` | not already present |
| `brew` | `install`, `upgrade` | `--quiet` | not already present |
| `pip` | `install` | `-q` | not already present |
| `helm` | `install`, `upgrade` | `--no-color` | not already present |
| `go` | `test` | `-v` | not already present (needed for output) |

Each guard must check for prefix, not exact match (e.g., `--progress` not just `--progress=plain`).

### Test Strategy

- Unit test for `rewrite_single("cargo build")` → assert result is `"ccr run cargo build --message-format json"`.
- Unit test for `rewrite_single("git log")` → assert result contains `--oneline`.
- **No-double-injection test**: `rewrite_single("cargo build --message-format human")` → assert `--message-format` appears exactly once.
- **Unknown command test**: `rewrite_single("mycustom tool")` → `None`.
- Integration: run `ccr rewrite "npm install"` and assert stdout contains `--no-progress`.

### Edge Cases

- Compound commands (`cmd1 && cmd2`): already handled by `rewrite_compound()` which calls `rewrite_single()` on each part.
- Commands with pipes (`cmd | grep`): `rewrite_compound` doesn't split on `|`. Document as limitation.
- Subcommand not in handler's flag list (e.g. `cargo fmt`): `rewrite_args` must return args unchanged.

---

## P5 — Delta Compression Wiring

### Background

`hook.rs` already calls `session.compute_delta()` — but `content_preview` is truncated to 600 characters (~5-10 lines), making line-level matching mostly useless for real outputs. Delta compression is also missing from the `cmd/run.rs` path.

### Files to Modify

1. `ccr/src/session.rs` — increase `content_preview` from 600 to 4000 chars
2. `ccr/src/cmd/run.rs` — add delta compression after the handler filtering step

### Implementation

**`ccr/src/session.rs`** — increase preview size:

```rust
// line 135 (approximate):
content_preview: content.chars().take(4000).collect(),
```

Make it a named constant:

```rust
const CONTENT_PREVIEW_CHARS: usize = 4000;
```

Also rename `prior_sentences` → `prior_lines` in `compute_delta()` for clarity (line 226).

**`ccr/src/cmd/run.rs`** — add after handler filtering, before B3 session check:

```rust
// Idea 3: Delta compression
let filtered = {
    match ccr_core::summarizer::embed_single(&filtered) {
        Ok(emb) => {
            let lines: Vec<&str> = filtered.lines().collect();
            match session.compute_delta(&cmd_name, &lines, &emb) {
                Some(delta) => delta.output,
                None => filtered,
            }
        }
        Err(_) => filtered,
    }
};
```

Note: `embed_single()` may not exist — check if `embed_batch(&[text])` is the public API and use that, popping the first result.

**Analytics:** Add delta hit count to `AnalyticsRecord` or log to stderr for observability:

```rust
eprintln!("[ccr] delta: {} lines unchanged, {} new", delta.same_count, delta.new_count);
```

### Test Strategy

- Unit: create a session with a prior entry with 4000-char `content_preview`; run `compute_delta` with output that overlaps 80%. Assert `same_count` reflects overlap.
- **Regression test for old behavior**: with 600-char preview (old value), verify delta misses most matches. With 4000-char preview, verify it catches them.
- Unit test `cmd/run.rs` path: use a mock session file with a prior run, verify filtered output has delta markers.

### Edge Cases

- First run of a command (no prior session entry): `compute_delta` returns `None`, falls through to existing logic — correct behavior.
- Output that is 100% new: `delta.same_count == 0`, output returned unchanged — correct.
- Very short outputs (<5 lines): delta overhead may exceed savings. Add a minimum-lines guard (e.g., skip delta for outputs under 20 lines).

---

## P7 — Streaming / Chunked Processing

### Background

`pipeline.rs` processes all input as a single batch. BERT requires all lines before scoring — true streaming isn't possible. But chunked processing reduces peak memory for large outputs, and streaming pattern pre-filtering reduces what gets buffered for BERT.

### Files to Modify

1. `ccr-core/src/pipeline.rs` — add chunked path for inputs > threshold
2. `ccr-core/src/patterns.rs` — add `should_remove(line) -> bool` for streaming pre-filter
3. `ccr/src/cmd/run.rs` — use `Command::spawn()` + streaming pre-filter

### Implementation

**`ccr-core/src/pipeline.rs`** — constants and chunked dispatcher:

```rust
const CHUNK_THRESHOLD_LINES: usize = 2000;
const CHUNK_SIZE_LINES: usize = 500;

pub fn process_chunked(
    &self,
    input: &str,
    command_hint: Option<&str>,
    query: Option<&str>,
    historical_centroid: Option<&[f32]>,  // from P2
) -> anyhow::Result<PipelineResult> {
    // Stage 1 & 2: ANSI strip + whitespace normalization (cheap, apply to full input)
    let normalized = self.normalize(input);

    // Stage 3: pattern filters (cheap, apply to full normalized)
    let pattern_filtered = self.apply_patterns(&normalized, command_hint);

    let lines: Vec<&str> = pattern_filtered.lines().collect();
    if lines.len() <= CHUNK_THRESHOLD_LINES {
        // Fall through to normal BERT summarization
        return self.summarize_lines(&lines, command_hint, query, historical_centroid);
    }

    // Chunked path: summarize each chunk independently
    let mut chunk_summaries = Vec::new();
    let mut total_input_tokens = 0usize;
    let mut total_output_tokens = 0usize;

    for chunk in lines.chunks(CHUNK_SIZE_LINES) {
        let chunk_text = chunk.join("\n");
        let result = self.summarize_lines(
            &chunk.iter().map(|s| *s).collect::<Vec<_>>(),
            command_hint, query, historical_centroid,
        )?;
        total_input_tokens += result.analytics.input_tokens;
        total_output_tokens += result.analytics.output_tokens;
        chunk_summaries.push(result.output);
    }

    let combined = chunk_summaries.join("\n[--- next chunk ---]\n");
    Ok(PipelineResult {
        output: combined,
        analytics: Analytics {
            input_tokens: total_input_tokens,
            output_tokens: total_output_tokens,
        },
    })
}
```

Make `process()` call `process_chunked()` (they share the same logic, `process()` becomes a wrapper).

**`ccr-core/src/patterns.rs`** — add streaming helper:

```rust
pub fn should_remove(&self, line: &str, command_hint: Option<&str>) -> bool {
    // Check only Remove-action patterns for the given command
    // Returns true if line matches any Remove pattern
}
```

**`ccr/src/cmd/run.rs`** — streaming pre-filter with `spawn()`:

```rust
use std::io::{BufRead, BufReader};

let mut child = Command::new(&final_args[0])
    .args(&final_args[1..])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;

let stdout = child.stdout.take().unwrap();
let reader = BufReader::new(stdout);
let mut pre_filtered = String::new();
let pattern_filter = config.get_pattern_filter(&cmd_name);

for line in reader.lines() {
    let line = line?;
    if !pattern_filter.should_remove(&line, Some(&cmd_name)) {
        pre_filtered.push_str(&line);
        pre_filtered.push('\n');
    }
}
child.wait()?;

// Then run BERT summarization on pre_filtered (already pattern-filtered)
```

### Test Strategy

- Unit test `process_chunked`: feed 3000 lines; verify output contains chunk separators; verify an error on line 501 (chunk boundary) is preserved.
- Benchmark: compare peak `RSS` memory for 10k-line input between `process()` and `process_chunked()`.
- Integration: run `cargo build` output (typically 500-2000 lines) through chunked path; verify output quality matches non-chunked path.

### Edge Cases

- Error line at chunk boundary (last line of chunk N = first line of chunk N+1): not an issue since chunking is by lines, not bytes.
- Chunk summary that is itself empty (all noise): emit a `[chunk omitted — all noise]` marker rather than empty string.

---

## P2 — Wire Historical Centroid

**Why here:** Depends on P4 (configurable hard-keep integrated into pipeline) and P8 (model initialization), which must come first.

### Background

`summarize_against_centroid()` in `summarizer.rs` is implemented but never called from the primary pipeline path. `session.rs` stores and updates `command_centroids`. The centroid represents "what typical output for this command looks like" — lines that deviate from it are anomalies worth keeping.

The centroid IS used in the C2 path (second compression pass when cumulative tokens > 50k in `hook.rs`), but not in the primary first-pass summarization.

### Files to Modify

1. `ccr-core/src/pipeline.rs` — add `historical_centroid` parameter to `process()`
2. `ccr/src/hook.rs` — hoist session load, pass centroid to `pipeline.process()`
3. `ccr/src/cmd/run.rs` — same change
4. `ccr-core/src/summarizer.rs` — add `compute_output_centroid()` helper

### Implementation

**`ccr-core/src/pipeline.rs`** — updated `process()` signature:

```rust
pub fn process(
    &self,
    input: &str,
    command_hint: Option<&str>,
    query: Option<&str>,
    historical_centroid: Option<&[f32]>,
) -> anyhow::Result<PipelineResult>
```

In the summarizer dispatch block, add centroid arm:

```rust
// Priority order for summarization strategy:
match (command_hint, query, historical_centroid) {
    // 1. centroid available, no query → score against history
    (Some(_), None, Some(centroid)) =>
        summarize_against_centroid(&text, budget, centroid, &critical)?,
    // 2. query available → BERT-biased toward query
    (_, Some(q), _) =>
        summarize_with_query(&text, budget, q, &critical)?,
    // 3. command known, no query, no centroid → clustering
    (Some(cmd), None, None) =>
        summarize_with_clustering(&text, budget, &critical)?,
    // 4. nothing → anomaly anchoring
    _ =>
        summarize_with_anchoring(&text, budget, &critical)?,
}
```

**`ccr-core/src/summarizer.rs`** — add helper for better centroid updates:

```rust
/// Compute the mean embedding of all lines in text.
/// Better for centroid tracking than embedding the whole text as one string.
pub fn compute_output_centroid(text: &str) -> anyhow::Result<Vec<f32>> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return Ok(vec![0.0f32; 384]);
    }
    let embeddings = embed_batch(&lines)?;
    let dim = embeddings[0].len();
    let mut centroid = vec![0.0f32; dim];
    for emb in &embeddings {
        for (c, v) in centroid.iter_mut().zip(emb.iter()) {
            *c += v;
        }
    }
    let n = embeddings.len() as f32;
    centroid.iter_mut().for_each(|c| *c /= n);
    Ok(centroid)
}
```

**`ccr/src/hook.rs`** — hoist session load before pipeline call:

```rust
// Load session BEFORE pipeline call (was loaded after at line 82)
let sid = crate::session::session_id();
let mut session = crate::session::SessionState::load(&sid);
let cmd_key = command_hint.as_deref().unwrap_or("unknown");
let historical_centroid = session.command_centroid(cmd_key).cloned();

let result = pipeline.process(
    &output_text,
    command_hint.as_deref(),
    query.as_deref(),
    historical_centroid.as_deref(),
)?;

// Update centroid with current output's line-mean embedding (not whole-text embedding)
if let Ok(new_centroid) = ccr_core::summarizer::compute_output_centroid(&result.output) {
    session.update_command_centroid(cmd_key, new_centroid);
}
```

### Test Strategy

- Unit: feed a pipeline instance with a historical centroid representing "normal cargo build output". Feed an input that is 90% normal + 1 error line. Verify the error line is kept and compression is higher than without centroid.
- Unit: all-zeros centroid → `summarize_against_centroid` should fall back gracefully (check existing code at line 910-913).
- Integration: simulate two consecutive hook invocations with the same command. Verify second invocation uses the centroid from the first.

### Edge Cases

- `command_hint` is `None` (unknown command): no centroid lookup, falls through to anchoring — correct.
- First-ever run of a command: centroid is `None`, falls through to clustering — correct.
- Centroid of length 0 or wrong dimension: `summarize_against_centroid` must validate dimension matches model output (384 for AllMiniLML6V2).

---

## P9 — Discover Accuracy Improvement

**Two tracks; implement Track A now, Track B after P2 is stable.**

### Background

`cmd/discover.rs` uses hardcoded savings ratios per command (e.g. `cargo=0.87`) and reports bytes not tokens. Track A replaces static ratios with measured ratios from `analytics.jsonl`. Track B replays actual outputs through the pipeline.

### Files to Modify

1. `ccr/src/cmd/discover.rs` — load actual ratios from analytics, fix bytes→tokens
2. `ccr/src/cmd/discover.rs` — extend static ratios table to cover all handlers
3. `ccr/src/cmd/discover.rs` — add `--replay` flag for Track B

### Track A Implementation

**Load actual ratios from analytics:**

```rust
fn load_actual_savings_ratios() -> HashMap<String, f32> {
    let path = dirs::data_local_dir()
        .map(|d| d.join("ccr").join("analytics.jsonl"));
    // ... reuse analytics record parsing from gain.rs ...
    // aggregate: for each command, sum input_tokens and output_tokens
    // ratio = (input - output) / input
}
```

In savings estimation:

```rust
let actual_ratios = load_actual_savings_ratios();
let savings_pct = actual_ratios
    .get(cmd)
    .copied()
    .or_else(|| static_ratios.get(cmd).copied())
    .unwrap_or(0.40);
```

**Fix bytes → tokens:** In `scan_jsonl()`, replace byte counting with token counting:

```rust
// Before:
total_output_bytes += output_str.len();

// After:
total_output_tokens += ccr_core::tokens::count_tokens(output_str);
```

**Extend static ratios table** to cover all handlers:

```rust
let handler_savings: &[(&str, f32)] = &[
    // existing
    ("cargo", 0.87), ("curl", 0.96), ("git", 0.80),
    ("docker", 0.85), ("npm", 0.85),
    // new
    ("kubectl", 0.75), ("terraform", 0.70),
    ("pytest", 0.80), ("jest", 0.75),
    ("pip", 0.60), ("go", 0.65),
    ("helm", 0.70), ("brew", 0.65),
    ("gh", 0.60), ("make", 0.55),
    ("tsc", 0.70), ("mvn", 0.80),
    ("python", 0.50), ("eslint", 0.65),
];
```

### Track B Implementation (`--replay`)

Add flag to `Commands::Discover`:

```rust
/// Replay sampled historical outputs through the actual pipeline for exact savings.
#[arg(long)]
replay: bool,
```

```rust
fn replay_savings(cmd: &str, sample_outputs: &[String]) -> f32 {
    let config = config_loader::load_config().unwrap_or_default();
    let pipeline = ccr_core::pipeline::Pipeline::new(config);
    let (mut total_in, mut total_out) = (0usize, 0usize);
    for output in sample_outputs.iter().take(5) {  // max 5 samples per command
        if let Ok(result) = pipeline.process(output, Some(cmd), None, None) {
            total_in += result.analytics.input_tokens;
            total_out += result.analytics.output_tokens;
        }
    }
    if total_in == 0 { return 0.0; }
    (total_in - total_out.min(total_in)) as f32 / total_in as f32
}
```

### Test Strategy

- Unit Track A: mock `analytics.jsonl` with known ratios; verify `discover` output uses measured values for covered commands and static for uncovered.
- Unit: verify token counting is used, not byte counting.
- Integration Track B: create fixture output file for `cargo build`; run replay; verify ratio is within 5% of actual pipeline ratio.
- **Edge case**: empty analytics (no prior usage) → static ratios apply, no crash.

---

## P6 — Integrate ccr-sdk Conversation Compression

### Background

`ccr-sdk` has a complete `Optimizer` (deduplicator + tiered compressor + optional Ollama generative summarization). It's used partially: `hook.rs` calls `deduplicator::deduplicate()` directly but bypasses `Optimizer`. There is no `ccr compress` CLI command.

### Files to Modify

1. `ccr/src/main.rs` — add `Compress` command variant
2. New file: `ccr/src/cmd/compress.rs`

### Implementation

**`ccr/src/main.rs`** — add to `Commands`:

```rust
/// Compress a conversation JSON to reduce token count
Compress {
    /// Path to conversation JSON file (use - for stdin)
    #[arg(default_value = "-")]
    input: String,

    /// Write compressed output to file (default: stdout)
    #[arg(long, short = 'o')]
    output: Option<String>,

    /// Number of most-recent turns to preserve verbatim
    #[arg(long, default_value = "3")]
    recent_turns: usize,

    /// Number of tier-1 turns (moderate compression) after recent
    #[arg(long, default_value = "5")]
    tier1_turns: usize,

    /// Ollama base URL for generative summarization (optional, e.g. http://localhost:11434)
    #[arg(long)]
    ollama: Option<String>,

    /// Ollama model to use for generative compression
    #[arg(long, default_value = "mistral:instruct")]
    ollama_model: String,
},
```

**New file: `ccr/src/cmd/compress.rs`**

```rust
use ccr_sdk::{message::Message, optimizer::Optimizer, compressor::CompressionConfig};

pub fn run(
    input: &str,
    output: Option<&str>,
    recent_turns: usize,
    tier1_turns: usize,
    ollama_url: Option<&str>,
    ollama_model: &str,
) -> anyhow::Result<()> {
    // 1. Read input
    let raw = if input == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        std::fs::read_to_string(input)?
    };

    // 2. Parse: accept both [{role, content}] array and {messages: [...]} object
    let messages: Vec<Message> = parse_conversation(&raw)?;

    if messages.is_empty() {
        if let Some(out) = output {
            std::fs::write(out, "[]")?;
        } else {
            println!("[]");
        }
        return Ok(());
    }

    // 3. Build config
    let config = CompressionConfig {
        recent_turns,
        tier1_turns,
        ollama: ollama_url.map(|url| ccr_sdk::ollama::OllamaConfig {
            base_url: url.to_string(),
            model: ollama_model.to_string(),
            similarity_threshold: 0.80,
        }),
        ..Default::default()
    };

    // 4. Compress
    let optimizer = Optimizer { config };
    let result = optimizer.compress(messages)?;

    // 5. Output
    let json = serde_json::to_string_pretty(&result.messages)?;
    match output {
        Some(path) => std::fs::write(path, &json)?,
        None => println!("{}", json),
    }

    // 6. Stats to stderr
    eprintln!(
        "[ccr compress] tokens: {} → {} ({:.0}% saved)",
        result.input_tokens,
        result.output_tokens,
        100.0 * (result.input_tokens - result.output_tokens) as f64 / result.input_tokens as f64
    );

    Ok(())
}

fn parse_conversation(raw: &str) -> anyhow::Result<Vec<Message>> {
    // Try array first
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(raw) {
        return arr.iter().map(|v| Ok(Message {
            role: v["role"].as_str().unwrap_or("user").to_string(),
            content: v["content"].as_str().unwrap_or("").to_string(),
        })).collect();
    }
    // Try {messages: [...]} object
    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(raw) {
        if let Some(msgs) = obj["messages"].as_array() {
            return msgs.iter().map(|v| Ok(Message {
                role: v["role"].as_str().unwrap_or("user").to_string(),
                content: v["content"].as_str().unwrap_or("").to_string(),
            })).collect();
        }
    }
    anyhow::bail!("input is not a valid conversation JSON (expected array or {{messages: [...]}})")
}
```

**Update `hook.rs`** to use `Optimizer` instead of calling `deduplicator::deduplicate()` directly (architecture consistency — optional but recommended):

```rust
// Replace direct deduplicator call with Optimizer
let optimizer = ccr_sdk::optimizer::Optimizer::default();
let deduped = optimizer.dedup_only(messages)?;
```

Or leave the hook's direct call as-is if the Optimizer's full pipeline is too slow for the hot path.

### Test Strategy

- Unit: compress a conversation with 5 repetitive assistant turns; verify `output_tokens < input_tokens`.
- Unit: empty input `[]` → returns `[]` without error.
- Unit: all messages in "recent" window → returned verbatim.
- Unit: both input formats (`[...]` and `{messages: [...]}`) parse correctly.
- Unit: Ollama unavailable → graceful fallback to extractive compression (test by providing invalid Ollama URL).
- Integration: pipe a real Claude Code history JSON through `ccr compress`, verify output is valid JSON and token count is lower.

### Edge Cases

- Input JSON has extra fields beyond `role`/`content` (e.g. `id`, `timestamp`): preserve in output or discard? Document behavior (recommend: discard unknown fields in compressed output).
- Single message: return verbatim.
- Very large messages (>32k tokens each): Ollama generative path may fail. Add per-message token limit guard before calling Ollama.

---

## Cross-Cutting Test Infrastructure

### Fixture Directory

Create `ccr-core/tests/fixtures/` with representative raw outputs:

```
fixtures/
  cargo_build_verbose.txt       # 500+ lines, mix of compiling + error
  git_log_long.txt              # 200 commit log entries
  kubectl_get_pods.txt          # pods table + klog noise
  terraform_plan.txt            # plan output with refresh noise
  npm_install.txt               # download progress spam
  docker_build.txt              # layer progress
  brew_install.txt              # fetch + install output
  pip_install.txt               # collecting + downloading lines
  make_build.txt                # entering/leaving directory noise
```

Each fixture has a paired `*.expected.txt` snapshot of what CCR should produce.

### Shared Test Helper

Add to `ccr-core/src/lib.rs` (or a `test_utils` module):

```rust
#[cfg(test)]
pub mod test_utils {
    use crate::{config::CcrConfig, pipeline::Pipeline};

    pub fn test_pipeline(input: &str, cmd: &str) -> crate::pipeline::PipelineResult {
        let config = CcrConfig::default();
        let pipeline = Pipeline::new(config);
        pipeline.process(input, Some(cmd), None, None)
            .expect("test pipeline failed")
    }
}
```

### Benchmark Suite

Add `ccr-core/benches/pipeline.rs` using `criterion`:

```rust
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_summarize(c: &mut Criterion) {
    let input_1k = include_str!("../tests/fixtures/cargo_build_verbose.txt");
    c.bench_function("pipeline_1k_lines", |b| {
        b.iter(|| test_utils::test_pipeline(input_1k, "cargo"))
    });
}
```

### Regression Snapshots

Use file-based comparison (no extra crate needed):

```rust
fn assert_snapshot(actual: &str, fixture_name: &str) {
    let expected_path = format!("tests/fixtures/{}.expected.txt", fixture_name);
    if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
        std::fs::write(&expected_path, actual).unwrap();
    } else {
        let expected = std::fs::read_to_string(&expected_path)
            .expect("snapshot not found — run with UPDATE_SNAPSHOTS=1 to create");
        assert_eq!(actual.trim(), expected.trim(), "snapshot mismatch for {}", fixture_name);
    }
}
```

Run `UPDATE_SNAPSHOTS=1 cargo test` to regenerate all snapshots.

---

## Summary Table

| Item | Primary Files | Risk | Dependencies | Test Focus |
|---|---|---|---|---|
| P3 TOML patterns | `config/default_filters.toml` | Low | None | Property: no pattern kills critical lines |
| P4 Hard-keep config | `config.rs`, `summarizer.rs`, `pipeline.rs` | Low | None | Config deserialization, regex safety |
| P8 BERT model | `config.rs`, `summarizer.rs`, `main.rs` | Low | None | Init idempotency, fallback to default |
| P1 Flag injection | `cmd/rewrite.rs`, handler files | Low-Med | None | No-double-flag, unknown command |
| P5 Delta wiring | `session.rs`, `cmd/run.rs` | Med | None | Preview size regression, delta correctness |
| P7 Chunked processing | `pipeline.rs`, `patterns.rs`, `cmd/run.rs` | Med | P3 | Chunk boundary error preservation |
| P2 Historical centroid | `pipeline.rs`, `summarizer.rs`, `hook.rs` | Med | P4, P8 | Centroid improves compression on 2nd run |
| P9 Discover accuracy | `cmd/discover.rs` | Low | P2 (Track B) | Empty analytics fallback |
| P6 Compress command | `main.rs`, new `cmd/compress.rs` | Low | None | Both JSON formats, Ollama fallback |
