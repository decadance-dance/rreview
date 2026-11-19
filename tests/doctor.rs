//! `rreview doctor` against fake `claude`, `codex`, `gh` and `orca` (tests/fixtures/bin) on PATH.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

#[path = "fixtures/fake_env.rs"]
mod fake_env;

struct Run {
    root: String,
    passed: bool,
    stdout: String,
    log: String,
    /// Every prompt the fake orca typed into a terminal titled `rreview:claude...`.
    claude_prompts: String,
    /// The self-test dirs doctor left in its TMPDIR.
    kept: usize,
}

fn doctor(fakes: &[&str], scenario: &[(&str, &str)]) -> Run {
    doctor_with(&[], fakes, scenario)
}

/// Runs `rreview doctor <args>` with only `fakes` (and the system dirs) on PATH, the given
/// scenario vars, a temp HOME, CODEX_HOME and TMPDIR, and no ORCA_TERMINAL_HANDLE unless the
/// scenario sets one.
fn doctor_with(args: &[&str], fakes: &[&str], scenario: &[(&str, &str)]) -> Run {
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
    let log = root.join("log").display().to_string();
    let vars: Vec<_> = scenario.iter().map(|&(k, v)| (k, v.to_string())).collect();
    fake_env::write(&bin, &[vec![("FAKE_LOG", log)], vars].concat());
    let out = Command::new(env!("CARGO_BIN_EXE_rreview"))
        .arg("doctor")
        .args(args)
        .env_remove("ORCA_TERMINAL_HANDLE")
        .env_remove("CLAUDE_CONFIG_DIR")
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
        claude_prompts: fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("log.prompt.claude"))
            })
            .map(|p| fs::read_to_string(p).unwrap())
            .collect(),
        kept: fs::read_dir(&root)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("rreview-doctor-")
            })
            .count(),
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

const WITH_ORCA: &[&str] = &["claude", "codex", "gh", "orca"];
/// An Orca terminal whose fake agents play the probe prompts.
const IN_ORCA: &[(&str, &str)] = &[("ORCA_TERMINAL_HANDLE", "term_test"), ("FAKE_ORCA", "play")];

fn doctor_orca(scenario: &[(&str, &str)]) -> Run {
    doctor_with(
        &["--runner", "orca"],
        WITH_ORCA,
        &[IN_ORCA, scenario].concat(),
    )
}

fn assert_lines(run: &Run, lines: &[&str]) {
    for line in lines {
        assert!(
            run.stdout.contains(line),
            "missing {line:?} in\n{}",
            run.stdout
        );
    }
}

#[test]
fn orca_runner_passes_through_the_orca_launch_builders() {
    let run = doctor_orca(&[]);
    assert!(run.passed, "{}", run.stdout);
    assert_lines(
        &run,
        &[
            "ok    orca: orca 9.9.9",
            "ok    orca worktree: found",
            "ok    claude sandbox: git diff: works",
            "ok    orca claude: Write tool into the worktree: blocked",
            "ok    orca claude: Edit tool in the worktree: blocked",
            "ok    orca claude sandbox: write into the worktree: blocked",
            "ok    orca claude sandbox: git commit: blocked",
            "ok    orca claude sandbox: git branch: blocked",
            "ok    orca claude sandbox: git diff: works",
            "ok    orca claude --safe-mode: planted project hook: did not fire",
            "ok    orca codex sandbox: write into the worktree: blocked",
            "ok    orca codex sandbox: write into the git dir: blocked",
            "ok    orca codex sandbox: write into HOME: blocked",
            "ok    orca codex: user AGENTS.md: not loaded",
        ],
    );
    for title in ["claude", "codex", "codex_control"] {
        assert!(
            run.log.contains(&format!(
                "orca terminal create --worktree current --title rreview:{title}:rreview-doctor-"
            )),
            "{}",
            run.log
        );
    }
    assert!(
        run.log
            .contains("'--permission-mode' 'acceptEdits' '--add-dir' '/"),
        "{}",
        run.log
    );
    assert!(
        run.log.contains("'-s' 'workspace-write' '-a' 'never'"),
        "{}",
        run.log
    );
}

