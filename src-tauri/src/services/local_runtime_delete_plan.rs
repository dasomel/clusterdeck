//! Confirmed-plan binding for local runtime delete (ADR-0012 D11-D13).
//!
//! The user confirms a *plan*: the exact targets (and, for a stale Vagrant prune, every cache
//! entry the prune removes) that were true when the dialog opened. Execution re-derives a fresh
//! plan from live CLI output and refuses unless it is identical to the confirmed one. The caller's
//! plan is therefore only ever compared, never used as argv: every argv/env value comes from the
//! fresh plan and is validated again at the sink.

use serde::{Deserialize, Serialize};

use crate::services::inventory::DeleteKind;
use crate::services::local_runtime::{self, VagrantEntry};
use crate::services::local_runtime_group_delete::{execute, MachineDeleteResult};
use crate::services::local_runtime_lifecycle::{
    fetch_vagrant_entries, find_fresh_instance, unique_safe_vagrant_entry, vagrant_delete_kind,
    DeletableRuntimeProvider, LifecycleActionResult, LifecycleGuard, LifecycleLock,
    LocalRuntimeProvider,
};
use crate::services::local_runtime_vagrant_identity::verify_destroy_identity;
use crate::services::process::CommandRunner;
use crate::services::validate;

pub(super) const STATE_CHANGED: &str = "State changed since you confirmed — review again";

/// One thing that will be deleted. `name` is the Colima/Lima instance, or the Vagrant machine
/// name (the `vagrant destroy` argument). `project_dir` and `vagrant_id` are Vagrant only: the
/// canonical project directory for a destroy (`VAGRANT_CWD`), the directory as reported for a
/// stale entry, and the full id as reported by `global-status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteTarget {
    pub provider: DeletableRuntimeProvider,
    pub name: String,
    pub project_dir: Option<String>,
    pub vagrant_id: Option<String>,
    /// Vagrant destroy only: `index_uuid:provider:provider_id` read from the project's own
    /// `.vagrant` state (D15), so a replaced project/`.vagrant` at the same path no longer matches.
    pub machine_identity: Option<String>,
    pub delete_kind: DeleteKind,
}

/// A Vagrant cache entry that `global-status --prune` will drop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleEntry {
    pub vagrant_id: String,
    pub machine_name: String,
    pub project_dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletePlan {
    pub targets: Vec<DeleteTarget>,
    /// Non-empty only when some target is a `Prune`: prune is global, so this lists every stale
    /// entry it removes, not just the targets.
    pub stale_entries: Vec<StaleEntry>,
}

impl DeletePlan {
    /// Sorted so equality does not depend on listing order.
    pub fn new(mut targets: Vec<DeleteTarget>, mut stale_entries: Vec<StaleEntry>) -> Self {
        targets.sort_by(|a, b| {
            (a.provider, &a.name, &a.project_dir).cmp(&(b.provider, &b.name, &b.project_dir))
        });
        stale_entries.sort_by(|a, b| {
            (&a.project_dir, &a.machine_name, &a.vagrant_id).cmp(&(
                &b.project_dir,
                &b.machine_name,
                &b.vagrant_id,
            ))
        });
        Self {
            targets,
            stale_entries,
        }
    }
}

/// Aborts unless the user's confirmed plan equals the fresh one: same targets, same project
/// directories and ids, same delete kinds, same stale list. Nothing has run at this point.
pub(super) fn ensure_unchanged(confirmed: DeletePlan, fresh: &DeletePlan) -> Result<(), String> {
    let confirmed = DeletePlan::new(confirmed.targets, confirmed.stale_entries);
    if &confirmed == fresh {
        Ok(())
    } else {
        Err(STATE_CHANGED.to_string())
    }
}

/// Locks every confirmed target (same keys as start/stop) before anything is re-planned or run;
/// one busy target drops the others' locks. The keys come from the caller's plan, which is safe:
/// a lock only excludes, and a wrong key makes `ensure_unchanged` fail anyway.
pub(super) fn acquire_all<'a>(
    guard: &'a LifecycleGuard,
    plan: &DeletePlan,
) -> Result<Vec<LifecycleLock<'a>>, String> {
    plan.targets
        .iter()
        .map(|t| match (t.provider, t.project_dir.as_deref()) {
            (DeletableRuntimeProvider::Vagrant, Some(dir)) => guard.try_acquire(
                DeletableRuntimeProvider::Vagrant
                    .key(&local_runtime::vagrant_instance_name(dir, &t.name)),
            ),
            (provider, _) => guard.try_acquire(provider.key(&t.name)),
        })
        .collect()
}

