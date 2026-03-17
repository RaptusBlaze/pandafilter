use super::Handler;
use super::util;

pub struct KubectlHandler;

impl Handler for KubectlHandler {
    fn rewrite_args(&self, args: &[String]) -> Vec<String> {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        if subcmd == "logs" && !args.iter().any(|a| a.starts_with("--tail")) {
            let mut out = args.to_vec();
            out.push("--tail=200".to_string());
            return out;
        }
        args.to_vec()
    }

    fn filter(&self, output: &str, args: &[String]) -> String {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        match subcmd {
            "get" => util::compact_table(output, &[0, 1, 2, 4]),
            "logs" => filter_logs(output),
            "describe" => filter_describe(output),
            "apply" | "delete" | "rollout" => filter_changes(output),
            _ => output.to_string(),
        }
    }
}

fn filter_logs(output: &str) -> String {
    let lines_in = output.lines().count();
    if lines_in == 0 {
        return output.to_string();
    }
    let budget = (lines_in / 3).max(20).min(200);
    ccr_core::summarizer::summarize(output, budget).output
}

fn filter_describe(output: &str) -> String {
    let keep_sections = ["Name:", "Status:", "Conditions:", "Events:"];
    let mut out: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut annotation_count = 0usize;
    let mut in_annotations = false;

    for line in output.lines() {
        let t = line.trim();

        // Check if we're starting an annotation/label block
        if t.starts_with("Annotations:") || t.starts_with("Labels:") {
            in_annotations = true;
            annotation_count = 0;
            out.push(line.to_string());
            continue;
        }

        if in_annotations {
            // Indented continuation lines are annotation entries
            if line.starts_with(' ') || line.starts_with('\t') {
                annotation_count += 1;
                if annotation_count <= 5 {
                    out.push(line.to_string());
                } else if annotation_count == 6 {
                    out.push(format!("[{} annotations]", annotation_count));
                }
                continue;
            } else {
                in_annotations = false;
            }
        }

        let is_section = keep_sections.iter().any(|s| t.starts_with(s));
        if is_section {
            in_section = true;
        } else if !t.is_empty() && !line.starts_with(' ') && !line.starts_with('\t') {
            in_section = false;
        }

        if in_section || is_section {
            out.push(line.to_string());
        }
    }

    if out.is_empty() {
        output.to_string()
    } else {
        out.join("\n")
    }
}

fn filter_changes(output: &str) -> String {
    let out: Vec<&str> = output
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && (t.contains("created")
                    || t.contains("deleted")
                    || t.contains("configured")
                    || t.contains("unchanged")
                    || t.contains("error")
                    || t.contains("Error")
                    || t.starts_with("deployment.")
                    || t.starts_with("service.")
                    || t.starts_with("pod/"))
        })
        .collect();
    if out.is_empty() {
        output.to_string()
    } else {
        out.join("\n")
    }
}
