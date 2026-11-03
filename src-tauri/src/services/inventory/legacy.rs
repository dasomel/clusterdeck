use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::apply::Target;
use crate::services::config::Profile;
use crate::services::validate::is_safe_path_segment;

fn legacy_id(project: &Path) -> Option<String> {
    let name = project.file_name()?.to_str()?;
    // Match ProfileEditor deriveBaseId: only the provider is lowercased. JS's non-Unicode
    // regex replaces each UTF-16 code unit, including both halves of an astral character.
    let suffix: String = name
        .encode_utf16()
        .map(|c| match c {
            97..=122 | 48..=57 | 95 | 45 => char::from_u32(c.into()).unwrap(),
            _ => '-',
        })
        .collect();
    Some(format!("vagrant-{suffix}"))
}

pub(super) fn candidate(
    profiles: &[Profile],
    target: &Target,
    canonicalize: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Option<usize> {
    let project = target.vagrant_project_dir.as_deref()?;
    let id = legacy_id(project)?;
    let names: HashSet<_> = target.hosts.iter().map(|h| h.name.as_str()).collect();
    if names.is_empty()
        || names.len() != target.hosts.len()
        || names.iter().any(|name| !is_safe_path_segment(name))
    {
        return None;
    }
    let machines = canonicalize(project)?.join(".vagrant/machines");

    // D1 (#131): Recover old detection IDs only with exact names and project-local key
    // evidence, never addresses. Cost: blocking canonicalization; missing/ambiguous evidence
    // falls back to normal ID matching/collision refusal. Remove this bridge after migration.
    let mut candidates = profiles.iter().enumerate().filter(|(_, p)| {
        p.id == id
            && p.hosts.len() == names.len()
            && p.hosts
                .iter()
                .map(|h| h.name.as_str())
                .collect::<HashSet<_>>()
                == names
            && p.hosts.iter().all(|host| {
                host.identity_file
                    .as_deref()
                    .and_then(|key| canonicalize(Path::new(key)))
                    .is_some_and(|key| {
                        let machine = machines.join(&host.name);
                        key != machine && key.starts_with(machine)
                    })
            })
    });
    let (idx, _) = candidates.next()?;
    candidates.next().is_none().then_some(idx)
}

#[cfg(test)]
#[path = "legacy_tests.rs"]
mod tests;
