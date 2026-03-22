use super::Handler;

pub struct GitHandler;

const PUSH_PULL_ERROR_TERMS: &[&str] = &["error:", "rejected", "conflict", "denied", "fatal:"];

impl Handler for GitHandler {
    fn rewrite_args(&self, args: &[String]) -> Vec<String> {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        match subcmd {
            "log" => {
                if !args.iter().any(|a| a == "--oneline") {
                    let mut out = args.to_vec();
                    out.insert(2, "--oneline".to_string());
                    return out;
                }
            }
            "status" => {
                // Inject --porcelain -b so filter_status gets XY format + branch line
                let has_porcelain = args.iter().any(|a| a == "--porcelain" || a == "--short" || a == "-s");
                if !has_porcelain {
                    let mut out = args.to_vec();
                    out.insert(2, "-b".to_string());
                    out.insert(2, "--porcelain".to_string());
                    return out;
                }
            }
            _ => {}
        }
        args.to_vec()
    }

    fn filter(&self, output: &str, args: &[String]) -> String {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        match subcmd {
            "status" => filter_status(output),
            "log" => filter_log(output),
            "diff" => filter_diff(output),
            "push" | "pull" | "fetch" => filter_push_pull(output),
            "commit" | "add" => filter_commit(output),
            "branch" | "stash" => filter_list(output),
            _ => output.to_string(),
        }
    }
}

// ─── status ──────────────────────────────────────────────────────────────────

fn filter_status(output: &str) -> String {
    if output.contains("nothing to commit") || output.trim().is_empty() {
        return "nothing to commit, working tree clean".to_string();
    }

    let mut branch_line: Option<String> = None;
    let mut staged: Vec<String> = Vec::new();
    let mut modified: Vec<String> = Vec::new();
    let mut untracked: Vec<String> = Vec::new();
    let mut conflicts: usize = 0;

    for line in output.lines() {
        // Porcelain -b branch line: "## main...origin/main [ahead 2]"
        if line.starts_with("## ") {
            branch_line = Some(parse_branch_line(&line[3..]));
            continue;
        }
        if line.trim().is_empty()
            || line.trim().starts_with("(use \"git")
            || line.trim().starts_with("no changes added")
        {
            continue;
        }
        if line.len() < 2 {
            continue;
        }

        let x = line.chars().next().unwrap_or(' ');
        let y = line.chars().nth(1).unwrap_or(' ');

        if x == '?' && y == '?' {
            let name = line.get(3..).unwrap_or("").trim().to_string();
            if !name.is_empty() {
                untracked.push(name);
            }
            continue;
        }

        // Unmerged / conflict states: DD, AU, UD, UA, DU, AA, UU
        if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
            conflicts += 1;
            continue;
        }

        let rest = line.get(3..).unwrap_or("").trim().to_string();
        if rest.is_empty() {
            continue;
        }
        if x != ' ' && x != '#' {
            staged.push(rest.clone());
        }
        if y != ' ' && y != '#' {
            modified.push(rest);
        }
    }

    if staged.is_empty() && modified.is_empty() && untracked.is_empty() && conflicts == 0 {
        return "nothing to commit, working tree clean".to_string();
    }

    let mut out: Vec<String> = Vec::new();

    if let Some(b) = branch_line {
        out.push(b);
    }

    if conflicts > 0 {
        out.push(format!("! Conflicts: {} files", conflicts));
    }

    const MAX_STAGED: usize = 15;
    const MAX_MODIFIED: usize = 15;
    const MAX_UNTRACKED: usize = 10;

    if !staged.is_empty() {
        out.push(format!("+ Staged: {} files", staged.len()));
        for f in staged.iter().take(MAX_STAGED) {
            out.push(format!("   {}", f));
        }
        let extra = staged.len().saturating_sub(MAX_STAGED);
        if extra > 0 {
            out.push(format!("   [+{} more]", extra));
        }
    }

    if !modified.is_empty() {
        out.push(format!("~ Modified: {} files", modified.len()));
        for f in modified.iter().take(MAX_MODIFIED) {
            out.push(format!("   {}", f));
        }
        let extra = modified.len().saturating_sub(MAX_MODIFIED);
        if extra > 0 {
            out.push(format!("   [+{} more]", extra));
        }
    }

    if !untracked.is_empty() {
        out.push(format!("? Untracked: {} files", untracked.len()));
        for f in untracked.iter().take(MAX_UNTRACKED) {
            out.push(format!("   {}", f));
        }
        let extra = untracked.len().saturating_sub(MAX_UNTRACKED);
        if extra > 0 {
            out.push(format!("   [+{} more]", extra));
        }
    }

    out.join("\n")
}

