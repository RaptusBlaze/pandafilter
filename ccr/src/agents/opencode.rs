//! OpenCode agent installer.
//!
//! OpenCode is an open-source AI coding agent by SST/anomalyco.
//! It uses a JavaScript plugin system for tool hooks.
//!
//! Hook registration:
//!   A JavaScript plugin is placed at `~/.config/opencode/plugins/panda-filter.js`.
//!   OpenCode auto-discovers plugins from `{plugin,plugins}/*.{ts,js}` within its
//!   config directories, so no changes to `opencode.json` are required.
//!
//! The plugin hooks into:
//!   - `tool.execute.before` (bash tool): rewrites commands via `panda rewrite`
//!   - `tool.execute.after`  (bash/read/grep/glob/webfetch): compresses output
//!     via `panda hook` with `PANDA_AGENT=opencode`
//!
//! Config directory: `~/.config/opencode/`  (opencode follows XDG conventions)
//! Plugin file:      `~/.config/opencode/plugins/panda-filter.js`

use super::AgentInstaller;
use std::path::PathBuf;

pub struct OpencodeInstaller;

/// Return `~/.config/opencode` — opencode follows XDG Base Directory spec.
fn opencode_config_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".config").join("opencode"))
}

fn plugins_dir() -> Option<PathBuf> {
    Some(opencode_config_dir()?.join("plugins"))
}

fn plugin_path() -> Option<PathBuf> {
    Some(plugins_dir()?.join("panda-filter.js"))
}

impl AgentInstaller for OpencodeInstaller {
    fn name(&self) -> &'static str {
        "OpenCode"
    }

    fn install(&self, panda_bin: &str) -> anyhow::Result<()> {
        let Some(plugins_dir) = plugins_dir() else {
            anyhow::bail!("Cannot determine OpenCode plugins directory");
        };

        std::fs::create_dir_all(&plugins_dir)?;

        let plugin_path = plugins_dir.join("panda-filter.js");
        let plugin_content = generate_plugin(panda_bin);
        std::fs::write(&plugin_path, &plugin_content)?;

        // Integrity baseline — written to the plugins dir so `panda hook` can
        // check it at runtime with PANDA_AGENT=opencode.
        if let Err(e) = crate::integrity::write_baseline(&plugin_path, &plugins_dir) {
            eprintln!("warning: could not write integrity baseline: {e}");
        }

        println!("PandaFilter plugin installed (OpenCode):");
        println!("  Plugin file: {}", plugin_path.display());
        println!(
            "  Auto-discovered from {}",
            plugins_dir.display()
        );
        println!();
        println!("Run 'panda doctor' to verify your installation.");
        println!();
        println!("Tip: On large repos (25+ files, 2000+ lines), focus ranking");
        println!("     gives the agent confidence-ranked file hints.");
        println!("     Run 'panda focus --enable' to activate it.");

        Ok(())
    }

    fn uninstall(&self) -> anyhow::Result<()> {
        let Some(plugins_dir) = plugins_dir() else {
            return Ok(());
        };

        let plugin_path = plugins_dir.join("panda-filter.js");
        if plugin_path.exists() {
            std::fs::remove_file(&plugin_path)?;
            println!("Removed {}", plugin_path.display());
        }

        let hash_path = plugins_dir.join(".panda-hook.sha256");
        if hash_path.exists() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&hash_path) {
                    let mut perms = meta.permissions();
                    perms.set_mode(0o644);
                    let _ = std::fs::set_permissions(&hash_path, perms);
                }
            }
            std::fs::remove_file(&hash_path)?;
            println!("Removed {}", hash_path.display());
        }

        println!();
        println!("PandaFilter OpenCode plugin removed.");

        Ok(())
    }
}

/// Generate the JavaScript plugin file content.
///
/// The plugin is a V1-format OpenCode plugin (default-exported object with `id`
/// and `server` fields).  It uses `node:child_process` `spawnSync` to call the
/// panda binary synchronously — acceptable because panda hook is very fast
/// (pure text processing, no I/O waits).
fn generate_plugin(panda_bin: &str) -> String {
    PLUGIN_TEMPLATE.replace("__PANDA_BIN__", panda_bin)
}

