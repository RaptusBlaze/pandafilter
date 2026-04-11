use super::Handler;

pub struct CmakeHandler;

impl Handler for CmakeHandler {
    fn filter(&self, output: &str, args: &[String]) -> String {
        let has_build_flag = args.iter().any(|a| a == "--build");
        let has_config_flag = args.iter().any(|a| a == "-B" || a == "-S" || a == "-G");

        let looks_like_build = output.lines().any(|l| {
            let t = l.trim();
            t.starts_with('[') && t.contains('%') && t.contains(']')
        });
        let looks_like_configure = output.lines().any(|l| l.trim().starts_with("-- "));

        if has_build_flag || (looks_like_build && !looks_like_configure) {
            filter_cmake_build(output)
        } else if has_config_flag || looks_like_configure {
            filter_cmake_configure(output)
        } else {
            output.to_string()
        }
    }
}

fn filter_cmake_configure(output: &str) -> String {
    let mut out: Vec<String> = Vec::new();

    for line in output.lines() {
        let t = line.trim();
        if t.is_empty() { continue; }
        if t.starts_with("CMake Error") || t.starts_with("CMake Warning") {
            out.push(line.to_string());
        } else if t.starts_with("-- Build files have been written")
            || t.starts_with("-- Configuring done")
            || t.starts_with("-- Generating done")
            || t.starts_with("-- Build files written")
        {
            out.push(line.to_string());
        }
        // Drop all other "-- " detection/checking lines on success
    }

    if out.is_empty() { output.to_string() } else { out.join("\n") }
}

fn filter_cmake_build(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();

    // Count build progress entries
    let target_count = lines
        .iter()
        .filter(|l| {
            let t = l.trim();
            t.starts_with('[') && t.contains('%') && t.contains(']') && t.contains("Building")
        })
        .count();

    let mut out: Vec<String> = Vec::new();

    if target_count > 0 {
        out.push(format!("[{} targets built]", target_count));
    }

    for line in &lines {
        let t = line.trim();
        // Keep compiler errors/warnings with source location
        if t.contains(": error:") || t.contains(": error ") {
            out.push(line.to_string());
        } else if t.contains(": warning:")
            && (t.contains(".cpp:") || t.contains(".c:") || t.contains(".h:")
                || t.contains(".cc:") || t.contains(".cxx:"))
        {
            out.push(line.to_string());
        }
        // Keep make errors
        else if t.starts_with("make[") && t.contains("Error") {
            out.push(line.to_string());
        }
        // CMake errors
        else if t.starts_with("CMake Error") {
            out.push(line.to_string());
        }
    }

    if out.is_empty() { output.to_string() } else { out.join("\n") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler() -> CmakeHandler { CmakeHandler }
    fn args(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn test_cmake_configure_success_compressed() {
        let input = "-- The C compiler identification is GNU 11.4.0\n-- Detecting C compiler ABI info\n-- Detecting C compiler ABI info - done\n-- Check for working C compiler: /usr/bin/gcc - skipped\n-- Configuring done (0.5s)\n-- Generating done (0.1s)\n-- Build files have been written to: /project/build\n";
        let result = handler().filter(input, &args(&["cmake", "-B", "build"]));
        assert!(result.contains("Build files have been written"), "got: {}", result);
        assert!(!result.contains("Detecting"), "noise lines should be dropped, got: {}", result);
    }

    #[test]
    fn test_cmake_configure_error_kept() {
        let input = "-- Checking for module 'libssl'\nCMake Error at CMakeLists.txt:10: Could not find OpenSSL\n-- Configuring incomplete, errors occurred!\n";
        let result = handler().filter(input, &args(&["cmake", "-B", "build"]));
        assert!(result.contains("CMake Error"), "got: {}", result);
    }

    #[test]
    fn test_cmake_build_success_compressed() {
        let mut input = String::new();
        for i in 1..=12 {
            input.push_str(&format!("[ {}%] Building CXX object src/CMakeFiles/mylib.dir/foo.cpp.o\n", i * 8));
        }
        input.push_str("[100%] Linking CXX executable myapp\n");
        input.push_str("[100%] Built target myapp\n");
        let result = handler().filter(&input, &args(&["cmake", "--build", "build/"]));
        assert!(result.contains("targets built") || result.contains("Built target"), "got: {}", result);
        assert!(!result.contains("Building CXX"), "progress lines should be dropped, got: {}", result);
    }

    #[test]
    fn test_cmake_build_error_kept_progress_dropped() {
        let mut input = String::new();
        for i in 1..=4 {
            input.push_str(&format!("[ {}%] Building CXX object src.dir/file{}.cpp.o\n", i * 25, i));
        }
        input.push_str("src/main.cpp:15:3: error: 'foo' was not declared in this scope\n");
        let result = handler().filter(&input, &args(&["cmake", "--build", "build/"]));
        assert!(result.contains("error:"), "got: {}", result);
    }

    #[test]
    fn test_cmake_build_warning_kept() {
        let input = "[ 50%] Building CXX object CMakeFiles/app.dir/src/main.cpp.o\nsrc/main.cpp:10:5: warning: unused variable 'x'\n";
        let result = handler().filter(input, &args(&["cmake", "--build", "build/"]));
        assert!(result.contains("warning:"), "got: {}", result);
    }

    #[test]
    fn test_cmake_configure_detection_from_args() {
        let input = "-- Checking dependencies\n-- Configuring done\n";
        let result = handler().filter(input, &args(&["cmake", "-S", ".", "-B", "build"]));
        assert!(result.contains("Configuring done"), "got: {}", result);
    }

    #[test]
    fn test_cmake_build_detection_from_args() {
        let input = "[ 50%] Building CXX object main.cpp.o\n[ 100%] Building CXX object lib.cpp.o\n";
        let result = handler().filter(input, &args(&["cmake", "--build", "."]));
        assert!(result.contains("targets built"), "got: {}", result);
    }

    #[test]
    fn test_cmake_unknown_passthrough() {
        let input = "some cmake output\n";
        let result = handler().filter(input, &args(&["cmake", "--version"]));
        assert_eq!(result, input);
    }
}
