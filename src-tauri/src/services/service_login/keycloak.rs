use std::path::Path;

use super::transport::{can_i_get_secret, get_json};
use super::workload::{ResolvedLogin, UserSource};
use super::{LoginAccount, LoginErrorCode, SecretValue};
use crate::services::k8s_endpoints::{decode_base64, DiscoveredEndpoint};
use crate::services::process::CommandRunner;
use crate::services::validate::{is_safe_host_domain, is_safe_sso_username_template};

// ADR-0014 SSO amendment: SSO-protected apps sign in with Keycloak realm users, whose
// passwords both measured clusters keep in `keycloak-user-passwords` (one data key per
// username) in the Keycloak Service's namespace. Cost: convention-based names, up to three
// Secret GETs per click; escape: delete this module and the registry/plan hooks.
pub(super) const REALM_USERS_SECRET: &str = "keycloak-user-passwords";
// Permanent admin Secrets only. The bootstrap `keycloak-initial-admin` is deliberately absent:
// it is commonly invalid after setup (ADR-0014 D2 amendment) and must never be read.
pub(super) const KEYCLOAK_ADMIN_SECRETS: [&str; 2] =
    ["keycloak-admin", "keycloak-admin-credential"];
const KEYCLOAK_SERVICES: [&str; 2] = ["keycloak", "keycloak-service"];
pub(super) const MAX_REALM_USERS: usize = 20;
const MAX_ISSUER_PATH: usize = 64;

pub(super) fn is_keycloak_service(service_name: &str) -> bool {
    KEYCLOAK_SERVICES.contains(&service_name)
}

fn is_safe_username(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'@' | b'-'))
}

fn decode_utf8(b64: &str) -> Result<String, LoginErrorCode> {
    let bytes = decode_base64(b64).ok_or(LoginErrorCode::KubectlFailed)?;
    String::from_utf8(bytes).map_err(|_| LoginErrorCode::KubectlFailed)
}

/// `can-i` then the authoritative GET; returns the Secret's `data` object.
pub(super) async fn read_secret_data(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    secret_name: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, LoginErrorCode> {
    can_i_get_secret(runner, kubeconfig, context, namespace, secret_name).await?;
    let api_path = format!("/api/v1/namespaces/{namespace}/secrets/{secret_name}");
    let val = get_json(
        runner,
        kubeconfig,
        context,
        &["secret", secret_name, "-n", namespace],
        &api_path,
    )
    .await?;
    val.get("data")
        .and_then(|d| d.as_object())
        .cloned()
        .ok_or(LoginErrorCode::NotFound)
}

/// One account from a Secret per `login` (password key required, username fixed or by key).
pub(super) async fn reveal_account(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    login: &ResolvedLogin,
    label: &str,
) -> Result<LoginAccount, LoginErrorCode> {
    let data = read_secret_data(runner, kubeconfig, context, namespace, &login.secret_name).await?;
    let raw_pw = data
        .get(login.password_key.as_str())
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or(LoginErrorCode::NotFound)?;
    let password = decode_utf8(raw_pw)?;
    let username = match &login.username {
        UserSource::Fixed(u) => Some(u.clone()),
        UserSource::SecretKey(k) => match data.get(k.as_str()).and_then(|v| v.as_str()) {
            Some(b64) if !b64.is_empty() => Some(decode_utf8(b64)?),
            _ => None,
        },
    };
    Ok(LoginAccount {
        label: label.to_string(),
        key: None,
        username,
        password: SecretValue::new(password),
    })
}

/// First existing permanent Keycloak admin Secret; a missing or unreadable candidate moves on
/// to the next. Returns the account and the Secret that supplied it.
pub(super) async fn reveal_keycloak_admin(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
) -> (String, Result<LoginAccount, LoginErrorCode>) {
    let mut first_error: Option<LoginErrorCode> = None;
    for name in KEYCLOAK_ADMIN_SECRETS {
        let login = ResolvedLogin {
            secret_name: name.to_string(),
            username: UserSource::SecretKey("username".to_string()),
            password_key: "password".to_string(),
        };
        match reveal_account(
            runner,
            kubeconfig,
            context,
            namespace,
            &login,
            "Admin console (master realm)",
        )
        .await
        {
            Ok(account) => return (name.to_string(), Ok(account)),
            Err(error) => {
                // A concrete failure (forbidden, transport) is more useful than "not found".
                if first_error.is_none() || first_error == Some(LoginErrorCode::NotFound) {
                    first_error = Some(error);
                }
            }
        }
    }
    (
        KEYCLOAK_ADMIN_SECRETS[0].to_string(),
        Err(first_error.unwrap_or(LoginErrorCode::NotFound)),
    )
}

/// Realm users: one account per safe data key (capped), password = decoded value. The shown
/// username is the profile's SSO username template applied to the key (LDAP-federated realms
/// often prefix it); a result that is not a safe username drops the account.
pub(super) async fn reveal_realm_users(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    username_template: Option<&str>,
) -> Result<Vec<LoginAccount>, LoginErrorCode> {
    let template = username_template.filter(|t| is_safe_sso_username_template(t));
    let data = read_secret_data(runner, kubeconfig, context, namespace, REALM_USERS_SECRET).await?;
    let mut keys: Vec<&String> = data.keys().filter(|k| is_safe_username(k)).collect();
    keys.sort();
    let accounts: Vec<LoginAccount> = keys
        .into_iter()
        .filter_map(|key| {
            let password = decode_utf8(data.get(key)?.as_str().filter(|s| !s.is_empty())?).ok()?;
            let username = template.map_or_else(|| key.clone(), |t| t.replacen("{key}", key, 1));
            is_safe_username(&username).then_some(())?;
            Some(LoginAccount {
                label: "Realm user".to_string(),
                key: Some(key.clone()),
                username: Some(username),
                password: SecretValue::new(password),
            })
        })
        .take(MAX_REALM_USERS)
        .collect();
    if accounts.is_empty() {
        Err(LoginErrorCode::NotFound)
    } else {
        Ok(accounts)
    }
}

/// `https://<host>/realms/<realm>` -> (host, realm); anything else is not evidence.
pub(super) fn parse_realm_url(value: &str) -> Option<(String, String)> {
    let rest = value.strip_prefix("https://")?;
    let (host, path) = rest.split_once("/realms/")?;
    let realm = path.trim_end_matches('/');
    let realm_ok = !realm.is_empty()
        && realm.len() <= MAX_ISSUER_PATH
        && realm
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    (is_safe_host_domain(host) && realm_ok).then(|| (host.to_string(), realm.to_string()))
}

/// Every literal env on a qualified pod whose name contains `ISSUER` and whose value is a
/// Keycloak realm URL. Values from `valueFrom` (Secrets/ConfigMaps) are never read or resolved.
fn pod_issuers(pod: &serde_json::Value) -> Vec<(String, String)> {
    pod.pointer("/spec/containers")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("env")?.as_array())
        .flatten()
        .filter_map(|env| {
            let name = env.get("name")?.as_str()?;
            if !name.to_ascii_uppercase().contains("ISSUER") {
                return None;
            }
            parse_realm_url(env.get("value")?.as_str()?)
        })
        .collect()
}