/// Parse `main...origin/main [ahead 2, behind 1]` into a compact branch summary.
fn parse_branch_line(rest: &str) -> String {
    // "main...origin/main [ahead 2]" or "HEAD (no branch)" or "main"
    if rest.starts_with("HEAD (no branch)") {
        return "* HEAD (detached)".to_string();
    }
    let (tracking, sync) = if let Some(idx) = rest.find("...") {
        let branch = &rest[..idx];
        let tail = &rest[idx + 3..];
        // tail may be "origin/main [ahead 2, behind 1]" or "origin/main"
        let remote = tail.split_whitespace().next().unwrap_or("");
        let info = if let Some(start) = tail.find('[') {
            tail[start..].trim_matches(|c| c == '[' || c == ']').to_string()
        } else {
            String::new()
        };
        let remote_short = remote.splitn(2, '/').nth(1).unwrap_or(remote);
        let sync_str = if info.is_empty() {
            format!("== {}", remote_short)
        } else {
            format!("{} ({})", remote_short, info)
        };
        (branch.to_string(), sync_str)
    } else {
        (rest.trim().to_string(), String::new())
    };

    if sync.is_empty() {
        format!("* {}", tracking)
    } else {
        format!("* {}...{}", tracking, sync)
    }
}

// ─── log ─────────────────────────────────────────────────────────────────────

/// Trailer prefixes stripped from one-line commit subjects.
const TRAILERS: &[&str] = &[
    "Signed-off-by:", "Co-authored-by:", "Change-Id:", "Reviewed-by:",
    "Acked-by:", "Tested-by:", "Reported-by:", "Cc:",
];

fn filter_log(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter(|l| {
            let msg = l.splitn(2, ' ').nth(1).unwrap_or("");
            !TRAILERS.iter().any(|t| msg.trim_start().starts_with(t))
        })
        .take(20)
        .collect();

    let total = output.lines().count();
    let mut result: Vec<String> = lines
        .iter()
        .map(|l| {
            let chars: Vec<char> = l.chars().collect();
            if chars.len() > 100 {
                format!("{}…", chars[..99].iter().collect::<String>())
            } else {
                l.to_string()
            }
        })
        .collect();

    if total > 20 {
        result.push(format!("[+{} more commits]", total - 20));
    }
    result.join("\n")
}

// ─── diff ────────────────────────────────────────────────────────────────────

/// Hard cap per hunk and across the whole diff.
const HUNK_LINE_CAP: usize = 30;
const DIFF_TOTAL_CAP: usize = 500;

fn filter_diff(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut file_adds: usize = 0;
    let mut file_dels: usize = 0;
    let mut in_file = false;
    let mut hunk_lines: usize = 0;
    let mut hunk_truncated = false;
    let mut total_lines: usize = 0;
    let mut global_truncated = false;

    for line in &lines {
        if global_truncated {
            // Still tally for the last file summary
            if line.starts_with('+') && !line.starts_with("+++") { file_adds += 1; }
            else if line.starts_with('-') && !line.starts_with("---") { file_dels += 1; }
            continue;
        }

        if line.starts_with("diff --git ") {
            // Flush previous file's +N -N summary
            if in_file {
                out.push(format!("  +{} -{}", file_adds, file_dels));
            }
            // Extract "b/path" filename
            let fname = line
                .split_whitespace()
                .last()
                .and_then(|s| s.strip_prefix("b/"))
                .unwrap_or(line);
            out.push(fname.to_string());
            total_lines += 1;
            in_file = true;
            file_adds = 0;
            file_dels = 0;
            hunk_lines = 0;
            hunk_truncated = false;
            continue;
        }

        // Drop noisy headers
        if line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("index ")
            || line.starts_with("\\ No newline")
        {
            continue;
        }

        // Hunk header
        if line.starts_with("@@") {
            hunk_lines = 0;
            hunk_truncated = false;
            out.push(hunk_context(line));
            total_lines += 1;
            continue;
        }

        // Change and context lines
        let is_add = line.starts_with('+');
        let is_del = line.starts_with('-');
        let is_ctx = line.starts_with(' ');

        if is_add || is_del || is_ctx {
            if is_add { file_adds += 1; }
            if is_del { file_dels += 1; }

            if hunk_truncated {
                continue;
            }
            if hunk_lines >= HUNK_LINE_CAP {
                hunk_truncated = true;
                out.push("  [...truncated...]".to_string());
                total_lines += 1;
                continue;
            }
            out.push(line.to_string());
            hunk_lines += 1;
            total_lines += 1;

            if total_lines >= DIFF_TOTAL_CAP {
                global_truncated = true;
            }
        }
    }

    // Flush last file
    if in_file {
        out.push(format!("  +{} -{}", file_adds, file_dels));
    }
    if global_truncated {
        out.push("[... diff truncated — run `git diff` for full output]".to_string());
    }

    if out.is_empty() {
        output.to_string()
    } else {
        out.join("\n")
    }
}

