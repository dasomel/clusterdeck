# ADR-0015: Choose the browser and terminal ClusterDeck opens

Status: Accepted

## Context

"Open in browser" ran `open <url>` (system default browser) and "Open SSH session" / VM shells
ran Terminal.app (`open ssh://<alias>` or AppleScript `do script`). Users with Arc, Ghostty,
iTerm2 and similar want ClusterDeck to use their own apps. Both actions end in a process
launch with a user-controlled value, so this is a security-relevant change (AGENTS.md: process
execution and Tauri command exposure are design changes).

## Decision

**D1 — Fixed catalog, not a free-form app picker.** Settings store only a bundle id from a
compiled-in list (`services/app_settings.rs`): browsers Safari, Chrome, Firefox, Edge, Brave,
Arc, Vivaldi, Opera, Whale, Orion, Zen; terminals Terminal, iTerm2, Ghostty, WezTerm, kitty,
Alacritty, Tabby. Reason: the id ends up in `open -b <id>` or a launch recipe, so only apps with
a known-safe recipe are accepted. Cost: an app outside the list cannot be chosen. Escape hatch:
add a catalog row plus a recipe; the `every_catalog_terminal_has_a_launch_recipe` test fails if
the two drift.

**D2 — Validate at load and at the sink.** `save_settings` rejects ids outside the matching
catalog (a terminal id is not a valid browser id). The file is user-editable, so
`resolve_choice` and `open_terminal` re-check the id right before launching; a tampered id is
treated like a missing app (default + note "Unsupported app in settings ignored"), never an
argv element and not echoed back. Cost: a typo in the file is silent apart from the note.
Escape hatch: Settings overwrites it on next save. A missing or corrupt `settings.json` loads as defaults. Settings
are read in the Rust command, never accepted from the frontend at launch time.

**D3 — Storage.** `~/.clusterdeck/settings.json`, written atomically with mode `0600` through
`ClusterDeckPaths::settings_file()`. It holds bundle ids only (no credentials, no hosts).

**D4 — Unset or uninstalled falls back, with a note.** Unset = previous behavior (system default
browser; `open ssh://<alias>` / Terminal.app). If the saved app is no longer installed the
action still succeeds with the default and the command returns a status note shown as a
warning. Reason: a removed app must not break every "open" until the user visits Settings. The
check runs before launch because AppleScript `tell application "<missing>"` raises a "where is
it?" file dialog instead of failing. Terminal.app and Safari skip the lookup (always present).
Cost: one `mdfind` per launch with a non-default app.

**D5 — Launch recipes per terminal.** Callers pass a `TerminalCommand::{Argv, Script}` (D8), not
a joined string. Terminal and iTerm2 use AppleScript: each Argv token is POSIX single-quoted
(`'` -> `'\''`) and then AppleScript-escaped (`\`, `"`). iTerm2 creates a window
(`create window with default profile`) and types the line with `write text`, so it runs in the
login shell like Terminal's `do script` (`create window ... command` execs without a shell and
breaks multi-statement scripts). Every other terminal gets argv through
`open -n -b <id> --args <flags> <argv...>`: Ghostty/Alacritty `-e`, WezTerm `start --`, kitty
positional, Tabby `run --`, so no shell string is built and tool flags (`--profile`) are not
parsed by the terminal. LaunchServices does not hand over our child `PATH`, so argv[0] is
resolved with `resolve_cli_path` to an absolute path (Homebrew `colima`/`limactl`); an
unresolvable program fails before `open`. The argv is `ssh <alias>` (alias re-validated to
`[A-Za-z0-9._-]`, no leading dash) or the existing validated local-runtime commands. The VM
"context shell" is a `Script`: AppleScript terminals run it natively, argv terminals get
`/bin/sh -c "<script>; exec $SHELL -l"`; the `kubectl` alias is shell-local and is lost there
(documented limitation, `DOCKER_CONTEXT` survives). Uncertainty: the argv recipes follow each
terminal's documented exec syntax but were not exercised against the real apps (not installed
on the dev machine); Tabby's `run` subcommand and its `--` handling are the least certain,
and kitty relies on positional program args via `open --args`. Escape hatch: a wrong recipe
is a one-line change in `process::open_terminal_with`.

**D6 — Browser launch.** `open -b <id> <url>`; the URL is still limited to http/https by
`is_safe_open_url`, which also rejects any control character. All launches go through `CommandRunner`.

**D7 — Scope of "browser".** Endpoint URLs use the chosen browser. The update-check release
page and Finder reveals keep the system default. Reason: smallest change; the release page is
the app's own link, not a cluster endpoint. Escape hatch: pass the saved browser into
`update_check` if users ask for it.

**D8 — `TerminalCommand` enum, control characters rejected.** Reason: the earlier
`argv.join(" ")` meant "argv" or "shell script" depending on the terminal. Cost: callers
choose `Argv` vs `Script` explicitly. Any token or script containing a control character
(`\n`, `\r`, `\t`, NUL, ...) is an error before any process starts, because a newline in an
AppleScript/`write text` line would run as a second command.

**D9 — Detection and fallback wording.** Spotlight hits under `/Volumes/`, `/.Trash*` and
`Backups.backupdb` are ignored (unmounted disks, deleted or backup copies). When no terminal
is chosen or it is gone, an SSH session uses `open ssh://<alias>` (the default SSH handler, an
unchanged behavior), so the note says "the default SSH handler"; VM shells say "Terminal".

## Consequences

A Settings screen (NavRail, `⌘,`) lists only detected apps (Spotlight `mdfind` by bundle id,
then standard install directories). `open_ssh_session`, `open_url_in_browser` and the
local-runtime shell commands return an optional fallback note. Terminal launches for argv
terminals use a new app instance (`-n`), which is the only way macOS passes `--args` to an
already running app.
