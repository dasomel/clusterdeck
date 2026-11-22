use crate::services::local_dns::agent::atomic_write;
use crate::services::paths::ClusterDeckPaths;
use crate::services::process::CommandRunner;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A catalog entry: `file` is the `.app` bundle name on disk (differs from `name` for iTerm2).
struct CatalogApp {
    bundle_id: &'static str,
    name: &'static str,
    file: &'static str,
}

const fn app(bundle_id: &'static str, name: &'static str, file: &'static str) -> CatalogApp {
    CatalogApp {
        bundle_id,
        name,
        file,
    }
}

// D1: a fixed catalog, not a free-form app picker. The saved bundle id ends up in
// `open -b <id>` / the terminal launch recipe, so only ids we know a recipe for are accepted.
// Terminals are limited to those with a reliable "run this argv" launch recipe in
// `process::open_terminal`.
const BROWSERS: &[CatalogApp] = &[
    app("com.apple.Safari", "Safari", "Safari"),
    app("com.google.Chrome", "Google Chrome", "Google Chrome"),
    app("org.mozilla.firefox", "Firefox", "Firefox"),
    app("com.microsoft.edgemac", "Microsoft Edge", "Microsoft Edge"),
    app("com.brave.Browser", "Brave", "Brave Browser"),
    app("company.thebrowser.Browser", "Arc", "Arc"),
    app("com.vivaldi.Vivaldi", "Vivaldi", "Vivaldi"),
    app("com.operasoftware.Opera", "Opera", "Opera"),
    app("com.naver.Whale", "Whale", "Whale"),
    app("com.kagi.kagimacOS", "Orion", "Orion"),
    app("app.zen-browser.zen", "Zen", "Zen"),
];

