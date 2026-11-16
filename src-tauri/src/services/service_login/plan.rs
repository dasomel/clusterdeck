use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use tokio::time::{timeout_at, Instant};

use super::backend::validate_live_backend;
use super::keycloak::{
    find_keycloak_endpoint, has_keycloak_endpoint, is_keycloak_service, reveal_account,
    reveal_keycloak_admin, reveal_realm_users, workload_issuer, workload_oauth_env,
    KEYCLOAK_ADMIN_SECRETS, REALM_USERS_SECRET,
};
use super::snapshot::KubeconfigSnapshot;
use super::sso_probe::probe_login_surface;
use super::transport::can_i_get_secret;
use super::workload::{self, qualified_pods, ResolvedLogin};
use super::{
    endpoint_login_options_for_slice, lookup_service_login, option_for, resolve_backend,
    resolve_context_and_check_impersonation, EndpointLoginOption, LoginAccount, LoginErrorCode,
    LoginGroup, RevealedEndpointLogin, SsoHint,
};
use crate::commands::local_dns::cached_endpoints;
use crate::services::k8s_endpoints::DiscoveredEndpoint;
use crate::services::paths::ClusterDeckPaths;
use crate::services::process::CommandRunner;
use crate::services::store;
use crate::services::validate::{is_safe_k8s_dns_label, is_safe_profile_id};

/// Endpoints probed for a workload issuer per options load; each costs two bounded reads.
const MAX_ISSUER_PROBES: usize = 20;
/// Concurrent login-surface HTTP probes during one options load.
const HTTP_PROBE_CONCURRENCY: usize = 4;
/// Overall budget for the options load's cluster reads and probes; unresolved backends then
/// contribute no SSO evidence while registry options are still returned.
const OPTIONS_DEADLINE: Duration = Duration::from_secs(15);

enum Source {
    /// The app's own admin Secret (registry, possibly workload-derived).
    Admin(ResolvedLogin),
    KeycloakAdmin,
    RealmUsers,
    /// Plan-time failure (e.g. the Keycloak endpoint's backend no longer validates).
    Failed(LoginErrorCode),
}

struct GroupPlan {
    label: String,
    namespace: String,
    secret_name: String,
    source: Source,
}

struct Plan {
    namespace: String,
    service_name: String,
    groups: Vec<GroupPlan>,
    sso: Option<SsoHint>,
}

/// Everything one click may read, decided from the stored endpoint plus bounded live reads.
/// Evidence for the SSO group is only the route plugin flag or a literal issuer env on a
/// qualified backend pod; Secrets are never opened to find it (ADR-0014 SSO amendment).
async fn plan_groups(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    ep: &DiscoveredEndpoint,
    endpoints: &[DiscoveredEndpoint],
) -> Result<Plan, LoginErrorCode> {
    let (namespace, service_name) = resolve_backend(ep)?;
    let service =
        validate_live_backend(runner, kubeconfig, context, ep, &namespace, &service_name).await?;
    let mut groups = Vec::new();
    let mut sso = None;
    if is_keycloak_service(&service_name) {
        groups.push(GroupPlan {
            label: "Admin console (master realm)".to_string(),
            namespace: namespace.clone(),
            secret_name: KEYCLOAK_ADMIN_SECRETS[0].to_string(),
            source: Source::KeycloakAdmin,
        });
        groups.push(GroupPlan {
            label: "Realm users".to_string(),
            namespace: namespace.clone(),
            secret_name: REALM_USERS_SECRET.to_string(),
            source: Source::RealmUsers,
        });
    } else {
        // One bounded pod read feeds both the workload-derived Secret and the issuer evidence.
        let pods = qualified_pods(runner, kubeconfig, context, &namespace, &service).await;
        if let Some(spec) = lookup_service_login(&service_name) {
            if !is_safe_k8s_dns_label(&spec.secret_name) {
                return Err(LoginErrorCode::UnsupportedService);
            }
            let login = workload::resolve(pods.as_deref(), &spec);
            groups.push(GroupPlan {
                label: "Admin account".to_string(),
                namespace: namespace.clone(),
                secret_name: login.secret_name.clone(),
                source: Source::Admin(login),
            });
        }
        // Evidence order: route plugin, literal issuer env, OAuth/OIDC env names, then an HTTP
        // probe. The last two only attach when the profile has exactly one Keycloak backend,
        // so an ambiguous cluster never lends its realm users (ADR-0014 SSO amendment 2).
        let issuer = pods.as_deref().and_then(workload_issuer);
        let mut evidence = (ep.sso_oidc || issuer.is_some()).then(|| match &issuer {
            Some((host, realm)) => (Some(host.clone()), Some(realm.clone())),
            None => (None, None),
        });
        if evidence.is_none() && find_keycloak_endpoint(endpoints, None).is_some() {
            if pods.as_deref().is_some_and(workload_oauth_env) {
                evidence = Some((None, None));
            } else if let Some(found) = probe_login_surface(runner, ep).await {
                evidence = Some((None, found.realm));
            }
        }
        if let Some((issuer_host, realm)) = evidence {
            if let Some(kc_ep) = find_keycloak_endpoint(endpoints, issuer_host.as_deref()) {
                sso = Some(SsoHint {
                    realm: realm.clone(),
                });
                let label = format!(
                    "SSO (Keycloak realm {})",
                    realm.as_deref().unwrap_or("unknown")
                );
                groups.push(sso_group(runner, kubeconfig, context, kc_ep, label).await);
            }
        }
    }
    if groups.is_empty() {
        return Err(LoginErrorCode::UnsupportedService);
    }
    Ok(Plan {
        namespace,
        service_name,
        groups,
        sso,
    })
}

