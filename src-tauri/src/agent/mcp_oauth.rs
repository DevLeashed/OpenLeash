//! Native MCP OAuth. Credentials never enter Settings or IPC responses.
use super::store::{self, McpServerCfg};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::OnceLock, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize, Deserialize)]
struct Credential {
    resource: String,
    #[serde(default)]
    audience: String,
    client_id: String,
    token_endpoint: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: i64,
}
struct Flow {
    cfg: McpServerCfg,
    result: tokio::task::JoinHandle<Result<Credential, String>>,
}
static FLOWS: OnceLock<Mutex<HashMap<String, Flow>>> = OnceLock::new();
static CANCELS: OnceLock<Mutex<HashMap<String, (String, CancellationToken)>>> = OnceLock::new();
static SECRETS: OnceLock<Mutex<()>> = OnceLock::new();
fn flows() -> &'static Mutex<HashMap<String, Flow>> {
    FLOWS.get_or_init(Default::default)
}
fn random() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "OAuth HTTP client failed".into())
}
fn secure_url(s: &str) -> Result<reqwest::Url, String> {
    let u = reqwest::Url::parse(s).map_err(|_| "Invalid OAuth URL")?;
    if u.scheme() != "https"
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
    {
        return Err("OAuth endpoints require HTTPS without userinfo or fragments".into());
    }
    Ok(u)
}
fn public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_multicast()
                || v.is_broadcast()
                || o[0] == 0
                || o[0] >= 240
                || (o[0] == 100 && (64..=127).contains(&o[1])))
        }
        std::net::IpAddr::V6(v) => v
            .to_ipv4_mapped()
            .map(|v| public_ip(v.into()))
            .unwrap_or_else(|| {
                let s = v.segments();
                !(v.is_loopback()
                    || v.is_unspecified()
                    || v.is_multicast()
                    || s[0] & 0xfe00 == 0xfc00
                    || s[0] & 0xffc0 == 0xfe80)
            }),
    }
}
async fn endpoint_client(url: &reqwest::Url) -> Result<reqwest::Client, String> {
    let host = url.host_str().ok_or("OAuth URL missing host")?;
    let addresses: Vec<_> =
        tokio::net::lookup_host((host, url.port_or_known_default().unwrap_or(443)))
            .await
            .map_err(|_| "OAuth DNS resolution failed")?
            .collect();
    if addresses.is_empty() || addresses.iter().any(|a| !public_ip(a.ip())) {
        return Err("OAuth endpoints must resolve only to public internet addresses".into());
    }
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, &addresses)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "OAuth HTTP client failed".into())
}
async fn metadata(_c: &reqwest::Client, url: &str) -> Result<Value, String> {
    let url = secure_url(url)?;
    let r = endpoint_client(&url)
        .await?
        .get(url)
        .send()
        .await
        .map_err(|_| "OAuth discovery request failed")?;
    if !r.status().is_success() {
        return Err("OAuth discovery endpoint rejected request".into());
    }
    r.json().await.map_err(|_| "Invalid OAuth metadata".into())
}
fn well_known(u: &reqwest::Url, name: &str) -> String {
    format!(
        "{}/.well-known/{}{}",
        u.origin().ascii_serialization(),
        name,
        u.path().trim_end_matches('/')
    )
}
async fn discover(c: &reqwest::Client, resource: &str) -> Result<Value, String> {
    let u = secure_url(resource)?;
    let r = endpoint_client(&u)
        .await?
        .get(u.clone())
        .send()
        .await
        .map_err(|_| "MCP discovery request failed")?;
    let advertised = r
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split("resource_metadata=\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .map(String::from)
        });
    let resource_meta = match advertised {
        Some(s) => metadata(c, &s).await?,
        None => match metadata(c, &well_known(&u, "oauth-protected-resource")).await {
            Ok(v) => v,
            Err(_) => {
                metadata(
                    c,
                    &format!(
                        "{}/.well-known/oauth-protected-resource",
                        u.origin().ascii_serialization()
                    ),
                )
                .await?
            }
        },
    };
    let audience = resource_meta["resource"]
        .as_str()
        .ok_or("Missing OAuth resource identity")?;
    let audience_url = secure_url(audience)?;
    let path = audience_url.path().trim_end_matches('/');
    if audience_url.origin() != u.origin()
        || audience_url.query().is_some()
        || !(u.path() == path || u.path().starts_with(&format!("{path}/")))
    {
        return Err("OAuth resource metadata identity mismatch".into());
    }
    let issuer = resource_meta["authorization_servers"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .ok_or("No OAuth authorization server advertised")?;
    let issuer_url = secure_url(issuer)?;
    if issuer_url.query().is_some() {
        return Err("OAuth issuer must not contain a query".into());
    }
    let mut m = match metadata(c, &well_known(&issuer_url, "oauth-authorization-server")).await {
        Ok(v) => v,
        Err(_) => match metadata(c, &well_known(&issuer_url, "openid-configuration")).await {
            Ok(v) => v,
            Err(_) => {
                metadata(
                    c,
                    &format!(
                        "{}/.well-known/openid-configuration",
                        issuer.trim_end_matches('/')
                    ),
                )
                .await?
            }
        },
    };
    if m["issuer"].as_str().map(|s| s.trim_end_matches('/')) != Some(issuer.trim_end_matches('/')) {
        return Err("OAuth issuer mismatch".into());
    }
    if !m["code_challenge_methods_supported"]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == "S256"))
    {
        return Err("Authorization server does not advertise PKCE S256".into());
    }
    for key in ["authorization_endpoint", "token_endpoint"] {
        secure_url(m[key].as_str().ok_or("Incomplete OAuth metadata")?)?;
    }
    m["openleash_resource"] = json!(audience);
    Ok(m)
}
#[derive(Serialize)]
pub struct Started {
    pub flow_id: String,
    pub authorization_url: String,
}

