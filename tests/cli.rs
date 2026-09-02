use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Duration, Local};
use serde_json::json;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

static TEST_HOME_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn isolated_home() -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after Unix epoch")
        .as_nanos();
    let sequence = TEST_HOME_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "codex-switcher-test-{}-{unique}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(path.join(".codex")).expect("test home should be created");
    path
}

fn run(home: &PathBuf, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_codex-switcher"))
        .args(arguments)
        .env("HOME", home)
        .env_remove("CODEX_HOME")
        .env_remove("CODEX_SWITCHER_HOME")
        .output()
        .expect("codex-switcher should run")
}

fn exit_code(output: &std::process::Output) -> i32 {
    output.status.code().expect("process should exit normally")
}

#[cfg(unix)]
fn write_fake_codex(home: &Path, body: &str) -> PathBuf {
    let bin = home.join("bin");
    fs::create_dir_all(&bin).expect("fake bin should be created");
    let fake_codex = bin.join("codex");
    fs::write(&fake_codex, format!("#!/bin/sh\n{body}\n")).expect("fake codex should be written");
    fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o755))
        .expect("fake codex should be executable");
    bin
}

#[test]
fn captures_refreshes_and_switches_profiles() {
    let home = isolated_home();
    let auth = home.join(".codex/auth.json");
    fs::write(&auth, b"personal-v1").expect("auth should be written");

    assert!(run(&home, &["init"]).status.success());
    assert!(run(&home, &["add", "personal"]).status.success());

    fs::write(&auth, b"personal-refreshed").expect("auth should be refreshed");
    fs::write(home.join(".codex-switcher/profiles/work.json"), b"work-v1")
        .expect("work profile should be written");

    assert!(run(&home, &["use", "work"]).status.success());
    assert_eq!(
        fs::read(&auth).expect("work auth should be active"),
        b"work-v1"
    );
    assert!(run(&home, &["use", "personal"]).status.success());
    assert_eq!(
        fs::read(&auth).expect("active auth should exist"),
        b"personal-refreshed",
        "the refreshed token cache should be retained when switching away"
    );

    fs::remove_dir_all(home).expect("test home should be removed");
}

#[test]
fn failed_login_restores_previous_authentication() {
    let home = isolated_home();
    let auth = home.join(".codex/auth.json");
    fs::write(&auth, b"personal").expect("auth should be written");
    assert!(run(&home, &["init"]).status.success());
    assert!(run(&home, &["add", "personal"]).status.success());

    let output = Command::new(env!("CARGO_BIN_EXE_codex-switcher"))
        .args(["login", "work"])
        .env("HOME", &home)
        .env("PATH", "")
        .env_remove("CODEX_HOME")
        .env_remove("CODEX_SWITCHER_HOME")
        .output()
        .expect("codex-switcher should run");

    assert!(!output.status.success());
    assert_eq!(
        fs::read(&auth).expect("auth should be restored"),
        b"personal"
    );
    assert_eq!(
        fs::read_to_string(home.join(".codex-switcher/active"))
            .expect("active profile should be restored")
            .trim(),
        "personal"
    );

    fs::remove_dir_all(home).expect("test home should be removed");
}

#[cfg(unix)]
#[test]
fn list_displays_plan_and_both_limit_windows() {
    let home = isolated_home();
    let auth = home.join(".codex/auth.json");
    fs::write(&auth, b"personal").expect("auth should be written");
    assert!(run(&home, &["init"]).status.success());
    assert!(run(&home, &["add", "personal"]).status.success());

    let bin = home.join("bin");
    fs::create_dir_all(&bin).expect("fake bin should be created");
    let fake_codex = bin.join("codex");
    fs::write(
        &fake_codex,
        r#"#!/bin/sh
read -r initialize
printf '%s\n' '{"id":0,"result":{"userAgent":"test"}}'
read -r initialized
read -r account
printf '%s\n' '{"id":1,"result":{"account":{"type":"chatgpt","planType":"plus"},"requiresOpenaiAuth":true}}'
read -r limits
printf '%s\n' '{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":25,"windowDurationMins":300,"resetsAt":1893456000},"secondary":{"usedPercent":40,"windowDurationMins":10080,"resetsAt":1893888000}}}}'
read -r usage
printf '%s\n' '{"id":3,"result":{"summary":{"lifetimeTokens":1500000},"dailyUsageBuckets":[]}}'
read -r wait
"#,
    )
    .expect("fake codex should be written");
    fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o755))
        .expect("fake codex should be executable");

    let output = Command::new(env!("CARGO_BIN_EXE_codex-switcher"))
        .arg("list")
        .env("HOME", &home)
        .env("PATH", &bin)
        .env_remove("CODEX_HOME")
        .env_remove("CODEX_SWITCHER_HOME")
        .output()
        .expect("codex-switcher should run");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("output should be UTF-8");
    assert!(stdout.contains("PROFILE"));
    assert!(stdout.contains("5H LEFT"));
    assert!(stdout.contains("WEEKLY LEFT"));
    assert!(stdout.contains("WEEKLY RESET"));
    assert!(stdout.contains("* personal"));
    assert!(stdout.contains("PLUS"));
    assert!(stdout.contains("75%"));
    assert!(stdout.contains("60%"));
    assert!(!stdout.contains("TOKENS USED"));
    assert!(!stdout.contains("1,5M"));
    assert!(!stdout.contains("5H USED"));
    assert!(!stdout.contains("WEEKLY USED"));
    assert!(!stdout.contains("\u{1b}["));
    assert!(stdout.starts_with('\n'));
    assert!(stdout.ends_with("\n\n"));

    fs::remove_dir_all(home).expect("test home should be removed");
}

