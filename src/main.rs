use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveDate, TimeZone, Utc};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const CREDENTIAL_SETTING: &str = "cli_auth_credentials_store";
const FIVE_HOURS_MINS: u64 = 5 * 60;
const WEEK_MINS: u64 = 7 * 24 * 60;
const QUERY_TIMEOUT: Duration = Duration::from_secs(20);
const ANSI_RESET: &str = "\x1b[0m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_PURPLE: &str = "\x1b[35m";
const ANSI_WHITE: &str = "\x1b[37m";
const ANSI_GOLD: &str = "\x1b[1;33m";
const ANSI_HEADING: &str = "\x1b[1;36m";
const ANSI_HINT: &str = "\x1b[2;37m";

const SCHEMA_VERSION: u64 = 1;

type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorKind {
    Runtime,
    Arguments,
    Initialization,
    Profile,
    Authentication,
    Timeout,
    Locked,
}

impl ErrorKind {
    const fn exit_code(self) -> u8 {
        match self {
            Self::Runtime => 1,
            Self::Arguments => 2,
            Self::Initialization => 3,
            Self::Profile => 4,
            Self::Authentication => 5,
            Self::Timeout => 6,
            Self::Locked => 7,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct AppError {
    kind: ErrorKind,
    message: String,
}

impl AppError {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn runtime(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Runtime, message)
    }

    fn initialization(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Initialization, message)
    }

    fn profile(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Profile, message)
    }

    fn authentication(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Authentication, message)
    }

    fn timeout(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Timeout, message)
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AppError {}

impl From<String> for AppError {
    fn from(message: String) -> Self {
        Self::runtime(message)
    }
}

trait ResultContext<T> {
    fn runtime(self, context: impl FnOnce() -> String) -> Result<T>;
}

impl<T, E: fmt::Display> ResultContext<T> for std::result::Result<T, E> {
    fn runtime(self, context: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|error| AppError::runtime(format!("{}: {error}", context())))
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "codex-switcher",
    version,
    about = "Fast account switching for Codex CLI and IDE integrations"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

impl Cli {
    fn requests_json(&self) -> bool {
        matches!(
            &self.command,
            Some(
                Commands::List { json: true }
                    | Commands::Stats { json: true, .. }
                    | Commands::Current { json: true, .. }
                    | Commands::Doctor { json: true }
            )
        )
    }
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Print the installed codex-switcher version.
    Version,
    /// Configure file-based Codex credentials and initialize profile storage.
    Init,
    /// Save the current Codex authentication as a named profile.
    #[command(alias = "capture")]
    Add {
        /// Profile name.
        name: String,
        /// Replace an existing profile with the same name.
        #[arg(long)]
        force: bool,
    },
    /// Run Codex login and save the authenticated account as a named profile.
    Login {
        /// Profile name.
        name: String,
        /// Replace an existing profile with the same name.
        #[arg(long)]
        force: bool,
        /// Use Codex device-code authentication.
        #[arg(long)]
        device_auth: bool,
    },
    /// Switch the active Codex authentication to a saved profile.
    #[command(alias = "switch")]
    Use {
        /// Profile name.
        name: String,
    },
    /// List saved profiles with their plans, limits, and reset times.
    #[command(alias = "ls")]
    List {
        /// Print versioned machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show detailed usage statistics for a saved profile.
    Stats {
        /// Profile name.
        name: String,
        /// Activity period from 1d to 365d.
        #[arg(long, default_value = "7d", value_parser = parse_period_value)]
        period: u64,
        /// Print versioned machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the currently active managed profile.
    Current {
        /// Print versioned machine-readable JSON.
        #[arg(long, conflicts_with = "format")]
        json: bool,
        /// Select regular text or minimal prompt output.
        #[arg(long, value_enum, default_value_t = CurrentFormat::Text, conflicts_with = "json")]
        format: CurrentFormat,
    },
    /// Delete a saved profile without logging out the active Codex account.
    #[command(alias = "rm")]
    Remove {
        /// Profile name.
        name: String,
        /// Allow removal when this profile is currently active.
        #[arg(long)]
        force: bool,
    },
    /// Diagnose credentials, active profile state, and Codex availability.
    Doctor {
        /// Print versioned machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Generate shell completion for commands, flags, and saved profiles.
    Completion {
        /// Target shell.
        shell: SupportedShell,
    },
    /// Generate the optional `cs` convenience function for a shell.
    #[command(name = "shell-function")]
    ShellFunction {
        /// Target shell.
        shell: SupportedShell,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum CurrentFormat {
    #[default]
    Text,
    Prompt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum SupportedShell {
    Zsh,
    Bash,
    Fish,
}

impl From<SupportedShell> for Shell {
    fn from(shell: SupportedShell) -> Self {
        match shell {
            SupportedShell::Zsh => Self::Zsh,
            SupportedShell::Bash => Self::Bash,
            SupportedShell::Fish => Self::Fish,
        }
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = if error.use_stderr() {
                ErrorKind::Arguments.exit_code()
            } else {
                0
            };
            let _ = error.print();
            return ExitCode::from(code);
        }
    };
    let json_output = cli.requests_json();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            print_error(&error.to_string(), !json_output);
            ExitCode::from(error.kind.exit_code())
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let command = match cli.command {
        Some(Commands::Version) => {
            println!("codex-switcher {VERSION}");
            return Ok(());
        }
        Some(Commands::ShellFunction { shell }) => return shell_function(shell),
        command => command,
    };
    let paths = Paths::discover()?;

    match command {
        Some(Commands::Init) => initialize(&paths),
        Some(Commands::Add { name, force }) => capture(&paths, &name, force),
        Some(Commands::Login {
            name,
            force,
            device_auth,
        }) => login(&paths, &name, force, device_auth),
        Some(Commands::Use { name }) => switch(&paths, &name),
        Some(Commands::List { json }) => list(&paths, json),
        Some(Commands::Stats { name, period, json }) => stats(&paths, &name, period, json),
        Some(Commands::Current { json, format }) => current(&paths, json, format),
        Some(Commands::Remove { name, force }) => remove(&paths, &name, force),
        Some(Commands::Doctor { json }) => doctor(&paths, json),
        Some(Commands::Completion { shell }) => completion(&paths, shell),
        Some(Commands::Version | Commands::ShellFunction { .. }) => unreachable!(),
        None => {
            Cli::command()
                .print_help()
                .runtime(|| "cannot print help".to_owned())?;
            println!();
            Ok(())
        }
    }
}

#[derive(Debug)]
struct Paths {
    codex_home: PathBuf,
    switcher_home: PathBuf,
}

impl Paths {
    fn discover() -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| AppError::initialization("HOME is not set"))?;
        let codex_home = env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        let switcher_home = env::var_os("CODEX_SWITCHER_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex-switcher"));

        Ok(Self {
            codex_home,
            switcher_home,
        })
    }

    fn auth(&self) -> PathBuf {
        self.codex_home.join("auth.json")
    }

    fn config(&self) -> PathBuf {
        self.codex_home.join("config.toml")
    }

    fn profiles(&self) -> PathBuf {
        self.switcher_home.join("profiles")
    }

    fn profile(&self, name: &str) -> PathBuf {
        self.profiles().join(format!("{name}.json"))
    }

    fn state(&self) -> PathBuf {
        self.switcher_home.join("active")
    }

    fn lock(&self) -> PathBuf {
        self.switcher_home.join("lock")
    }
}

struct Lock {
    path: PathBuf,
}

impl Lock {
    fn acquire(paths: &Paths) -> Result<Self> {
        secure_dir(&paths.switcher_home)?;
        for _ in 0..20 {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(paths.lock())
            {
                Ok(mut file) => {
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(Self { path: paths.lock() });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(error) => {
                    return Err(AppError::new(
                        ErrorKind::Locked,
                        format!("cannot create operation lock: {error}"),
                    ));
                }
            }
        }
        Err(AppError::new(
            ErrorKind::Locked,
            "another codex-switcher operation is running (remove ~/.codex-switcher/lock if it is stale)",
        ))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn initialize(paths: &Paths) -> Result<()> {
    let _lock = Lock::acquire(paths)?;
    secure_dir(&paths.codex_home)?;
    secure_dir(&paths.profiles())?;

    let config_path = paths.config();
    let original = match fs::read_to_string(&config_path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(AppError::initialization(format!(
                "cannot read {}: {error}",
                config_path.display()
            )));
        }
    };
    let updated = configure_file_credentials(&original);

    if updated != original {
        if !original.is_empty() {
            let backup = paths.codex_home.join("config.toml.codex-switcher.bak");
            if !backup.exists() {
                atomic_write(&backup, original.as_bytes(), 0o600)?;
            }
        }
        atomic_write(&config_path, updated.as_bytes(), 0o600)?;
        print_success(&format!(
            "Configured file-based Codex credentials in {}.",
            config_path.display()
        ));
    } else {
        print_success("File-based Codex credentials are already configured.");
    }

    print_hint("Next: codex-switcher add personal");
    Ok(())
}

fn capture(paths: &Paths, name: &str, force: bool) -> Result<()> {
    validate_name(name)?;
    let _lock = Lock::acquire(paths)?;
    ensure_initialized(paths)?;
    let auth = paths.auth();
    validate_auth_file(&auth)?;
    let destination = paths.profile(name);
    if destination.exists() && !force {
        return Err(AppError::profile(format!(
            "profile '{name}' already exists; pass --force to replace it"
        )));
    }

    save_active(paths)?;
    secure_copy(&auth, &destination)?;
    write_active(paths, Some(name))?;
    print_profile_status(
        "Captured the current Codex login as profile ",
        name,
        ".",
        ANSI_GREEN,
    );
    Ok(())
}

fn login(paths: &Paths, name: &str, force: bool, device_auth: bool) -> Result<()> {
    validate_name(name)?;
    let profile = paths.profile(name);
    if profile.exists() && !force {
        return Err(AppError::profile(format!(
            "profile '{name}' already exists; pass --force to replace it"
        )));
    }

    let _lock = Lock::acquire(paths)?;
    ensure_initialized(paths)?;
    save_active(paths)?;

    let previous_auth = fs::read(paths.auth()).ok();
    let previous_active = read_active(paths)?;
    remove_if_exists(&paths.auth())?;

    print_profile_status("Starting Codex login for profile ", name, "...", ANSI_CYAN);
    let mut command = Command::new("codex");
    command.arg("login");
    if device_auth {
        command.arg("--device-auth");
    }
    let status = match command.status() {
        Ok(status) => status,
        Err(error) => {
            restore_auth(paths, previous_auth.as_deref())?;
            write_active(paths, previous_active.as_deref())?;
            return Err(AppError::authentication(format!(
                "cannot start 'codex login': {error}; restored the previous active profile"
            )));
        }
    };

    if !status.success() || validate_auth_file(&paths.auth()).is_err() {
        restore_auth(paths, previous_auth.as_deref())?;
        write_active(paths, previous_active.as_deref())?;
        return Err(AppError::authentication(format!(
            "Codex login failed; restored the previous active profile (exit {status})"
        )));
    }

    secure_copy(&paths.auth(), &profile)?;
    write_active(paths, Some(name))?;
    print_profile_status("Profile ", name, " is logged in and active.", ANSI_GREEN);
    Ok(())
}

fn switch(paths: &Paths, name: &str) -> Result<()> {
    validate_name(name)?;
    let _lock = Lock::acquire(paths)?;
    ensure_initialized(paths)?;
    let profile = paths.profile(name);
    validate_auth_file(&profile)
        .map_err(|_| AppError::profile(format!("profile '{name}' does not exist or is invalid")))?;

    if read_active(paths)?.as_deref() == Some(name) {
        print_profile_status("Profile ", name, " is already active.", ANSI_CYAN);
        return Ok(());
    }

    save_active(paths)?;
    secure_copy(&profile, &paths.auth())?;
    write_active(paths, Some(name))?;
    print_switch_confirmation(name);
    Ok(())
}

fn list(paths: &Paths, json_output: bool) -> Result<()> {
    let _lock = Lock::acquire(paths)?;
    ensure_initialized(paths)?;
    save_active(paths)?;
    let active = read_active(paths)?;
    let mut names = profile_names(paths)?;
    names.sort_unstable();
    if names.is_empty() {
        if json_output {
            print_json(&ListJson {
                schema_version: SCHEMA_VERSION,
                profiles: Vec::new(),
            })?;
        } else {
            print_warning_stdout("No profiles. Run 'codex-switcher add personal'.");
        }
        return Ok(());
    }

    let mut spinner = if json_output {
        Spinner::disabled()
    } else {
        Spinner::start("Loading profile usage...")
    };
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| {
        for name in &names {
            let sender = sender.clone();
            scope.spawn(move || {
                let _ = sender.send((name.clone(), query_profile(paths, name)));
            });
        }
    });
    drop(sender);
    spinner.stop();

    let results: HashMap<String, Result<Usage>> = receiver.into_iter().collect();
    let mut rows = Vec::new();
    let mut profiles = Vec::new();
    let mut warnings = Vec::new();
    for name in names {
        let is_active = active.as_deref() == Some(&name);
        let profile_label = if active.as_deref() == Some(&name) {
            format!("* {name}")
        } else {
            format!("  {name}")
        };
        match results.get(&name) {
            Some(Ok(usage)) => {
                rows.push(usage.row(profile_label));
                profiles.push(ProfileJson::from_usage(name, is_active, usage, None));
            }
            Some(Err(error)) => {
                rows.push(Usage::unavailable().row(profile_label));
                warnings.push(format!("profile '{name}': {error}"));
                profiles.push(ProfileJson::from_usage(
                    name,
                    is_active,
                    &Usage::unavailable(),
                    Some(error.to_string()),
                ));
            }
            None => {
                rows.push(Usage::unavailable().row(profile_label));
                warnings.push(format!("profile '{name}': usage query did not finish"));
                profiles.push(ProfileJson::from_usage(
                    name,
                    is_active,
                    &Usage::unavailable(),
                    Some("usage query did not finish".to_owned()),
                ));
            }
        }
    }

    if json_output {
        print_json(&ListJson {
            schema_version: SCHEMA_VERSION,
            profiles,
        })?;
    } else {
        print_table(&rows);
        for warning in warnings {
            print_warning(&warning);
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
struct Window {
    used_percent: f64,
    duration_mins: u64,
    resets_at: Option<i64>,
}

#[derive(Clone, Debug, Default)]
struct Usage {
    plan: String,
    five_hours: Option<Window>,
    weekly: Option<Window>,
    lifetime_tokens: Option<u64>,
    peak_daily_tokens: Option<u64>,
    longest_running_turn_sec: Option<u64>,
    current_streak_days: Option<u64>,
    longest_streak_days: Option<u64>,
    daily_usage: Vec<DailyUsage>,
    daily_usage_available: bool,
    reset_credits: Option<u64>,
}

#[derive(Clone, Debug)]
struct DailyUsage {
    date: NaiveDate,
    tokens: u64,
}

#[derive(Serialize)]
struct ListJson {
    schema_version: u64,
    profiles: Vec<ProfileJson>,
}

#[derive(Serialize)]
struct ProfileJson {
    name: String,
    active: bool,
    plan: Option<String>,
    limits: LimitsJson,
    lifetime_tokens: Option<u64>,
    error: Option<String>,
}

impl ProfileJson {
    fn from_usage(name: String, active: bool, usage: &Usage, error: Option<String>) -> Self {
        Self {
            name,
            active,
            plan: string_value(&usage.plan),
            limits: LimitsJson::from_usage(usage),
            lifetime_tokens: usage.lifetime_tokens,
            error,
        }
    }
}

#[derive(Serialize)]
struct LimitsJson {
    five_hour: Option<WindowJson>,
    weekly: Option<WindowJson>,
}

impl LimitsJson {
    fn from_usage(usage: &Usage) -> Self {
        Self {
            five_hour: usage.five_hours.as_ref().map(WindowJson::from),
            weekly: usage.weekly.as_ref().map(WindowJson::from),
        }
    }
}

#[derive(Serialize)]
struct WindowJson {
    used_percent: f64,
    remaining_percent: f64,
    window_duration_minutes: u64,
    resets_at: Option<String>,
}

impl From<&Window> for WindowJson {
    fn from(window: &Window) -> Self {
        Self {
            used_percent: window.used_percent,
            remaining_percent: (100.0 - window.used_percent).clamp(0.0, 100.0),
            window_duration_minutes: window.duration_mins,
            resets_at: iso_timestamp(window.resets_at),
        }
    }
}

impl Usage {
    fn unavailable() -> Self {
        Self {
            plan: "—".to_owned(),
            ..Self::default()
        }
    }

    fn row(&self, profile: String) -> Vec<String> {
        vec![
            profile,
            value_or_dash(&self.plan),
            format_left(self.five_hours.as_ref()),
            format_reset(self.five_hours.as_ref()),
            format_left(self.weekly.as_ref()),
            format_reset(self.weekly.as_ref()),
        ]
    }
}

fn stats(paths: &Paths, name: &str, period_days: u64, json_output: bool) -> Result<()> {
    validate_name(name)?;
    let _lock = Lock::acquire(paths)?;
    ensure_initialized(paths)?;
    save_active(paths)?;
    validate_auth_file(&paths.profile(name))
        .map_err(|_| AppError::profile(format!("profile '{name}' does not exist or is invalid")))?;

    let mut spinner = if json_output {
        Spinner::disabled()
    } else {
        Spinner::start("Loading account statistics...")
    };
    let usage = query_profile(paths, name);
    spinner.stop();
    let usage = usage?;
    if json_output {
        print_json(&StatsJson::new(name, &usage, period_days))?;
    } else {
        print_stats(name, &usage, period_days);
    }
    Ok(())
}

#[derive(Serialize)]
struct StatsJson<'a> {
    schema_version: u64,
    profile: &'a str,
    plan: Option<String>,
    lifetime_tokens: Option<u64>,
    peak_daily_tokens: Option<u64>,
    longest_running_turn_seconds: Option<u64>,
    current_streak_days: Option<u64>,
    longest_streak_days: Option<u64>,
    available_resets: Option<u64>,
    limits: LimitsJson,
    period_days: u64,
    daily_usage: Vec<DailyUsageJson>,
}

impl<'a> StatsJson<'a> {
    fn new(profile: &'a str, usage: &Usage, period_days: u64) -> Self {
        let today = Local::now().date_naive();
        Self {
            schema_version: SCHEMA_VERSION,
            profile,
            plan: string_value(&usage.plan),
            lifetime_tokens: usage.lifetime_tokens,
            peak_daily_tokens: usage.peak_daily_tokens,
            longest_running_turn_seconds: usage.longest_running_turn_sec,
            current_streak_days: usage.current_streak_days,
            longest_streak_days: usage.longest_streak_days,
            available_resets: usage.reset_credits,
            limits: LimitsJson::from_usage(usage),
            period_days,
            daily_usage: usage_for_period(&usage.daily_usage, today, period_days)
                .into_iter()
                .map(|bucket| DailyUsageJson {
                    date: bucket.date.format("%Y-%m-%d").to_string(),
                    tokens: bucket.tokens,
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct DailyUsageJson {
    date: String,
    tokens: u64,
}

fn print_stats(name: &str, usage: &Usage, period_days: u64) {
    let colors = colors_enabled();
    let today = Local::now().date_naive();
    let selected = usage_for_period(&usage.daily_usage, today, period_days);
    let today_tokens = daily_tokens_for_date(&usage.daily_usage, today);

    println!();
    println!(
        "{}",
        styled_message("ACCOUNT STATISTICS", ANSI_HEADING, colors)
    );
    println!("{}", format_stat_row("Profile", name, ANSI_GOLD, colors));
    println!(
        "{}",
        format_stat_row("Plan", &value_or_dash(&usage.plan), ANSI_PURPLE, colors)
    );
    println!(
        "{}",
        format_stat_row(
            "Lifetime tokens",
            &format_tokens(usage.lifetime_tokens),
            ANSI_WHITE,
            colors
        )
    );
    println!(
        "{}",
        format_stat_row(
            &format!("Today ({today})"),
            &format_tokens(today_tokens),
            ANSI_CYAN,
            colors
        )
    );
    println!(
        "{}",
        format_stat_row(
            "Daily record",
            &format_tokens(usage.peak_daily_tokens),
            ANSI_CYAN,
            colors
        )
    );
    println!(
        "{}",
        format_stat_row(
            "Longest turn",
            &format_duration(usage.longest_running_turn_sec),
            ANSI_WHITE,
            colors
        )
    );
    println!(
        "{}",
        format_stat_row(
            "Current streak",
            &format_days(usage.current_streak_days),
            ANSI_GREEN,
            colors
        )
    );
    println!(
        "{}",
        format_stat_row(
            "Longest streak",
            &format_days(usage.longest_streak_days),
            ANSI_WHITE,
            colors
        )
    );
    let resets = usage
        .reset_credits
        .map(|count| count.to_string())
        .unwrap_or_else(|| "—".to_owned());
    let reset_color = if 0 == usage.reset_credits.unwrap_or(0) {
        ANSI_HINT
    } else {
        ANSI_GREEN
    };
    println!(
        "{}",
        format_stat_row("Available resets", &resets, reset_color, colors)
    );

    println!();
    println!("{}", styled_message("LIMITS", ANSI_HEADING, colors));
    println!(
        "{}",
        styled_message(
            "WINDOW    LEFT  RESET             PACE       FORECAST",
            ANSI_HINT,
            colors
        )
    );
    print_limit_stats("5 hours", usage.five_hours.as_ref(), colors);
    print_limit_stats("Weekly", usage.weekly.as_ref(), colors);

    println!();
    let trend = if usage.daily_usage_available {
        format_activity_trend(&usage.daily_usage, today, period_days)
    } else {
        "trend unavailable".to_owned()
    };
    let activity_title = format!(
        "ACTIVITY · LAST {period_days} {}",
        if 1 == period_days { "DAY" } else { "DAYS" }
    );
    println!(
        "{} · {}",
        styled_message(&activity_title, ANSI_HEADING, colors),
        styled_message(&trend, trend_color(&trend), colors)
    );
    if selected.is_empty() {
        if usage.daily_usage_available {
            println!("No activity in this period.");
        } else {
            println!("No daily activity returned for this period.");
        }
    } else {
        let peak = selected
            .iter()
            .map(|bucket| bucket.tokens)
            .max()
            .unwrap_or(0);
        for bucket in selected {
            println!(
                "{}  {}  {}",
                styled_message(&bucket.date.to_string(), ANSI_HINT, colors),
                styled_message(
                    &format!("{:>8}", format_token_count(bucket.tokens)),
                    ANSI_WHITE,
                    colors
                ),
                styled_message(&activity_bar(bucket.tokens, peak), ANSI_CYAN, colors)
            );
        }
    }
    println!();
}

fn format_stat_row(label: &str, value: &str, value_color: &str, colors: bool) -> String {
    format!(
        "{}{}",
        styled_message(&format!("{label:<20}"), ANSI_HINT, colors),
        styled_message(value, value_color, colors)
    )
}

fn print_limit_stats(label: &str, window: Option<&Window>, colors: bool) {
    let left = format_left(window);
    let reset = format_reset(window);
    let (pace, forecast) = window
        .map(limit_forecast)
        .unwrap_or_else(|| ("—".to_owned(), "—".to_owned()));
    let forecast_color = if forecast.starts_with("empty") {
        ANSI_RED
    } else if "lasts until reset" == forecast {
        ANSI_GREEN
    } else {
        ANSI_HINT
    };
    println!(
        "{} {}  {}  {}  {}",
        styled_message(&format!("{label:<9}"), ANSI_WHITE, colors),
        styled_message(&format!("{left:>4}"), remaining_color(&left), colors),
        styled_message(&format!("{reset:<16}"), ANSI_WHITE, colors),
        styled_message(&format!("{pace:<9}"), ANSI_YELLOW, colors),
        styled_message(&forecast, forecast_color, colors)
    );
}

fn trend_color(trend: &str) -> &'static str {
    if trend.starts_with("trend +") || "trend new activity" == trend {
        ANSI_GREEN
    } else if trend.starts_with("trend -") {
        ANSI_RED
    } else {
        ANSI_HINT
    }
}

fn limit_forecast(window: &Window) -> (String, String) {
    let Some(resets_at) = window.resets_at else {
        return ("—".to_owned(), "—".to_owned());
    };
    if !window.used_percent.is_finite() || window.used_percent <= 0.0 {
        return ("0%/h".to_owned(), "not at current pace".to_owned());
    }

    let now = Local::now().timestamp();
    let window_seconds = match i64::try_from(window.duration_mins.saturating_mul(60)) {
        Ok(seconds) => seconds,
        Err(_) => return ("—".to_owned(), "—".to_owned()),
    };
    let started_at = resets_at.saturating_sub(window_seconds);
    let elapsed = now.saturating_sub(started_at);
    if 0 >= elapsed || resets_at <= now {
        return ("—".to_owned(), "—".to_owned());
    }

    let used_per_second = window.used_percent / elapsed as f64;
    let pace_per_hour = used_per_second * 3_600.0;
    let seconds_left = (100.0 - window.used_percent).max(0.0) / used_per_second;
    let exhaustion = now.saturating_add(seconds_left.round() as i64);
    let forecast = if exhaustion < resets_at {
        Local
            .timestamp_opt(exhaustion, 0)
            .single()
            .map(|time| format!("empty ~{}", time.format("%Y-%m-%d %H:%M")))
            .unwrap_or_else(|| "—".to_owned())
    } else {
        "lasts until reset".to_owned()
    };
    (format!("{pace_per_hour:.1}%/h"), forecast)
}

fn usage_for_period(usage: &[DailyUsage], today: NaiveDate, period_days: u64) -> Vec<&DailyUsage> {
    let days = i64::try_from(period_days).unwrap_or(i64::MAX);
    let start = today - ChronoDuration::days(days.saturating_sub(1));
    let mut selected: Vec<&DailyUsage> = usage
        .iter()
        .filter(|bucket| start <= bucket.date && bucket.date <= today)
        .collect();
    selected.sort_unstable_by_key(|bucket| bucket.date);
    selected
}

fn daily_tokens_for_date(usage: &[DailyUsage], date: NaiveDate) -> Option<u64> {
    usage
        .iter()
        .find(|bucket| date == bucket.date)
        .map(|bucket| bucket.tokens)
}

fn format_activity_trend(usage: &[DailyUsage], today: NaiveDate, period_days: u64) -> String {
    let days = i64::try_from(period_days).unwrap_or(i64::MAX);
    let current_start = today - ChronoDuration::days(days.saturating_sub(1));
    let previous_end = current_start - ChronoDuration::days(1);
    let previous_start = previous_end - ChronoDuration::days(days.saturating_sub(1));
    let current: u64 = usage
        .iter()
        .filter(|bucket| current_start <= bucket.date && bucket.date <= today)
        .map(|bucket| bucket.tokens)
        .sum();
    let previous: u64 = usage
        .iter()
        .filter(|bucket| previous_start <= bucket.date && bucket.date <= previous_end)
        .map(|bucket| bucket.tokens)
        .sum();

    match (current, previous) {
        (0, 0) => "trend flat".to_owned(),
        (_, 0) => "trend new activity".to_owned(),
        _ => {
            let change = (current as f64 - previous as f64) / previous as f64 * 100.0;
            format!("trend {change:+.0}% vs previous period")
        }
    }
}

fn activity_bar(tokens: u64, peak: u64) -> String {
    if 0 == tokens || 0 == peak {
        return String::new();
    }
    let width = ((tokens as f64 / peak as f64) * 20.0).ceil() as usize;
    "█".repeat(width.max(1))
}

fn format_duration(seconds: Option<u64>) -> String {
    let Some(seconds) = seconds else {
        return "—".to_owned();
    };
    let hours = seconds / 3_600;
    let minutes = seconds % 3_600 / 60;
    let seconds = seconds % 60;
    if 0 < hours {
        format!("{hours}h {minutes}m {seconds}s")
    } else if 0 < minutes {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn format_days(days: Option<u64>) -> String {
    days.map(|days| format!("{days} days"))
        .unwrap_or_else(|| "—".to_owned())
}

struct Spinner {
    stop_sender: Option<mpsc::Sender<()>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Spinner {
    fn disabled() -> Self {
        Self {
            stop_sender: None,
            handle: None,
        }
    }

    fn start(message: &'static str) -> Self {
        if !io::stderr().is_terminal() || terminal_is_dumb() {
            return Self {
                stop_sender: None,
                handle: None,
            };
        }

        let (stop_sender, stop_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut frame = 0;
            loop {
                eprint!("\r{} {message}", FRAMES[frame]);
                let _ = io::stderr().flush();
                match stop_receiver.recv_timeout(Duration::from_millis(80)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        frame = (frame + 1) % FRAMES.len();
                    }
                }
            }
            eprint!("\r{}\r", " ".repeat(message.chars().count() + 2));
            let _ = io::stderr().flush();
        });

        Self {
            stop_sender: Some(stop_sender),
            handle: Some(handle),
        }
    }

    fn stop(&mut self) {
        if let Some(sender) = self.stop_sender.take() {
            let _ = sender.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
    }
}

struct QueryDirectory {
    path: PathBuf,
}

struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

impl QueryDirectory {
    fn create(paths: &Paths, name: &str) -> Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system clock error: {error}"))?
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "codex-switcher-{}-{name}-{nonce}",
            std::process::id()
        ));
        secure_dir(&path)?;
        secure_copy(&paths.profile(name), &path.join("auth.json"))?;
        if paths.config().exists() {
            secure_copy(&paths.config(), &path.join("config.toml"))?;
        } else {
            atomic_write(
                &path.join("config.toml"),
                format!("{CREDENTIAL_SETTING} = \"file\"\n").as_bytes(),
                0o600,
            )?;
        }
        Ok(Self { path })
    }
}

impl Drop for QueryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn query_profile(paths: &Paths, name: &str) -> Result<Usage> {
    validate_name(name)?;
    let query_dir = QueryDirectory::create(paths, name)?;
    let mut child = ChildGuard {
        child: Command::new("codex")
            .arg("app-server")
            .env("CODEX_HOME", &query_dir.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                AppError::authentication(format!("cannot start 'codex app-server': {error}"))
            })?,
    };

    let stdout = child
        .child
        .stdout
        .take()
        .ok_or_else(|| AppError::authentication("cannot read from Codex app-server"))?;
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(|line| line.ok()) {
            if let Ok(message) = serde_json::from_str::<Value>(&line) {
                let _ = sender.send(message);
            }
        }
    });

    let mut stdin = child
        .child
        .stdin
        .take()
        .ok_or_else(|| AppError::authentication("cannot write to Codex app-server"))?;
    for message in [
        json!({
            "method": "initialize",
            "id": 0,
            "params": {
                "clientInfo": {
                    "name": "codex_switcher",
                    "title": "Codex Switcher",
                    "version": VERSION
                }
            }
        }),
        json!({ "method": "initialized", "params": {} }),
        json!({ "method": "account/read", "id": 1, "params": { "refreshToken": false } }),
        json!({ "method": "account/rateLimits/read", "id": 2, "params": {} }),
        json!({ "method": "account/usage/read", "id": 3, "params": {} }),
    ] {
        serde_json::to_writer(&mut stdin, &message)
            .map_err(|error| format!("cannot encode app-server request: {error}"))?;
        stdin
            .write_all(b"\n")
            .map_err(|error| format!("cannot send app-server request: {error}"))?;
    }
    stdin
        .flush()
        .map_err(|error| format!("cannot flush app-server request: {error}"))?;

    let deadline = Instant::now() + QUERY_TIMEOUT;
    let mut account = None;
    let mut rate_limits = None;
    let mut token_usage = None;
    let mut token_usage_finished = false;
    while account.is_none() || rate_limits.is_none() || !token_usage_finished {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match receiver.recv_timeout(remaining) {
            Ok(message) if message.get("id").and_then(Value::as_i64) == Some(1) => {
                account = Some(response_result(message, "account/read")?);
            }
            Ok(message) if message.get("id").and_then(Value::as_i64) == Some(2) => {
                rate_limits = Some(response_result(message, "account/rateLimits/read")?);
            }
            Ok(message) if message.get("id").and_then(Value::as_i64) == Some(3) => {
                token_usage = response_result(message, "account/usage/read").ok();
                token_usage_finished = true;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }

    drop(stdin);
    child.stop();
    let _ = reader.join();

    let account = account.ok_or_else(|| AppError::timeout("account query timed out"))?;
    let rate_limits = rate_limits.ok_or_else(|| AppError::timeout("rate-limit query timed out"))?;
    let mut usage = parse_usage(&account, &rate_limits);
    if let Some(token_usage) = token_usage.as_ref() {
        apply_token_usage(&mut usage, token_usage);
    }

    let refreshed_auth = query_dir.path.join("auth.json");
    if validate_auth_file(&refreshed_auth).is_ok() {
        secure_copy(&refreshed_auth, &paths.profile(name))?;
    }
    Ok(usage)
}

fn response_result(message: Value, method: &str) -> Result<Value> {
    if let Some(error) = message.get("error") {
        let detail = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown app-server error");
        return Err(AppError::authentication(format!(
            "{method} failed: {detail}"
        )));
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| AppError::authentication(format!("{method} returned no result")))
}

fn parse_usage(account: &Value, rate_limits: &Value) -> Usage {
    let account_type = account
        .pointer("/account/type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let plan = if "apiKey" == account_type {
        "API".to_owned()
    } else {
        account
            .pointer("/account/planType")
            .and_then(Value::as_str)
            .or_else(|| find_plan_type(rate_limits))
            .map(|value| value.to_ascii_uppercase())
            .unwrap_or_else(|| "—".to_owned())
    };

    let mut windows = Vec::new();
    if let Some(buckets) = rate_limits
        .get("rateLimitsByLimitId")
        .and_then(Value::as_object)
    {
        for bucket in buckets.values() {
            collect_windows(bucket, &mut windows);
        }
    } else if let Some(bucket) = rate_limits.get("rateLimits") {
        collect_windows(bucket, &mut windows);
    }

    Usage {
        plan,
        five_hours: find_window(&windows, FIVE_HOURS_MINS),
        weekly: find_window(&windows, WEEK_MINS),
        lifetime_tokens: None,
        peak_daily_tokens: None,
        longest_running_turn_sec: None,
        current_streak_days: None,
        longest_streak_days: None,
        daily_usage: Vec::new(),
        daily_usage_available: false,
        reset_credits: rate_limits
            .pointer("/rateLimitResetCredits/availableCount")
            .and_then(Value::as_u64),
    }
}

fn apply_token_usage(usage: &mut Usage, token_usage: &Value) {
    usage.lifetime_tokens = summary_u64(token_usage, "lifetimeTokens");
    usage.peak_daily_tokens = summary_u64(token_usage, "peakDailyTokens");
    usage.longest_running_turn_sec = summary_u64(token_usage, "longestRunningTurnSec");
    usage.current_streak_days = summary_u64(token_usage, "currentStreakDays");
    usage.longest_streak_days = summary_u64(token_usage, "longestStreakDays");
    let daily_usage = token_usage
        .get("dailyUsageBuckets")
        .and_then(Value::as_array);
    usage.daily_usage_available = daily_usage.is_some();
    usage.daily_usage = daily_usage
        .into_iter()
        .flatten()
        .filter_map(|bucket| {
            let date = bucket
                .get("startDate")
                .and_then(Value::as_str)
                .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())?;
            let tokens = bucket.get("tokens").and_then(Value::as_u64)?;
            Some(DailyUsage { date, tokens })
        })
        .collect();
}

fn summary_u64(token_usage: &Value, field: &str) -> Option<u64> {
    token_usage
        .get("summary")
        .and_then(|summary| summary.get(field))
        .and_then(Value::as_u64)
}

fn collect_windows(bucket: &Value, windows: &mut Vec<(u64, Window)>) {
    for key in ["primary", "secondary"] {
        let Some(value) = bucket.get(key) else {
            continue;
        };
        let Some(duration) = value.get("windowDurationMins").and_then(Value::as_u64) else {
            continue;
        };
        let Some(used_percent) = value.get("usedPercent").and_then(Value::as_f64) else {
            continue;
        };
        windows.push((
            duration,
            Window {
                used_percent,
                duration_mins: duration,
                resets_at: value.get("resetsAt").and_then(Value::as_i64),
            },
        ));
    }
}

fn find_window(windows: &[(u64, Window)], duration: u64) -> Option<Window> {
    windows
        .iter()
        .find(|(window_duration, _)| duration == *window_duration)
        .map(|(_, window)| window.clone())
}

fn find_plan_type(value: &Value) -> Option<&str> {
    value
        .get("planType")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("rateLimits")
                .and_then(|bucket| bucket.get("planType"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            value
                .get("rateLimitsByLimitId")
                .and_then(Value::as_object)
                .and_then(|buckets| {
                    buckets
                        .values()
                        .find_map(|bucket| bucket.get("planType").and_then(Value::as_str))
                })
        })
}

fn format_left(window: Option<&Window>) -> String {
    window
        .map(|window| format_percent((100.0 - window.used_percent).clamp(0.0, 100.0)))
        .unwrap_or_else(|| "—".to_owned())
}

fn format_percent(value: f64) -> String {
    format!("{value:.0}%")
}

fn format_reset(window: Option<&Window>) -> String {
    window
        .and_then(|window| window.resets_at)
        .and_then(|resets_at| Local.timestamp_opt(resets_at, 0).single())
        .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".to_owned())
}

fn format_tokens(tokens: Option<u64>) -> String {
    tokens
        .map(format_token_count)
        .unwrap_or_else(|| "—".to_owned())
}

fn format_token_count(tokens: u64) -> String {
    const UNITS: [(u64, u64, &str); 4] = [
        (1_000_000_000_000, 999_950_000_000, "T"),
        (1_000_000_000, 999_950_000, "B"),
        (1_000_000, 999_950, "M"),
        (1_000, 1_000, "K"),
    ];

    for (divisor, threshold, suffix) in UNITS {
        if threshold <= tokens {
            let tenths = (u128::from(tokens) * 10 + u128::from(divisor) / 2) / u128::from(divisor);
            let whole = tenths / 10;
            let fraction = tenths % 10;
            return if 0 == fraction {
                format!("{whole}{suffix}")
            } else {
                format!("{whole},{fraction}{suffix}")
            };
        }
    }

    tokens.to_string()
}

fn value_or_dash(value: &str) -> String {
    if value.is_empty() {
        "—".to_owned()
    } else {
        value.to_owned()
    }
}

fn print_table(rows: &[Vec<String>]) {
    const HEADERS: [&str; 6] = [
        "PROFILE",
        "PLAN",
        "5H LEFT",
        "5H RESET",
        "WEEKLY LEFT",
        "WEEKLY RESET",
    ];
    let mut widths: Vec<usize> = HEADERS.iter().map(|header| header.len()).collect();
    for row in rows {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(value.chars().count());
        }
    }
    let colors = colors_enabled();
    println!();
    print_table_row(&HEADERS.map(str::to_owned), &widths, true, colors);
    for row in rows {
        print_table_row(row, &widths, false, colors);
    }
    println!();
}

fn print_table_row(row: &[String], widths: &[usize], header: bool, colors: bool) {
    for (index, value) in row.iter().enumerate() {
        if 0 < index {
            print!("  ");
        }
        let cell = if index + 1 == row.len() {
            value.to_owned()
        } else {
            format!("{value:<width$}", width = widths[index])
        };
        print!("{}", colorize_cell(&cell, value, index, header, colors));
    }
    println!();
}

fn colors_enabled() -> bool {
    io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none() && !terminal_is_dumb()
}

fn stderr_colors_enabled() -> bool {
    io::stderr().is_terminal() && env::var_os("NO_COLOR").is_none() && !terminal_is_dumb()
}

fn terminal_is_dumb() -> bool {
    env::var("TERM").is_ok_and(|term| "dumb" == term)
}

fn print_switch_confirmation(name: &str) {
    for line in switch_confirmation(name, colors_enabled()) {
        println!("{line}");
    }
}

fn switch_confirmation(name: &str, colors: bool) -> [String; 2] {
    [
        profile_status("Switched to profile ", name, ".", ANSI_GREEN, colors),
        styled_message(
            "Restart the Codex chat in your IDE if it was already open.",
            ANSI_HINT,
            colors,
        ),
    ]
}

fn print_success(message: &str) {
    println!("{}", styled_message(message, ANSI_GREEN, colors_enabled()));
}

fn print_hint(message: &str) {
    println!("{}", styled_message(message, ANSI_HINT, colors_enabled()));
}

fn print_warning_stdout(message: &str) {
    println!("{}", styled_message(message, ANSI_YELLOW, colors_enabled()));
}

fn print_error(message: &str, allow_colors: bool) {
    eprintln!(
        "{}",
        styled_message(
            &format!("error: {message}"),
            ANSI_RED,
            allow_colors && stderr_colors_enabled(),
        )
    );
}

fn print_warning(message: &str) {
    eprintln!(
        "{}",
        styled_message(
            &format!("warning: {message}"),
            ANSI_YELLOW,
            stderr_colors_enabled(),
        )
    );
}

fn print_profile_status(prefix: &str, name: &str, suffix: &str, color: &str) {
    println!(
        "{}",
        profile_status(prefix, name, suffix, color, colors_enabled())
    );
}

fn profile_status(prefix: &str, name: &str, suffix: &str, color: &str, colors: bool) -> String {
    if colors {
        format!("{color}{prefix}{ANSI_GOLD}'{name}'{ANSI_RESET}{color}{suffix}{ANSI_RESET}")
    } else {
        format!("{prefix}'{name}'{suffix}")
    }
}

fn styled_message(message: &str, color: &str, colors: bool) -> String {
    if colors {
        format!("{color}{message}{ANSI_RESET}")
    } else {
        message.to_owned()
    }
}

fn colorize_cell(cell: &str, value: &str, index: usize, header: bool, colors: bool) -> String {
    if !colors {
        return cell.to_owned();
    }

    let style = if header {
        "\x1b[1;36m"
    } else {
        match index {
            0 if value.starts_with("* ") => "\x1b[1;33m",
            1 => "\x1b[35m",
            2 | 4 => remaining_color(value),
            3 | 5 => "\x1b[37m",
            _ => "",
        }
    };
    if style.is_empty() {
        cell.to_owned()
    } else {
        format!("{style}{cell}\x1b[0m")
    }
}

fn remaining_color(value: &str) -> &'static str {
    let remaining = value.trim_end_matches('%').parse::<f64>().ok();
    match remaining {
        Some(value) if 50.0 <= value => "\x1b[32m",
        Some(value) if 20.0 <= value => "\x1b[33m",
        Some(_) => "\x1b[31m",
        None => "\x1b[2m",
    }
}

fn current(paths: &Paths, json_output: bool, format: CurrentFormat) -> Result<()> {
    let active = read_active(paths)?;
    if json_output {
        return print_json(&CurrentJson {
            schema_version: SCHEMA_VERSION,
            profile: active,
        });
    }
    match (format, active) {
        (_, Some(name)) if CurrentFormat::Prompt == format => println!("{name}"),
        (CurrentFormat::Prompt, None) => {}
        (_, Some(name)) => println!("{}", styled_message(&name, ANSI_GOLD, colors_enabled())),
        (_, None) => print_warning_stdout("No managed profile is active."),
    }
    Ok(())
}

#[derive(Serialize)]
struct CurrentJson {
    schema_version: u64,
    profile: Option<String>,
}

fn remove(paths: &Paths, name: &str, force: bool) -> Result<()> {
    validate_name(name)?;
    let _lock = Lock::acquire(paths)?;
    let profile = paths.profile(name);
    if !profile.exists() {
        return Err(AppError::profile(format!(
            "profile '{name}' does not exist"
        )));
    }
    let is_active = read_active(paths)?.as_deref() == Some(name);
    if is_active && !force {
        return Err(AppError::profile(format!(
            "profile '{name}' is active; switch first or pass --force"
        )));
    }
    fs::remove_file(&profile)
        .map_err(|error| format!("cannot remove profile '{name}': {error}"))?;
    if is_active {
        write_active(paths, None)?;
    }
    print_profile_status(
        "Removed profile ",
        name,
        ". The active Codex login was not logged out.",
        ANSI_GREEN,
    );
    Ok(())
}

fn doctor(paths: &Paths, json_output: bool) -> Result<()> {
    let config = fs::read_to_string(paths.config()).unwrap_or_default();
    let configured = has_file_credentials(&config);

    let auth_ok = validate_auth_file(&paths.auth()).is_ok();

    let active = read_active(paths)?;
    let profile_ok = active
        .as_deref()
        .is_some_and(|name| validate_auth_file(&paths.profile(name)).is_ok());

    let codex_ok = Command::new("codex")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    let checks = [
        DoctorCheck {
            name: "file_credentials",
            ok: configured,
            message: "credential storage is set to 'file'",
        },
        DoctorCheck {
            name: "active_auth",
            ok: auth_ok,
            message: "active auth.json exists and is non-empty",
        },
        DoctorCheck {
            name: "active_profile",
            ok: profile_ok,
            message: "active profile points to a valid saved login",
        },
        DoctorCheck {
            name: "codex_executable",
            ok: codex_ok,
            message: "codex executable is available",
        },
    ];
    let healthy = checks.iter().all(|check| check.ok);

    if json_output {
        print_json(&DoctorJson {
            schema_version: SCHEMA_VERSION,
            healthy,
            checks,
        })?;
    } else {
        for check in &checks {
            report(check.ok, check.message);
        }
    }

    if healthy {
        if !json_output {
            print_success("Everything looks good.");
        }
        Ok(())
    } else {
        Err(AppError::initialization("one or more checks failed"))
    }
}

#[derive(Serialize)]
struct DoctorJson {
    schema_version: u64,
    healthy: bool,
    checks: [DoctorCheck; 4],
}

#[derive(Serialize)]
struct DoctorCheck {
    name: &'static str,
    ok: bool,
    message: &'static str,
}

fn report(ok: bool, message: &str) {
    let marker = if ok { "[ok]" } else { "[!!]" };
    let color = if ok { ANSI_GREEN } else { ANSI_RED };
    if colors_enabled() {
        println!("{color}{marker}{ANSI_RESET} {message}");
    } else {
        println!("{marker} {message}");
    }
}

fn save_active(paths: &Paths) -> Result<()> {
    let Some(name) = read_active(paths)? else {
        return Ok(());
    };
    if validate_auth_file(&paths.auth()).is_ok() {
        secure_copy(&paths.auth(), &paths.profile(&name))?;
    }
    Ok(())
}

fn restore_auth(paths: &Paths, content: Option<&[u8]>) -> Result<()> {
    match content {
        Some(bytes) => atomic_write(&paths.auth(), bytes, 0o600),
        None => remove_if_exists(&paths.auth()),
    }
}

fn ensure_initialized(paths: &Paths) -> Result<()> {
    let config = fs::read_to_string(paths.config()).unwrap_or_default();
    if !has_file_credentials(&config) {
        return Err(AppError::initialization("run 'codex-switcher init' first"));
    }
    secure_dir(&paths.profiles())
}

fn read_active(paths: &Paths) -> Result<Option<String>> {
    match fs::read_to_string(paths.state()) {
        Ok(value) => {
            let name = value.trim();
            if name.is_empty() {
                Ok(None)
            } else {
                validate_name(name).map_err(|_| {
                    AppError::profile("active profile state contains an invalid profile name")
                })?;
                Ok(Some(name.to_owned()))
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AppError::profile(format!(
            "cannot read active profile: {error}"
        ))),
    }
}

fn write_active(paths: &Paths, name: Option<&str>) -> Result<()> {
    match name {
        Some(name) => atomic_write(&paths.state(), format!("{name}\n").as_bytes(), 0o600),
        None => remove_if_exists(&paths.state()),
    }
}

fn profile_names(paths: &Paths) -> Result<Vec<String>> {
    let entries = match fs::read_dir(paths.profiles()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(AppError::profile(format!("cannot list profiles: {error}"))),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read profile entry: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("json")
            && let Some(stem) = path.file_stem().and_then(|value| value.to_str())
            && validate_name(stem).is_ok()
        {
            names.push(stem.to_owned());
        }
    }
    Ok(names)
}

fn validate_auth_file(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path).map_err(|error| {
        AppError::authentication(format!("cannot read {}: {error}", path.display()))
    })?;
    if !metadata.is_file() || 0 == metadata.len() {
        return Err(AppError::authentication(format!(
            "{} is not a non-empty file",
            path.display()
        )));
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    let valid_length = !name.is_empty() && name.len() <= 64;
    let valid_start = name
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric);
    let valid_chars = name
        .bytes()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, b'-' | b'_'));
    if valid_length && valid_start && valid_chars {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorKind::Arguments,
            "profile name must be 1-64 characters: letters, numbers, '-' or '_', starting with a letter or number",
        ))
    }
}

fn secure_copy(source: &Path, destination: &Path) -> Result<()> {
    let content =
        fs::read(source).map_err(|error| format!("cannot read {}: {error}", source.display()))?;
    atomic_write(destination, &content, 0o600)
}

fn atomic_write(path: &Path, content: &[u8], mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    secure_dir(parent)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    let temporary = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| format!("cannot create {}: {error}", temporary.display()))?;
        file.write_all(content)
            .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync {}: {error}", temporary.display()))?;
        set_mode(&temporary, mode)?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("cannot replace {}: {error}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn secure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    set_mode(path, 0o700)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| AppError::runtime(format!("cannot secure {}: {error}", path.display())))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::runtime(format!(
            "cannot remove {}: {error}",
            path.display()
        ))),
    }
}

