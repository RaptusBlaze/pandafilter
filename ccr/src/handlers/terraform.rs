use super::util;
use super::Handler;

pub struct TerraformHandler;

impl Handler for TerraformHandler {
    fn rewrite_args(&self, args: &[String]) -> Vec<String> {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        match subcmd {
            "plan" | "apply" | "destroy" => {
                if args.iter().any(|a| a == "-no-color" || a == "--no-color") {
                    args.to_vec()
                } else {
                    let mut out = args.to_vec();
                    out.push("-no-color".to_string());
                    out
                }
            }
            _ => args.to_vec(),
        }
    }

    fn filter(&self, output: &str, args: &[String]) -> String {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        match subcmd {
            "plan" => filter_plan(output),
            "apply" => filter_apply(output),
            "init" => filter_init(output),
            "validate" => filter_validate(output),
            "output" => filter_output(output),
            "state" => filter_state(output, args),
            _ => output.to_string(),
        }
    }
}

fn filter_plan(output: &str) -> String {
    const PLAN_NO_CHANGE_RULES: &[util::MatchOutputRule] = &[util::MatchOutputRule {
        success_pattern: r"No changes\. Your infrastructure matches the configuration",
        error_pattern: r"Error:",
        ok_message: "no changes detected",
    }];
    if let Some(msg) = util::check_match_output(output, PLAN_NO_CHANGE_RULES) {
        return msg;
    }

    let mut out: Vec<String> = Vec::new();
    for line in output.lines() {
        let t = line.trim();
        if t.starts_with('+')
            || t.starts_with('-')
            || t.starts_with('~')
            || t.starts_with("Plan:")
            || t.contains("No changes")
        {
            out.push(line.to_string());
        }
    }
    if out.is_empty() {
        output.to_string()
    } else {
        out.join("\n")
    }
}

fn filter_apply(output: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in output.lines() {
        let t = line.trim();
        if t.contains(": Creating...")
            || t.contains(": Creation complete")
            || t.contains(": Destroying...")
            || t.contains(": Destruction complete")
            || t.contains(": Modifying...")
            || t.contains(": Modifications complete")
            || t.starts_with("Apply complete!")
            || t.contains("Error:")
            || t.starts_with("Error ")
        {
            out.push(line.to_string());
        }
    }
    if out.is_empty() {
        output.to_string()
    } else {
        out.join("\n")
    }
}

fn filter_init(output: &str) -> String {
    let has_error = output.lines().any(|l| {
        let t = l.trim();
        t.starts_with("Error") || t.starts_with("error")
    });
    if has_error {
        let errors: Vec<&str> = output
            .lines()
            .filter(|l| {
                let t = l.trim();
                t.starts_with("Error") || t.starts_with("error") || t.contains("Error:")
            })
            .collect();
        return errors.join("\n");
    }
    "[terraform init complete]".to_string()
}

fn filter_validate(output: &str) -> String {
    const VALIDATE_OK_RULES: &[util::MatchOutputRule] = &[util::MatchOutputRule {
        success_pattern: r"(?i)The configuration is valid|Success!",
        error_pattern: r"(?i)error|Error",
        ok_message: "terraform validate: ok",
    }];
    if let Some(msg) = util::check_match_output(output, VALIDATE_OK_RULES) {
        return msg;
    }

    let mut out: Vec<String> = Vec::new();
    for line in output.lines() {
        let t = line.trim();
        if t.contains("Success")
            || t.contains("error")
            || t.contains("Error")
            || t.contains("warning")
        {
            out.push(line.to_string());
        }
    }
    if out.is_empty() {
        output.to_string()
    } else {
        out.join("\n")
    }
}

fn filter_output(output: &str) -> String {
    let mut pairs: Vec<String> = Vec::new();
    let mut current_key: Option<String> = None;
    let mut is_sensitive = false;

    for line in output.lines() {
        let t = line.trim();
        if t.is_empty() || t == "{" {
            continue;
        }
        if t == "}" {
            current_key = None;
            is_sensitive = false;
            continue;
        }
        if t == "sensitive = true" {
            if let Some(ref key) = current_key {
                pairs.push(format!("{} = <sensitive>", key));
            }
            current_key = None;
            is_sensitive = false;
            continue;
        }
        // Skip type/sensitive annotation lines inside a block
        if t.starts_with("type      =") || t.starts_with("sensitive =") {
            if t.contains("true") {
                is_sensitive = true;
            }
            continue;
        }
        if t.starts_with("value     =") || t.starts_with("value =") {
            let val_part = t.splitn(2, '=').nth(1).unwrap_or("").trim();
            if let Some(ref key) = current_key {
                if is_sensitive {
                    pairs.push(format!("{} = <sensitive>", key));
                } else {
                    pairs.push(format!("{} = {}", key, val_part));
                }
            }
            current_key = None;
            is_sensitive = false;
            continue;
        }
        // "key = value" or "key = {" pattern
        if let Some(eq_pos) = t.find(" = ") {
            let key = t[..eq_pos].trim();
            let val = t[eq_pos + 3..].trim();
            if val == "{" {
                current_key = Some(key.to_string());
            } else {
                pairs.push(format!("{} = {}", key, val));
            }
        }
    }

    if pairs.is_empty() { output.to_string() } else { pairs.join("\n") }
}