/// Issuer `(host, realm)` advertised by the endpoint's qualified backend pods. Pods or
/// containers that disagree (rollout, several `*ISSUER*` envs) give no evidence rather than a
/// first-wins pick.
pub(super) fn workload_issuer(pods: &[serde_json::Value]) -> Option<(String, String)> {
    let mut issuers: Vec<(String, String)> = pods.iter().flat_map(pod_issuers).collect();
    issuers.sort();
    issuers.dedup();
    match issuers.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

/// An env *name* mentioning OAuth/OIDC on a qualified backend pod hints that the app signs in
/// through an identity provider. Values are never read (they may be client secrets).
pub(super) fn workload_oauth_env(pods: &[serde_json::Value]) -> bool {
    pods.iter().any(|pod| {
        pod.pointer("/spec/containers")
            .and_then(|c| c.as_array())
            .into_iter()
            .flatten()
            .filter_map(|c| c.get("env")?.as_array())
            .flatten()
            .filter_map(|env| env.get("name")?.as_str())
            .any(|name| {
                let name = name.to_ascii_uppercase();
                name.contains("OAUTH") || name.contains("OIDC")
            })
    })
}

fn backend_service_name(ep: &DiscoveredEndpoint) -> Option<&str> {
    ep.backend_service
        .as_deref()?
        .split_once('/')
        .map(|(_, s)| s)
}

pub(super) fn has_keycloak_endpoint(endpoints: &[DiscoveredEndpoint]) -> bool {
    endpoints
        .iter()
        .any(|ep| backend_service_name(ep).is_some_and(is_keycloak_service))
}

/// The cluster's Keycloak endpoint from the profile's own stored endpoints: the one whose host
/// equals the issuer host, else the only distinct Keycloak backend. Ambiguity (several distinct
/// backends on the issuer host, or several with no host match) yields `None` (no guess) so a
/// look-alike Service cannot lend its Secret and cache order never decides.
pub(super) fn find_keycloak_endpoint<'a>(
    endpoints: &'a [DiscoveredEndpoint],
    issuer_host: Option<&str>,
) -> Option<&'a DiscoveredEndpoint> {
    let candidates: Vec<&DiscoveredEndpoint> = endpoints
        .iter()
        .filter(|ep| backend_service_name(ep).is_some_and(is_keycloak_service))
        .collect();
    let unique = |list: Vec<&'a DiscoveredEndpoint>| {
        let first = *list.first()?;
        list.iter()
            .all(|ep| ep.backend_service == first.backend_service)
            .then_some(first)
    };
    if let Some(host) = issuer_host {
        let matching: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|ep| ep.host == host)
            .collect();
        if !matching.is_empty() {
            return unique(matching);
        }
    }
    unique(candidates)
}