/// Absolute, existing, canonical directory that contains a `Vagrantfile`. This is the only form
/// that reaches `VAGRANT_CWD`, so a symlink or `..` cannot redirect the destroy.
pub(super) async fn canonical_vagrant_project_dir(dir: &str) -> Result<String, String> {
    if !validate::is_safe_vagrant_project_dir(dir) {
        return Err(format!("invalid Vagrant project directory: {dir}"));
    }
    let canonical = tokio::fs::canonicalize(dir)
        .await
        .map_err(|_| format!("Vagrant project directory does not exist: {dir}"))?;
    let mut has_vagrantfile = false;
    for file in ["Vagrantfile", "vagrantfile"] {
        has_vagrantfile |= tokio::fs::metadata(canonical.join(file))
            .await
            .is_ok_and(|md| md.is_file());
    }
    if !has_vagrantfile {
        return Err(format!("no Vagrantfile in project directory: {dir}"));
    }
    let canonical = canonical
        .to_str()
        .ok_or_else(|| format!("project directory is not valid UTF-8: {dir}"))?
        .to_string();
    if !validate::is_safe_vagrant_project_dir(&canonical) {
        return Err(format!("invalid Vagrant project directory: {dir}"));
    }
    Ok(canonical)
}

/// Every entry `vagrant global-status --prune` will drop (project directory gone, the same rule
/// as `vagrant_delete_kind`).
pub(super) async fn stale_entries(entries: &[VagrantEntry]) -> Vec<StaleEntry> {
    let mut stale = Vec::new();
    for e in entries {
        if vagrant_delete_kind(&e.directory).await == DeleteKind::Prune {
            stale.push(StaleEntry {
                vagrant_id: e.id.clone(),
                machine_name: e.name.clone(),
                project_dir: e.directory.clone(),
            });
        }
    }
    stale
}

pub(super) async fn vagrant_target(
    entry: &VagrantEntry,
    kind: DeleteKind,
) -> Result<DeleteTarget, String> {
    let (project_dir, machine_identity) = match kind {
        DeleteKind::Destroy => {
            if !validate::is_safe_local_runtime_instance_name(&entry.name) {
                return Err(format!("invalid Vagrant machine name: {}", entry.name));
            }
            let dir = canonical_vagrant_project_dir(&entry.directory).await?;
            let identity = verify_destroy_identity(&dir, &entry.name, &entry.id).await?;
            (dir, Some(identity))
        }
        // The directory is gone by definition; kept as reported so the user sees what is pruned.
        DeleteKind::Prune => (entry.directory.clone(), None),
    };
    Ok(DeleteTarget {
        provider: DeletableRuntimeProvider::Vagrant,
        name: entry.name.clone(),
        project_dir: Some(project_dir),
        vagrant_id: Some(entry.id.clone()),
        machine_identity,
        delete_kind: kind,
    })
}

/// Re-derives one Vagrant destroy target from its own planned fields (local identity files, the
/// machine index, the name-prefix rule) right before it runs; anything but an exact match fails
/// closed (D16). Other targets have no local state to re-check.
pub(super) async fn reverify_target(t: &DeleteTarget) -> Result<(), String> {
    if t.provider != DeletableRuntimeProvider::Vagrant || t.delete_kind != DeleteKind::Destroy {
        return Ok(());
    }
    let entry = VagrantEntry {
        id: t.vagrant_id.clone().ok_or(STATE_CHANGED)?,
        name: t.name.clone(),
        provider: String::new(),
        state: String::new(),
        directory: t.project_dir.clone().ok_or(STATE_CHANGED)?,
    };
    match vagrant_target(&entry, DeleteKind::Destroy).await {
        Ok(fresh) if &fresh == t => Ok(()),
        Ok(_) => Err(STATE_CHANGED.to_string()),
        Err(e) => Err(e),
    }
}

