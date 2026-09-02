# codex-switcher

`codex-switcher` is a small command-line tool for keeping multiple local
ChatGPT/Codex account profiles and switching between them quickly. It targets
macOS first and works with Codex CLI and Codex integrations that use the same
local authentication cache, including JetBrains IDEs such as PhpStorm.

> [!IMPORTANT]
> This is an independent, unofficial project. It is not affiliated with,
> endorsed by, or supported by OpenAI. Codex, ChatGPT, and OpenAI are trademarks
> of their respective owner.

## Features

- Create named profiles such as `personal` and `work`.
- Switch the active account with one command.
- Use the same active profile in Codex CLI and supported IDE integrations.
- Display the ChatGPT plan, remaining five-hour and weekly quota, and reset
  times for every profile.
- Inspect detailed account activity, streaks, earned resets, usage trends, and
  estimated limit exhaustion for a selected profile.
- Preserve refreshed OAuth credentials when switching profiles.
- Replace the active authentication file atomically.
- Store profile files with restrictive Unix permissions (`0600` for files and
  `0700` for directories).

## Requirements

- macOS. Other Unix-like systems may work but are not currently a primary
  support target.
- [Rust](https://www.rust-lang.org/tools/install) 1.85 or newer.
- Codex CLI available as the `codex` command.
- File-based Codex credential storage. `codex-switcher init` configures it.

## Installation

### Install from a GitHub Release (Apple Silicon)

Download the `v1.2.0` archive and its checksum from
[GitHub Releases](https://github.com/IVDamianVI/codex-switcher/releases):

```sh
curl -fLO https://github.com/IVDamianVI/codex-switcher/releases/download/v1.2.0/codex-switcher-v1.2.0-aarch64-apple-darwin.tar.gz
curl -fLO https://github.com/IVDamianVI/codex-switcher/releases/download/v1.2.0/codex-switcher-v1.2.0-aarch64-apple-darwin.tar.gz.sha256
```

Verify the download before extracting it:

```sh
shasum -a 256 -c codex-switcher-v1.2.0-aarch64-apple-darwin.tar.gz.sha256
```

Install the executable for the current user:

```sh
tar -xzf codex-switcher-v1.2.0-aarch64-apple-darwin.tar.gz
mkdir -p "$HOME/.local/bin"
install -m 755 codex-switcher "$HOME/.local/bin/codex-switcher"
codex-switcher --version
codex-switcher init
```

The expected version is `codex-switcher 1.2.0`. If `codex-switcher` is not
found, add `$HOME/.local/bin` to your shell's `PATH` and open a new terminal.

The current prebuilt release targets Apple Silicon Macs. To check your Mac,
run `uname -m`; `arm64` means Apple Silicon. Intel Mac users can install from
source until an `x86_64-apple-darwin` release is available.

### Install from source

From a local checkout:

```sh
cargo install --path .
codex-switcher init
```

`init` sets `cli_auth_credentials_store = "file"` in
`~/.codex/config.toml`. If the file already exists, its original contents are
backed up once to `~/.codex/config.toml.codex-switcher.bak`.

### Updating an existing installation

To update from a future Release, repeat the Release installation steps with the
new version number. To update from source, run the following from the repository
directory:

```sh
cargo install --path . --force
codex-switcher --version
```

The update replaces only the executable. Existing profiles in
`~/.codex-switcher` and the active Codex authentication file are not removed.

## Quick start

If Codex is currently signed in to your personal account, capture it:

```sh
codex-switcher add personal
```

Log in to another account and save it as `work`:

```sh
codex-switcher login work
```

Use device-code authentication when a browser callback is unavailable:

```sh
codex-switcher login work --device-auth
```

Switch profiles:

```sh
codex-switcher use personal
codex-switcher use work
```

The next Codex CLI process uses the selected profile. If a Codex chat is already
open in PhpStorm, close and reopen the chat. Some integration versions may
require restarting the IDE because a running process can retain credentials in
memory.

## Commands

| Command | Description |
| --- | --- |
| `codex-switcher init` | Configure file-based credentials and create the profile directory. |
| `codex-switcher add <name>` | Save the current Codex login as a profile. |
| `codex-switcher login <name>` | Run the Codex login flow and save the resulting profile. |
| `codex-switcher use <name>` | Make a saved profile active. |
| `codex-switcher list` | List profiles with plan and quota information. |
| `codex-switcher stats <name>` | Show detailed usage statistics for a profile. |
| `codex-switcher current` | Print the active managed profile. |
| `codex-switcher remove <name>` | Remove a saved profile. |
| `codex-switcher doctor` | Check the local setup. |
| `codex-switcher completion <zsh\|bash\|fish>` | Generate shell completion with current profile names. |
| `codex-switcher shell-function <zsh\|bash\|fish>` | Generate the optional `cs` convenience function. |

Use `--force` with `add`, `login`, or `remove` where supported. Run
`codex-switcher help` for the current command synopsis.

Aliases remain available: `capture` for `add`, `switch` for `use`, `ls` for
`list`, and `rm` for `remove`.

## Scripting interface

Text output remains the default. Use `--json` with `list`, `stats`, `current`,
or `doctor` for a versioned machine-readable response. Every response contains
`schema_version`; missing service data is represented by `null`, quantities are
JSON numbers, and timestamps use ISO 8601. JSON mode never emits terminal
colors or a spinner.

For a latency-sensitive prompt, use:

```sh
codex-switcher current --format prompt
```

It prints only the active profile name and a newline, or no output when no
managed profile is active. This path only reads local state and does not start
the Codex App Server.

Public exit codes are stable:

| Code | Meaning |
| ---: | --- |
| `0` | Success |
| `1` | Other runtime or I/O error |
| `2` | Invalid command-line arguments |
| `3` | Missing or invalid initialization/setup |
| `4` | Missing, invalid, or conflicting profile state |
| `5` | Authentication or Codex App Server error |
| `6` | Codex App Server query timeout |
| `7` | Another operation holds the switcher lock |

Generate completion for the current set of profiles and source or install the
result according to your shell's conventions:

```sh
codex-switcher completion zsh
codex-switcher completion bash
codex-switcher completion fish
```

The optional `cs` function is printed, never installed automatically. Review
and source the output in your shell configuration if wanted:

```sh
codex-switcher shell-function zsh
```

After loading it, `cs work` runs `codex-switcher use work`; calling `cs`
without arguments shows the current profile.

## Profile usage and limits

`codex-switcher list` queries profiles in parallel through the stable Codex
app-server account API. It does not change the account active in Codex CLI or
your IDE.

```text
PROFILE     PLAN  5H LEFT  5H RESET          WEEKLY LEFT  WEEKLY RESET
* personal  PLUS  75%      2026-09-01 23:15  60%          2026-09-07 10:00
  work      PRO   90%      2026-09-01 22:40  85%          2026-09-06 08:00
```

Reset times use the computer's local time zone. A dash (`—`) means the service
did not return that field, the exact quota window is unavailable, or the
profile uses API-key billing instead of a ChatGPT subscription quota. In an
interactive terminal, remaining quota is colored green, yellow, or red and a
spinner is shown while profile information is loading. The active profile is
gold, the plan is purple, and reset times use plain white for contrast. Set
`NO_COLOR` to disable colors.

## Detailed account statistics

Use `stats` to inspect one profile without switching to it:

```sh
codex-switcher stats work
codex-switcher stats work --period 30d
```

The default activity period is 7 days. `--period` accepts values from `1d` to
`365d`. The report includes lifetime and today's tokens, the daily record, the
longest turn, current and longest activity streaks, available earned resets,
daily activity bars, and a comparison with the preceding period.

The limit forecast extrapolates the current window's average consumption rate.
It is an estimate, not a value returned by the service, and can change sharply
as usage changes. A dash (`—`) means the App Server did not provide enough data
to calculate or display a metric. In particular, today's token count is shown
only when the service returns a bucket matching the displayed local date; a
missing bucket is not interpreted as zero usage.

## Storage and security

Codex stores file-based credentials in `~/.codex/auth.json`.
`codex-switcher` stores one protected copy per profile in
`~/.codex-switcher/profiles/` and atomically replaces the active cache when
switching. Before a switch, it saves the current cache back to the active
profile so refreshed OAuth credentials are retained.

Authentication files contain secrets equivalent to a signed-in session:

- Never commit, share, print, or paste them into an issue.
- Do not put `~/.codex-switcher` in an untrusted or unencrypted backup.
- Do not switch accounts while another Codex process is actively refreshing
  authentication state.
- Run `codex-switcher doctor` after changing Codex authentication settings.

The program does not print token values. It treats authentication files as
opaque data and only copies them between protected locations. Temporary
directories used to query limits are removed after use.

See [SECURITY.md](SECURITY.md) for reporting vulnerabilities.

## Environment variables

- `CODEX_HOME` changes the Codex data directory. The default is `~/.codex`.
- `CODEX_SWITCHER_HOME` changes the profile directory. The default is
  `~/.codex-switcher`.

For PhpStorm compatibility, prefer the default `CODEX_HOME`. Applications
launched from Finder do not necessarily inherit variables configured in a shell.

## Limitations

- The tool switches local Codex authentication, not a browser session on
  chatgpt.com.
- macOS Keychain entries are not manipulated. Codex does not expose a public
  account-profile interface for those entries, so file-based storage is
  required.
- Already-running Codex or IDE processes may continue using credentials loaded
  before the switch.
- Usage and plan information depends on fields returned by the installed Codex
  app-server and the authenticated account.

## Development

Keep `Cargo.lock` committed because this repository builds an application.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening
a pull request.

## License

Licensed under the [MIT License](LICENSE). Copyright © 2026 Damian Grubecki.