/// Extract the human-readable function/class context from a `@@ ... @@ context` line.
fn hunk_context(header: &str) -> String {
    // "@@ -L,N +L,N @@ fn foo() {" → "@@ fn foo() {"
    let parts: Vec<&str> = header.splitn(4, "@@").collect();
    if parts.len() >= 3 {
        let ctx = parts[2].trim();
        if !ctx.is_empty() {
            return format!("@@ {}", ctx);
        }
    }
    "@@".to_string()
}

// ─── push / pull / fetch ─────────────────────────────────────────────────────

fn filter_push_pull(output: &str) -> String {
    let has_error = output.lines().any(|l| {
        let t = l.trim().to_lowercase();
        PUSH_PULL_ERROR_TERMS.iter().any(|e| t.contains(e))
    });

    // Already up to date (only if no errors)
    if !has_error && (output.contains("Everything up-to-date") || output.contains("Already up to date")) {
        return "ok (up to date)".to_string();
    }

    if has_error {
        let errors: Vec<&str> = output
            .lines()
            .filter(|l| {
                let t = l.trim().to_lowercase();
                PUSH_PULL_ERROR_TERMS.iter().any(|e| t.contains(e))
            })
            .collect();
        return errors.join("\n");
    }

    // Success — find the branch ref line: "main -> origin/main" or "branch 'main' set up to track..."
    for line in output.lines() {
        let t = line.trim();
        if t.contains(" -> ") && !t.starts_with("remote:") {
            return format!("ok {}", t);
        }
    }

    // Pull / fetch with file stats
    for line in output.lines() {
        let t = line.trim();
        if t.contains("file") && (t.contains("changed") || t.contains("insertion") || t.contains("deletion")) {
            return format!("ok ({})", t);
        }
    }

    // Fallback: last meaningful line
    output
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| format!("ok {}", l.trim()))
        .unwrap_or_else(|| "ok".to_string())
}

// ─── commit / add ────────────────────────────────────────────────────────────

fn filter_commit(output: &str) -> String {
    let mut bracket_line: Option<String> = None;
    let mut stats_line: Option<String> = None;

    for line in output.lines() {
        let t = line.trim();
        if t.starts_with('[') && bracket_line.is_none() {
            bracket_line = Some(t.to_string());
        }
        if t.contains("file") && (t.contains("changed") || t.contains("insertion") || t.contains("deletion")) {
            stats_line = Some(t.to_string());
        }
    }

    match (bracket_line, stats_line) {
        (Some(b), Some(s)) => format!("ok — {}\n{}", b, s),
        (Some(b), None) => format!("ok — {}", b),
        _ => output.to_string(),
    }
}

// ─── branch / stash ──────────────────────────────────────────────────────────