/// Fresh plan for one machine. Colima/Lima go through `find_fresh_instance`. A Vagrant machine
/// is identified by `(project directory, machine name)`, never by basename, so two projects that
/// share a directory name cannot be confused (P1-2); `project_dir` may be the raw or canonical
/// form of the entry's directory.
pub async fn plan_instance(
    runner: &dyn CommandRunner,
    provider: DeletableRuntimeProvider,
    instance_name: &str,
    project_dir: Option<&str>,
) -> Result<DeletePlan, String> {
    let lima_colima = match provider {
        DeletableRuntimeProvider::Colima => Some(LocalRuntimeProvider::Colima),
        DeletableRuntimeProvider::Lima => Some(LocalRuntimeProvider::Lima),
        DeletableRuntimeProvider::Vagrant => None,
    };
    if let Some(p) = lima_colima {
        find_fresh_instance(runner, p, instance_name).await?;
        return Ok(DeletePlan::new(
            vec![DeleteTarget {
                provider,
                name: instance_name.to_string(),
                project_dir: None,
                vagrant_id: None,
                machine_identity: None,
                delete_kind: DeleteKind::Destroy,
            }],
            Vec::new(),
        ));
    }

    let dir = project_dir.ok_or("a Vagrant machine needs its project directory")?;
    if !validate::is_safe_vagrant_project_dir(dir) {
        return Err(format!("invalid Vagrant project directory: {dir}"));
    }
    let entries = fetch_vagrant_entries(runner).await?;
    let mut matches = Vec::new();
    for e in entries.iter().filter(|e| e.name == instance_name) {
        let same = e.directory == dir
            || tokio::fs::canonicalize(&e.directory)
                .await
                .is_ok_and(|c| c.to_str() == Some(dir));
        if same {
            matches.push(e);
        }
    }
    let entry = unique_safe_vagrant_entry(matches.into_iter(), &format!("{dir}: {instance_name}"))?;
    let kind = vagrant_delete_kind(&entry.directory).await;
    let stale = if kind == DeleteKind::Prune {
        stale_entries(&entries).await
    } else {
        Vec::new()
    };
    Ok(DeletePlan::new(
        vec![vagrant_target(entry, kind).await?],
        stale,
    ))
}

/// Deletes the one machine in `confirmed`, only if it still matches a fresh plan exactly.
pub async fn delete_instance(
    runner: &dyn CommandRunner,
    guard: &LifecycleGuard,
    confirmed: DeletePlan,
) -> Result<LifecycleActionResult, String> {
    let [target] = confirmed.targets.as_slice() else {
        return Err("a single delete needs exactly one target".to_string());
    };
    let (provider, name, project_dir) = (
        target.provider,
        target.name.clone(),
        target.project_dir.clone(),
    );
    let _locks = acquire_all(guard, &confirmed)?;
    let fresh = plan_instance(runner, provider, &name, project_dir.as_deref()).await?;
    ensure_unchanged(confirmed, &fresh)?;
    let results: Vec<MachineDeleteResult> =
        execute(runner, &fresh.targets, &fresh.stale_entries).await;
    let r = results.into_iter().next().ok_or("nothing was deleted")?;
    Ok(LifecycleActionResult {
        success: r.success,
        message: r.message,
    })
}

#[cfg(test)]
pub(super) mod test_support {
    use async_trait::async_trait;
    use std::sync::Mutex;

    use crate::services::process::{CommandOutput, CommandRunner};

    type ArgsEnv = (Vec<String>, Vec<(String, String)>);
    type Call = (String, Vec<String>, Vec<(String, String)>);

    /// Mutable listings so a test can change the world between plan and execute.
    pub struct FakeRunner {
        pub colima_list: Mutex<String>,
        pub lima_list: Mutex<String>,
        pub vagrant_status: Mutex<String>,
        pub calls: Mutex<Vec<Call>>,
    }

    impl FakeRunner {
        pub fn new() -> Self {
            Self {
                colima_list: Mutex::new(String::new()),
                lima_list: Mutex::new(String::new()),
                vagrant_status: Mutex::new(String::new()),
                calls: Mutex::new(Vec::new()),
            }
        }

        pub fn set_vagrant(&self, rows: &[(&str, &str, &str)]) {
            *self.vagrant_status.lock().unwrap() = vagrant_status(rows);
            // Rows whose project exists get matching local identity files + index entries.
            for (id, name, dir) in rows {
                if std::path::Path::new(dir).join("Vagrantfile").is_file() {
                    crate::services::local_runtime_vagrant_identity::test_support::register(
                        dir, name, id,
                    );
                }
            }
        }