fn completion(paths: &Paths, shell: SupportedShell) -> Result<()> {
    let mut names = profile_names(paths)?;
    names.sort_unstable();
    let command = Cli::command();
    let mut command = add_profile_completions(command, "use", &names);
    command = add_profile_completions(command, "stats", &names);
    command = add_profile_completions(command, "remove", &names);
    let name = command.get_name().to_owned();
    let generator: Shell = shell.into();
    generate(generator, &mut command, name, &mut io::stdout());
    if SupportedShell::Fish == shell && !names.is_empty() {
        println!(
            "complete -c codex-switcher -n '__fish_seen_subcommand_from use switch stats remove rm' -f -a '{}'",
            names.join(" ")
        );
    }
    Ok(())
}

fn add_profile_completions(
    command: clap::Command,
    subcommand_name: &'static str,
    names: &[String],
) -> clap::Command {
    let values = names.to_vec();
    command.mut_subcommand(subcommand_name, move |subcommand| {
        subcommand.mut_arg("name", move |argument| {
            argument.value_parser(clap::builder::PossibleValuesParser::new(values))
        })
    })
}

fn shell_function(shell: SupportedShell) -> Result<()> {
    let function = match shell {
        SupportedShell::Bash | SupportedShell::Zsh => {
            "cs() {\n  if [ \"$#\" -eq 0 ]; then\n    codex-switcher current\n  else\n    codex-switcher use \"$@\"\n  fi\n}"
        }
        SupportedShell::Fish => {
            "function cs\n  if test (count $argv) -eq 0\n    codex-switcher current\n  else\n    codex-switcher use $argv\n  end\nend"
        }
    };
    println!("{function}");
    Ok(())
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    serde_json::to_writer(&mut handle, value)
        .map_err(|error| AppError::runtime(format!("cannot encode JSON output: {error}")))?;
    handle
        .write_all(b"\n")
        .map_err(|error| AppError::runtime(format!("cannot write JSON output: {error}")))
}