pub async fn start(
    cfg: McpServerCfg,
    client_id: Option<String>,
    scopes: Vec<String>,
    redirect_port: Option<u16>,
) -> Result<Started, String> {
    if !cfg.oauth || !cfg.is_http() || cfg.enabled {
        return Err("Save a disabled HTTP OAuth server before connecting".into());
    }
    if let Some(e) = super::mcp::server_name_error(&cfg.name) {
        return Err(e);
    }
    let c = client()?;
    let m = discover(&c, &cfg.url).await?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, redirect_port.unwrap_or(0)))
        .await
        .map_err(|_| "Could not bind OAuth callback")?;
    let redirect = format!(
        "http://127.0.0.1:{}/callback",
        listener
            .local_addr()
            .map_err(|_| "Callback address failed")?
            .port()
    );
    let client_id = match client_id.filter(|s| !s.trim().is_empty()) {
        Some(id) => id,
        None => {
            let endpoint = secure_url(
                m["registration_endpoint"]
                    .as_str()
                    .ok_or("This server requires a registered public client ID")?,
            )?;
            let r = endpoint_client(&endpoint).await?.post(endpoint).json(&json!({"client_name":"OpenLeash","redirect_uris":[redirect],"grant_types":["authorization_code","refresh_token"],"response_types":["code"],"token_endpoint_auth_method":"none"})).send().await.map_err(|_| "OAuth registration failed")?;
            if !r.status().is_success() {
                return Err("OAuth registration rejected".into());
            }
            let v: Value = r
                .json()
                .await
                .map_err(|_| "Invalid registration response")?;
            if v.get("client_secret").is_some()
                || v["token_endpoint_auth_method"]
                    .as_str()
                    .is_some_and(|s| s != "none")
            {
                return Err("OAuth requires a public client without a client secret".into());
            }
            v["client_id"]
                .as_str()
                .ok_or("Registration did not supply a client ID")?
                .to_owned()
        }
    };
    let state = random();
    let verifier = random();
    let mut url = secure_url(
        m["authorization_endpoint"]
            .as_str()
            .ok_or("Missing authorization endpoint")?,
    )?;
    if url.query_pairs().any(|(k, _)| {
        [
            "response_type",
            "client_id",
            "redirect_uri",
            "state",
            "code_challenge",
            "code_challenge_method",
            "resource",
            "scope",
        ]
        .contains(&k.as_ref())
    }) {
        return Err("Authorization endpoint contains reserved OAuth parameters".into());
    }
    url.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", &client_id),
        ("redirect_uri", &redirect),
        ("state", &state),
        (
            "code_challenge",
            &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        ),
        ("code_challenge_method", "S256"),
        (
            "resource",
            m["openleash_resource"]
                .as_str()
                .ok_or("Missing OAuth audience")?,
        ),
    ]);
    if !scopes.is_empty() {
        url.query_pairs_mut()
            .append_pair("scope", &scopes.join(" "));
    }
    let endpoint = m["token_endpoint"]
        .as_str()
        .ok_or("Missing token endpoint")?
        .to_owned();
    let resource = cfg.url.clone();
    let audience = m["openleash_resource"]
        .as_str()
        .ok_or("Missing OAuth audience")?
        .to_owned();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let result = tokio::spawn(async move {
        tokio::select! {
            _ = stop.cancelled() => Err("OAuth cancelled".into()),
            result = tokio::time::timeout(Duration::from_secs(300), async {
                let code = callback(listener, &state).await?;
                let fields = [("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", &redirect), ("client_id", &client_id), ("code_verifier", &verifier), ("resource", &audience)];
                exchange(&c, &endpoint, &fields, Credential { audience: audience.clone(), resource: resource.clone(), client_id: client_id.clone(), token_endpoint: endpoint.clone(), access_token: String::new(), refresh_token: None, expires_at: 0 }).await
            }) => result.map_err(|_| "OAuth timed out".to_string())?,
        }
    });
    let id = random();
    {
        let mut registry = CANCELS.get_or_init(Default::default).lock().await;
        for (name, token) in registry.values() {
            if name == &cfg.name {
                token.cancel();
            }
        }
        registry.insert(id.clone(), (cfg.name.clone(), cancel.clone()));
    }
    flows()
        .lock()
        .await
        .insert(id.clone(), Flow { cfg, result });
    let cleanup_id = id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(360)).await;
        if let Some((_, token)) = CANCELS
            .get_or_init(Default::default)
            .lock()
            .await
            .remove(&cleanup_id)
        {
            token.cancel();
        }
        if let Some(flow) = flows().lock().await.remove(&cleanup_id) {
            flow.result.abort();
        }
    });
    Ok(Started {
        flow_id: id,
        authorization_url: url.into(),
    })
}
async fn callback(listener: TcpListener, state: &str) -> Result<String, String> {
    loop {
        let (mut stream, peer) = listener
            .accept()
            .await
            .map_err(|_| "OAuth callback failed")?;
        if !peer.ip().is_loopback() {
            continue;
        }
        let mut buf = Vec::new();
        let received = tokio::time::timeout(Duration::from_secs(5), async {
            let mut chunk = [0; 1024];
            while buf.len() < 8192 {
                let n = stream.read(&mut chunk).await?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    return Ok::<_, std::io::Error>(true);
                }
            }
            Ok(false)
        })
        .await;
        if !matches!(received, Ok(Ok(true))) {
            continue;
        }
        let request = String::from_utf8_lossy(&buf);
        let target = request
            .lines()
            .next()
            .and_then(|s| s.strip_prefix("GET "))
            .and_then(|s| s.strip_suffix(" HTTP/1.1"));
        let parsed = target.and_then(|s| reqwest::Url::parse(&format!("http://127.0.0.1{s}")).ok());
        let result = parsed.filter(|u| u.path() == "/callback").and_then(|u| {
            let pairs: Vec<_> = u.query_pairs().collect();
            if pairs.iter().filter(|(k, _)| k == "state").count() != 1
                || pairs
                    .iter()
                    .find(|(k, _)| k == "state")
                    .map(|(_, v)| v.as_ref())
                    != Some(state)
            {
                return None;
            }
            if pairs.iter().any(|(k, _)| k == "error") {
                return Some(Err("Authorization denied".into()));
            }
            if pairs.iter().filter(|(k, _)| k == "code").count() != 1 {
                return None;
            }
            pairs
                .iter()
                .find(|(k, _)| k == "code")
                .map(|(_, v)| Ok(v.to_string()))
        });
        let body = if result.is_some() {
            "Authorization received. Return to OpenLeash."
        } else {
            "Invalid callback."
        };
        let response = format!("HTTP/1.1 {}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", if result.is_some() { "200 OK" } else { "400 Bad Request" }, body.len(), body);
        let _ = stream.write_all(response.as_bytes()).await;
        if let Some(r) = result {
            return r;
        }
    }
}
async fn exchange(
    _c: &reqwest::Client,
    endpoint: &str,
    fields: &[(&str, &str)],
    mut cred: Credential,
) -> Result<Credential, String> {
    let url = secure_url(endpoint)?;
    let r = endpoint_client(&url)
        .await?
        .post(url)
        .form(fields)
        .send()
        .await
        .map_err(|_| "OAuth token request failed")?;
    if !r.status().is_success() {
        return Err("OAuth token request rejected; reconnect explicitly".into());
    }
    let v: Value = r.json().await.map_err(|_| "Invalid OAuth token response")?;
    if !v["token_type"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
    {
        return Err("Unsupported OAuth token type".into());
    }
    cred.access_token = v["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("Missing OAuth access token")?
        .into();
    if let Some(r) = v["refresh_token"].as_str() {
        cred.refresh_token = Some(r.into());
    }
    cred.expires_at = chrono::Utc::now()
        .timestamp()
        .saturating_add(v["expires_in"].as_i64().unwrap_or(3600).max(0));
    Ok(cred)
}
fn load() -> Result<HashMap<String, Credential>, String> {
    match std::fs::read(store::data_dir().join("mcp-oauth.json")) {
        Ok(b) => serde_json::from_slice(&b).map_err(|_| "Invalid MCP credential file".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(_) => Err("Could not read MCP credentials".into()),
    }
}
fn save(v: &HashMap<String, Credential>) -> Result<(), String> {
    let bytes = serde_json::to_vec(v).map_err(|_| "Could not encode MCP credentials")?;
    let path = store::data_dir().join("mcp-oauth.json");
    let temp = store::data_dir().join(format!(".mcp-oauth-{}.tmp", random()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let result = (|| {
        let mut file = options.open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(|_| "Could not save MCP credentials".into())
}
pub async fn wait(id: &str) -> Result<(McpServerCfg, PendingCredential), String> {
    let flow = flows()
        .lock()
        .await
        .remove(id)
        .ok_or("Unknown OAuth flow")?;
    let result = match flow.result.await {
        Ok(Ok(credential)) => credential,
        _ => {
            CANCELS
                .get_or_init(Default::default)
                .lock()
                .await
                .remove(id);
            return Err("OAuth flow failed or cancelled".into());
        }
    };
    Ok((flow.cfg, PendingCredential(result, id.to_owned())))
}
pub struct PendingCredential(Credential, String);
pub async fn persist(cfg: &McpServerCfg, cred: PendingCredential) -> Result<(), String> {
    let _guard = SECRETS.get_or_init(Default::default).lock().await;
    let mut cancellations = CANCELS.get_or_init(Default::default).lock().await;
    let (_, token) = cancellations
        .remove(&cred.1)
        .ok_or("OAuth flow no longer active")?;
    if token.is_cancelled() {
        return Err("OAuth cancelled".into());
    }
    let mut all = load()?;
    all.insert(cfg.name.clone(), cred.0);
    save(&all)
}
pub async fn cancel(id: &str) {
    if let Some((_, token)) = CANCELS.get_or_init(Default::default).lock().await.get(id) {
        token.cancel();
    }
}
pub async fn disconnect(name: &str) -> Result<(), String> {
    for (server, token) in CANCELS.get_or_init(Default::default).lock().await.values() {
        if server == name {
            token.cancel();
        }
    }
    let _guard = SECRETS.get_or_init(Default::default).lock().await;
    let mut all = load()?;
    all.remove(name);
    save(&all)
}
pub async fn access(cfg: &McpServerCfg) -> Result<String, String> {
    secure_url(&cfg.url)?;
    let _guard = SECRETS.get_or_init(Default::default).lock().await;
    let mut all = load()?;
    let mut cred = all
        .get(&cfg.name)
        .filter(|c| c.resource == cfg.url)
        .cloned()
        .ok_or("OAuth connection required")?;
    if cred.expires_at <= chrono::Utc::now().timestamp() + 60 {
        let refresh = cred
            .refresh_token
            .clone()
            .ok_or("OAuth expired; reconnect explicitly")?;
        let id = cred.client_id.clone();
        let resource = if cred.audience.is_empty() {
            cred.resource.clone()
        } else {
            cred.audience.clone()
        };
        let endpoint = cred.token_endpoint.clone();
        cred = exchange(
            &client()?,
            &endpoint,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", &refresh),
                ("client_id", &id),
                ("resource", &resource),
            ],
            cred,
        )
        .await?;
        all.insert(cfg.name.clone(), cred.clone());
        save(&all)?;
    }
    Ok(cred.access_token)
}
pub async fn invalidate_config_changes(
    old: &[McpServerCfg],
    new: &[McpServerCfg],
) -> Result<(), String> {
    for cfg in old {
        if !new
            .iter()
            .any(|next| same_identity(cfg, next) && cfg.enabled == next.enabled)
        {
            let cancellations = CANCELS.get_or_init(Default::default).lock().await;
            for (name, token) in cancellations.values() {
                if name == &cfg.name {
                    token.cancel();
                }
            }
        }
    }
    let removed: Vec<_> = old
        .iter()
        .filter(|cfg| cfg.oauth && !new.iter().any(|next| same_identity(cfg, next)))
        .map(|cfg| &cfg.name)
        .collect();
    if !removed.is_empty() {
        let _guard = SECRETS.get_or_init(Default::default).lock().await;
        let mut all = load()?;
        let before = all.len();
        all.retain(|name, _| !removed.contains(&name));
        if all.len() != before {
            save(&all)?;
        }
    }
    Ok(())
}
pub fn same_identity(a: &McpServerCfg, b: &McpServerCfg) -> bool {
    a.name == b.name
        && a.url == b.url
        && a.transport == b.transport
        && a.oauth == b.oauth
        && a.headers == b.headers
        && a.env == b.env
        && a.command == b.command
        && a.args == b.args
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connection_identity_includes_stdio_launch_configuration() {
        let cfg = McpServerCfg {
            command: "dummy-command".into(),
            args: vec!["old".into()],
            ..Default::default()
        };
        let mut changed = cfg.clone();
        changed.command = "different-command".into();
        assert!(!same_identity(&cfg, &changed));
        changed = cfg.clone();
        changed.args = vec!["new".into()];
        assert!(!same_identity(&cfg, &changed));
        changed = cfg.clone();
        changed.enabled = !cfg.enabled;
        assert!(same_identity(&cfg, &changed));
    }
    #[test]
    fn rejects_insecure_endpoints_and_userinfo() {
        for u in [
            "http://example.com/token",
            "https://user:secret@example.com/token",
            "https://example.com/token#fragment",
        ] {
            assert!(secure_url(u).is_err());
        }
        assert!(secure_url("https://example.com/token").is_ok());
    }
    #[test]
    fn pkce_s256_matches_rfc7636() {
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(
                b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
            )),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_ne!(random(), random());
        assert_eq!(random().len(), 64);
    }
    #[test]
    fn cancelled_completed_flow_cannot_persist_and_disconnect_deletes() {
        let dir = std::env::temp_dir().join(format!("ol-mcp-oauth-{}", random()));
        let _home = store::test_home(&dir);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let cfg = McpServerCfg {
                name: random(),
                url: "https://example.com/mcp".into(),
                transport: "http".into(),
                oauth: true,
                enabled: false,
                ..Default::default()
            };
            let credential = || Credential {
                audience: cfg.url.clone(),
                resource: cfg.url.clone(),
                client_id: "dummy-client".into(),
                token_endpoint: "https://example.com/token".into(),
                access_token: "dummy-access".into(),
                refresh_token: Some("dummy-refresh".into()),
                expires_at: i64::MAX,
            };
            let id = random();
            let token = CancellationToken::new();
            CANCELS
                .get_or_init(Default::default)
                .lock()
                .await
                .insert(id.clone(), (cfg.name.clone(), token.clone()));
            token.cancel();
            assert!(persist(&cfg, PendingCredential(credential(), id))
                .await
                .is_err());
            assert!(load().unwrap().is_empty());
            let id = random();
            CANCELS
                .get_or_init(Default::default)
                .lock()
                .await
                .insert(id.clone(), (cfg.name.clone(), CancellationToken::new()));
            persist(&cfg, PendingCredential(credential(), id))
                .await
                .unwrap();
            assert_eq!(access(&cfg).await.unwrap(), "dummy-access");
            let changed = McpServerCfg {
                url: "https://different.example/mcp".into(),
                ..cfg.clone()
            };
            assert!(access(&changed).await.is_err());
            disconnect(&cfg.name).await.unwrap();
            assert!(access(&cfg).await.is_err());
        });
        std::env::remove_var("OPENLEASH_HOME");
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn config_removal_invalidates_pending_flow() {
        let dir = std::env::temp_dir().join(format!("ol-mcp-invalidate-{}", random()));
        let _home = store::test_home(&dir);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let cfg = McpServerCfg {
                name: random(),
                ..Default::default()
            };
            let id = random();
            let token = CancellationToken::new();
            CANCELS
                .get_or_init(Default::default)
                .lock()
                .await
                .insert(id.clone(), (cfg.name.clone(), token.clone()));
            invalidate_config_changes(&[cfg], &[]).await.unwrap();
            assert!(token.is_cancelled());
            CANCELS
                .get_or_init(Default::default)
                .lock()
                .await
                .remove(&id);
        });
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn config_changes_delete_credentials_but_enabled_toggle_preserves_them() {
        let dir = std::env::temp_dir().join(format!("ol-mcp-credentials-{}", random()));
        let _home = store::test_home(&dir);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let cfg = McpServerCfg {
                name: "dummy-server".into(),
                url: "https://example.com/mcp".into(),
                transport: "http".into(),
                oauth: true,
                ..Default::default()
            };
            let credential = Credential {
                audience: cfg.url.clone(),
                resource: cfg.url.clone(),
                client_id: "dummy-client".into(),
                token_endpoint: "https://example.com/token".into(),
                access_token: "dummy-access".into(),
                refresh_token: None,
                expires_at: i64::MAX,
            };
            save(&HashMap::from([(cfg.name.clone(), credential.clone())])).unwrap();
            let toggled = McpServerCfg {
                enabled: !cfg.enabled,
                ..cfg.clone()
            };
            invalidate_config_changes(std::slice::from_ref(&cfg), &[toggled])
                .await
                .unwrap();
            assert!(load().unwrap().contains_key(&cfg.name));
            for change in ["remove", "url", "name", "transport", "oauth", "headers"] {
                save(&HashMap::from([(cfg.name.clone(), credential.clone())])).unwrap();
                let mut next = cfg.clone();
                match change {
                    "url" => next.url = "https://other.example/mcp".into(),
                    "name" => next.name = "different".into(),
                    "transport" => next.transport = "sse".into(),
                    "oauth" => next.oauth = false,
                    "headers" => {
                        next.headers.insert("X-Test".into(), "dummy".into());
                    }
                    _ => {}
                }
                let new = if change == "remove" {
                    vec![]
                } else {
                    vec![next]
                };
                invalidate_config_changes(std::slice::from_ref(&cfg), &new)
                    .await
                    .unwrap();
                assert!(load().unwrap().is_empty(), "{change}");
            }
            // Invalid storage must fail the settings mutation rather than leave
            // old credentials silently available to a recreated resource.
            std::fs::write(store::data_dir().join("mcp-oauth.json"), b"invalid").unwrap();
            assert!(invalidate_config_changes(&[cfg], &[]).await.is_err());
        });
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn private_metadata_addresses_are_rejected() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
    }
    #[test]
    fn path_discovery_uses_rfc8414_insertion() {
        let u = reqwest::Url::parse("https://example.com/tenant").unwrap();
        assert_eq!(
            well_known(&u, "oauth-authorization-server"),
            "https://example.com/.well-known/oauth-authorization-server/tenant"
        );
    }
    #[tokio::test]
    async fn callback_ignores_wrong_state() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(callback(listener, "expected"));
        for state in ["wrong", "expected"] {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.write_all(
                format!(
                    "GET /callback?state={state}&code=dummy HTTP/1.1\r\nHost: localhost\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
            let mut response = String::new();
            s.read_to_string(&mut response).await.unwrap();
            assert!(response.contains(if state == "wrong" { "400" } else { "200" }));
        }
        assert_eq!(handle.await.unwrap().unwrap(), "dummy");
    }
}