/// Realm-users group taken from the Keycloak endpoint's live-validated backend namespace.
async fn sso_group(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    kc_ep: &DiscoveredEndpoint,
    label: String,
) -> GroupPlan {
    let resolved = async {
        let (kc_ns, kc_svc) = resolve_backend(kc_ep)?;
        validate_live_backend(runner, kubeconfig, context, kc_ep, &kc_ns, &kc_svc).await?;
        Ok::<_, LoginErrorCode>(kc_ns)
    }
    .await;
    match resolved {
        Ok(namespace) => GroupPlan {
            label,
            namespace,
            secret_name: REALM_USERS_SECRET.to_string(),
            source: Source::RealmUsers,
        },
        Err(error) => GroupPlan {
            label,
            namespace: String::new(),
            secret_name: REALM_USERS_SECRET.to_string(),
            source: Source::Failed(error),
        },
    }
}

struct Session {
    snapshot: KubeconfigSnapshot,
    context: String,
    endpoints: Vec<DiscoveredEndpoint>,
    username_template: Option<String>,
}

fn open_session(paths: &ClusterDeckPaths, profile_id: &str) -> Result<Session, LoginErrorCode> {
    if !is_safe_profile_id(profile_id) {
        return Err(LoginErrorCode::NotFound);
    }
    let profile = store::get_profile(paths, profile_id).map_err(|_| LoginErrorCode::NotFound)?;
    let raw = std::fs::read(paths.kubeconfig_file(&profile.id))
        .map_err(|_| LoginErrorCode::KubectlFailed)?;
    let context = resolve_context_and_check_impersonation(&profile, &raw)?;
    let snapshot = KubeconfigSnapshot::create(paths, &raw)?;
    let endpoints = cached_endpoints(&paths.endpoints_cache_file(), profile_id);
    Ok(Session {
        snapshot,
        context,
        endpoints,
        username_template: profile.sso_username_template.clone(),
    })
}

fn find_endpoint<'a>(
    endpoints: &'a [DiscoveredEndpoint],
    endpoint_id: &str,
) -> Result<&'a DiscoveredEndpoint, LoginErrorCode> {
    endpoints
        .iter()
        .find(|e| e.endpoint_id() == endpoint_id)
        .ok_or(LoginErrorCode::NotFound)
}

