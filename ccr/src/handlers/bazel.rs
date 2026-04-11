use super::Handler;

pub struct BazelHandler;

fn bazel_subcmd(args: &[String]) -> &str {
    args.get(1).map(|s| s.as_str()).unwrap_or("")
}

impl Handler for BazelHandler {
    fn filter(&self, output: &str, args: &[String]) -> String {
        match bazel_subcmd(args) {
            "build" => filter_bazel_build(output),
            "test" => filter_bazel_test(output),
            "query" => filter_bazel_query(output),
            _ => output.to_string(),
        }
    }
}

fn filter_bazel_build(output: &str) -> String {
    let mut completion_line: Option<String> = None;
    let mut elapsed_line: Option<String> = None;
    let mut error_lines: Vec<String> = Vec::new();
    let mut target_lines: Vec<String> = Vec::new();
    let mut in_target_output = false;

    for line in output.lines() {
        let t = line.trim();

        if t.starts_with("INFO:") {
            if t.contains("Build completed successfully") || t.contains("Build completed,") {
                completion_line = Some(t.to_string());
            } else if t.contains("Elapsed time:") {
                elapsed_line = Some(t.to_string());
            }
            // Drop all other INFO: lines
            in_target_output = false;
            continue;
        }

        if t.starts_with("ERROR:") || t.starts_with("FAILED:") || t.contains(": error:") {
            error_lines.push(line.to_string());
            in_target_output = false;
            continue;
        }

        if t.starts_with("Target //") && t.contains("up-to-date:") {
            target_lines.push(line.to_string());
            in_target_output = true;
            continue;
        }

        if in_target_output && (line.starts_with("  ") || line.starts_with('\t')) {
            target_lines.push(line.to_string());
            continue;
        }
        in_target_output = false;
    }

    // Short-circuit: clean build with only INFO lines
    if error_lines.is_empty() && target_lines.is_empty() {
        if let (Some(ref completion), Some(ref elapsed)) = (&completion_line, &elapsed_line) {
            let actions = extract_bazel_actions(completion);
            let time = extract_bazel_time(elapsed);
            return format!("[{} actions, build OK ({})]", actions, time);
        }
    }

    let mut out: Vec<String> = Vec::new();
    out.extend(error_lines);
    out.extend(target_lines);
    if let Some(c) = completion_line {
        out.push(c);
    }
    if let Some(e) = elapsed_line {
        out.push(e);
    }

    if out.is_empty() { output.to_string() } else { out.join("\n") }
}

fn extract_bazel_actions(line: &str) -> String {
    // "INFO: Build completed successfully, 42 total actions"
    if let Some(pos) = line.find(", ") {
        let after = &line[pos + 2..];
        let num: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num.is_empty() {
            return num;
        }
    }
    "?".to_string()
}

fn extract_bazel_time(line: &str) -> String {
    // "INFO: Elapsed time: 3.456s, Critical Path: 1.234s"
    if let Some(pos) = line.find("Elapsed time: ") {
        let after = &line[pos + "Elapsed time: ".len()..];
        let time: String = after.chars().take_while(|c| *c != ',').collect();
        return time.trim().to_string();
    }
    "?s".to_string()
}

fn filter_bazel_test(output: &str) -> String {
    let mut failed: Vec<String> = Vec::new();
    let mut passed_count = 0usize;
    let mut failed_count = 0usize;
    let mut log_lines: Vec<String> = Vec::new();

    for line in output.lines() {
        let t = line.trim();
        if t.starts_with("FAILED: //") || (t.starts_with("//") && t.contains("FAILED")) {
            failed.push(line.to_string());
            failed_count += 1;
        } else if t.starts_with("PASSED: //") || (t.starts_with("//") && t.contains("PASSED")) {
            passed_count += 1;
        } else if (t.contains("test.log") || t.contains(".log")) && t.contains("see") {
            log_lines.push(line.to_string());
        }
    }

    let mut out = failed;
    out.extend(log_lines);
    out.push(format!("[{} passed, {} failed]", passed_count, failed_count));
    out.join("\n")
}

