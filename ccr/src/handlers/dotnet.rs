use super::Handler;

pub struct DotnetHandler;

fn dotnet_subcmd(args: &[String]) -> &str {
    args.get(1).map(|s| s.as_str()).unwrap_or("")
}

impl Handler for DotnetHandler {
    fn filter(&self, output: &str, args: &[String]) -> String {
        match dotnet_subcmd(args) {
            "build" => filter_dotnet_build(output),
            "test" => filter_dotnet_test(output),
            "restore" => filter_dotnet_restore(output),
            _ => output.to_string(),
        }
    }
}

fn filter_dotnet_build(output: &str) -> String {
    // Short-circuit for a fully clean build
    if output.contains("Build succeeded") && output.contains("0 Warning(s)") && output.contains("0 Error(s)") {
        return "Build succeeded.".to_string();
    }

    let mut error_lines: Vec<String> = Vec::new();
    let mut warning_map: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut summary_lines: Vec<String> = Vec::new();

    for line in output.lines() {
        let t = line.trim();

        if t.contains(": error CS") || t.contains(": error FS") {
            error_lines.push(line.to_string());
        } else if t.contains(": warning CS") || t.contains(": warning FS") {
            // Group by warning code
            let marker = if t.contains(": warning CS") { ": warning CS" } else { ": warning FS" };
            if let Some(pos) = t.find(marker) {
                let after = &t[pos + marker.len() - 2..]; // include "CS"/"FS"
                let code: String = after.chars().take_while(|c| c.is_alphanumeric()).collect();
                *warning_map.entry(code).or_insert(0) += 1;
            }
        }

        if t.starts_with("Build succeeded") || t.starts_with("Build FAILED")
            || t.ends_with("Error(s)") || t.ends_with("Warning(s)") || t.ends_with("Message(s)")
        {
            summary_lines.push(line.to_string());
        }
    }

    let mut out: Vec<String> = Vec::new();
    out.extend(error_lines);

    // Emit grouped warning codes
    let mut codes: Vec<(String, usize)> = warning_map.into_iter().collect();
    codes.sort_by(|a, b| b.1.cmp(&a.1));
    for (code, count) in &codes {
        if *count > 1 {
            out.push(format!("[{} ×{}]", code, count));
        } else {
            out.push(format!("[{}]", code));
        }
    }

    out.extend(summary_lines);

    if out.is_empty() { output.to_string() } else { out.join("\n") }
}

fn filter_dotnet_test(output: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_stack_trace = false;
    let mut stack_lines = 0usize;

    for line in output.lines() {
        let t = line.trim();

        // Failed test line
        if t.starts_with("Failed  ") || (t.starts_with("  Failed") && t.len() > 8) {
            out.push(line.to_string());
            in_stack_trace = false;
            stack_lines = 0;
            continue;
        }

        if t.starts_with("Error Message:") || t.starts_with("  Error Message:") {
            out.push(line.to_string());
            in_stack_trace = false;
            continue;
        }

        if t.starts_with("Stack Trace:") || t.starts_with("  Stack Trace:") {
            out.push(line.to_string());
            in_stack_trace = true;
            stack_lines = 0;
            continue;
        }

        if in_stack_trace && stack_lines < 5 {
            out.push(line.to_string());
            stack_lines += 1;
            if stack_lines >= 5 {
                in_stack_trace = false;
            }
            continue;
        }
        in_stack_trace = false;

        // Final summary line
        if t.contains("Failed:") && (t.contains("Passed:") || t.contains("Skipped:")) {
            out.push(line.to_string());
        }
    }

    if out.is_empty() { output.to_string() } else { out.join("\n") }
}

