use std::fs;
use std::path::PathBuf;
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