fn filter_bazel_query(output: &str) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    const MAX: usize = 30;
    if lines.len() > MAX {
        let extra = lines.len() - MAX;
        let mut out: Vec<String> = lines[..MAX].iter().map(|l| l.to_string()).collect();
        out.push(format!("[+{} more targets]", extra));
        out.join("\n")
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler() -> BazelHandler { BazelHandler }
    fn args(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn test_bazel_build_success_drops_info() {
        let input = "INFO: Analyzed target //app:main (5 packages loaded).\nINFO: Found 1 target...\nINFO: Build completed successfully, 42 total actions\nINFO: Elapsed time: 3.456s, Critical Path: 1.234s\n";
        let result = handler().filter(input, &args(&["bazel", "build", "//..."]));
        assert!(result.contains("42"), "should show action count, got: {}", result);
        assert!(result.contains("3.456s"), "should show elapsed time, got: {}", result);
        assert!(!result.contains("Analyzed"), "should drop other INFO lines, got: {}", result);
    }

    #[test]
    fn test_bazel_build_error_kept() {
        let input = "INFO: Analyzing target //app:main\nERROR: /project/app/BUILD:5:10: C++ compilation of rule '//app:main' failed\nINFO: Elapsed time: 1.2s\n";
        let result = handler().filter(input, &args(&["bazel", "build", "//app:main"]));
        assert!(result.contains("ERROR:"), "got: {}", result);
    }

    #[test]
    fn test_bazel_build_target_output_kept() {
        let input = "INFO: Build completed successfully, 10 total actions\nINFO: Elapsed time: 2.0s, Critical Path: 0.5s\nTarget //app:main up-to-date:\n  bazel-bin/app/main\n";
        let result = handler().filter(input, &args(&["bazel", "build", "//app:main"]));
        assert!(result.contains("Target //app:main"), "got: {}", result);
        assert!(result.contains("bazel-bin"), "got: {}", result);
    }

    #[test]
    fn test_bazel_test_failures_kept_passing_collapsed() {
        let input = "//app:test_foo     PASSED in 0.5s\n//app:test_bar     FAILED in 0.3s\n//app:test_baz     PASSED in 0.2s\n";
        let result = handler().filter(input, &args(&["bazel", "test", "//..."]));
        assert!(result.contains("FAILED"), "got: {}", result);
        assert!(result.contains("[2 passed"), "got: {}", result);
        assert!(result.contains("1 failed"), "got: {}", result);
    }

    #[test]
    fn test_bazel_test_summary_correct() {
        let input = "//tests:test_a     PASSED in 1.0s\n//tests:test_b     PASSED in 0.5s\n//tests:test_c     FAILED in 0.3s\n";
        let result = handler().filter(input, &args(&["bazel", "test", "//tests/..."]));
        assert!(result.contains("[2 passed, 1 failed]"), "got: {}", result);
    }

    #[test]
    fn test_bazel_query_short_passthrough() {
        let mut input = String::new();
        for i in 0..10 {
            input.push_str(&format!("//app:target_{}\n", i));
        }
        let result = handler().filter(&input, &args(&["bazel", "query", "//..."]));
        assert_eq!(result.lines().count(), 10);
    }

    #[test]
    fn test_bazel_query_long_capped() {
        let mut input = String::new();
        for i in 0..40 {
            input.push_str(&format!("//app:target_{}\n", i));
        }
        let result = handler().filter(&input, &args(&["bazel", "query", "//..."]));
        assert!(result.contains("[+10 more targets]"), "got: {}", result);
        assert_eq!(result.lines().count(), 31);
    }

    #[test]
    fn test_bazel_run_passthrough() {
        let input = "Running program output\nsome result\n";
        let result = handler().filter(input, &args(&["bazel", "run", "//app:main"]));
        assert_eq!(result, input);
    }

    #[test]
    fn test_bazel_unknown_subcmd_passthrough() {
        let input = "bazel version output\n";
        let result = handler().filter(input, &args(&["bazel", "version"]));
        assert_eq!(result, input);
    }
}