fn filter_dotnet_restore(output: &str) -> String {
    let has_error = output.lines().any(|l| {
        let t = l.trim();
        t.contains(": error ") || t.starts_with("Error") || t.contains("MSBUILD : error")
    });

    if has_error {
        let errors: Vec<String> = output
            .lines()
            .filter(|l| {
                let t = l.trim();
                t.contains(": error ") || t.starts_with("Error")
            })
            .map(|l| l.to_string())
            .collect();
        return if errors.is_empty() { output.to_string() } else { errors.join("\n") };
    }

    let pkg_count = output.lines().filter(|l| l.contains("Restored")).count();
    if pkg_count > 0 {
        format!("[restore complete — {} packages]", pkg_count)
    } else {
        "[restore complete]".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler() -> DotnetHandler { DotnetHandler }
    fn args(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn test_dotnet_build_success_compressed() {
        let input = "Build succeeded.\n    0 Warning(s)\n    0 Error(s)\n\nTime Elapsed 00:00:03.12\n";
        let result = handler().filter(input, &args(&["dotnet", "build"]));
        assert_eq!(result, "Build succeeded.");
    }

    #[test]
    fn test_dotnet_build_errors_kept_refs_dropped() {
        let input = "  MyProject -> /bin/Debug/MyProject.dll\nSrc/Foo.cs(42,10): error CS0246: type or namespace not found [MyProject.csproj]\nBuild FAILED.\n    1 Error(s)\n";
        let result = handler().filter(input, &args(&["dotnet", "build"]));
        assert!(result.contains("error CS0246"), "got: {}", result);
        assert!(!result.contains("MyProject ->"), "ref lines should be dropped, got: {}", result);
    }

    #[test]
    fn test_dotnet_build_warnings_grouped_by_code() {
        let mut input = String::new();
        for i in 0..3 {
            input.push_str(&format!("Src/File{}.cs(1,1): warning CS0168: variable declared but never used\n", i));
        }
        input.push_str("Src/Other.cs(5,1): warning CS0219: value assigned but never used\n");
        input.push_str("Build succeeded.\n    4 Warning(s)\n    0 Error(s)\n");
        let result = handler().filter(&input, &args(&["dotnet", "build"]));
        assert!(result.contains("CS0168 ×3") || result.contains("CS0168"), "got: {}", result);
    }

    #[test]
    fn test_dotnet_build_zero_warnings_short_circuits() {
        let input = "MyProject -> /bin/Debug/net8.0/MyProject.dll\nBuild succeeded.\n    0 Warning(s)\n    0 Error(s)\n";
        let result = handler().filter(input, &args(&["dotnet", "build"]));
        assert_eq!(result, "Build succeeded.");
    }

    #[test]
    fn test_dotnet_test_failures_kept_passing_dropped() {
        let input = "Passed  MyTests.PassingTest1\nPassed  MyTests.PassingTest2\nFailed  MyTests.FailingTest\n  Error Message:\n   Assert.Equal() Failure: expected 1, actual 2\n  Stack Trace:\n    at MyTests.FailingTest()\n";
        let result = handler().filter(input, &args(&["dotnet", "test"]));
        assert!(result.contains("Failed"), "got: {}", result);
        assert!(!result.contains("Passed  MyTests"), "passing tests should be dropped, got: {}", result);
    }

    #[test]
    fn test_dotnet_test_summary_kept() {
        let input = "Passed  TestA\nFailed  TestB\n\nFailed: 1, Passed: 1, Skipped: 0\n";
        let result = handler().filter(input, &args(&["dotnet", "test"]));
        assert!(result.contains("Failed: 1"), "got: {}", result);
    }

    #[test]
    fn test_dotnet_restore_success_compressed() {
        let input = "  Determining projects to restore...\n  Restored /project/MyProject.csproj (0.5s).\n  Restored /project/Tests.csproj (0.3s).\n";
        let result = handler().filter(input, &args(&["dotnet", "restore"]));
        assert!(result.contains("restore complete"), "got: {}", result);
        assert!(result.contains("2"), "should count 2 packages, got: {}", result);
    }

    #[test]
    fn test_dotnet_restore_error_kept() {
        let input = "  Determining projects to restore...\n/project/MyProject.csproj : error NU1101: unable to find package Foo [MyProject.csproj]\n";
        let result = handler().filter(input, &args(&["dotnet", "restore"]));
        assert!(result.contains("error"), "got: {}", result);
    }

    #[test]
    fn test_dotnet_rewrite_args_noop() {
        let h = DotnetHandler;
        let a = args(&["dotnet", "build"]);
        assert_eq!(h.rewrite_args(&a), a);
    }

    #[test]
    fn test_dotnet_unknown_subcmd_passthrough() {
        let input = "some output\n";
        let result = handler().filter(input, &args(&["dotnet", "publish"]));
        assert_eq!(result, input);
    }
}