#[cfg(unix)]
#[test]
fn stats_displays_detailed_account_activity() {
    let home = isolated_home();
    let auth = home.join(".codex/auth.json");
    fs::write(&auth, b"work").expect("auth should be written");
    assert!(run(&home, &["init"]).status.success());
    assert!(run(&home, &["add", "work"]).status.success());

    let bin = home.join("bin");
    fs::create_dir_all(&bin).expect("fake bin should be created");
    let fake_codex = bin.join("codex");
    let today = Local::now().date_naive();
    let previous_period = today - Duration::days(30);
    let resets_at = Local::now().timestamp() + 3_600;
    let account = json!({
        "id": 1,
        "result": {
            "account": { "type": "chatgpt", "planType": "team" },
            "requiresOpenaiAuth": true
        }
    });
    let limits = json!({
        "id": 2,
        "result": {
            "rateLimits": {
                "primary": {
                    "usedPercent": 50,
                    "windowDurationMins": 300,
                    "resetsAt": resets_at
                },
                "secondary": {
                    "usedPercent": 25,
                    "windowDurationMins": 10080,
                    "resetsAt": resets_at + 86_400
                }
            },
            "rateLimitResetCredits": { "availableCount": 2, "credits": null }
        }
    });
    let usage = json!({
        "id": 3,
        "result": {
            "summary": {
                "lifetimeTokens": 1_500_000,
                "peakDailyTokens": 45_678,
                "longestRunningTurnSec": 540,
                "currentStreakDays": 8,
                "longestStreakDays": 14
            },
            "dailyUsageBuckets": [
                { "startDate": previous_period.to_string(), "tokens": 5_000 },
                { "startDate": today.to_string(), "tokens": 12_345 }
            ]
        }
    });
    let script = format!(
        "#!/bin/sh\n\
         read -r initialize\n\
         printf '%s\\n' '{{\"id\":0,\"result\":{{\"userAgent\":\"test\"}}}}'\n\
         read -r initialized\n\
         read -r account\n\
         printf '%s\\n' '{account}'\n\
         read -r limits\n\
         printf '%s\\n' '{limits}'\n\
         read -r usage\n\
         printf '%s\\n' '{usage}'\n\
         read -r wait\n"
    );
    fs::write(&fake_codex, script).expect("fake codex should be written");
    fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o755))
        .expect("fake codex should be executable");

    let output = Command::new(env!("CARGO_BIN_EXE_codex-switcher"))
        .args(["stats", "work", "--period", "30d"])
        .env("HOME", &home)
        .env("PATH", &bin)
        .env_remove("CODEX_HOME")
        .env_remove("CODEX_SWITCHER_HOME")
        .output()
        .expect("codex-switcher should run");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("output should be UTF-8");
    assert!(stdout.contains("ACCOUNT STATISTICS"));
    assert!(stdout.contains("Profile             work"));
    assert!(stdout.contains("Plan                TEAM"));
    assert!(stdout.contains("Lifetime tokens     1,5M"));
    assert!(stdout.contains(&format!("Today ({today})")));
    assert!(stdout.contains("Daily record        45,7K"));
    assert!(stdout.contains("Longest turn        9m 0s"));
    assert!(stdout.contains("Current streak      8 days"));
    assert!(stdout.contains("Available resets    2"));
    assert!(stdout.contains("ACTIVITY · LAST 30 DAYS"));
    assert!(stdout.contains(&today.to_string()));
    assert!(stdout.contains("12,3K"));
    assert!(stdout.contains("trend +147% vs previous period"));
    assert!(stdout.starts_with('\n'));
    assert!(stdout.ends_with("\n\n"));
    assert!(!stdout.contains("\u{1b}["));

    fs::remove_dir_all(home).expect("test home should be removed");
}

