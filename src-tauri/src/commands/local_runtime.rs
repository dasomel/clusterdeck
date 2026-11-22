use std::str::FromStr;

use crate::services::local_runtime::{self, DiscoveredLocalHost};
use crate::services::local_runtime_delete_plan::{self, DeletePlan};
use crate::services::local_runtime_group_delete::{self, GroupDeleteResult};
use crate::services::local_runtime_lifecycle::{
    self, DeletableRuntimeProvider, LifecycleActionResult, LifecycleGuard, LocalRuntimeProvider,
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
    let _lock = guard.try_acquire(provider.key(&instance_name))?;
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
    let _lock = guard.try_acquire(provider.key(&instance_name))?;
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
    let _lock = guard.try_acquire(provider.key(&instance_name))?;
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

/// Read-only: the exact target(s) a delete of one machine would remove right now (ADR-0012 D11).
/// For Vagrant, `project_dir` is the machine's project directory as listed by the inventory.
#[tauri::command]
pub async fn plan_local_runtime_delete(
    provider: String,
    instance_name: String,
    project_dir: Option<String>,
) -> Result<DeletePlan, String> {
    let provider = DeletableRuntimeProvider::from_str(&provider)?;
    let runner = SystemRunner;
    local_runtime_delete_plan::plan_instance(
        &runner,
        provider,
        &instance_name,
        project_dir.as_deref(),
    )
    .await
}

/// Read-only: the exact members (and stale cache entries a prune would drop) of one environment.
#[tauri::command]
pub async fn plan_environment_delete(environment: String) -> Result<DeletePlan, String> {
    let runner = SystemRunner;
    local_runtime_group_delete::plan_environment(&runner, &environment).await
}

/// Permanently deletes one machine, only if a fresh plan equals the `plan` the user confirmed
/// (ADR-0012 D11). Shares `LifecycleGuard` with start/stop/restart so a delete and a lifecycle
/// action on the same instance exclude each other. `plan` is compared, never used as argv.
#[tauri::command]
pub async fn delete_local_runtime(
    guard: tauri::State<'_, LifecycleGuard>,
    plan: DeletePlan,
) -> Result<LifecycleActionResult, String> {
    let runner = SystemRunner;
    local_runtime_delete_plan::delete_instance(&runner, &guard, plan).await
}

/// Deletes every machine of one Infrastructure environment group (ADR-0012 D7), only if a fresh
/// plan equals the confirmed `plan`. `environment` is only ever compared against a fresh
/// inventory, never placed in argv.
#[tauri::command]
pub async fn delete_environment_group(
    guard: tauri::State<'_, LifecycleGuard>,
    environment: String,
    plan: DeletePlan,
) -> Result<GroupDeleteResult, String> {
    let runner = SystemRunner;
    local_runtime_group_delete::delete_environment(&runner, &guard, &environment, plan).await
}