#[test]
fn orca_runner_is_skipped_outside_an_orca_terminal() {
    let run = doctor_with(&["--runner", "orca"], ALL, &[]);
    assert!(run.passed, "{}", run.stdout);
    assert_lines(
        &run,
        &[
            "warn  orca: not found on PATH (optional)",
            "skip  orca self-test: not in an Orca terminal (ORCA_TERMINAL_HANDLE is unset)",
            "ok    claude sandbox: write into the worktree: blocked",
        ],
    );
    assert!(!run.stdout.contains("orca claude"), "{}", run.stdout);
}

#[test]
fn orca_runner_needs_orca_in_an_orca_terminal() {
    let run = doctor_with(&["--runner", "orca"], ALL, IN_ORCA);
    assert!(!run.passed, "{}", run.stdout);
    assert_lines(&run, &["FAIL  orca: not found on PATH"]);
}

/// Each orca check has a broken-isolation scenario that fails it: none passes vacuously.
#[test]
fn orca_runner_fails_each_check_on_broken_isolation() {
    for (scenario, lines) in [
        (
            ("FAKE_CLAUDE", "leak"),
            &[
                "FAIL  orca claude sandbox: write into the worktree: went through",
                "FAIL  orca claude sandbox: git commit: went through",
                "FAIL  orca claude sandbox: git branch: went through",
            ][..],
        ),
        (
            ("FAKE_CLAUDE", "tools"),
            &[
                "FAIL  orca claude: Write tool into the worktree: went through",
                "FAIL  orca claude: Edit tool in the worktree: went through",
            ],
        ),
        (
            ("FAKE_CLAUDE", "lie"),
            &["FAIL  orca claude sandbox: git diff: failed"],
        ),
        (
            ("FAKE_CLAUDE", "hook"),
            &["FAIL  orca claude --safe-mode: planted project hook: fired"],
        ),
        (
            ("FAKE_CLAUDE", "dead-hook"),
            &["FAIL  orca claude --safe-mode: planted project hook: control failed"],
        ),
        (
            ("FAKE_CLAUDE", "ignore"),
            &["FAIL  orca claude self-test: the probe did not run in Claude's sandbox"],
        ),
        (
            ("FAKE_CODEX", "leak"),
            &[
                "FAIL  orca codex sandbox: write into the worktree: went through",
                "FAIL  orca codex sandbox: write into the git dir: went through",
                "FAIL  orca codex sandbox: write into HOME: went through",
            ],
        ),
        (
            ("FAKE_CODEX", "user-agents"),
            &["FAIL  orca codex: user AGENTS.md: loaded"],
        ),
        (
            ("FAKE_CODEX", "ignore"),
            &[
                "FAIL  orca codex sandbox: write into the worktree: the probe did not run",
                "FAIL  orca codex: user AGENTS.md: control failed",
            ],
        ),
    ] {
        let run = doctor_orca(&[scenario]);
        assert!(!run.passed, "{scenario:?}: {}", run.stdout);
        assert_lines(&run, lines);
    }
}

#[test]
fn orca_codex_gets_one_reminder_to_run_the_probe() {
    let run = doctor_orca(&[("FAKE_CODEX", "decline-once")]);
    assert!(run.passed, "{}", run.stdout);
    assert_lines(
        &run,
        &["ok    orca codex sandbox: write into the worktree: blocked"],
    );
}

/// Unchanged files alone do not prove Write and Edit blocked: Claude may never have tried them.
#[test]
fn write_and_edit_need_evidence_that_claude_could_not_use_them() {
    for (scenario, ok) in [
        ("offered", false),
        ("no-transcript", false),
        ("refused", true),
    ] {
        let run = doctor_orca(&[("FAKE_CLAUDE", scenario)]);
        for a in ["claude", "orca claude"] {
            for tool in ["Write tool into the worktree", "Edit tool in the worktree"] {
                let line = format!("{} {a}: {tool}:", if ok { "ok   " } else { "FAIL " });
                assert!(
                    run.stdout.contains(&line),
                    "{scenario}: missing {line:?} in\n{}",
                    run.stdout
                );
            }
        }
    }
}

