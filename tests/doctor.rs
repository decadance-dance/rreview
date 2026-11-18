//! `rreview doctor` against fake `claude`, `codex` and `gh` (tests/fixtures/bin) on PATH.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

struct Run {
    root: String,
    passed: bool,
    stdout: String,
    log: String,
}

/// Runs `rreview doctor` with only `fakes` (and the system dirs) on PATH, the given
/// `FAKE_*` scenario vars, and a temp HOME, CODEX_HOME and TMPDIR.
fn doctor(fakes: &[&str], scenario: &[(&str, &str)]) -> Run {
    static N: AtomicU32 = AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!(
        "rreview-it-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&root);
    let (bin, codex_home) = (root.join("bin"), root.join("codex"));
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&codex_home).unwrap();
    fs::write(codex_home.join("auth.json"), "{}").unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bin");
    for fake in fakes {
        std::os::unix::fs::symlink(fixtures.join(fake), bin.join(fake)).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_rreview"))
        .arg("doctor")
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", &root)
        .env("CODEX_HOME", &codex_home)
        .env("TMPDIR", &root)
        .env("FAKE_LOG", root.join("log"))
        .envs(scenario.iter().copied())
        .output()
        .unwrap();
    let run = Run {
        root: fs::canonicalize(&root).unwrap().display().to_string(),
        passed: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into(),
        log: fs::read_to_string(root.join("log")).unwrap_or_default(),
    };
    fs::remove_dir_all(&root).unwrap();
    run
}

const ALL: &[&str] = &["claude", "codex", "gh"];

#[test]
fn passes_with_working_isolation_and_launches_through_the_builders() {
    let run = doctor(ALL, &[]);
    assert!(run.passed, "{}", run.stdout);
    for line in [
        "ok    claude: 9.9.9 (Claude Code)",
        "warn  orca: not found on PATH (optional)",
        "ok    claude sandbox: write into the worktree: blocked",
        "ok    claude sandbox: git commit: blocked",
        "ok    claude sandbox: git branch: blocked",
        "ok    claude sandbox: git diff: works",
        "ok    claude --safe-mode: planted project hook: did not fire",
        "ok    codex sandbox: write into the worktree: blocked",
        "ok    codex: user AGENTS.md: not loaded",
    ] {
        assert!(
            run.stdout.contains(line),
            "missing {line:?} in\n{}",
            run.stdout
        );
    }
    assert!(
        run.log.contains("claude -p --safe-mode --settings /"),
        "{}",
        run.log
    );
    assert!(
        run.log.contains("codex exec --json -s read-only -C /"),
        "{}",
        run.log
    );
    assert!(run.log.contains("/codex-home\n"), "{}", run.log);
}

#[test]
fn fails_when_a_binary_is_missing() {
    let run = doctor(&["claude", "gh"], &[]);
    assert!(!run.passed);
    assert!(
        run.stdout.contains("FAIL  codex: not found on PATH"),
        "{}",
        run.stdout
    );
}

#[test]
fn fails_when_claude_has_no_safe_mode() {
    let run = doctor(ALL, &[("FAKE_CLAUDE", "no-safe-mode")]);
    assert!(!run.passed);
    assert!(
        run.stdout.contains(
            "FAIL  claude --safe-mode: this claude has no --safe-mode; update Claude Code"
        ),
        "{}",
        run.stdout
    );
    assert!(
        !run.log.contains("claude -p"),
        "a pass ran without --safe-mode:\n{}",
        run.log
    );
}

#[test]
fn fails_when_a_claude_write_goes_through() {
    let run = doctor(ALL, &[("FAKE_CLAUDE", "leak")]);
    assert!(!run.passed);
    for line in [
        "FAIL  claude sandbox: write into the worktree: went through",
        "FAIL  claude sandbox: git commit: went through",
        "FAIL  claude sandbox: git branch: went through",
        "self-test files kept in",
    ] {
        assert!(
            run.stdout.contains(line),
            "missing {line:?} in\n{}",
            run.stdout
        );
    }
}

#[test]
fn fails_when_a_codex_write_goes_through() {
    let run = doctor(ALL, &[("FAKE_CODEX", "leak")]);
    assert!(!run.passed);
    assert!(
        run.stdout
            .contains("FAIL  codex sandbox: write into the worktree: went through"),
        "{}",
        run.stdout
    );
}

#[test]
fn fails_when_codex_loads_the_user_agents_md() {
    let run = doctor(ALL, &[("FAKE_CODEX", "user-agents")]);
    assert!(!run.passed);
    assert!(
        run.stdout.contains("FAIL  codex: user AGENTS.md: loaded"),
        "{}",
        run.stdout
    );
}

#[test]
fn fails_when_the_agents_skip_the_probe_instead_of_passing_vacuously() {
    let run = doctor(ALL, &[("FAKE_CLAUDE", "ignore"), ("FAKE_CODEX", "ignore")]);
    assert!(!run.passed);
    for line in [
        "FAIL  claude self-test: the probe did not run in Claude's sandbox",
        "FAIL  codex sandbox: write into the worktree: no evidence",
        "FAIL  codex: user AGENTS.md: control failed",
    ] {
        assert!(
            run.stdout.contains(line),
            "missing {line:?} in\n{}",
            run.stdout
        );
    }
}

#[test]
fn fails_when_codex_claims_a_blocked_write_without_running_the_probe() {
    let run = doctor(ALL, &[("FAKE_CODEX", "lie")]);
    assert!(!run.passed, "{}", run.stdout);
    assert!(
        run.stdout
            .contains("FAIL  codex sandbox: write into the worktree: no evidence"),
        "{}",
        run.stdout
    );
}

#[test]
fn fails_when_the_claude_probe_report_contradicts_the_sandbox() {
    let run = doctor(ALL, &[("FAKE_CLAUDE", "lie")]);
    assert!(!run.passed, "{}", run.stdout);
    for line in [
        "FAIL  claude sandbox: write into the worktree: went through",
        "FAIL  claude sandbox: git commit: went through",
        "FAIL  claude sandbox: git branch: went through",
        "FAIL  claude sandbox: git diff: failed",
    ] {
        assert!(
            run.stdout.contains(line),
            "missing {line:?} in\n{}",
            run.stdout
        );
    }
}

#[test]
fn fails_when_the_claude_pass_exits_non_zero() {
    let run = doctor(ALL, &[("FAKE_CLAUDE", "crash")]);
    assert!(!run.passed, "{}", run.stdout);
    assert!(
        run.stdout
            .contains("FAIL  claude self-test: claude exit status: 42"),
        "{}",
        run.stdout
    );
}

#[test]
fn fails_when_the_planted_hook_fires() {
    let run = doctor(ALL, &[("FAKE_CLAUDE", "hook")]);
    assert!(!run.passed, "{}", run.stdout);
    assert!(
        run.stdout
            .contains("FAIL  claude --safe-mode: planted project hook: fired"),
        "{}",
        run.stdout
    );
}

#[test]
fn fails_when_the_planted_hook_cannot_fire_at_all() {
    let run = doctor(ALL, &[("FAKE_CLAUDE", "dead-hook")]);
    assert!(!run.passed, "{}", run.stdout);
    assert!(
        run.stdout
            .contains("FAIL  claude --safe-mode: planted project hook: control failed"),
        "{}",
        run.stdout
    );
}

#[test]
fn fails_when_codex_only_reads_the_probe() {
    let run = doctor(ALL, &[("FAKE_CODEX", "read")]);
    assert!(!run.passed, "{}", run.stdout);
    assert!(
        run.stdout
            .contains("FAIL  codex sandbox: write into the worktree: no evidence"),
        "{}",
        run.stdout
    );
}

#[test]
fn hook_control_keeps_claude_out_of_the_user_home() {
    let run = doctor(ALL, &[]);
    assert!(run.passed, "{}", run.stdout);
    let env = run
        .log
        .lines()
        .find_map(|l| l.strip_prefix("control-env "))
        .expect("the hook control ran");
    let control = format!("{}/", run.root);
    for var in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "CLAUDE_CONFIG_DIR",
    ] {
        let value = env
            .split(' ')
            .find_map(|kv| kv.strip_prefix(&format!("{var}=")))
            .unwrap();
        assert!(
            value.starts_with(&control) && value.contains("/claude-hook-control/"),
            "{var}={value:?} is not inside the control's run dir"
        );
    }
}

#[test]
fn fails_when_codex_rebuilds_the_result_line_from_the_probe_source() {
    let run = doctor(ALL, &[("FAKE_CODEX", "rebuild")]);
    assert!(!run.passed, "{}", run.stdout);
    assert!(
        run.stdout
            .contains("FAIL  codex sandbox: write into the worktree: no evidence"),
        "{}",
        run.stdout
    );
}
