use std::str::FromStr;

use crate::services::local_runtime::{self, DiscoveredLocalHost};
use crate::services::local_runtime_lifecycle::{
    self, LifecycleActionResult, LifecycleGuard, LocalRuntimeProvider,
};
use crate::services::process::SystemRunner;

#[tauri::command]
pub async fn detect_local_hosts() -> Result<Vec<DiscoveredLocalHost>, String> {
    let runner = SystemRunner;
    local_runtime::detect_local_hosts(&runner).await
}

/// Saved terminal after the sink-side catalog + installed check; the note says the default was
/// used because the saved one is gone.
async fn configured_terminal(
    runner: &SystemRunner,
) -> Result<(Option<String>, Option<String>), String> {
    use crate::services::app_settings::{self, AppKind};
    let paths = crate::services::paths::ClusterDeckPaths::resolve()?;
    let saved = app_settings::load_settings(&paths).terminal;
    app_settings::resolve_choice(runner, AppKind::Terminal, saved.as_deref(), "Terminal").await
}

fn parse_provider(provider: &str) -> Result<LocalRuntimeProvider, String> {
    LocalRuntimeProvider::from_str(provider)
}

/// Start/Stop/Restart are the only actions guarded against a second concurrent invocation on the
/// same instance (ADR-0007 D5) — they mutate long-running VM state, unlike Shell/Copy which are
/// instant and non-mutating.
#[tauri::command]
pub async fn start_local_runtime(
    guard: tauri::State<'_, LifecycleGuard>,
    provider: String,
    instance_name: String,
) -> Result<LifecycleActionResult, String> {
    let provider = parse_provider(&provider)?;
    let _lock = guard.try_acquire(provider, &instance_name)?;
    let runner = SystemRunner;
    local_runtime_lifecycle::start_instance(&runner, provider, &instance_name).await
}

#[tauri::command]
pub async fn stop_local_runtime(
    guard: tauri::State<'_, LifecycleGuard>,
    provider: String,
    instance_name: String,
) -> Result<LifecycleActionResult, String> {
    let provider = parse_provider(&provider)?;
    let _lock = guard.try_acquire(provider, &instance_name)?;
    let runner = SystemRunner;
    local_runtime_lifecycle::stop_instance(&runner, provider, &instance_name).await
}

#[tauri::command]
pub async fn restart_local_runtime(
    guard: tauri::State<'_, LifecycleGuard>,
    provider: String,
    instance_name: String,
) -> Result<LifecycleActionResult, String> {
    let provider = parse_provider(&provider)?;
    let _lock = guard.try_acquire(provider, &instance_name)?;
    let runner = SystemRunner;
    local_runtime_lifecycle::restart_instance(&runner, provider, &instance_name).await
}

#[tauri::command]
pub async fn open_local_runtime_shell(
    provider: String,
    instance_name: String,
) -> Result<Option<String>, String> {
    let provider = parse_provider(&provider)?;
    let runner = SystemRunner;
    let (terminal, note) = configured_terminal(&runner).await?;
    local_runtime_lifecycle::open_shell(&runner, provider, &instance_name, terminal.as_deref())
        .await?;
    Ok(note)
}

#[tauri::command]
pub async fn open_local_runtime_context(
    provider: String,
    instance_name: String,
) -> Result<Option<String>, String> {
    let provider = parse_provider(&provider)?;
    let runner = SystemRunner;
    let (terminal, note) = configured_terminal(&runner).await?;
    local_runtime_lifecycle::open_context_shell(
        &runner,
        provider,
        &instance_name,
        terminal.as_deref(),
    )
    .await?;
    Ok(note)
}