        pub fn calls_for(&self, bin: &str, first: &str) -> Vec<ArgsEnv> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(b, a, _)| b == bin && a.first().map(String::as_str) == Some(first))
                .map(|(_, a, e)| (a.clone(), e.clone()))
                .collect()
        }

        pub fn prunes(&self) -> usize {
            self.calls_for("vagrant", "global-status")
                .iter()
                .filter(|(a, _)| a.contains(&"--prune".to_string()))
                .count()
        }
    }

    pub fn vagrant_status(rows: &[(&str, &str, &str)]) -> String {
        let mut s = String::from(
            "id       name    provider   state   directory\n\
             ------------------------------------------------------------\n",
        );
        for (id, name, dir) in rows {
            s.push_str(&format!("{id}  {name}  virtualbox running {dir}\n"));
        }
        s.push_str(" \nThe above shows information about all known Vagrant environments\n");
        s
    }

    /// A fresh temp project directory containing a Vagrantfile; returns its canonical path.
    pub fn project_dir(tag: &str) -> String {
        let dir = std::env::temp_dir()
            .join(format!("cd-plan-{tag}-{}", std::process::id()))
            .join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Vagrantfile"), "").unwrap();
        std::fs::canonicalize(dir)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    pub fn ok(stdout: &str) -> Result<CommandOutput, String> {
        Ok(CommandOutput {
            stdout: stdout.to_string(),
            stderr: String::new(),
            success: true,
        })
    }

    #[async_trait]
    impl CommandRunner for FakeRunner {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            self.run_with_env(bin, args, &[]).await
        }

        async fn run_with_env(
            &self,
            bin: &str,
            args: &[String],
            env: &[(String, String)],
        ) -> Result<CommandOutput, String> {
            self.calls
                .lock()
                .unwrap()
                .push((bin.to_string(), args.to_vec(), env.to_vec()));
            let first = args.first().map(String::as_str);
            match (bin, first) {
                ("colima", Some("list")) => ok(&self.colima_list.lock().unwrap()),
                ("colima", Some("ssh-config")) => ok(""),
                ("limactl", Some("list")) => ok(&self.lima_list.lock().unwrap()),
                ("colima" | "limactl", _) => ok("deleted"),
                ("vagrant", Some("global-status")) if args.len() == 1 => {
                    ok(&self.vagrant_status.lock().unwrap())
                }
                ("vagrant", _) => ok("vagrant ok"),
                _ => Err(format!("unexpected command {bin}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    const COLIMA_LIST: &str = r#"{"name":"default","status":"Running","runtime":"docker+k3s"}"#;
    const LIMA_LIST: &str =
        r#"{"name":"work","status":"Running","sshAddress":"127.0.0.1","sshLocalPort":50000}"#;

    async fn vagrant_plan(runner: &FakeRunner, dir: &str) -> Result<DeletePlan, String> {
        plan_instance(
            runner,
            DeletableRuntimeProvider::Vagrant,
            "master",
            Some(dir),
        )
        .await
    }

    #[tokio::test]
    async fn vagrant_destroy_uses_machine_name_and_vagrant_cwd_never_the_id() {
        let dir = project_dir("argv");
        // Two ids sharing a 7-char prefix: irrelevant now because the id never reaches argv.
        let runner = FakeRunner::new();
        runner.set_vagrant(&[
            ("abcdef1", "master", &dir),
            ("abcdef1ffff", "other", "/nonexistent-cd/x"),
        ]);
        let plan = vagrant_plan(&runner, &dir).await.unwrap();
        assert_eq!(plan.targets[0].vagrant_id.as_deref(), Some("abcdef1"));
        let res = delete_instance(&runner, &LifecycleGuard::default(), plan)
            .await
            .unwrap();
        assert!(res.success);
        let destroys = runner.calls_for("vagrant", "destroy");
        assert_eq!(destroys.len(), 1);
        assert_eq!(destroys[0].0, vec!["destroy", "-f", "master"]);
        assert_eq!(
            destroys[0].1,
            crate::services::local_runtime_vagrant_identity::destroy_env(&dir).unwrap()
        );
        assert_eq!(runner.prunes(), 0);
    }

    #[tokio::test]
    async fn symlinked_project_dir_is_canonicalized_for_vagrant_cwd() {
        let dir = project_dir("link");
        let link = format!("{dir}-link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("abc123", "master", &link)]);
        let plan = vagrant_plan(&runner, &link).await.unwrap();
        assert_eq!(plan.targets[0].project_dir.as_deref(), Some(dir.as_str()));
        delete_instance(&runner, &LifecycleGuard::default(), plan)
            .await
            .unwrap();
        std::fs::remove_file(&link).ok();
        assert_eq!(runner.calls_for("vagrant", "destroy")[0].1[0].1, dir);
    }

    #[tokio::test]
    async fn id_replaced_between_plan_and_execute_aborts() {
        let dir = project_dir("replaced");
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("abc123", "master", &dir)]);
        let plan = vagrant_plan(&runner, &dir).await.unwrap();
        runner.set_vagrant(&[("zzz999", "master", &dir)]);
        let err = delete_instance(&runner, &LifecycleGuard::default(), plan)
            .await
            .unwrap_err();
        assert_eq!(err, STATE_CHANGED);
        assert!(runner.calls_for("vagrant", "destroy").is_empty());
    }

    #[tokio::test]
    async fn same_basename_other_project_is_never_deleted_when_original_disappears() {
        // /a/proj/master confirmed; during the dialog it disappears while /b/proj/master stays.
        let a = project_dir("moved-a");
        let b = project_dir("moved-b");
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("aaa111", "master", &a), ("bbb222", "master", &b)]);
        let plan = vagrant_plan(&runner, &a).await.unwrap();
        runner.set_vagrant(&[("bbb222", "master", &b)]);
        let err = delete_instance(&runner, &LifecycleGuard::default(), plan)
            .await
            .unwrap_err();
        assert!(err.contains("not found"), "{err}");
        assert!(runner.calls_for("vagrant", "destroy").is_empty());
    }

    #[tokio::test]
    async fn stale_to_live_flip_aborts_without_prune_or_destroy() {
        let dir = std::env::temp_dir()
            .join(format!("cd-plan-flip-{}", std::process::id()))
            .join("proj");
        let d = dir.to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&dir);
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("abc123", "master", &d)]);
        let plan = vagrant_plan(&runner, &d).await.unwrap();
        assert_eq!(plan.targets[0].delete_kind, DeleteKind::Prune);
        // The project directory reappears.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Vagrantfile"), "").unwrap();
        crate::services::local_runtime_vagrant_identity::test_support::register(
            &d, "master", "abc123",
        );
        let err = delete_instance(&runner, &LifecycleGuard::default(), plan)
            .await
            .unwrap_err();
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
        assert_eq!(err, STATE_CHANGED);
        assert!(runner.calls_for("vagrant", "destroy").is_empty());
        assert_eq!(runner.prunes(), 0);
    }

    #[tokio::test]
    async fn prune_plan_lists_every_stale_entry_and_a_new_one_aborts() {
        let live = project_dir("prune-live");
        let runner = FakeRunner::new();
        runner.set_vagrant(&[
            ("aaa111", "master", "/nonexistent-cd/one"),
            ("bbb222", "worker", "/nonexistent-cd/two"),
            ("ccc333", "live", &live),
        ]);
        let plan = plan_instance(
            &runner,
            DeletableRuntimeProvider::Vagrant,
            "master",
            Some("/nonexistent-cd/one"),
        )
        .await
        .unwrap();
        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].delete_kind, DeleteKind::Prune);
        let ids: Vec<_> = plan
            .stale_entries
            .iter()
            .map(|s| s.vagrant_id.as_str())
            .collect();
        assert_eq!(ids, ["aaa111", "bbb222"]);

        // A third stale entry appears: prune would now remove more than was confirmed.
        runner.set_vagrant(&[
            ("aaa111", "master", "/nonexistent-cd/one"),
            ("bbb222", "worker", "/nonexistent-cd/two"),
            ("ddd444", "extra", "/nonexistent-cd/three"),
        ]);
        let err = delete_instance(&runner, &LifecycleGuard::default(), plan)
            .await
            .unwrap_err();
        assert_eq!(err, STATE_CHANGED);
        assert_eq!(runner.prunes(), 0);
    }

    #[tokio::test]
    async fn matching_prune_plan_runs_one_prune_and_no_destroy() {
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("aaa111", "master", "/nonexistent-cd/one")]);
        let plan = plan_instance(
            &runner,
            DeletableRuntimeProvider::Vagrant,
            "master",
            Some("/nonexistent-cd/one"),
        )
        .await
        .unwrap();
        let res = delete_instance(&runner, &LifecycleGuard::default(), plan)
            .await
            .unwrap();
        assert!(res.success && res.message.contains("stale"));
        assert_eq!(runner.prunes(), 1);
        assert!(runner.calls_for("vagrant", "destroy").is_empty());
    }

    #[tokio::test]
    async fn tampered_plan_cannot_redirect_the_destroy() {
        let dir = project_dir("tamper");
        let other = project_dir("tamper-other");
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("abc123", "master", &dir), ("def456", "master", &other)]);
        let mut plan = vagrant_plan(&runner, &dir).await.unwrap();
        plan.targets[0].project_dir = Some(other);
        let err = delete_instance(&runner, &LifecycleGuard::default(), plan)
            .await
            .unwrap_err();
        assert_eq!(err, STATE_CHANGED);
        assert!(runner.calls_for("vagrant", "destroy").is_empty());
    }

    #[tokio::test]
    async fn colima_name_reused_after_recreate_is_the_same_target() {
        // Same name = same target: Colima has no id, so a recreate during the dialog is
        // indistinguishable by design (ADR-0012 D13).
        let runner = FakeRunner::new();
        *runner.colima_list.lock().unwrap() = COLIMA_LIST.to_string();
        let plan = plan_instance(&runner, DeletableRuntimeProvider::Colima, "default", None)
            .await
            .unwrap();
        let res = delete_instance(&runner, &LifecycleGuard::default(), plan)
            .await
            .unwrap();
        assert!(res.success);
        assert_eq!(
            runner.calls_for("colima", "delete")[0].0,
            vec!["delete", "--force", "--profile", "default"]
        );
    }

    #[tokio::test]
    async fn lima_delete_argv_and_only_its_provider_is_probed() {
        let runner = FakeRunner::new();
        *runner.lima_list.lock().unwrap() = LIMA_LIST.to_string();
        let plan = plan_instance(&runner, DeletableRuntimeProvider::Lima, "work", None)
            .await
            .unwrap();
        delete_instance(&runner, &LifecycleGuard::default(), plan)
            .await
            .unwrap();
        assert_eq!(
            runner.calls_for("limactl", "delete")[0].0,
            vec!["delete", "--force", "work"]
        );
        assert!(runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(b, _, _)| b == "limactl"));
    }

    #[tokio::test]
    async fn colima_vanished_before_execute_aborts() {
        let runner = FakeRunner::new();
        *runner.colima_list.lock().unwrap() = COLIMA_LIST.to_string();
        let plan = plan_instance(&runner, DeletableRuntimeProvider::Colima, "default", None)
            .await
            .unwrap();
        runner.colima_list.lock().unwrap().clear();
        let err = delete_instance(&runner, &LifecycleGuard::default(), plan)
            .await
            .unwrap_err();
        assert!(err.contains("not found"));
        assert!(runner.calls_for("colima", "delete").is_empty());
    }

    #[tokio::test]
    async fn busy_instance_aborts_before_planning() {
        let runner = FakeRunner::new();
        *runner.colima_list.lock().unwrap() = COLIMA_LIST.to_string();
        let plan = plan_instance(&runner, DeletableRuntimeProvider::Colima, "default", None)
            .await
            .unwrap();
        let guard = LifecycleGuard::default();
        let _held = guard
            .try_acquire(LocalRuntimeProvider::Colima.key("default"))
            .unwrap();
        let err = delete_instance(&runner, &guard, plan).await.unwrap_err();
        assert!(err.contains("already in progress"));
        assert!(runner.calls_for("colima", "delete").is_empty());
    }

    #[tokio::test]
    async fn plan_rejects_unknown_or_unsafe_targets_before_any_delete() {
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("abc123", "master", "/home/u/proj")]);
        // Not in the listing.
        let err = plan_instance(
            &runner,
            DeletableRuntimeProvider::Vagrant,
            "master",
            Some("/home/u/other"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("not found"));
        // Relative directory and missing directory.
        for bad in ["proj", "./proj", ""] {
            let err = plan_instance(
                &runner,
                DeletableRuntimeProvider::Vagrant,
                "master",
                Some(bad),
            )
            .await
            .unwrap_err();
            assert!(err.contains("invalid Vagrant project directory"), "{err}");
        }
        assert!(
            plan_instance(&runner, DeletableRuntimeProvider::Vagrant, "master", None)
                .await
                .is_err()
        );
        let err = plan_instance(&runner, DeletableRuntimeProvider::Colima, "-bad", None)
            .await
            .unwrap_err();
        assert!(err.contains("invalid instance name"));
        assert!(runner.calls_for("vagrant", "destroy").is_empty());
    }

    #[tokio::test]
    async fn live_entry_with_unsafe_id_or_without_vagrantfile_cannot_be_planned() {
        let dir = project_dir("unsafe");
        let runner = FakeRunner::new();
        runner.set_vagrant(&[("ab;c1", "master", &dir)]);
        assert!(vagrant_plan(&runner, &dir)
            .await
            .unwrap_err()
            .contains("unsafe"));

        let empty = std::env::temp_dir().join(format!("cd-plan-novf-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        let e = empty.to_string_lossy().into_owned();
        runner.set_vagrant(&[("abc123", "master", &e)]);
        let err = vagrant_plan(&runner, &e).await.unwrap_err();
        std::fs::remove_dir_all(&empty).ok();
        assert!(err.contains("no Vagrantfile"), "{err}");
    }

    #[tokio::test]
    async fn canonical_dir_rejects_relative_and_missing() {
        assert!(canonical_vagrant_project_dir("relative/dir").await.is_err());
        assert!(canonical_vagrant_project_dir("/nonexistent-cd/none")
            .await
            .unwrap_err()
            .contains("does not exist"));
        let dir = project_dir("canon");
        assert_eq!(canonical_vagrant_project_dir(&dir).await.unwrap(), dir);
    }

    mod identity {
        use super::*;
        use crate::services::local_runtime_vagrant_identity::test_support as idx;

        fn pdir(dir: &str) -> std::path::PathBuf {
            std::path::Path::new(dir).join(".vagrant/machines/master/virtualbox")
        }

        async fn planned(tag: &str) -> (FakeRunner, String, DeletePlan) {
            let dir = project_dir(tag);
            let runner = FakeRunner::new();
            runner.set_vagrant(&[("abc123", "master", &dir)]);
            let plan = vagrant_plan(&runner, &dir).await.unwrap();
            let want = format!("{}:virtualbox:vm-abc123", idx::full_id("abc123"));
            assert_eq!(
                plan.targets[0].machine_identity.as_deref(),
                Some(want.as_str())
            );
            (runner, dir, plan)
        }

        async fn assert_aborts(runner: &FakeRunner, plan: DeletePlan, expect: &str) {
            let err = delete_instance(runner, &LifecycleGuard::default(), plan)
                .await
                .unwrap_err();
            assert!(err.contains(expect), "{err}");
            assert!(runner.calls_for("vagrant", "destroy").is_empty());
        }

        #[tokio::test]
        async fn name_prefixing_another_index_id_is_refused_case_insensitively() {
            for other in ["master9999", "MASTERxyz"] {
                let dir = project_dir("prefix");
                let runner = FakeRunner::new();
                runner.set_vagrant(&[("abc123", "master", &dir)]);
                idx::add_index_entry(other, "other", None);
                let err = vagrant_plan(&runner, &dir).await.unwrap_err();
                assert!(err.contains("prefix of another"), "{err}");
            }
        }

        #[tokio::test]
        async fn prefix_appearing_after_confirm_aborts_before_destroy() {
            let (runner, _dir, plan) = planned("prefix-late").await;
            idx::add_index_entry("master1234", "other", None);
            assert_aborts(&runner, plan, "prefix of another").await;
        }

        #[tokio::test]
        async fn own_id_and_unrelated_ids_do_not_trigger_the_prefix_refusal() {
            let dir = project_dir("prefix-ok");
            let runner = FakeRunner::new();
            runner.set_vagrant(&[("abc123", "master", &dir)]);
            idx::add_index_entry("ffff0000", "other", None);
            assert!(vagrant_plan(&runner, &dir).await.is_ok());
        }

        #[tokio::test]
        async fn unreadable_or_corrupt_index_fails_closed() {
            let (runner, _dir, plan) = planned("index-bad").await;
            let path = idx::index_file();
            std::fs::write(&path, "{not json").unwrap();
            assert_aborts(&runner, plan.clone(), "not valid JSON").await;
            std::fs::write(&path, r#"{"version":1}"#).unwrap();
            assert_aborts(&runner, plan.clone(), "no machines map").await;
            std::fs::remove_file(&path).unwrap();
            assert_aborts(&runner, plan, "machine index").await;
        }

        #[tokio::test]
        async fn replaced_dot_vagrant_with_other_index_uuid_aborts() {
            let (runner, dir, plan) = planned("replaced-uuid").await;
            let other = format!("abc123{}", "1".repeat(26));
            idx::add_index_entry(&other, "master", Some(&dir));
            std::fs::write(pdir(&dir).join("index_uuid"), &other).unwrap();
            assert_aborts(&runner, plan, "State changed").await;
        }

        #[tokio::test]
        async fn replaced_dot_vagrant_with_other_provider_id_aborts() {
            let (runner, dir, plan) = planned("replaced-id").await;
            std::fs::write(pdir(&dir).join("id"), "vm-other").unwrap();
            assert_aborts(&runner, plan, "State changed").await;
        }

        #[tokio::test]
        async fn index_uuid_not_in_index_or_not_matching_listed_id_is_refused() {
            let (runner, dir, plan) = planned("uuid-unknown").await;
            std::fs::write(pdir(&dir).join("index_uuid"), "feedface").unwrap();
            assert_aborts(&runner, plan.clone(), "not in the Vagrant machine index").await;
            // In the index, but a different machine than the listed id.
            idx::add_index_entry("feedface", "master", Some(&dir));
            assert_aborts(&runner, plan, "does not match the listed").await;
        }

        #[tokio::test]
        async fn index_entry_for_another_project_or_name_is_refused() {
            let (runner, dir, plan) = planned("index-other").await;
            idx::add_index_entry(&idx::full_id("abc123"), "master", Some("/nonexistent-cd/p"));
            assert_aborts(&runner, plan.clone(), "another project").await;
            idx::add_index_entry(&idx::full_id("abc123"), "worker", Some(&dir));
            assert_aborts(&runner, plan, "different machine").await;
        }

        #[tokio::test]
        async fn missing_identity_file_or_ambiguous_provider_is_refused() {
            let (runner, dir, plan) = planned("missing").await;
            let other = pdir(&dir).parent().unwrap().join("vmware_desktop");
            std::fs::create_dir_all(&other).unwrap();
            // A provider dir with an `id` is active even without an `index_uuid`: ambiguous.
            std::fs::write(other.join("id"), "x").unwrap();
            assert_aborts(&runner, plan.clone(), "exactly one active provider").await;
            std::fs::remove_dir_all(&other).unwrap();
            std::fs::remove_file(pdir(&dir).join("id")).unwrap();
            assert_aborts(&runner, plan, "exactly one active provider").await;
        }

        #[tokio::test]
        async fn empty_or_id_less_leftover_provider_dir_is_ignored() {
            let dir = project_dir("leftover");
            let runner = FakeRunner::new();
            runner.set_vagrant(&[("abc123", "master", &dir)]);
            let machine = pdir(&dir).parent().unwrap().to_path_buf();
            std::fs::create_dir_all(machine.join("vmware_desktop")).unwrap();
            std::fs::create_dir_all(machine.join("libvirt")).unwrap();
            std::fs::write(machine.join("libvirt/index_uuid"), "x").unwrap();
            assert!(vagrant_plan(&runner, &dir).await.is_ok());
        }

        #[tokio::test]
        async fn active_provider_without_index_uuid_is_refused() {
            let dir = project_dir("no-uuid");
            let runner = FakeRunner::new();
            runner.set_vagrant(&[("abc123", "master", &dir)]);
            std::fs::remove_file(pdir(&dir).join("index_uuid")).unwrap();
            let err = vagrant_plan(&runner, &dir).await.unwrap_err();
            assert!(err.contains("missing index_uuid"), "{err}");
        }

        #[tokio::test]
        async fn id_only_second_provider_is_ambiguous_even_with_a_valid_first() {
            let dir = project_dir("id-only");
            let runner = FakeRunner::new();
            runner.set_vagrant(&[("abc123", "master", &dir)]);
            let other = pdir(&dir).parent().unwrap().join("vmware_desktop");
            std::fs::create_dir_all(&other).unwrap();
            std::fs::write(other.join("id"), "vm-x").unwrap();
            let err = vagrant_plan(&runner, &dir).await.unwrap_err();
            assert!(err.contains("exactly one active provider"), "{err}");
        }

        #[tokio::test]
        async fn symlinked_provider_entry_is_refused() {
            let (runner, dir, plan) = planned("symlink-provider").await;
            let machine = pdir(&dir).parent().unwrap().to_path_buf();
            let target = machine.join("virtualbox");
            let real = machine.join("real-provider");
            std::fs::rename(&target, &real).unwrap();
            std::os::unix::fs::symlink(&real, &target).unwrap();
            assert_aborts(&runner, plan, "symlinked provider").await;
            // A symlink that is not the active provider is refused too.
            std::fs::remove_file(&target).unwrap();
            std::fs::rename(&real, &target).unwrap();
            std::os::unix::fs::symlink("/nonexistent-cd", machine.join("ghost")).unwrap();
            let err = vagrant_plan(&runner, &dir).await.unwrap_err();
            assert!(err.contains("symlinked provider"), "{err}");
        }

        #[tokio::test]
        async fn path_style_provider_id_such_as_a_vmx_path_is_accepted() {
            let dir = project_dir("vmx");
            let runner = FakeRunner::new();
            runner.set_vagrant(&[("abc123", "master", &dir)]);
            std::fs::write(pdir(&dir).join("id"), "/p/with space/x.vmx\n").unwrap();
            let plan = vagrant_plan(&runner, &dir).await.unwrap();
            assert!(plan.targets[0]
                .machine_identity
                .as_deref()
                .unwrap()
                .ends_with(":virtualbox:/p/with space/x.vmx"));
        }

        #[tokio::test]
        async fn identity_file_symlinked_outside_the_project_is_refused() {
            let (runner, dir, plan) = planned("symlink").await;
            let outside = std::env::temp_dir().join(format!("cd-outside-{}", std::process::id()));
            std::fs::write(&outside, idx::full_id("abc123")).unwrap();
            let f = pdir(&dir).join("index_uuid");
            std::fs::remove_file(&f).unwrap();
            std::os::unix::fs::symlink(&outside, &f).unwrap();
            let r = delete_instance(&runner, &LifecycleGuard::default(), plan).await;
            std::fs::remove_file(&outside).ok();
            assert!(r.unwrap_err().contains("outside the project"));
            assert!(runner.calls_for("vagrant", "destroy").is_empty());
        }
    }
}
