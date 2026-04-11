use super::Handler;

pub struct SwiftHandler;

fn swift_subcmd(args: &[String]) -> &str {
    args.get(1).map(|s| s.as_str()).unwrap_or("")
}

impl Handler for SwiftHandler {
    fn filter(&self, output: &str, args: &[String]) -> String {
        match swift_subcmd(args) {
            "build" => filter_swift_build(output),
            "test" => filter_swift_test(output),
            "run" => filter_swift_run(output),
            "package" => filter_swift_package(output, args),
            _ => output.to_string(),
        }
    }
}

fn filter_swift_build(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();

    // Short output — passthrough
    if lines.len() <= 3 {
        return output.to_string();
    }

    // Collect error and warning lines (file:line format)
    let important: Vec<String> = lines
        .iter()
        .filter(|l| {
            let t = l.trim();
            (t.contains(": error:") || t.contains(": warning:")) && t.contains(".swift:")
        })
        .map(|l| l.to_string())
        .collect();

    if !important.is_empty() {
        const MAX: usize = 20;
        let total = important.len();
        let mut out = important[..total.min(MAX)].to_vec();
        if total > MAX {
            out.push(format!("[+{} more errors]", total - MAX));
        }
        return out.join("\n");
    }

    // No errors — emit "Build complete" if present
    for line in &lines {
        let t = line.trim();
        if t.starts_with("Build complete") {
            return t.to_string();
        }
    }

    output.to_string()
}

fn filter_swift_test(output: &str) -> String {
    let mut out: Vec<String> = Vec::new();

    for line in output.lines() {
        let t = line.trim();
        if (t.contains("Test Suite") && t.contains("failed"))
            || (t.contains("Test Case") && t.contains("failed"))
            || t.contains("Assertion failed")
            || t.contains("XCTAssert")
        {
            out.push(line.to_string());
        } else if t.starts_with("Executed") && t.contains("test") {
            out.push(line.to_string());
        }
    }

    if out.is_empty() {
        // All passed — find summary line
        for line in output.lines() {
            let t = line.trim();
            if t.starts_with("Executed") && t.contains("test") {
                return t.to_string();
            }
        }
        return output.to_string();
    }

    out.join("\n")
}

fn filter_swift_run(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 30 {
        return output.to_string();
    }

    // Preserve panic output
    let panic_idx = lines
        .iter()
        .position(|l| l.contains("Fatal error:") || l.contains("fatal error:"));
    if let Some(idx) = panic_idx {
        return lines[idx..].join("\n");
    }

    output.to_string()
}

fn filter_swift_package(output: &str, args: &[String]) -> String {
    let pkg_subcmd = args.get(2).map(|s| s.as_str()).unwrap_or("");
    match pkg_subcmd {
        "resolve" | "update" => {
            let out: Vec<&str> = output
                .lines()
                .filter(|l| {
                    let t = l.trim();
                    if t.is_empty() { return false; }
                    if t.starts_with("Fetching") || t.starts_with("Updating")
                        || t.contains("error") || t.contains("Error")
                    {
                        return true;
                    }
                    !t.starts_with("Computing") && !t.starts_with("Resolving")
                })
                .collect();
            if out.is_empty() { output.to_string() } else { out.join("\n") }
        }
        "init" => {
            output.lines().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n")
        }
        _ => output.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler() -> SwiftHandler { SwiftHandler }
    fn args(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn test_swift_build_ok_compressed() {
        let mut input = String::new();
        for i in 0..30 {
            input.push_str(&format!("Compiling MyModule source{}.swift\n", i));
        }
        input.push_str("Build complete! (3.2s)\n");
        let result = handler().filter(&input, &args(&["swift", "build"]));
        assert_eq!(result, "Build complete! (3.2s)");
    }

    #[test]
    fn test_swift_build_error_kept_compile_dropped() {
        let mut input = String::new();
        for i in 0..10 {
            input.push_str(&format!("Compiling MyModule source{}.swift\n", i));
        }
        input.push_str("Sources/App/main.swift:42:5: error: use of unresolved identifier 'foo'\n");
        let result = handler().filter(&input, &args(&["swift", "build"]));
        assert!(result.contains("error:"), "got: {}", result);
        assert!(!result.contains("Compiling"), "got: {}", result);
    }

    #[test]
    fn test_swift_build_warning_kept() {
        let mut input = String::new();
        for i in 0..10 {
            input.push_str(&format!("Compiling Module src{}.swift\n", i));
        }
        input.push_str("Sources/App/lib.swift:10:3: warning: result of call is unused\n");
        let result = handler().filter(&input, &args(&["swift", "build"]));
        assert!(result.contains("warning:"), "got: {}", result);
    }

    #[test]
    fn test_swift_test_pass_summary_only() {
        let input = "Test Suite 'All tests' passed at 2024-01-01 12:00:00.\nExecuted 42 tests, with 0 failures (0 unexpected) in 1.234 (1.567) seconds\n";
        let result = handler().filter(input, &args(&["swift", "test"]));
        assert!(result.contains("Executed 42"), "got: {}", result);
        assert!(!result.contains("passed"), "should not keep suite-passed lines, got: {}", result);
    }

    #[test]
    fn test_swift_test_failure_kept_passing_dropped() {
        let input = "Test Suite 'MyTests' passed.\nTest Case '-[MyTests testFoo]' failed (0.001 seconds).\nAssertion failed: expected true, got false\nExecuted 5 tests, with 1 failure\n";
        let result = handler().filter(input, &args(&["swift", "test"]));
        assert!(result.contains("failed"), "got: {}", result);
        assert!(result.contains("Executed"), "got: {}", result);
    }

    #[test]
    fn test_swift_package_resolve_strips_progress() {
        let input = "Computing version for https://github.com/apple/swift-nio.git\nFetching https://github.com/apple/swift-nio.git\nResolving dependencies\n";
        let result = handler().filter(input, &args(&["swift", "package", "resolve"]));
        assert!(result.contains("Fetching"), "got: {}", result);
        assert!(!result.contains("Computing"), "got: {}", result);
    }

    #[test]
    fn test_swift_rewrite_args_noop() {
        let handler = SwiftHandler;
        let a = args(&["swift", "build"]);
        assert_eq!(handler.rewrite_args(&a), a);
    }

    #[test]
    fn test_swift_build_short_passthrough() {
        let input = "Build complete!\n";
        let result = handler().filter(input, &args(&["swift", "build"]));
        assert_eq!(result, input);
    }

    #[test]
    fn test_swift_unknown_subcmd_passthrough() {
        let input = "some output\n";
        let result = handler().filter(input, &args(&["swift", "format"]));
        assert_eq!(result, input);
    }
}
