# ADR-0013: Launch update check and macOS application menu

Status: Accepted — issue #145

## Context

Users need to discover newer ClusterDeck releases at launch and from the macOS menu
bar. The update action belongs in the ClusterDeck application menu at the top of
the screen. It opens a release page; it does not install software.

## Decision

- Check GitHub's latest release asynchronously at launch. Failures are silent.
- Add **Check for Updates…** after About in the native application menu. Preserve
  Tauri's standard menus, including Edit shortcuts and Window actions. Menu setup
  failures are logged and do not prevent startup.
- For a newer stable numeric `vX.Y.Z` release, change the item to
  **Update Available (vX.Y.Z)…** and cache its URL in memory. Clicking opens the
  GitHub release page in the default browser. Without a cached URL, clicking checks
  again and shows a native up-to-date or failure alert when appropriate.
- Use system `/usr/bin/curl` through `CommandRunner::run_bounded`, with an 8-second
  curl timeout, 10-second process timeout and 256 KiB output cap per stream. Reject
  invalid JSON/URLs and nonnumeric version tags; ignore draft and prerelease releases.
  Only safe URLs exactly matching
  `https://github.com/dasomel/clusterdeck/releases/tag/<tag_name>` for a numeric
  `vX.Y.Z` tag may be opened.
- Pass alert text as argv to a fixed AppleScript. Alerts dismiss after 60 seconds;
  the runner allows 65 seconds. Lock managed state only to read/write the cached URL;
  concurrent manual checks may run independently.

## Network and privacy

The normal launch check makes one unauthenticated GET to
`https://api.github.com/repos/dasomel/clusterdeck/releases/latest`. Curl follows
redirects if GitHub returns them. The headers identify only the app and its version
(`User-Agent: ClusterDeck/<version>`); no user, machine, profile, credential or
infrastructure identifiers are sent. Manual checks can make additional requests;
opening a release page starts ordinary browser traffic to GitHub. There is no
telemetry, download or persistent update state. Offline and rate-limit failures
do not interrupt launch.

## Rejected alternative

`tauri-plugin-updater` would require maintaining an updater signing key, CI secrets,
signed update artifacts and a `latest.json` endpoint. That operational cost is not
justified for release discovery. Current builds also use ad-hoc macOS signing:
replacing the app can re-prompt for TCC file-access permissions, which an automatic
installer would not resolve. Users retain control of downloading and replacing it.

## Consequences and escape hatch

Only stable numeric triples are compared, and GitHub availability/rate limits can
prevent discovery. The cached URL lasts only for the process lifetime. Remove the
startup task for manual-only checks, or replace the service with a proper semver
parser/updater after release policy, Developer ID signing, secrets and distribution
infrastructure justify it. No frontend command or automatic installation API is added.