#[cfg(unix)]
#[test]
fn json_contract_uses_raw_values_iso_dates_and_nulls_without_ansi() {
    let home = isolated_home();
    let auth = home.join(".codex/auth.json");
    fs::write(&auth, b"personal").expect("auth should be written");
    assert!(run(&home, &["init"]).status.success());
    assert!(run(&home, &["add", "personal"]).status.success());

    let bin = write_fake_codex(
        &home,
        r#"read -r initialize
printf '%s\n' '{"id":0,"result":{"userAgent":"test"}}'
read -r initialized
read -r account
printf '%s\n' '{"id":1,"result":{"account":{"type":"chatgpt","planType":"plus"}}}'
read -r limits
printf '%s\n' '{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":12.5,"windowDurationMins":300,"resetsAt":1893456000}}}}'
read -r usage
printf '%s\n' '{"id":3,"result":{"summary":{},"dailyUsageBuckets":[]}}'
read -r wait"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_codex-switcher"))
        .args(["list", "--json"])
        .env("HOME", &home)
        .env("PATH", &bin)
        .env_remove("CODEX_HOME")
        .env_remove("CODEX_SWITCHER_HOME")
        .output()
        .expect("codex-switcher should run");

    assert!(output.status.success());
    assert!(!output.stdout.windows(2).any(|bytes| b"\x1b[" == bytes));
    assert!(!output.stderr.windows(2).any(|bytes| b"\x1b[" == bytes));
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["profiles"][0]["name"], "personal");
    assert_eq!(document["profiles"][0]["active"], true);
    assert_eq!(document["profiles"][0]["plan"], "PLUS");
    assert_eq!(
        document["profiles"][0]["limits"]["five_hour"]["used_percent"],
        12.5
    );
    assert_eq!(
        document["profiles"][0]["limits"]["five_hour"]["remaining_percent"],
        87.5
    );
    assert_eq!(
        document["profiles"][0]["limits"]["five_hour"]["resets_at"],
        "2030-01-01T00:00:00+00:00"
    );
    assert!(document["profiles"][0]["limits"]["weekly"].is_null());
    assert!(document["profiles"][0]["lifetime_tokens"].is_null());
    assert!(document["profiles"][0]["error"].is_null());

    let stats = Command::new(env!("CARGO_BIN_EXE_codex-switcher"))
        .args(["stats", "personal", "--json"])
        .env("HOME", &home)
        .env("PATH", &bin)
        .env_remove("CODEX_HOME")
        .env_remove("CODEX_SWITCHER_HOME")
        .output()
        .expect("codex-switcher should run");
    assert!(stats.status.success());
    let stats: serde_json::Value =
        serde_json::from_slice(&stats.stdout).expect("stats should be JSON");
    assert_eq!(stats["schema_version"], 1);
    assert_eq!(stats["profile"], "personal");
    assert_eq!(stats["limits"]["five_hour"]["used_percent"], 12.5);
    assert!(stats["lifetime_tokens"].is_null());

    fs::remove_dir_all(home).expect("test home should be removed");
}

#[test]
fn doctor_json_is_machine_readable_even_when_checks_fail() {
    let home = isolated_home();
    let output = run(&home, &["doctor", "--json"]);
    assert_eq!(exit_code(&output), 3);
    assert!(!output.stdout.windows(2).any(|bytes| b"\x1b[" == bytes));
    assert!(!output.stderr.windows(2).any(|bytes| b"\x1b[" == bytes));
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor should be JSON");
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["healthy"], false);
    assert_eq!(document["checks"].as_array().map(Vec::len), Some(4));

    fs::remove_dir_all(home).expect("test home should be removed");
}

#[test]
fn current_json_and_prompt_cover_active_missing_and_corrupt_state() {
    let home = isolated_home();
    fs::create_dir_all(home.join(".codex-switcher")).expect("state dir should exist");
    fs::write(home.join(".codex-switcher/active"), "work\n").expect("state should be written");

    let prompt = run(&home, &["current", "--format", "prompt"]);
    assert_eq!(exit_code(&prompt), 0);
    assert_eq!(prompt.stdout, b"work\n");
    assert!(prompt.stderr.is_empty());

    let json_output = run(&home, &["current", "--json"]);
    let current: serde_json::Value =
        serde_json::from_slice(&json_output.stdout).expect("current should be JSON");
    assert_eq!(current, json!({"schema_version": 1, "profile": "work"}));

    fs::remove_file(home.join(".codex-switcher/active")).expect("state should be removed");
    let missing = run(&home, &["current", "--format", "prompt"]);
    assert_eq!(exit_code(&missing), 0);
    assert!(missing.stdout.is_empty());
    let missing_json = run(&home, &["current", "--json"]);
    let current: serde_json::Value =
        serde_json::from_slice(&missing_json.stdout).expect("current should be JSON");
    assert!(current["profile"].is_null());

    fs::write(home.join(".codex-switcher/active"), "../broken\n")
        .expect("corrupt state should be written");
    let corrupt = run(&home, &["current", "--format", "prompt"]);
    assert_eq!(exit_code(&corrupt), 4);
    assert!(corrupt.stdout.is_empty());

    fs::remove_dir_all(home).expect("test home should be removed");
}