/// Claude's probe pass asks for a plain-text reply, in the delivery line and the runner's
/// reminder too: a JSON heredoc stops the TUI at a permission dialog.
#[test]
fn the_orca_claude_probe_never_asks_for_json() {
    let run = doctor_orca(&[("FAKE_CLAUDE", "nudge")]);
    assert!(run.passed, "{}", run.stdout);
    assert!(
        run.claude_prompts
            .contains("You stopped without writing the reply file"),
        "{}",
        run.claude_prompts
    );
    assert!(
        !run.claude_prompts.contains("JSON"),
        "{}",
        run.claude_prompts
    );
}

/// A terminal whose `create` reply was lost is found by its title and closed: doctor's
/// terminals live in the caller's worktree, which no teardown removes.
#[test]
fn orca_terminals_with_a_lost_create_reply_are_closed() {
    let run = doctor_orca(&[("FAKE_ORCA", "lost-create")]);
    assert!(!run.passed, "{}", run.stdout);
    let count = |what: &str| run.log.lines().filter(|l| l.starts_with(what)).count();
    let created = count("orca terminal create");
    assert!(created >= 2, "{}", run.log);
    assert_eq!(created, count("orca terminal close"), "{}", run.log);
    assert!(
        run.log.contains("--title rreview:claude:rreview-doctor-"),
        "{}",
        run.log
    );
}

/// A `create` that opens its terminal but answers without a handle is found by title too.
#[test]
fn orca_terminals_created_without_a_handle_are_closed() {
    let run = doctor_orca(&[("FAKE_ORCA", "no-handle")]);
    assert!(!run.passed, "{}", run.stdout);
    let count = |what: &str| run.log.lines().filter(|l| l.starts_with(what)).count();
    let created = count("orca terminal create");
    assert!(created >= 2, "{}", run.log);
    assert_eq!(created, count("orca terminal close"), "{}", run.log);
}

/// Codex's reminder in a new session must not forget the first session's terminal when its
/// close failed: it stays for the runner's final cleanup.
#[test]
fn orca_codex_retry_keeps_the_first_terminal_for_cleanup() {
    let run = doctor_orca(&[
        ("FAKE_CODEX", "chat-refuse"),
        ("FAKE_ORCA_CLOSE", "fail-once"),
        ("FAKE_ORCA_HANDLES", "numbered"),
    ]);
    assert!(run.passed, "{}", run.stdout);
    let handles = |prefix: &str| {
        let mut h: Vec<_> = run
            .log
            .lines()
            .filter_map(|l| l.strip_prefix(prefix))
            .collect();
        h.sort();
        h.dedup();
        h
    };
    let created = handles("created ");
    assert!(
        created.iter().any(|h| h.ends_with("~2")),
        "no retry: {}",
        run.log
    );
    assert_eq!(created, handles("closed "), "{}", run.log);
}

/// A Codex that refuses in chat, writing no reply file, still gets the script reminder, and one
/// that never runs the probe reads as "the probe did not run", not as a failed self-test.
#[test]
fn orca_codex_refusing_in_chat_gets_the_reminder_and_a_probe_diagnostic() {
    let run = doctor_orca(&[("FAKE_CODEX", "chat-refuse")]);
    assert!(run.passed, "{}", run.stdout);
    let run = doctor_orca(&[("FAKE_CODEX", "mute")]);
    assert!(!run.passed, "{}", run.stdout);
    assert_lines(
        &run,
        &[
            "FAIL  orca codex sandbox: write into the worktree: the probe did not run",
            "FAIL  orca codex: user AGENTS.md: no reply",
        ],
    );
    assert!(
        !run.stdout.contains("orca codex self-test"),
        "{}",
        run.stdout
    );
}

/// Only the fixture whose checks failed stays, with how to remove it; a passing one goes.
#[test]
fn only_a_failed_fixture_is_kept() {
    let run = doctor_orca(&[("FAKE_CODEX", "mute")]);
    assert!(!run.passed, "{}", run.stdout);
    assert_eq!(run.kept, 1, "{}", run.stdout);
    assert_eq!(
        run.stdout.matches("self-test files kept in").count(),
        1,
        "{}",
        run.stdout
    );
    assert_lines(&run, &["remove it with: rm -rf "]);
    let run = doctor_orca(&[]);
    assert!(run.passed, "{}", run.stdout);
    assert_eq!(run.kept, 0, "{}", run.stdout);
}