/// Options without cluster calls, plus endpoints whose backend pod advertises a Keycloak issuer
/// (no plugin flag, no registry entry: e.g. a manager UI). Cost: two bounded reads per distinct
/// unknown backend, capped; escape: return `endpoint_login_options_for_slice` only.
pub async fn endpoint_login_options_with_deps(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile_id: &str,
) -> Result<Vec<EndpointLoginOption>, LoginErrorCode> {
    if !is_safe_profile_id(profile_id) {
        return Err(LoginErrorCode::NotFound);
    }
    let _ = store::get_profile(paths, profile_id).map_err(|_| LoginErrorCode::NotFound)?;
    let endpoints = cached_endpoints(&paths.endpoints_cache_file(), profile_id);
    let mut options = endpoint_login_options_for_slice(&endpoints);
    if !has_keycloak_endpoint(&endpoints) {
        return Ok(options);
    }
    // Weak sources (env names, HTTP probe) need exactly one Keycloak backend; a literal issuer
    // env may still pick one of several by host.
    let single_keycloak = find_keycloak_endpoint(&endpoints, None).is_some();
    let known: Vec<String> = options.iter().map(|o| o.endpoint_id.clone()).collect();
    let mut candidates: Vec<&DiscoveredEndpoint> = Vec::new();
    for ep in &endpoints {
        let unknown = resolve_backend(ep).is_ok_and(|(_, svc)| !is_keycloak_service(&svc));
        let same_backend = |c: &&DiscoveredEndpoint| {
            c.backend_service == ep.backend_service && c.backend_upstream == ep.backend_upstream
        };
        if unknown && !known.contains(&ep.endpoint_id()) && !candidates.iter().any(same_backend) {
            candidates.push(ep);
        }
    }
    candidates.truncate(MAX_ISSUER_PROBES);
    if candidates.is_empty() {
        return Ok(options);
    }
    // Any setup failure (impersonation, unreadable kubeconfig) just means no probed options.
    let Ok(session) = open_session(paths, profile_id) else {
        return Ok(options);
    };
    let (kubeconfig, context) = (session.snapshot.path(), session.context.as_str());
    let deadline = Instant::now() + OPTIONS_DEADLINE;
    let mut evidence: HashMap<BackendKey, SsoHint> = HashMap::new();
    let mut validated: Vec<BackendKey> = Vec::new();
    for batch in candidates.chunks(HTTP_PROBE_CONCURRENCY) {
        if Instant::now() >= deadline {
            break;
        }
        // Per-task deadline: a late backend contributes nothing but never discards the others.
        let results = futures::future::join_all(batch.iter().map(|ep| async {
            timeout_at(
                deadline,
                backend_evidence(runner, kubeconfig, context, ep, &endpoints, single_keycloak),
            )
            .await
            .ok()
            .flatten()
        }))
        .await;
        for (key, hint) in results.into_iter().flatten() {
            validated.push(key.clone());
            if let Some(hint) = hint {
                evidence.insert(key, hint);
            }
        }
    }
    // Endpoints still without evidence get at most one HTTP probe pair each (bounded).
    let to_probe: Vec<&DiscoveredEndpoint> = if single_keycloak {
        endpoints
            .iter()
            .filter(|ep| !known.contains(&ep.endpoint_id()))
            .filter(|ep| {
                let key = (ep.backend_service.clone(), ep.backend_upstream.clone());
                validated.contains(&key) && !evidence.contains_key(&key)
            })
            .take(MAX_ISSUER_PROBES)
            .collect()
    } else {
        Vec::new()
    };
    let mut probed: Vec<(&DiscoveredEndpoint, Option<SsoHint>)> = Vec::new();
    for batch in to_probe.chunks(HTTP_PROBE_CONCURRENCY) {
        if Instant::now() >= deadline {
            break;
        }
        let results = futures::future::join_all(batch.iter().map(|ep| async move {
            let hint = timeout_at(deadline, probe_login_surface(runner, ep))
                .await
                .ok()
                .flatten()
                .map(|found| SsoHint { realm: found.realm });
            (*ep, hint)
        }))
        .await;
        probed.extend(results);
    }
    let probed: HashMap<String, SsoHint> = probed
        .into_iter()
        .filter_map(|(ep, hint)| Some((ep.endpoint_id(), hint?)))
        .collect();
    for ep in &endpoints {
        let key = (ep.backend_service.clone(), ep.backend_upstream.clone());
        let hint = evidence.get(&key).or_else(|| probed.get(&ep.endpoint_id()));
        if let (Some(hint), Ok((namespace, service_name))) = (hint, resolve_backend(ep)) {
            if !known.contains(&ep.endpoint_id()) {
                options.push(option_for(
                    ep,
                    namespace,
                    service_name,
                    REALM_USERS_SECRET.to_string(),
                    Some(hint.clone()),
                ));
            }
        }
    }
    Ok(options)
}

type BackendKey = (Option<String>, Option<String>);

/// One backend's live validation plus pod-derived SSO evidence for the options listing.
/// `None` when the backend does not validate (no option, no probe).
async fn backend_evidence(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    ep: &DiscoveredEndpoint,
    endpoints: &[DiscoveredEndpoint],
    single_keycloak: bool,
) -> Option<(BackendKey, Option<SsoHint>)> {
    let (namespace, service_name) = resolve_backend(ep).ok()?;
    let service = validate_live_backend(runner, kubeconfig, context, ep, &namespace, &service_name)
        .await
        .ok()?;
    let key = (ep.backend_service.clone(), ep.backend_upstream.clone());
    let pods = qualified_pods(runner, kubeconfig, context, &namespace, &service).await;
    let hint = if let Some((host, realm)) = pods.as_deref().and_then(workload_issuer) {
        find_keycloak_endpoint(endpoints, Some(&host)).map(|_| SsoHint { realm: Some(realm) })
    } else if single_keycloak && pods.as_deref().is_some_and(workload_oauth_env) {
        Some(SsoHint { realm: None })
    } else {
        None
    };
    Some((key, hint))
}