#[cfg(unix)]
#[test]
fn every_public_error_category_has_a_stable_exit_code() {
    let arguments = isolated_home();
    assert_eq!(exit_code(&run(&arguments, &["unknown"])), 2);

    let initialization = isolated_home();
    assert_eq!(exit_code(&run(&initialization, &["use", "work"])), 3);

    let profile = isolated_home();
    assert!(run(&profile, &["init"]).status.success());
    assert_eq!(exit_code(&run(&profile, &["use", "missing"])), 4);

    let authentication = isolated_home();
    assert!(run(&authentication, &["init"]).status.success());
    assert_eq!(exit_code(&run(&authentication, &["add", "work"])), 5);

    let timeout = isolated_home();
    fs::write(timeout.join(".codex/auth.json"), b"work").expect("auth should be written");
    assert!(run(&timeout, &["init"]).status.success());
    assert!(run(&timeout, &["add", "work"]).status.success());
    let bin = write_fake_codex(&timeout, "exit 0");
    let timed_out = Command::new(env!("CARGO_BIN_EXE_codex-switcher"))
        .args(["stats", "work"])
        .env("HOME", &timeout)
        .env("PATH", &bin)
        .output()
        .expect("codex-switcher should run");
    assert_eq!(exit_code(&timed_out), 6);

    let locked = isolated_home();
    fs::create_dir_all(locked.join(".codex-switcher")).expect("switcher dir should exist");
    fs::write(locked.join(".codex-switcher/lock"), b"1\n").expect("lock should exist");
    assert_eq!(exit_code(&run(&locked, &["init"])), 7);

    let runtime = isolated_home();
    let invalid_home = runtime.join("not-a-directory");
    fs::write(&invalid_home, b"file").expect("blocking file should exist");
    let output = Command::new(env!("CARGO_BIN_EXE_codex-switcher"))
        .arg("init")
        .env("HOME", &runtime)
        .env("CODEX_SWITCHER_HOME", &invalid_home)
        .output()
        .expect("codex-switcher should run");
    assert_eq!(exit_code(&output), 1);

    for home in [
        arguments,
        initialization,
        profile,
        authentication,
        timeout,
        locked,
        runtime,
    ] {
        fs::remove_dir_all(home).expect("test home should be removed");
    }
}

#[test]
fn completion_supports_all_shells_and_dynamic_profiles() {
    let home = isolated_home();
    let profiles = home.join(".codex-switcher/profiles");
    fs::create_dir_all(&profiles).expect("profiles should exist");
    fs::write(profiles.join("personal.json"), b"personal").expect("profile should exist");
    fs::write(profiles.join("work-team.json"), b"work").expect("profile should exist");

    for shell in ["zsh", "bash", "fish"] {
        let output = run(&home, &["completion", shell]);
        assert_eq!(exit_code(&output), 0, "completion failed for {shell}");
        let completion = String::from_utf8(output.stdout).expect("completion should be UTF-8");
        assert!(
            completion.contains("personal"),
            "missing profile for {shell}"
        );
        assert!(
            completion.contains("work-team"),
            "missing profile for {shell}"
        );
        let json_flag = if "fish" == shell { "-l json" } else { "--json" };
        assert!(completion.contains(json_flag), "missing flag for {shell}");
        assert!(
            completion.contains("current"),
            "missing command for {shell}"
        );
    }

    fs::remove_dir_all(home).expect("test home should be removed");
}

#[test]
fn aliases_and_shell_function_remain_available() {
    let home = isolated_home();
    let version = run(&home, &["version"]);
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("codex-switcher "));
    let auth = home.join(".codex/auth.json");
    fs::write(&auth, b"personal").expect("auth should be written");
    assert!(run(&home, &["init"]).status.success());
    assert!(run(&home, &["capture", "personal"]).status.success());
    fs::write(home.join(".codex-switcher/profiles/work.json"), b"work")
        .expect("profile should be written");
    assert!(run(&home, &["switch", "work"]).status.success());
    assert!(run(&home, &["ls", "--json"]).status.success());
    assert!(run(&home, &["rm", "personal"]).status.success());

    for shell in ["zsh", "bash", "fish"] {
        let output = run(&home, &["shell-function", shell]);
        assert!(output.status.success());
        let function = String::from_utf8(output.stdout).expect("function should be UTF-8");
        assert!(function.contains("cs"));
        assert!(function.contains("codex-switcher use"));
    }

    fs::remove_dir_all(home).expect("test home should be removed");
}
