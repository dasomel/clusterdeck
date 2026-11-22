//! Delete every machine of one Infrastructure environment group (ADR-0012 D7-D9, D11-D13).
//!
//! The caller supplies the environment string and the plan the user confirmed. The environment is
//! compared against a fresh live listing and never reaches argv; the plan is only compared with
//! the fresh plan. Every id/name/directory that does reach argv or env comes from the fresh plan
//! and is re-validated at the sink (AGENTS.md: validate at every sink).

use futures::future::join_all;
use serde::Serialize;

use crate::services::inventory::{self, DeleteKind, Machine};
use crate::services::local_runtime_delete_plan::{
    acquire_all, ensure_unchanged, reverify_target, stale_entries, vagrant_target, DeletePlan,
    DeleteTarget, StaleEntry, STATE_CHANGED,
};
use crate::services::local_runtime_lifecycle::{
    fetch_vagrant_entries, prune_vagrant_cache, run_with_timeout_env, tail_message,
    unique_safe_vagrant_entry, DeletableRuntimeProvider, LifecycleActionResult, LifecycleGuard,
    DISCOVERY_TIMEOUT,
};
use crate::services::local_runtime_vagrant_identity::destroy_env;
use crate::services::process::{CommandOutput, CommandRunner};
use crate::services::validate;

const SKIPPED: &str = "skipped: an earlier target no longer matched the confirmed plan";

/// Colima profile deletes run this many at a time; Vagrant destroys stay sequential.
const COLIMA_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, Serialize)]
pub struct MachineDeleteResult {
    pub name: String,
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupDeleteResult {
    pub environment: String,
    pub results: Vec<MachineDeleteResult>,
}

/// A group member that passed eligibility, before any id is resolved.
#[derive(Debug, PartialEq)]
enum Selected<'a> {
    Colima {
        name: &'a str,
    },
    /// `kind` is `Machine.delete_kind`: `Prune` = stale entry, no command of its own.
    Vagrant {
        machine: &'a Machine,
        kind: DeleteKind,
    },
}

/// Exact-match selection plus all-or-nothing eligibility: one member without a `delete_kind`
/// refuses the whole group, so a delete never leaves a half-removed environment by design.
fn select_targets<'a>(
    machines: &'a [Machine],
    environment: &str,
) -> Result<Vec<Selected<'a>>, String> {
    let group: Vec<&Machine> = machines
        .iter()
        .filter(|m| m.environment == environment)
        .collect();
    if group.is_empty() {
        return Err(format!("environment not found: {environment}"));
    }
    group
        .into_iter()
        .map(|m| match (m.delete_kind, m.orchestrator.as_deref()) {
            (Some(kind), Some("vagrant")) => Ok(Selected::Vagrant { machine: m, kind }),
            (Some(_), _) if m.runtime == "colima" => {
                let name = m.runtime_id.as_deref().unwrap_or_default();
                if !validate::is_safe_local_runtime_instance_name(name) {
                    return Err(format!("invalid Colima instance name: {}", m.name));
                }
                Ok(Selected::Colima { name })
            }
            _ => Err(format!(
                "{} ({}) cannot be deleted by ClusterDeck; refusing to delete the group",
                m.name, m.runtime
            )),
        })
        .collect()
}

fn machine_result(name: &str, res: Result<CommandOutput, String>) -> MachineDeleteResult {
    let (success, message) = match res {
        Ok(out) => (out.success, tail_message(&out)),
        Err(e) => (false, e),
    };
    MachineDeleteResult {
        name: name.to_string(),
        success,
        message,
    }
}