fn filter_state(output: &str, args: &[String]) -> String {
    let state_subcmd = args.get(2).map(|s| s.as_str()).unwrap_or("");
    match state_subcmd {
        "list" => {
            let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
            const MAX: usize = 50;
            if lines.len() > MAX {
                let extra = lines.len() - MAX;
                let mut out: Vec<String> = lines[..MAX].iter().map(|l| l.to_string()).collect();
                out.push(format!("[+{} more]", extra));
                out.join("\n")
            } else {
                lines.join("\n")
            }
        }
        "show" => {
            const KEEP_ATTRS: &[&str] = &["id", "name", "type", "status", "arn", "region"];
            let mut out: Vec<String> = Vec::new();
            for line in output.lines() {
                let t = line.trim();
                if t.is_empty() { continue; }
                if t.starts_with('#') || t.starts_with("resource ") || t == "{" || t == "}" {
                    out.push(line.to_string());
                    continue;
                }
                if let Some(eq_pos) = t.find(" = ") {
                    let attr = t[..eq_pos].trim().trim_matches('"');
                    if KEEP_ATTRS.iter().any(|k| {
                        attr == *k || attr.ends_with(&format!("_{}", k)) || attr.starts_with(&format!("{}_", k))
                    }) {
                        out.push(line.to_string());
                    }
                }
            }
            if out.is_empty() { output.to_string() } else { out.join("\n") }
        }
        _ => output.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler() -> TerraformHandler {
        TerraformHandler
    }

    #[test]
    fn plan_no_changes_short_circuits() {
        let output = "Refreshing state...\nNo changes. Your infrastructure matches the configuration.\nTerraform has compared your real infrastructure against your configuration\nand found no differences, so no changes are needed.";
        let result = handler().filter(output, &["terraform".to_string(), "plan".to_string()]);
        assert_eq!(result, "no changes detected");
    }

    #[test]
    fn plan_with_error_not_short_circuited() {
        let output = "No changes. Your infrastructure matches the configuration.\nError: Invalid resource configuration";
        let result = handler().filter(output, &["terraform".to_string(), "plan".to_string()]);
        assert_ne!(result, "no changes detected");
    }

    #[test]
    fn validate_ok_short_circuits() {
        let output = "Success! The configuration is valid.\n";
        let result = handler().filter(output, &["terraform".to_string(), "validate".to_string()]);
        assert_eq!(result, "terraform validate: ok");
    }

    #[test]
    fn test_output_compact() {
        let output = "db_endpoint = \"rds.example.com\"\nbucket_name = \"my-bucket\"\n";
        let result = handler().filter(output, &["terraform".to_string(), "output".to_string()]);
        assert!(result.contains("db_endpoint"), "got: {}", result);
        assert!(result.contains("bucket_name"), "got: {}", result);
    }

    #[test]
    fn test_output_sensitive_redacted() {
        let output = "db_password = {\n  sensitive = true\n  value     = \"supersecret\"\n  type      = \"string\"\n}\n";
        let result = handler().filter(output, &["terraform".to_string(), "output".to_string()]);
        assert!(result.contains("<sensitive>"), "got: {}", result);
        assert!(!result.contains("supersecret"), "got: {}", result);
    }

    #[test]
    fn test_state_list_capped() {
        let mut output = String::new();
        for i in 0..60 {
            output.push_str(&format!("aws_instance.web_{}\n", i));
        }
        let result = handler().filter(
            &output,
            &["terraform".to_string(), "state".to_string(), "list".to_string()],
        );
        assert!(result.contains("[+10 more]"), "got: {}", result);
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 51, "should have 50 resources + 1 overflow line, got {}", lines.len());
    }
}