fn string_value(value: &str) -> Option<String> {
    if value.is_empty() || "—" == value {
        None
    } else {
        Some(value.to_owned())
    }
}

fn iso_timestamp(timestamp: Option<i64>) -> Option<String> {
    timestamp
        .and_then(DateTime::<Utc>::from_timestamp_secs)
        .map(|value| value.to_rfc3339())
}

fn configure_file_credentials(config: &str) -> String {
    let mut output = String::new();
    let mut replaced = false;
    let mut in_table = false;
    for line in config.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_table = true;
        }
        if !in_table && is_credential_assignment(trimmed) {
            output.push_str(CREDENTIAL_SETTING);
            output.push_str(" = \"file\"");
            if let Some((_, comment)) = line.split_once('#') {
                output.push_str(" #");
                output.push_str(comment);
            }
            output.push('\n');
            replaced = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !replaced {
        output.insert_str(0, &format!("{CREDENTIAL_SETTING} = \"file\"\n"));
    }
    output
}

fn has_file_credentials(config: &str) -> bool {
    let mut in_table = false;
    for line in config.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_table = true;
        }
        if !in_table && is_credential_assignment(trimmed) {
            let value = trimmed
                .split_once('=')
                .map(|(_, value)| value)
                .unwrap_or_default();
            return value
                .split('#')
                .next()
                .unwrap_or_default()
                .trim()
                .trim_matches('\"')
                == "file";
        }
    }
    false
}