type DeleteCommand = (&'static str, Vec<String>, Vec<(String, String)>);

/// The delete command for one target of a *fresh* plan, validated again at the sink. No Colima
/// `--data` (ADR-0012 D2). Vagrant is `vagrant destroy -f <machine name>` with `VAGRANT_CWD` set
/// to the canonical project directory (D12): the id is never put in argv, so Vagrant's id-prefix
/// resolution cannot pick another machine. The rest of its environment is pinned too (D17).
fn delete_command(t: &DeleteTarget) -> Result<DeleteCommand, String> {
    let name = t.name.clone();
    match t.provider {
        DeletableRuntimeProvider::Colima | DeletableRuntimeProvider::Lima
            if !validate::is_safe_local_runtime_instance_name(&name) =>
        {
            Err(format!("invalid instance name: {name}"))
        }
        DeletableRuntimeProvider::Colima => Ok((
            "colima",
            vec!["delete".into(), "--force".into(), "--profile".into(), name],
            Vec::new(),
        )),
        DeletableRuntimeProvider::Lima => Ok((
            "limactl",
            vec!["delete".into(), "--force".into(), name],
            Vec::new(),
        )),
        DeletableRuntimeProvider::Vagrant => {
            let dir = t
                .project_dir
                .as_deref()
                .filter(|d| validate::is_safe_vagrant_project_dir(d))
                .ok_or_else(|| format!("invalid Vagrant project directory for {name}"))?;
            if !validate::is_safe_local_runtime_instance_name(&name) {
                return Err(format!("invalid Vagrant machine name: {name}"));
            }
            Ok((
                "vagrant",
                vec!["destroy".into(), "-f".into(), name],
                destroy_env(dir)?,
            ))
        }
    }
}

async fn run_delete(runner: &dyn CommandRunner, t: &DeleteTarget) -> MachineDeleteResult {
    let res = match delete_command(t) {
        Ok((bin, args, env)) => run_with_timeout_env(runner, bin, &args, &env).await,
        Err(e) => Err(e),
    };
    machine_result(&t.name, res)
}

fn failed(t: &DeleteTarget, message: String) -> MachineDeleteResult {
    MachineDeleteResult {
        name: t.name.clone(),
        success: false,
        message,
    }
}

/// Prune is global: before it runs, the set of entries it would drop must still be the planned one.
async fn reverify_prune(runner: &dyn CommandRunner, stale: &[StaleEntry]) -> Result<(), String> {
    let entries = fetch_vagrant_entries(runner).await?;
    let now = DeletePlan::new(Vec::new(), stale_entries(&entries).await).stale_entries;
    if now == stale {
        Ok(())
    } else {
        Err(STATE_CHANGED.to_string())
    }
}

/// Runs a fresh, already-confirmed plan: Colima deletes concurrently (bounded), then Lima and
/// Vagrant destroys one at a time, then one prune; continues past failed commands. Results keep
/// the plan's order regardless of completion order; stale machines take their outcome from the
/// prune. Each Vagrant destroy and the prune are re-verified immediately before they run (D16):
/// the first target that no longer matches the plan stops everything after it, and the rest are
/// reported as skipped.
pub(super) async fn execute(
    runner: &dyn CommandRunner,
    plan: &[DeleteTarget],
    stale: &[StaleEntry],
) -> Vec<MachineDeleteResult> {
    let mut results: Vec<Option<MachineDeleteResult>> = plan.iter().map(|_| None).collect();

    let colima: Vec<(usize, &DeleteTarget)> = plan
        .iter()
        .enumerate()
        .filter(|(_, t)| t.provider == DeletableRuntimeProvider::Colima)
        .collect();
    for chunk in colima.chunks(COLIMA_CONCURRENCY) {
        let done = join_all(
            chunk
                .iter()
                .map(|&(i, t)| async move { (i, run_delete(runner, t).await) }),
        )
        .await;
        for (i, r) in done {
            results[i] = Some(r);
        }
    }

    let mut needs_prune = false;
    // Set by the first target that failed its pre-run check; everything later is skipped.
    let mut halted = false;
    for (i, t) in plan.iter().enumerate() {
        if t.delete_kind == DeleteKind::Prune {
            needs_prune = true;
        } else if t.provider != DeletableRuntimeProvider::Colima {
            results[i] = Some(if halted {
                failed(t, SKIPPED.to_string())
            } else {
                match reverify_target(t).await {
                    Ok(()) => run_delete(runner, t).await,
                    Err(e) => {
                        halted = true;
                        failed(t, format!("not deleted: {e}"))
                    }
                }
            });
        }
    }

    if needs_prune {
        // One prune per run drops every cache entry whose project is gone (the plan listed them
        // all); it is also the only "delete" a stale entry gets.
        let prune = if halted {
            LifecycleActionResult {
                success: false,
                message: SKIPPED.to_string(),
            }
        } else {
            match reverify_prune(runner, stale).await {
                Ok(()) => prune_vagrant_cache(runner).await,
                Err(e) => LifecycleActionResult {
                    success: false,
                    message: format!("not deleted: {e}"),
                },
            }
        };
        for (i, t) in plan.iter().enumerate() {
            if t.delete_kind == DeleteKind::Prune {
                results[i] = Some(MachineDeleteResult {
                    name: t.name.clone(),
                    success: prune.success,
                    message: prune.message.clone(),
                });
            }
        }
    }
    results.into_iter().flatten().collect()
}

/// Fresh plan for every machine in `environment`: listing -> eligibility -> per-member target,
/// with Vagrant ids and canonical project directories resolved from the one `global-status`
/// listing. Nothing runs. `Machine.id` is not used for Vagrant (the inventory keeps the first
/// provider's id when a Vagrant machine merges with a standalone VirtualBox row).
pub async fn plan_environment(
    runner: &dyn CommandRunner,
    environment: &str,
) -> Result<DeletePlan, String> {
    // A failure is kept to explain an empty group.
    let (entries, vagrant_err) = match fetch_vagrant_entries(runner).await {
        Ok(entries) => (entries, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    let machines = tokio::time::timeout(
        DISCOVERY_TIMEOUT,
        inventory::discover_delete_candidates(runner, &entries),
    )
    .await
    .map_err(|_| format!("discovery timed out after {}s", DISCOVERY_TIMEOUT.as_secs()))?;
    let selected = select_targets(&machines, environment).map_err(|e| match &vagrant_err {
        Some(v) if e.starts_with("environment not found") => format!("{e} ({v})"),
        _ => e,
    })?;

    let mut targets = Vec::new();
    for s in &selected {
        targets.push(match s {
            Selected::Colima { name } => DeleteTarget {
                provider: DeletableRuntimeProvider::Colima,
                name: (*name).to_string(),
                project_dir: None,
                vagrant_id: None,
                machine_identity: None,
                delete_kind: DeleteKind::Destroy,
            },
            Selected::Vagrant { machine, kind } => {
                let entry = unique_safe_vagrant_entry(
                    entries
                        .iter()
                        .filter(|e| e.directory == machine.environment && e.name == machine.name),
                    &machine.name,
                )?;
                vagrant_target(entry, *kind).await?
            }
        });
    }
    let stale = if targets.iter().any(|t| t.delete_kind == DeleteKind::Prune) {
        stale_entries(&entries).await
    } else {
        Vec::new()
    };
    Ok(DeletePlan::new(targets, stale))
}

/// Deletes the machines of `environment`, but only if a fresh plan equals the one the user
/// confirmed (P1-3: a member added after the confirm aborts instead of being deleted). Order:
/// lock every confirmed member -> fresh plan -> exact equality -> run, so a busy member or any
/// drift aborts before the first destroy. A failed machine does not stop the rest (per-machine
/// results; an interrupted partial delete is more useful reported than rolled back, which is
/// impossible for a destroyed VM anyway).
pub async fn delete_environment(
    runner: &dyn CommandRunner,
    guard: &LifecycleGuard,
    environment: &str,
    confirmed: DeletePlan,
) -> Result<GroupDeleteResult, String> {
    // Held until the end of the function.
    let _locks = acquire_all(guard, &confirmed)?;
    let fresh = plan_environment(runner, environment).await?;
    ensure_unchanged(confirmed, &fresh)?;
    let results = execute(runner, &fresh.targets, &fresh.stale_entries).await;
    Ok(GroupDeleteResult {
        environment: environment.to_string(),
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::local_runtime::VagrantEntry;
    use crate::services::local_runtime_delete_plan::test_support::*;
    use crate::services::local_runtime_delete_plan::STATE_CHANGED;
    use crate::services::local_runtime_lifecycle::vagrant_delete_kind;
    use async_trait::async_trait;
    use std::sync::Mutex;

    // Directories do not exist on disk, so the inventory reports these as stale machines.
    const DIR: &str = "/nonexistent-clusterdeck-test/proj";

    fn two_machine_runner() -> FakeRunner {
        let r = FakeRunner::new();
        r.set_vagrant(&[("abc123", "master", DIR), ("def456", "worker", DIR)]);
        r
    }

    async fn plan_and_delete(
        runner: &FakeRunner,
        guard: &LifecycleGuard,
        env: &str,
    ) -> Result<GroupDeleteResult, String> {
        let plan = plan_environment(runner, env).await?;
        delete_environment(runner, guard, env, plan).await
    }

    #[tokio::test]
    async fn vagrant_group_destroys_each_machine_by_name_and_cwd_then_prunes_once() {
        let d = project_dir("group-destroy");
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("abc123", "master", &d), ("def456", "worker", &d)]);
        let guard = LifecycleGuard::default();
        let res = plan_and_delete(&runner, &guard, &d).await.unwrap();
        assert_eq!(res.results.len(), 2);
        assert!(res.results.iter().all(|r| r.success));
        let destroys = runner.calls_for("vagrant", "destroy");
        assert_eq!(destroys.len(), 2);
        let cwd = destroy_env(&d).unwrap();
        assert!(destroys.contains(&(
            vec!["destroy".into(), "-f".into(), "master".into()],
            cwd.clone()
        )));
        assert!(destroys.contains(&(vec!["destroy".into(), "-f".into(), "worker".into()], cwd)));
        assert_eq!(runner.prunes(), 0);
        // Locks are released afterwards.
        assert!(guard
            .try_acquire(DeletableRuntimeProvider::Vagrant.key(
                &crate::services::local_runtime::vagrant_instance_name(&d, "master")
            ))
            .is_ok());
    }

    #[tokio::test]
    async fn member_added_after_confirm_aborts_without_deleting_anything() {
        let d = project_dir("group-grew");
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("abc123", "master", &d)]);
        let plan = plan_environment(&runner, &d).await.unwrap();
        assert_eq!(plan.targets.len(), 1);
        runner.set_vagrant(&[("abc123", "master", &d), ("def456", "worker", &d)]);
        let err = delete_environment(&runner, &LifecycleGuard::default(), &d, plan)
            .await
            .unwrap_err();
        assert_eq!(err, STATE_CHANGED);
        assert!(runner.calls_for("vagrant", "destroy").is_empty());
        assert_eq!(runner.prunes(), 0);
    }

    #[tokio::test]
    async fn member_removed_after_confirm_aborts() {
        let d = project_dir("group-shrank");
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("abc123", "master", &d), ("def456", "worker", &d)]);
        let plan = plan_environment(&runner, &d).await.unwrap();
        runner.set_vagrant(&[("abc123", "master", &d)]);
        let err = delete_environment(&runner, &LifecycleGuard::default(), &d, plan)
            .await
            .unwrap_err();
        assert_eq!(err, STATE_CHANGED);
        assert!(runner.calls_for("vagrant", "destroy").is_empty());
    }

    #[tokio::test]
    async fn stale_group_flipping_live_aborts() {
        let dir = std::env::temp_dir().join(format!("cd-group-flip-{}", std::process::id()));
        let d = dir.to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&dir);
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("abc123", "master", &d)]);
        let plan = plan_environment(&runner, &d).await.unwrap();
        assert_eq!(plan.targets[0].delete_kind, DeleteKind::Prune);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Vagrantfile"), "").unwrap();
        crate::services::local_runtime_vagrant_identity::test_support::register(
            &d, "master", "abc123",
        );
        let err = delete_environment(&runner, &LifecycleGuard::default(), &d, plan).await;
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(err.unwrap_err(), STATE_CHANGED);
        assert!(runner.calls_for("vagrant", "destroy").is_empty());
        assert_eq!(runner.prunes(), 0);
    }

    #[tokio::test]
    async fn group_delete_lists_vagrant_once_and_probes_only_deletable_providers() {
        let d = project_dir("group-scope");
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("abc123", "master", &d)]);
        let plan = plan_environment(&runner, &d).await.unwrap();
        runner.calls.lock().unwrap().clear();
        delete_environment(&runner, &LifecycleGuard::default(), &d, plan)
            .await
            .unwrap();
        let calls = runner.calls.lock().unwrap();
        assert!(calls
            .iter()
            .all(|(b, _, _)| b == "vagrant" || b == "colima"));
        let listings = calls
            .iter()
            .filter(|(b, a, _)| b == "vagrant" && a == &["global-status".to_string()])
            .count();
        assert_eq!(listings, 1);
    }

    #[tokio::test]
    async fn all_stale_group_plans_every_stale_entry_and_only_prunes() {
        let runner = two_machine_runner();
        // An unrelated stale entry is removed by the same prune, so the plan must list it.
        runner.set_vagrant(&[
            ("abc123", "master", DIR),
            ("def456", "worker", DIR),
            ("fff000", "other", "/nonexistent-clusterdeck-test/other"),
        ]);
        let plan = plan_environment(&runner, DIR).await.unwrap();
        assert_eq!(plan.targets.len(), 2);
        assert_eq!(plan.stale_entries.len(), 3);
        let res = delete_environment(&runner, &LifecycleGuard::default(), DIR, plan)
            .await
            .unwrap();
        assert!(runner.calls_for("vagrant", "destroy").is_empty());
        assert_eq!(runner.prunes(), 1);
        assert_eq!(res.results.len(), 2);
        assert!(res
            .results
            .iter()
            .all(|r| r.success && r.message.contains("stale")));
    }

    #[tokio::test]
    async fn mixed_plan_destroys_only_live_and_prunes_once() {
        // A real group is all-stale or all-live (the environment *is* the directory), so the
        // mixed case is exercised on `execute` directly.
        const GONE: &str = "/nonexistent-clusterdeck-test/gone";
        let d = project_dir("group-mixed");
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("abc123", "live", &d), ("fff000", "gone", GONE)]);
        let live_entry = VagrantEntry {
            id: "abc123".into(),
            name: "live".into(),
            provider: String::new(),
            state: String::new(),
            directory: d.clone(),
        };
        let plan = vec![
            vagrant_target(&live_entry, DeleteKind::Destroy)
                .await
                .unwrap(),
            DeleteTarget {
                provider: DeletableRuntimeProvider::Vagrant,
                name: "gone".into(),
                project_dir: Some(GONE.into()),
                vagrant_id: Some("fff000".into()),
                machine_identity: None,
                delete_kind: DeleteKind::Prune,
            },
        ];
        let stale = vec![StaleEntry {
            vagrant_id: "fff000".into(),
            machine_name: "gone".into(),
            project_dir: GONE.into(),
        }];
        let results = execute(&runner, &plan, &stale).await;
        assert_eq!(
            runner.calls_for("vagrant", "destroy"),
            vec![(
                vec!["destroy".to_string(), "-f".into(), "live".into()],
                destroy_env(&d).unwrap()
            )]
        );
        assert_eq!(runner.prunes(), 1);
        assert!(results.iter().all(|r| r.success));
        assert!(results[1].message.contains("stale"));
    }

    #[tokio::test]
    async fn destroy_env_pins_every_vagrant_path_to_the_validated_ones() {
        let d = project_dir("group-env");
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("abc123", "master", &d)]);
        plan_and_delete(&runner, &LifecycleGuard::default(), &d)
            .await
            .unwrap();
        let env = runner.calls_for("vagrant", "destroy").remove(0).1;
        let get = |k: &str| {
            env.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(env.len(), 4);
        assert_eq!(get("VAGRANT_CWD"), d);
        assert_eq!(get("VAGRANT_DOTFILE_PATH"), format!("{d}/.vagrant"));
        assert_eq!(get("VAGRANT_VAGRANTFILE"), "Vagrantfile");
        // The index the identity check read lives under exactly this home.
        let index = crate::services::local_runtime_vagrant_identity::test_support::index_file();
        assert_eq!(
            index,
            std::path::Path::new(&get("VAGRANT_HOME")).join("data/machine-index/index")
        );
        assert!(index.is_file());
    }

    /// Runs `after_first_destroy` once the first `vagrant destroy` returned, to change the world
    /// mid-group.
    struct Mutating {
        inner: FakeRunner,
        after_first_destroy: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }
    #[async_trait]
    impl CommandRunner for Mutating {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            self.run_with_env(bin, args, &[]).await
        }
        async fn run_with_env(
            &self,
            bin: &str,
            args: &[String],
            env: &[(String, String)],
        ) -> Result<CommandOutput, String> {
            let out = self.inner.run_with_env(bin, args, env).await;
            if bin == "vagrant" && args.first().map(String::as_str) == Some("destroy") {
                if let Some(f) = self.after_first_destroy.lock().unwrap().take() {
                    f();
                }
            }
            out
        }
    }

    /// Plan order is by name: extra, master, worker. `mutate` runs after `extra` was destroyed.
    async fn group_with_mutation(
        tag: &str,
        mutate: impl FnOnce(&str) + Send + 'static,
    ) -> (Vec<MachineDeleteResult>, Mutating) {
        let d = project_dir(tag);
        let inner = FakeRunner::new();
        inner.set_vagrant(&[
            ("abc111", "extra", &d),
            ("abc222", "master", &d),
            ("abc333", "worker", &d),
        ]);
        let plan = plan_environment(&inner, &d).await.unwrap();
        let dir = d.clone();
        let runner = Mutating {
            inner,
            after_first_destroy: Mutex::new(Some(Box::new(move || mutate(&dir)))),
        };
        let res = execute(&runner, &plan.targets, &plan.stale_entries).await;
        (res, runner)
    }

    fn assert_stopped_after_first(res: &[MachineDeleteResult], runner: &Mutating) {
        let names: Vec<_> = res.iter().map(|r| (r.name.as_str(), r.success)).collect();
        assert_eq!(
            names,
            [("extra", true), ("master", false), ("worker", false)]
        );
        assert!(
            res[1].message.starts_with("not deleted"),
            "{}",
            res[1].message
        );
        assert!(res[2].message.starts_with("skipped"), "{}", res[2].message);
        assert_eq!(runner.inner.calls_for("vagrant", "destroy").len(), 1);
    }

    #[tokio::test]
    async fn identity_changed_mid_group_stops_the_remaining_targets() {
        let (res, runner) = group_with_mutation("group-mid-id", |d| {
            let id = std::path::Path::new(d).join(".vagrant/machines/master/virtualbox/id");
            std::fs::write(id, "vm-swapped").unwrap();
        })
        .await;
        assert_stopped_after_first(&res, &runner);
    }

    #[tokio::test]
    async fn prefix_appearing_mid_group_stops_the_remaining_targets() {
        let (res, runner) = group_with_mutation("group-mid-prefix", |_| {
            crate::services::local_runtime_vagrant_identity::test_support::add_index_entry(
                "master-other-vm",
                "other",
                None,
            );
        })
        .await;
        assert_stopped_after_first(&res, &runner);
        assert!(res[1].message.contains("prefix of another"));
    }

    #[tokio::test]
    async fn stale_set_growing_before_prune_skips_the_prune() {
        let runner = two_machine_runner();
        let plan = plan_environment(&runner, DIR).await.unwrap();
        runner.set_vagrant(&[
            ("abc123", "master", DIR),
            ("def456", "worker", DIR),
            ("fff000", "late", "/nonexistent-clusterdeck-test/late"),
        ]);
        let res = execute(&runner, &plan.targets, &plan.stale_entries).await;
        assert_eq!(runner.prunes(), 0);
        assert!(res
            .iter()
            .all(|r| !r.success && r.message.contains("not deleted")));
    }

    #[tokio::test]
    async fn execute_refuses_unsafe_targets_at_the_sink() {
        let runner = FakeRunner::new();
        let bad = |dir: Option<&str>, name: &str| DeleteTarget {
            provider: DeletableRuntimeProvider::Vagrant,
            name: name.into(),
            project_dir: dir.map(String::from),
            vagrant_id: None,
            machine_identity: None,
            delete_kind: DeleteKind::Destroy,
        };
        let plan = vec![
            bad(Some("relative"), "master"),
            bad(None, "master"),
            bad(Some("/ok"), "-f"),
        ];
        let results = execute(&runner, &plan, &[]).await;
        assert!(results.iter().all(|r| !r.success));
        assert!(runner.calls_for("vagrant", "destroy").is_empty());
    }

    #[tokio::test]
    async fn delete_kind_follows_directory_existence_only() {
        let d = project_dir("group-live");
        assert_eq!(vagrant_delete_kind(&d).await, DeleteKind::Destroy);
        assert_eq!(vagrant_delete_kind(DIR).await, DeleteKind::Prune);
    }

    #[tokio::test]
    async fn colima_deletes_run_concurrently_and_results_keep_plan_order() {
        // The first profile finishes last; results must still follow the plan order.
        struct Slow {
            done: Mutex<Vec<String>>,
        }
        #[async_trait]
        impl CommandRunner for Slow {
            async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
                assert_eq!(bin, "colima");
                let name = args.last().unwrap().clone();
                if name == "slow" {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                }
                self.done.lock().unwrap().push(name);
                ok("deleted")
            }
        }
        let runner = Slow {
            done: Mutex::new(Vec::new()),
        };
        let plan: Vec<DeleteTarget> = ["slow", "b", "c"]
            .iter()
            .map(|n| DeleteTarget {
                provider: DeletableRuntimeProvider::Colima,
                name: (*n).into(),
                project_dir: None,
                vagrant_id: None,
                machine_identity: None,
                delete_kind: DeleteKind::Destroy,
            })
            .collect();
        let results = execute(&runner, &plan, &[]).await;
        assert_eq!(
            results.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["slow", "b", "c"]
        );
        assert!(results.iter().all(|r| r.success));
        assert_eq!(runner.done.lock().unwrap().last().unwrap(), "slow");
    }

    #[tokio::test]
    async fn failed_prune_fails_stale_machines() {
        struct PruneFails(FakeRunner);
        #[async_trait]
        impl CommandRunner for PruneFails {
            async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
                if bin == "vagrant" && args.contains(&"--prune".to_string()) {
                    return Ok(CommandOutput {
                        stdout: String::new(),
                        stderr: "prune boom".into(),
                        success: false,
                    });
                }
                self.0.run(bin, args).await
            }
        }
        let runner = PruneFails(two_machine_runner());
        let plan = plan_environment(&runner, DIR).await.unwrap();
        let res = delete_environment(&runner, &LifecycleGuard::default(), DIR, plan)
            .await
            .unwrap();
        assert!(res
            .results
            .iter()
            .all(|r| !r.success && r.message == "prune boom"));
    }

    #[tokio::test]
    async fn colima_group_runs_one_delete() {
        let runner = FakeRunner::new();
        *runner.colima_list.lock().unwrap() =
            r#"{"name":"default","status":"Running","runtime":"docker"}"#.to_string();
        let res = plan_and_delete(&runner, &LifecycleGuard::default(), "Colima / default")
            .await
            .unwrap();
        assert_eq!(res.results.len(), 1);
        assert_eq!(
            runner.calls_for("colima", "delete"),
            vec![(
                vec![
                    "delete".to_string(),
                    "--force".into(),
                    "--profile".into(),
                    "default".into()
                ],
                vec![]
            )]
        );
        assert_eq!(runner.prunes(), 0);
    }

    #[tokio::test]
    async fn unknown_environment_errors_without_destroy() {
        let runner = two_machine_runner();
        let err = plan_environment(&runner, "/nope").await.unwrap_err();
        assert!(err.contains("not found"));
        assert!(runner.calls_for("vagrant", "destroy").is_empty());
    }

    #[tokio::test]
    async fn unsafe_vagrant_id_cannot_be_planned() {
        let d = project_dir("group-unsafe");
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("ab;c1", "master", &d)]);
        let err = plan_environment(&runner, &d).await.unwrap_err();
        assert!(err.contains("unsafe"));
    }

    #[tokio::test]
    async fn busy_member_aborts_before_anything_runs() {
        let runner = two_machine_runner();
        let plan = plan_environment(&runner, DIR).await.unwrap();
        let guard = LifecycleGuard::default();
        let _held = guard
            .try_acquire(DeletableRuntimeProvider::Vagrant.key(
                &crate::services::local_runtime::vagrant_instance_name(DIR, "worker"),
            ))
            .unwrap();
        let err = delete_environment(&runner, &guard, DIR, plan)
            .await
            .unwrap_err();
        assert!(err.contains("already in progress"));
        assert_eq!(runner.prunes(), 0);
        // The lock taken for the first member was dropped with the failed acquire.
        assert!(guard
            .try_acquire(DeletableRuntimeProvider::Vagrant.key(
                &crate::services::local_runtime::vagrant_instance_name(DIR, "master")
            ),)
            .is_ok());
    }

    #[test]
    fn group_with_unsupported_runtime_is_refused_whole() {
        let mut vm = Machine::new("virtualbox", "uuid", "solo");
        vm.environment = "mixed".into();
        let mut vg = Machine::new("virtualbox", "uuid2", "master");
        vg.environment = "mixed".into();
        vg.orchestrator = Some("vagrant".into());
        vg.delete_kind = Some(DeleteKind::Destroy);
        let machines = [vg, vm];
        let err = select_targets(&machines, "mixed").unwrap_err();
        assert!(err.contains("cannot be deleted"));
    }

    #[test]
    fn colima_member_with_unsafe_name_is_refused() {
        let mut m = Machine::new("colima", "-bad", "-bad");
        m.environment = "Colima / -bad".into();
        m.delete_kind = Some(DeleteKind::Destroy);
        assert!(select_targets(&[m], "Colima / -bad").is_err());
    }
}
