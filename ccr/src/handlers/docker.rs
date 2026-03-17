use super::util;
use super::Handler;

pub struct DockerHandler;

impl Handler for DockerHandler {
    fn rewrite_args(&self, args: &[String]) -> Vec<String> {
        // For `docker logs`, add --tail 200 if not already specified
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        if subcmd == "logs" && !args.iter().any(|a| a == "--tail") {
            let mut out = args.to_vec();
            // Insert --tail 200 after "logs"
            out.insert(2, "200".to_string());
            out.insert(2, "--tail".to_string());
            return out;
        }
        args.to_vec()
    }

    fn filter(&self, output: &str, args: &[String]) -> String {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        // Handle compose subcommands
        let effective_subcmd = if subcmd == "compose" || subcmd == "stack" {
            args.get(2).map(|s| s.as_str()).unwrap_or("")
        } else {
            subcmd
        };

        match effective_subcmd {
            "logs" => filter_logs(output),
            "ps" => filter_ps(output),
            "images" => filter_images(output),
            _ => output.to_string(),
        }
    }
}

fn filter_logs(output: &str) -> String {
    let lines_in = output.lines().count();
    if lines_in == 0 {
        return output.to_string();
    }
    // Anomaly-based scoring: outlier lines (errors, unique events) score highest.
    // Budget: ~30% of lines, min 20, max 200.
    let budget = (lines_in / 3).max(20).min(200);
    ccr_core::summarizer::summarize(output, budget).output
}

fn filter_ps(output: &str) -> String {
    // Keep only name/container ID, status, and ports columns
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return output.to_string();
    }

    // Header + data rows
    let mut out: Vec<String> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            // Header: keep it but truncate
            out.push(line.to_string());
        } else if !line.trim().is_empty() {
            // Data row: extract name, status, ports
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 5 {
                // Typical docker ps columns: CONTAINER ID, IMAGE, COMMAND, CREATED, STATUS, PORTS, NAMES
                let name = parts.last().unwrap_or(&"");
                let status = parts[4];
                // Try to get ports (may span multiple columns)
                let ports_start = line.rfind("  ").unwrap_or(0);
                let ports = &line[ports_start..].trim();
                out.push(format!("{} [{}] {}", name, status, ports));
            } else {
                out.push(line.to_string());
            }
        }
    }
    out.join("\n")
}

fn filter_images(output: &str) -> String {
    // Keep only repo, tag, and size
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return output.to_string();
    }

    let mut out: Vec<String> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            out.push("REPOSITORY           TAG       SIZE".to_string());
        } else if !line.trim().is_empty() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 7 {
                // REPOSITORY TAG IMAGE_ID CREATED VIRTUAL_SIZE
                let repo = parts[0];
                let tag = parts[1];
                let size = parts.last().unwrap_or(&"");
                out.push(format!("{:<20} {:<9} {}", repo, tag, size));
            } else {
                out.push(line.to_string());
            }
        }
    }
    out.join("\n")
}