/// JavaScript plugin template.  `__PANDA_BIN__` is replaced with the absolute
/// path to the panda binary at install time.
const PLUGIN_TEMPLATE: &str = r#"// PandaFilter OpenCode plugin
// Provides command rewriting and output compression for the OpenCode agent.
// Auto-discovered from ~/.config/opencode/plugins/panda-filter.js
// Do not edit — regenerate with: panda init --agent opencode

import { spawnSync } from "node:child_process"

const PANDA = "__PANDA_BIN__"

// Map OpenCode tool names (lowercase) to PandaFilter tool names
const TOOL_NAME_MAP = {
  bash: "Bash",
  read: "Read",
  grep: "Grep",
  glob: "Glob",
  webfetch: "WebFetch",
  websearch: "WebSearch",
}

// Tools whose output PandaFilter compresses
const COMPRESSIBLE = new Set(["bash", "read", "grep", "glob", "webfetch"])

export default {
  id: "panda-filter",
  server: async (_input) => ({
    // PreToolUse equivalent: rewrite bash commands before execution
    "tool.execute.before": async ({ tool, sessionID }, output) => {
      if (tool !== "bash") return
      const cmd = output.args?.command
      if (!cmd || typeof cmd !== "string") return
      try {
        const r = spawnSync(PANDA, ["rewrite", cmd], {
          encoding: "utf-8",
          timeout: 5000,
          env: { ...process.env, PANDA_SESSION_ID: sessionID ?? "" },
        })
        if (r.status === 0 && r.stdout) {
          const rewritten = r.stdout.trim()
          if (rewritten && rewritten !== cmd) output.args.command = rewritten
        }
      } catch (_) {} // fail silently — never block the agent
    },

    // PostToolUse equivalent: compress tool output before the LLM sees it
    "tool.execute.after": async ({ tool, sessionID, args }, output) => {
      if (!COMPRESSIBLE.has(tool)) return
      try {
        const r = spawnSync(PANDA, ["hook"], {
          input: JSON.stringify({
            tool_name: TOOL_NAME_MAP[tool] ?? "Bash",
            tool_input: args ?? {},
            tool_response: { output: output.output },
          }),
          encoding: "utf-8",
          timeout: 10000,
          env: {
            ...process.env,
            PANDA_SESSION_ID: sessionID ?? "",
            PANDA_AGENT: "opencode",
          },
        })
        if (r.status === 0 && r.stdout) {
          const parsed = JSON.parse(r.stdout.trim())
          if (typeof parsed.output === "string") output.output = parsed.output
        }
      } catch (_) {} // fail silently — never block the agent
    },
  }),
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_contains_panda_bin() {
        let content = generate_plugin("/usr/local/bin/panda");
        assert!(content.contains("/usr/local/bin/panda"));
        assert!(!content.contains("__PANDA_BIN__"));
    }

    #[test]
    fn plugin_has_valid_structure() {
        let content = generate_plugin("/usr/local/bin/panda");
        // V1 plugin: id and server fields
        assert!(content.contains("id: \"panda-filter\""));
        assert!(content.contains("server:"));
        // Has both hooks
        assert!(content.contains("tool.execute.before"));
        assert!(content.contains("tool.execute.after"));
        // Uses panda rewrite
        assert!(content.contains("rewrite"));
        // Uses panda hook
        assert!(content.contains("\"hook\""));
        assert!(content.contains("PANDA_AGENT"));
        // Tool name mapping
        assert!(content.contains("TOOL_NAME_MAP"));
        assert!(content.contains("COMPRESSIBLE"));
    }

    #[test]
    fn plugin_silently_ignores_errors() {
        let content = generate_plugin("/usr/local/bin/panda");
        // catch blocks must be present
        assert!(content.contains("catch (_)"));
    }
}