async fn permitted(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    group: &GroupPlan,
) -> Result<(), LoginErrorCode> {
    let names: Vec<&str> = match &group.source {
        Source::Failed(error) => return Err(*error),
        Source::KeycloakAdmin => KEYCLOAK_ADMIN_SECRETS.to_vec(),
        _ => vec![group.secret_name.as_str()],
    };
    let mut first_error = None;
    for name in names {
        match can_i_get_secret(runner, kubeconfig, context, &group.namespace, name).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    Err(first_error.unwrap_or(LoginErrorCode::KubectlFailed))
}

/// Enabled when at least one source is readable; the reveal reports per-source failures.
pub async fn check_endpoint_login_with_deps(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile_id: &str,
    endpoint_id: &str,
) -> Result<EndpointLoginOption, LoginErrorCode> {
    if endpoint_id.is_empty() {
        return Err(LoginErrorCode::NotFound);
    }
    let session = open_session(paths, profile_id)?;
    let (kubeconfig, context) = (session.snapshot.path(), session.context.as_str());
    let ep = find_endpoint(&session.endpoints, endpoint_id)?;
    let plan = plan_groups(runner, kubeconfig, context, ep, &session.endpoints).await?;
    let mut first_error = None;
    let mut any_permitted = false;
    for group in &plan.groups {
        match permitted(runner, kubeconfig, context, group).await {
            Ok(()) => any_permitted = true,
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    if !any_permitted {
        return Err(first_error.unwrap_or(LoginErrorCode::KubectlFailed));
    }
    let secret_name = plan.groups[0].secret_name.clone();
    Ok(option_for(
        ep,
        plan.namespace,
        plan.service_name,
        secret_name,
        plan.sso,
    ))
}

async fn reveal_group(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    group: &mut GroupPlan,
    username_template: Option<&str>,
) -> Result<Vec<LoginAccount>, LoginErrorCode> {
    let namespace = group.namespace.as_str();
    match &group.source {
        Source::Failed(error) => Err(*error),
        Source::Admin(login) => reveal_account(
            runner,
            kubeconfig,
            context,
            namespace,
            login,
            "Admin account",
        )
        .await
        .map(|account| vec![account]),
        Source::KeycloakAdmin => {
            let (secret_name, account) =
                reveal_keycloak_admin(runner, kubeconfig, context, namespace).await;
            group.secret_name = secret_name;
            account.map(|account| vec![account])
        }
        Source::RealmUsers => {
            reveal_realm_users(runner, kubeconfig, context, namespace, username_template).await
        }
    }
}

/// Reads every applicable source. Partial results are returned with per-group error codes; the
/// call only fails when no source yields an account, and then with the first source's error.
pub async fn reveal_endpoint_login_with_deps(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile_id: &str,
    endpoint_id: &str,
) -> Result<RevealedEndpointLogin, LoginErrorCode> {
    if endpoint_id.is_empty() {
        return Err(LoginErrorCode::NotFound);
    }
    let session = open_session(paths, profile_id)?;
    let (kubeconfig, context) = (session.snapshot.path(), session.context.as_str());
    let ep = find_endpoint(&session.endpoints, endpoint_id)?;
    let plan = plan_groups(runner, kubeconfig, context, ep, &session.endpoints).await?;
    let mut groups = Vec::new();
    let mut first_error = None;
    for mut group in plan.groups {
        let result = reveal_group(
            runner,
            kubeconfig,
            context,
            &mut group,
            session.username_template.as_deref(),
        )
        .await;
        let (accounts, error) = match result {
            Ok(accounts) => (accounts, None),
            Err(error) => {
                first_error.get_or_insert(error);
                (Vec::new(), Some(error))
            }
        };
        groups.push(LoginGroup {
            label: group.label,
            namespace: group.namespace,
            secret_name: group.secret_name,
            accounts,
            error,
        });
    }
    if groups.iter().all(|g| g.accounts.is_empty()) {
        return Err(first_error.unwrap_or(LoginErrorCode::NotFound));
    }
    Ok(RevealedEndpointLogin {
        endpoint_id: ep.endpoint_id(),
        route_namespace: super::route_namespace(ep),
        route_resource: ep.resource_name.clone(),
        host: ep.host.clone(),
        namespace: plan.namespace,
        service_name: plan.service_name,
        groups,
    })
}