const TERMINALS: &[CatalogApp] = &[
    app("com.apple.Terminal", "Terminal", "Terminal"),
    app("com.googlecode.iterm2", "iTerm2", "iTerm"),
    app("com.mitchellh.ghostty", "Ghostty", "Ghostty"),
    app("com.github.wez.wezterm", "WezTerm", "WezTerm"),
    app("net.kovidgoyal.kitty", "kitty", "kitty"),
    app("org.alacritty", "Alacritty", "Alacritty"),
    app("org.tabby", "Tabby", "Tabby"),
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    /// Bundle id of the browser used for `open_url_in_browser`; `None` = system default.
    #[serde(default)]
    pub browser: Option<String>,
    /// Bundle id of the terminal used for SSH sessions / VM shells; `None` = Terminal.app.
    #[serde(default)]
    pub terminal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppChoice {
    pub bundle_id: String,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct InstalledApps {
    pub browsers: Vec<AppChoice>,
    pub terminals: Vec<AppChoice>,
}

/// Missing or corrupt file -> defaults: a bad settings file must never stop the app from
/// opening URLs or sessions (the corrupt file is simply replaced on the next save).
pub fn load_settings(paths: &ClusterDeckPaths) -> AppSettings {
    std::fs::read_to_string(paths.settings_file())
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

/// Every non-null id must be in the matching catalog; anything else is rejected before it can
/// reach an `open -b` argv.
pub fn validate_settings(settings: &AppSettings) -> Result<(), String> {
    if let Some(id) = settings.browser.as_deref() {
        if !BROWSERS.iter().any(|a| a.bundle_id == id) {
            return Err(format!("unsupported browser: {id}"));
        }
    }
    if let Some(id) = settings.terminal.as_deref() {
        if !TERMINALS.iter().any(|a| a.bundle_id == id) {
            return Err(format!("unsupported terminal: {id}"));
        }
    }
    Ok(())
}

pub fn save_settings(paths: &ClusterDeckPaths, settings: &AppSettings) -> Result<(), String> {
    validate_settings(settings)?;
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    atomic_write(&paths.settings_file(), json.as_bytes(), 0o600)
}

/// Spotlight also indexes mounted volumes, the Trash and Time Machine snapshots; an app found
/// there is not something the user "installed" and may vanish (an unmounted disk) or be a stale
/// copy, so it is never offered or launched.
fn is_excluded_location(path: &str) -> bool {
    path.starts_with("/Volumes/") || path.contains("/.Trash") || path.contains("/Backups.backupdb/")
}

/// Resolves one catalog app to its installed path: Spotlight first (finds apps anywhere), then
/// the standard install directories, because Spotlight can be disabled or not yet indexed.
async fn find_app(
    runner: &dyn CommandRunner,
    entry: &CatalogApp,
    exists: &(dyn Fn(&Path) -> bool + Sync),
) -> Option<String> {
    let query = format!("kMDItemCFBundleIdentifier == '{}'", entry.bundle_id);
    if let Ok(out) = runner.run("mdfind", &[query]).await {
        if out.success {
            if let Some(line) = out.stdout.lines().map(str::trim).find(|l| {
                l.ends_with(".app")
                    && Path::new(l).is_absolute()
                    && !is_excluded_location(l)
                    && exists(Path::new(l))
            }) {
                return Some(line.to_string());
            }
        }
    }
    let mut candidates = vec![
        PathBuf::from(format!("/Applications/{}.app", entry.file)),
        PathBuf::from(format!("/System/Applications/Utilities/{}.app", entry.file)),
        PathBuf::from(format!(
            "/System/Cryptexes/App/System/Applications/{}.app",
            entry.file
        )),
    ];
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(home).join(format!("Applications/{}.app", entry.file)));
    }
    candidates
        .into_iter()
        .find(|p| exists(p))
        .map(|p| p.to_string_lossy().into_owned())
}

async fn detect_group(
    runner: &dyn CommandRunner,
    catalog: &[CatalogApp],
    exists: &(dyn Fn(&Path) -> bool + Sync),
) -> Vec<AppChoice> {
    let mut found = Vec::new();
    for entry in catalog {
        if let Some(path) = find_app(runner, entry, exists).await {
            found.push(AppChoice {
                bundle_id: entry.bundle_id.to_string(),
                name: entry.name.to_string(),
                path,
            });
        }
    }
    found
}

pub async fn list_installed_apps_with(
    runner: &dyn CommandRunner,
    exists: &(dyn Fn(&Path) -> bool + Sync),
) -> InstalledApps {
    InstalledApps {
        browsers: detect_group(runner, BROWSERS, exists).await,
        terminals: detect_group(runner, TERMINALS, exists).await,
    }
}

pub async fn list_installed_apps(runner: &dyn CommandRunner) -> InstalledApps {
    list_installed_apps_with(runner, &|p| p.exists()).await
}

#[derive(Debug, Clone, Copy)]
pub enum AppKind {
    Browser,
    Terminal,
}

/// Sink-side gate for a saved choice. Returns `(app to use, status note)`:
/// - `None` saved -> `(None, None)` (system default / Terminal.app).
/// - id not in the matching catalog -> treated like a missing app (tampered file): default plus
///   a note; the id never reaches argv/AppleScript.
/// - catalog id but the app is gone -> `(None, Some(note))`: D4 falls back to the default and
///   tells the user, instead of failing every "open" until they visit Settings. The check runs
///   before launching because AppleScript `tell application "<missing>"` pops a "where is it?"
///   dialog rather than failing cleanly.
pub async fn resolve_choice(
    runner: &dyn CommandRunner,
    kind: AppKind,
    saved: Option<&str>,
    fallback: &str,
) -> Result<(Option<String>, Option<String>), String> {
    resolve_choice_with(runner, kind, saved, fallback, &|p| p.exists()).await
}

pub(crate) async fn resolve_choice_with(
    runner: &dyn CommandRunner,
    kind: AppKind,
    saved: Option<&str>,
    fallback: &str,
    exists: &(dyn Fn(&Path) -> bool + Sync),
) -> Result<(Option<String>, Option<String>), String> {
    let Some(id) = saved else {
        return Ok((None, None));
    };
    let catalog = match kind {
        AppKind::Browser => BROWSERS,
        AppKind::Terminal => TERMINALS,
    };
    let Some(entry) = catalog.iter().find(|a| a.bundle_id == id) else {
        // The id is deliberately not echoed: it came from an editable file.
        return Ok((
            None,
            Some(format!(
                "Unsupported app in settings ignored; using {fallback}. Change it in Settings."
            )),
        ));
    };
    // Terminal.app and Safari ship with macOS; skip the lookup so a disabled Spotlight can
    // never turn the guaranteed default into a "not installed" fallback.
    if matches!(id, "com.apple.Terminal" | "com.apple.Safari")
        || find_app(runner, entry, exists).await.is_some()
    {
        return Ok((Some(id.to_string()), None));
    }
    Ok((
        None,
        Some(format!(
            "{} is not installed; used {fallback} instead. Change it in Settings.",
            entry.name
        )),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::process::CommandOutput;
    use async_trait::async_trait;
    use std::collections::HashMap;

    /// Answers `mdfind` queries from a bundle-id -> path map.
    struct FakeMdfind(HashMap<&'static str, &'static str>);

    #[async_trait]
    impl CommandRunner for FakeMdfind {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            assert_eq!(bin, "mdfind");
            let stdout = self
                .0
                .iter()
                .find(|(id, _)| args[0].contains(&format!("'{id}'")))
                .map(|(_, p)| format!("{p}\n"))
                .unwrap_or_default();
            Ok(CommandOutput {
                stdout,
                stderr: String::new(),
                success: true,
            })
        }
    }

    fn temp_paths(tag: &str) -> ClusterDeckPaths {
        let dir = std::env::temp_dir().join(format!(
            "clusterdeck-settings-test-{tag}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        ClusterDeckPaths::at(dir)
    }

    #[test]
    fn load_returns_default_when_file_missing() {
        assert_eq!(
            load_settings(&temp_paths("missing")),
            AppSettings::default()
        );
    }

    #[test]
    fn load_returns_default_for_corrupt_file() {
        let paths = temp_paths("corrupt");
        std::fs::create_dir_all(&paths.base).unwrap();
        std::fs::write(paths.settings_file(), "{not json").unwrap();
        assert_eq!(load_settings(&paths), AppSettings::default());
    }

    #[test]
    fn save_then_load_roundtrips_with_private_mode() {
        use std::os::unix::fs::PermissionsExt;
        let paths = temp_paths("roundtrip");
        let s = AppSettings {
            browser: Some("com.google.Chrome".into()),
            terminal: Some("com.mitchellh.ghostty".into()),
        };
        save_settings(&paths, &s).unwrap();
        assert_eq!(load_settings(&paths), s);
        let mode = std::fs::metadata(paths.settings_file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[test]
    fn save_rejects_ids_outside_the_catalog() {
        let paths = temp_paths("reject");
        let bad_browser = AppSettings {
            browser: Some("com.evil.App".into()),
            terminal: None,
        };
        assert!(save_settings(&paths, &bad_browser).is_err());
        // A terminal id is not a valid browser id and vice versa.
        let swapped = AppSettings {
            browser: Some("com.apple.Terminal".into()),
            terminal: Some("com.google.Chrome".into()),
        };
        assert!(validate_settings(&swapped).is_err());
        assert!(!paths.settings_file().exists());
    }

    #[tokio::test]
    async fn every_catalog_terminal_has_a_launch_recipe() {
        // Guards catalog/recipe drift: a catalog entry `open_terminal` rejects would be
        // selectable in the UI but fail at launch.
        for t in TERMINALS {
            crate::services::process::open_terminal_with(
                &FakeMdfindOk,
                Some(t.bundle_id),
                &crate::services::process::TerminalCommand::Argv(vec![
                    "ssh".to_string(),
                    "x".to_string(),
                ]),
                &|b| Ok(PathBuf::from(format!("/usr/bin/{b}"))),
            )
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", t.bundle_id));
        }
    }

    struct FakeMdfindOk;

    #[async_trait]
    impl CommandRunner for FakeMdfindOk {
        async fn run(&self, _bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            Ok(CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                success: true,
            })
        }
    }

    #[tokio::test]
    async fn detection_reports_only_installed_apps() {
        let runner = FakeMdfind(HashMap::from([
            ("com.google.Chrome", "/Applications/Google Chrome.app"),
            ("com.mitchellh.ghostty", "/Applications/Ghostty.app"),
        ]));
        let apps = list_installed_apps_with(&runner, &|p| {
            p == Path::new("/Applications/Google Chrome.app")
                || p == Path::new("/Applications/Ghostty.app")
        })
        .await;
        assert_eq!(
            apps.browsers,
            vec![AppChoice {
                bundle_id: "com.google.Chrome".into(),
                name: "Google Chrome".into(),
                path: "/Applications/Google Chrome.app".into(),
            }]
        );
        assert_eq!(apps.terminals.len(), 1);
        assert_eq!(apps.terminals[0].bundle_id, "com.mitchellh.ghostty");
    }

    #[tokio::test]
    async fn detection_falls_back_to_standard_dirs_when_spotlight_finds_nothing() {
        let runner = FakeMdfind(HashMap::new());
        let apps = list_installed_apps_with(&runner, &|p| {
            p == Path::new("/System/Applications/Utilities/Terminal.app")
        })
        .await;
        assert!(apps.browsers.is_empty());
        assert_eq!(apps.terminals.len(), 1);
        assert_eq!(apps.terminals[0].bundle_id, "com.apple.Terminal");
    }

    #[tokio::test]
    async fn resolve_choice_unset_installed_missing_and_unknown() {
        let runner = FakeMdfind(HashMap::from([(
            "com.mitchellh.ghostty",
            "/Applications/Ghostty.app",
        )]));
        let only_ghostty = |p: &Path| p == Path::new("/Applications/Ghostty.app");
        let go = |kind, id| resolve_choice_with(&runner, kind, id, "the default", &only_ghostty);
        // Unset -> defaults, no note.
        assert_eq!(go(AppKind::Terminal, None).await, Ok((None, None)));
        // Bundled apps never need a lookup.
        assert_eq!(
            go(AppKind::Terminal, Some("com.apple.Terminal")).await,
            Ok((Some("com.apple.Terminal".into()), None))
        );
        // Installed catalog id is kept.
        assert_eq!(
            go(AppKind::Terminal, Some("com.mitchellh.ghostty")).await,
            Ok((Some("com.mitchellh.ghostty".into()), None))
        );
        // Catalog id that is gone -> fallback + note.
        let (app, note) = go(AppKind::Browser, Some("app.zen-browser.zen"))
            .await
            .unwrap();
        assert_eq!(app, None);
        assert!(note.unwrap().contains("Zen is not installed"));
        // Unknown id, or an id from the other catalog -> default + note, id never returned.
        for (kind, id) in [
            (AppKind::Browser, "com.evil.App"),
            (AppKind::Browser, "com.apple.Terminal"),
            (AppKind::Terminal, "-a/evil"),
        ] {
            let (app, note) = go(kind, Some(id)).await.unwrap();
            assert_eq!(app, None);
            let note = note.unwrap();
            assert!(note.contains("ignored; using the default"), "{note}");
            assert!(!note.contains(id), "{note}");
        }
    }

    #[tokio::test]
    async fn detection_ignores_volumes_trash_and_backups() {
        struct Bad;
        #[async_trait]
        impl CommandRunner for Bad {
            async fn run(&self, _b: &str, _a: &[String]) -> Result<CommandOutput, String> {
                Ok(CommandOutput {
                    stdout: "/Volumes/Backup/Ghostty.app\n/Users/x/.Trash/Ghostty.app\n/Volumes/TM/Backups.backupdb/m/Ghostty.app\n/Users/x/Apps/Ghostty.app\n".into(),
                    stderr: String::new(),
                    success: true,
                })
            }
        }
        let entry = TERMINALS
            .iter()
            .find(|a| a.bundle_id == "com.mitchellh.ghostty")
            .unwrap();
        let found = find_app(&Bad, entry, &|_| true).await;
        assert_eq!(found.as_deref(), Some("/Users/x/Apps/Ghostty.app"));
        for p in [
            "/Volumes/a/X.app",
            "/Users/x/.Trash/X.app",
            "/Volumes/.Trashes/501/X.app",
            "/Backups.backupdb/m/X.app",
        ] {
            assert!(is_excluded_location(p), "{p}");
        }
        assert!(!is_excluded_location("/Applications/X.app"));
    }
}
