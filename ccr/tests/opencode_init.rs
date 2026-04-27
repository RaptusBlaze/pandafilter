/// Integration tests for `panda init --agent opencode` / `--uninstall`.
///
/// Each test overrides $HOME with a temporary directory so nothing touches the
/// real ~/.config/opencode.
use assert_cmd::Command;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn ccr() -> Command {
    Command::cargo_bin("panda").unwrap()
}

fn opencode_plugins_dir(home: &TempDir) -> PathBuf {
    home.path()
        .join(".config")
        .join("opencode")
        .join("plugins")
}

fn plugin_file(home: &TempDir) -> PathBuf {
    opencode_plugins_dir(home).join("panda-filter.js")
}

/// Run `panda init --agent opencode` with a fake home directory.
fn run_opencode_init(home: &TempDir) {
    ccr()
        .args(["init", "--agent", "opencode"])
        .env("HOME", home.path())
        .assert()
        .success();
}

// ── 1. Plugin file created ────────────────────────────────────────────────────

#[test]
fn test_opencode_init_creates_plugin_file() {
    let home = TempDir::new().unwrap();
    run_opencode_init(&home);
    assert!(plugin_file(&home).exists(), "panda-filter.js should exist");
}

// ── 2. Plugin directory created ───────────────────────────────────────────────

#[test]
fn test_opencode_init_creates_plugins_dir() {
    let home = TempDir::new().unwrap();
    run_opencode_init(&home);
    assert!(
        opencode_plugins_dir(&home).exists(),
        "~/.config/opencode/plugins/ should be created"
    );
}

// ── 3. Plugin file has valid V1 structure ─────────────────────────────────────

#[test]
fn test_opencode_plugin_has_valid_structure() {
    let home = TempDir::new().unwrap();
    run_opencode_init(&home);

    let content = fs::read_to_string(plugin_file(&home)).unwrap();

    // V1 plugin: default export with id and server
    assert!(
        content.contains("export default"),
        "plugin must have default export"
    );
    assert!(
        content.contains("id: \"panda-filter\""),
        "plugin must export id"
    );
    assert!(
        content.contains("server:"),
        "plugin must export server function"
    );
}

// ── 4. Plugin contains both hooks ─────────────────────────────────────────────

#[test]
fn test_opencode_plugin_has_both_hooks() {
    let home = TempDir::new().unwrap();
    run_opencode_init(&home);

    let content = fs::read_to_string(plugin_file(&home)).unwrap();

    assert!(
        content.contains("tool.execute.before"),
        "plugin must have tool.execute.before hook"
    );
    assert!(
        content.contains("tool.execute.after"),
        "plugin must have tool.execute.after hook"
    );
}

// ── 5. Plugin contains panda rewrite ─────────────────────────────────────────

#[test]
fn test_opencode_plugin_contains_rewrite() {
    let home = TempDir::new().unwrap();
    run_opencode_init(&home);

    let content = fs::read_to_string(plugin_file(&home)).unwrap();
    assert!(
        content.contains("rewrite"),
        "plugin must call panda rewrite"
    );
}

// ── 6. Plugin contains PANDA_AGENT=opencode ───────────────────────────────────

#[test]
fn test_opencode_plugin_sets_panda_agent() {
    let home = TempDir::new().unwrap();
    run_opencode_init(&home);

    let content = fs::read_to_string(plugin_file(&home)).unwrap();
    assert!(
        content.contains("PANDA_AGENT") && content.contains("opencode"),
        "plugin must set PANDA_AGENT=opencode for integrity check"
    );
}

// ── 7. Plugin binary path is substituted ─────────────────────────────────────

#[test]
fn test_opencode_plugin_has_no_template_placeholder() {
    let home = TempDir::new().unwrap();
    run_opencode_init(&home);

    let content = fs::read_to_string(plugin_file(&home)).unwrap();
    assert!(
        !content.contains("__PANDA_BIN__"),
        "template placeholder must be replaced with actual binary path"
    );
}

// ── 8. Idempotent ─────────────────────────────────────────────────────────────

#[test]
fn test_opencode_init_idempotent() {
    let home = TempDir::new().unwrap();
    run_opencode_init(&home);
    run_opencode_init(&home); // second call

    // Plugin file should still exist and be valid (not duplicated or corrupted)
    assert!(plugin_file(&home).exists(), "plugin should still exist after two inits");

    let content = fs::read_to_string(plugin_file(&home)).unwrap();
    // Count occurrences of the id field — should appear exactly once
    let count = content.matches("id: \"panda-filter\"").count();
    assert_eq!(count, 1, "plugin id should appear exactly once after two inits");
}

// ── 9. Integrity baseline created ─────────────────────────────────────────────

#[test]
fn test_opencode_init_creates_integrity_baseline() {
    let home = TempDir::new().unwrap();
    run_opencode_init(&home);

    let hash_file = opencode_plugins_dir(&home).join(".panda-hook.sha256");
    assert!(hash_file.exists(), "integrity baseline .panda-hook.sha256 should be created");
}

// ── 10. Uninstall removes plugin and baseline ─────────────────────────────────

#[test]
fn test_opencode_uninstall_removes_plugin() {
    let home = TempDir::new().unwrap();
    run_opencode_init(&home);
    assert!(plugin_file(&home).exists());

    ccr()
        .args(["init", "--agent", "opencode", "--uninstall"])
        .env("HOME", home.path())
        .assert()
        .success();

    assert!(
        !plugin_file(&home).exists(),
        "plugin file should be removed after uninstall"
    );

    let hash_file = opencode_plugins_dir(&home).join(".panda-hook.sha256");
    assert!(
        !hash_file.exists(),
        "hash file should be removed after uninstall"
    );
}

// ── 11. Default `panda init` does not touch ~/.config/opencode ────────────────

#[test]
fn test_claude_init_does_not_touch_opencode() {
    let home = TempDir::new().unwrap();

    ccr()
        .args(["init"])
        .env("HOME", home.path())
        .assert()
        .success();

    assert!(
        !home.path().join(".config").join("opencode").exists(),
        "panda init (claude) should not create ~/.config/opencode"
    );
}

// ── 12. Plugin compressible tools list ───────────────────────────────────────

#[test]
fn test_opencode_plugin_compressible_tools() {
    let home = TempDir::new().unwrap();
    run_opencode_init(&home);

    let content = fs::read_to_string(plugin_file(&home)).unwrap();

    // The plugin should compress these tool outputs
    for tool in &["bash", "read", "grep", "glob", "webfetch"] {
        assert!(
            content.contains(tool),
            "plugin must reference tool '{}'",
            tool
        );
    }
}