fn is_credential_assignment(line: &str) -> bool {
    line.strip_prefix(CREDENTIAL_SETTING)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

fn parse_period(value: &str, maximum_days: u64) -> Result<u64> {
    let number = value.strip_suffix('d').ok_or_else(|| {
        AppError::new(
            ErrorKind::Arguments,
            "period must use the form Nd, for example 30d",
        )
    })?;
    let days = number.parse::<u64>().map_err(|_| {
        AppError::new(
            ErrorKind::Arguments,
            "period must use the form Nd, for example 30d",
        )
    })?;
    if 0 == days || maximum_days < days {
        return Err(AppError::new(
            ErrorKind::Arguments,
            format!("period must be between 1d and {maximum_days}d"),
        ));
    }
    Ok(days)
}

fn parse_period_value(value: &str) -> std::result::Result<u64, String> {
    parse_period(value, 365).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_names_are_restricted() {
        assert!(validate_name("personal").is_ok());
        assert!(validate_name("work-2_test").is_ok());
        assert!(validate_name("../secrets").is_err());
        assert!(validate_name(" space").is_err());
        assert!(validate_name("").is_err());
    }

    #[test]
    fn adds_credential_setting_before_tables() {
        let config = "model = \"gpt\"\n[projects.foo]\ntrusted = true\n";
        let updated = configure_file_credentials(config);
        assert!(updated.starts_with("cli_auth_credentials_store = \"file\"\n"));
        assert!(has_file_credentials(&updated));
    }

    #[test]
    fn replaces_existing_root_credential_setting() {
        let config = "cli_auth_credentials_store = \"keyring\" # old\nmodel = \"gpt\"\n";
        let updated = configure_file_credentials(config);
        assert_eq!(
            updated,
            "cli_auth_credentials_store = \"file\" # old\nmodel = \"gpt\"\n"
        );
        assert!(has_file_credentials(&updated));
    }

    #[test]
    fn ignores_similarly_named_setting() {
        let config = "cli_auth_credentials_store_backup = \"file\"\n";
        assert!(!has_file_credentials(config));
    }

    #[test]
    fn parses_five_hour_and_weekly_windows() {
        let account = json!({
            "account": { "type": "chatgpt", "planType": "plus" }
        });
        let limits = json!({
            "rateLimits": {
                "primary": {
                    "usedPercent": 25,
                    "windowDurationMins": 300,
                    "resetsAt": 1_893_456_000
                },
                "secondary": {
                    "usedPercent": 40,
                    "windowDurationMins": 10_080,
                    "resetsAt": 1_893_888_000
                }
            }
        });

        let usage = parse_usage(&account, &limits);
        assert_eq!(usage.plan, "PLUS");
        assert_eq!(usage.five_hours.expect("5h window").used_percent, 25.0);
        assert_eq!(usage.weekly.expect("weekly window").used_percent, 40.0);
    }

    #[test]
    fn does_not_invent_missing_limits_for_api_keys() {
        let usage = parse_usage(
            &json!({ "account": { "type": "apiKey" } }),
            &json!({ "rateLimits": null }),
        );
        assert_eq!(usage.plan, "API");
        assert!(usage.five_hours.is_none());
        assert!(usage.weekly.is_none());
    }

    #[test]
    fn parses_and_formats_lifetime_token_usage() {
        let token_usage = json!({
            "summary": {
                "lifetimeTokens": 1_500_000,
                "peakDailyTokens": 45_678,
                "longestRunningTurnSec": 540,
                "currentStreakDays": 8,
                "longestStreakDays": 14
            },
            "dailyUsageBuckets": [
                { "startDate": "2026-09-01", "tokens": 12_345 },
                { "startDate": "invalid", "tokens": 99 }
            ]
        });
        let mut usage = Usage::default();
        apply_token_usage(&mut usage, &token_usage);
        assert_eq!(usage.lifetime_tokens, Some(1_500_000));
        assert_eq!(usage.peak_daily_tokens, Some(45_678));
        assert_eq!(usage.longest_running_turn_sec, Some(540));
        assert_eq!(usage.current_streak_days, Some(8));
        assert_eq!(usage.longest_streak_days, Some(14));
        assert_eq!(usage.daily_usage.len(), 1);
        assert_eq!(usage.daily_usage[0].tokens, 12_345);
        assert_eq!(format_token_count(100), "100");
        assert_eq!(format_token_count(900), "900");
        assert_eq!(format_token_count(1_000), "1K");
        assert_eq!(format_token_count(654_400), "654,4K");
        assert_eq!(format_token_count(1_500_000), "1,5M");
        assert_eq!(format_token_count(4_200_000), "4,2M");
        assert_eq!(format_token_count(999_999), "1M");
        assert_eq!(format_tokens(None), "—");
    }

    #[test]
    fn parses_stats_period() {
        assert_eq!(parse_period("30d", 365), Ok(30));
        assert!(parse_period("0d", 365).is_err());
        assert!(parse_period("366d", 365).is_err());
        assert!(parse_period("30", 365).is_err());
    }

    #[test]
    fn calculates_activity_trend_and_bar() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 2).expect("valid date");
        let usage = vec![
            DailyUsage {
                date: today - ChronoDuration::days(8),
                tokens: 100,
            },
            DailyUsage {
                date: today,
                tokens: 150,
            },
        ];
        assert_eq!(
            format_activity_trend(&usage, today, 7),
            "trend +50% vs previous period"
        );
        assert_eq!(activity_bar(50, 100), "██████████");
        assert_eq!(format_duration(Some(3_661)), "1h 1m 1s");
    }

    #[test]
    fn does_not_treat_a_missing_daily_bucket_as_zero_usage() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 2).expect("valid date");
        let yesterday = DailyUsage {
            date: today - ChronoDuration::days(1),
            tokens: 12_345,
        };
        let zero_today = DailyUsage {
            date: today,
            tokens: 0,
        };

        assert_eq!(daily_tokens_for_date(&[yesterday], today), None);
        assert_eq!(daily_tokens_for_date(&[zero_today], today), Some(0));
    }

    #[test]
    fn styles_statistics_without_changing_plain_layout() {
        assert_eq!(
            format_stat_row("Profile", "work", ANSI_GOLD, false),
            "Profile             work"
        );
        let colored = format_stat_row("Profile", "work", ANSI_GOLD, true);
        assert!(colored.starts_with("\x1b[2;37mProfile             \x1b[0m"));
        assert!(colored.ends_with("\x1b[1;33mwork\x1b[0m"));
        assert_eq!(trend_color("trend +20% vs previous period"), ANSI_GREEN);
        assert_eq!(trend_color("trend -20% vs previous period"), ANSI_RED);
        assert_eq!(trend_color("trend flat"), ANSI_HINT);
    }

    #[test]
    fn colors_table_cells_by_meaning() {
        assert!(
            colorize_cell("* personal", "* personal", 0, false, true).starts_with("\x1b[1;33m")
        );
        assert!(colorize_cell("PLUS", "PLUS", 1, false, true).starts_with("\x1b[35m"));
        assert!(
            colorize_cell("2026-09-02 12:00", "2026-09-02 12:00", 3, false, true)
                .starts_with("\x1b[37m")
        );
    }

    #[test]
    fn styles_switch_confirmation_without_changing_plain_output() {
        let colored = switch_confirmation("priv", true);
        assert!(colored[0].starts_with("\x1b[32mSwitched to profile "));
        assert!(colored[0].contains("\x1b[1;33m'priv'"));
        assert!(colored[1].starts_with("\x1b[2;37mRestart"));

        assert_eq!(
            switch_confirmation("priv", false),
            [
                "Switched to profile 'priv'.",
                "Restart the Codex chat in your IDE if it was already open."
            ]
        );
    }

    #[test]
    fn styles_status_messages_without_changing_plain_output() {
        assert_eq!(
            profile_status("Removed profile ", "priv", ".", ANSI_GREEN, false),
            "Removed profile 'priv'."
        );
        assert_eq!(
            profile_status("Removed profile ", "priv", ".", ANSI_GREEN, true),
            "\x1b[32mRemoved profile \x1b[1;33m'priv'\x1b[0m\x1b[32m.\x1b[0m"
        );
        assert_eq!(
            styled_message("Everything looks good.", ANSI_GREEN, true),
            "\x1b[32mEverything looks good.\x1b[0m"
        );
        assert_eq!(
            styled_message("Everything looks good.", ANSI_GREEN, false),
            "Everything looks good."
        );
    }
}