fn filter_list(output: &str) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() > 30 {
        let extra = lines.len() - 30;
        let mut out: Vec<String> = lines[..30].iter().map(|l| l.to_string()).collect();
        out.push(format!("[+{} more]", extra));
        out.join("\n")
    } else {
        lines.join("\n")
    }
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rewrite_injects_porcelain_and_branch() {
        let handler = GitHandler;
        let args: Vec<String> = vec!["git".into(), "status".into()];
        let rewritten = handler.rewrite_args(&args);
        assert!(rewritten.contains(&"--porcelain".to_string()), "should inject --porcelain");
        assert!(rewritten.contains(&"-b".to_string()), "should inject -b");
    }

    #[test]
    fn test_rewrite_no_double_porcelain() {
        let handler = GitHandler;
        let args: Vec<String> = vec!["git".into(), "status".into(), "--porcelain".into()];
        let rewritten = handler.rewrite_args(&args);
        assert_eq!(rewritten.iter().filter(|a| *a == "--porcelain").count(), 1);
    }

    #[test]
    fn test_status_clean() {
        let output = "## main...origin/main\nOn branch main\nnothing to commit, working tree clean\n";
        assert_eq!(filter_status(output), "nothing to commit, working tree clean");
    }

    #[test]
    fn test_status_empty() {
        assert_eq!(filter_status(""), "nothing to commit, working tree clean");
    }

    #[test]
    fn test_status_with_branch_line() {
        let output = "## main...origin/main\nM  src/main.rs\n?? foo.txt\n";
        let result = filter_status(output);
        assert!(result.contains("main"), "branch name should appear");
        assert!(result.contains("Staged:"), "got: {}", result);
        assert!(result.contains("Untracked:"), "got: {}", result);
    }

    #[test]
    fn test_status_branch_ahead() {
        let output = "## feature/foo...origin/feature/foo [ahead 3]\nM  src/lib.rs\n";
        let result = filter_status(output);
        assert!(result.contains("ahead 3"), "got: {}", result);
    }

    #[test]
    fn test_status_staged_and_untracked() {
        let output = "## main...origin/main\nM  src/main.rs\nA  src/new.rs\n?? untracked.txt\n?? other.txt\n";
        let result = filter_status(output);
        assert!(result.contains("Staged: 2"), "expected Staged: 2, got: {}", result);
        assert!(result.contains("Untracked: 2"), "expected Untracked: 2, got: {}", result);
        assert!(result.contains("src/main.rs"));
        assert!(result.contains("untracked.txt"));
    }

    #[test]
    fn test_status_modified_unstaged() {
        let output = "## main\n M src/lib.rs\n?? foo.txt\n";
        let result = filter_status(output);
        assert!(result.contains("Modified: 1"), "got: {}", result);
        assert!(result.contains("Untracked: 1"), "got: {}", result);
    }

    #[test]
    fn test_status_caps_overflow() {
        // 20 modified files — should cap at 15 and show +5 more
        let mut output = "## main\n".to_string();
        for i in 0..20 {
            output.push_str(&format!(" M src/file{}.rs\n", i));
        }
        let result = filter_status(&output);
        assert!(result.contains("[+5 more]"), "got: {}", result);
    }

    #[test]
    fn test_diff_hunk_cap() {
        let mut input = "diff --git a/foo.rs b/foo.rs\n--- a/foo.rs\n+++ b/foo.rs\n@@ -1,40 +1,40 @@ fn main() {\n".to_string();
        for i in 0..35 {
            input.push_str(&format!("+    line {};\n", i));
        }
        let result = filter_diff(&input);
        assert!(result.contains("[...truncated...]"), "should truncate at 30 lines, got: {}", result);
        assert!(result.contains("+35 -0"), "should show tally, got: {}", result);
    }

    #[test]
    fn test_diff_strips_headers() {
        let output = "diff --git a/foo.rs b/foo.rs\nindex abc..def 100644\n--- a/foo.rs\n+++ b/foo.rs\n@@ -1,3 +1,3 @@ fn main() {\n-    old();\n+    new();\n";
        let result = filter_diff(output);
        assert!(!result.contains("index abc"), "index line should be stripped");
        assert!(!result.contains("--- a/"), "--- line should be stripped");
        assert!(!result.contains("+++ b/"), "+++ line should be stripped");
        assert!(result.contains("-    old();"));
        assert!(result.contains("+    new();"));
    }

    #[test]
    fn test_diff_hunk_context_extracted() {
        let output = "diff --git a/foo.rs b/foo.rs\n--- a/foo.rs\n+++ b/foo.rs\n@@ -10,5 +10,5 @@ fn main() {\n-    old();\n+    new();\n";
        let result = filter_diff(output);
        assert!(result.contains("@@ fn main()"), "hunk context should be kept, got: {}", result);
    }

    #[test]
    fn test_diff_per_file_tally() {
        let output = "diff --git a/foo.rs b/foo.rs\n--- a/foo.rs\n+++ b/foo.rs\n@@ -1,3 +1,4 @@\n-old\n+new\n+extra\n context\n";
        let result = filter_diff(output);
        assert!(result.contains("+2 -1"), "tally should appear, got: {}", result);
    }

    #[test]
    fn test_push_up_to_date() {
        let output = "Everything up-to-date\n";
        assert_eq!(filter_push_pull(output), "ok (up to date)");
    }

    #[test]
    fn test_push_success_one_liner() {
        let output = "remote: Counting objects: 3\nremote: Compressing objects: 100%\n   abc1234..def5678  main -> origin/main\n";
        let result = filter_push_pull(output);
        assert_eq!(result, "ok abc1234..def5678  main -> origin/main");
    }

    #[test]
    fn test_push_error_kept() {
        let output = "Everything up-to-date\nerror: failed to push some refs\n";
        let result = filter_push_pull(output);
        assert_ne!(result, "ok (up to date)");
        assert!(result.contains("error:"));
    }

    #[test]
    fn test_log_strips_trailers() {
        let output = "abc1234 fix: real commit\ndef5678 Signed-off-by: Bot <bot@ci.com>\n5678abc Co-authored-by: Alice <a@b.com>\n";
        let result = filter_log(output);
        assert!(result.contains("fix: real commit"), "real commit should remain");
        assert!(!result.contains("Signed-off-by"), "trailer commits should be stripped");
        assert!(!result.contains("Co-authored-by"), "trailer commits should be stripped");
    }

    #[test]
    fn test_commit_format() {
        let output = "[main abc1234] Add feature\n 2 files changed, 10 insertions(+), 3 deletions(-)\n";
        let result = filter_commit(output);
        assert!(result.starts_with("ok — [main abc1234]"), "got: {}", result);
        assert!(result.contains("2 files changed"), "got: {}", result);
    }
}
