//! OAuth 2.0 browser flow for MCP servers — discovery, PKCE, token exchange,
//! and local callback handling.

use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tiny_http::{Response, Server};
use url::Url;

use crate::constants::APP_NAME;
use crate::mcp::config::{McpAuth, McpServer};
use crate::util::normalize_url;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug)]
#[allow(dead_code)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
    pub scope: Option<String>,
    pub token_type: Option<String>,
    pub client_id: Option<String>,
}

/// Captures the in-flight OAuth state so the user can manually complete the flow
/// by pasting the authorization code or redirect URL.
#[derive(Clone, Debug)]
pub struct PendingOAuth {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub code_verifier: String,
    pub state: String,
    pub token_endpoint: String,
    pub resource_value: String,
}

#[derive(Debug, Deserialize)]
struct AuthServerMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    scopes_supported: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ProtectedResourceMetadata {
    #[serde(default)]
    resource: Option<String>,
    #[serde(default)]
    authorization_servers: Option<Vec<String>>,
    #[serde(default)]
    scopes_supported: Option<Vec<String>>,
}

#[derive(Debug)]
struct ResourceDiscovery {
    metadata: Option<ProtectedResourceMetadata>,
    scope_hint: Option<String>,
    auth_server_hint: Option<String>,
    resource_hint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegistrationResponse {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
}

#[derive(Debug)]
struct OAuthCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

fn resource_identifier(server: &McpServer) -> Result<Url> {
    let raw = normalize_url(&server.url);
    let mut url = Url::parse(&raw).context("parse resource URL")?;
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/').to_string();
    if path.is_empty() {
        url.set_path("/");
    } else {
        url.set_path(&path);
    }
    Ok(url)
}

async fn discover_auth_metadata(
    http: &Client,
    auth: &McpAuth,
    issuers: &[Url],
) -> Result<AuthServerMetadata> {
    if let (Some(auth_url), Some(token_url)) = (
        auth.authorization_endpoint.as_ref(),
        auth.token_endpoint.as_ref(),
    ) {
        return Ok(AuthServerMetadata {
            authorization_endpoint: auth_url.clone(),
            token_endpoint: token_url.clone(),
            registration_endpoint: auth.registration_endpoint.clone(),
            scopes_supported: auth.scopes.clone(),
        });
    }

    let mut errors = Vec::new();
    for issuer in issuers {
        match fetch_auth_metadata(http, issuer).await {
            Ok(metadata) => return Ok(metadata),
            Err(err) => errors.push(format!("{issuer}: {err}")),
        }
    }

    Err(anyhow!(
        "Failed to discover OAuth metadata. Tried: {}",
        errors.join(" | ")
    ))
}

fn resolve_scopes(
    auth: &McpAuth,
    _metadata: &AuthServerMetadata,
    resource_metadata: Option<&ProtectedResourceMetadata>,
    scope_hint: Option<&str>,
) -> Result<Option<String>> {
    // Follow the MCP SDK scope resolution order:
    // 1) scope hint from WWW-Authenticate header
    // 2) explicit scopes from the client auth config
    // 3) scopes_supported from Protected Resource Metadata (PRM)
    // We deliberately do NOT fall through to AS metadata scopes_supported,
    // as the MCP SDK doesn't do that either.  Requesting OIDC scopes like
    // offline_access can trigger a completely different auth flow on some
    // servers (e.g. Granola's native-app redirect).
    if let Some(scope) = scope_hint {
        if !scope.is_empty() {
            return Ok(Some(scope.to_string()));
        }
    }
    if let Some(scopes) = &auth.scopes {
        return Ok(Some(scopes.join(" ")));
    }
    if let Some(meta) = resource_metadata {
        if let Some(scopes) = &meta.scopes_supported {
            if !scopes.is_empty() {
                return Ok(Some(scopes.join(" ")));
            }
        }
    }

    Ok(None)
}

async fn discover_resource_metadata(http: &Client, resource: &Url) -> Result<ResourceDiscovery> {
    let mut discovery = ResourceDiscovery {
        metadata: None,
        scope_hint: None,
        auth_server_hint: None,
        resource_hint: None,
    };

    if let Ok(Some(challenge)) = probe_www_authenticate(resource).await {
        discovery.scope_hint = challenge.scope;
        discovery.auth_server_hint = challenge.authorization_server;
        discovery.resource_hint = challenge.resource;
        if let Some(metadata_url) = challenge.resource_metadata {
            if let Ok(meta) = fetch_resource_metadata(http, &metadata_url).await {
                discovery.metadata = Some(meta);
                return Ok(discovery);
            }
        }
    }

    for url in resource_metadata_urls(resource)? {
        if let Ok(meta) = fetch_resource_metadata(http, &url.to_string()).await {
            discovery.metadata = Some(meta);
            break;
        }
    }

    Ok(discovery)
}

fn resolve_auth_issuers(
    resource: &Url,
    resource_meta: Option<&ProtectedResourceMetadata>,
    auth_server_hint: Option<&str>,
) -> Vec<Url> {
    let mut issuers = Vec::new();
    if let Some(hint) = auth_server_hint {
        if let Ok(url) = Url::parse(hint) {
            issuers.push(url);
        }
    }
    if let Some(meta) = resource_meta {
        if let Some(servers) = &meta.authorization_servers {
            for server in servers {
                if let Ok(url) = Url::parse(server) {
                    issuers.push(url);
                }
            }
        }
    }

    if issuers.is_empty() {
        let mut origin = resource.clone();
        origin.set_path("");
        origin.set_query(None);
        origin.set_fragment(None);
        issuers.push(origin);
    }

    issuers
}

fn resource_metadata_urls(resource: &Url) -> Result<Vec<Url>> {
    let mut urls = Vec::new();
    let path = resource.path().trim_end_matches('/');

    if !path.is_empty() && path != "/" {
        let mut with_path = resource.clone();
        with_path.set_path(&format!("/.well-known/oauth-protected-resource{path}"));
        with_path.set_query(None);
        with_path.set_fragment(None);
        urls.push(with_path);
    }

    let mut root = resource.clone();
    root.set_path("/.well-known/oauth-protected-resource");
    root.set_query(None);
    root.set_fragment(None);
    urls.push(root);

    Ok(urls)
}

async fn fetch_resource_metadata(http: &Client, url: &str) -> Result<ProtectedResourceMetadata> {
    let response = http
        .get(url)
        .header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION)
        .send()
        .await
        .context("fetch resource metadata")?;
    let status = response.status();
    let text = response.text().await.context("read resource metadata")?;
    if !status.is_success() {
        return Err(anyhow!("HTTP {status}: {text}"));
    }
    let meta: ProtectedResourceMetadata =
        serde_json::from_str(&text).context("parse resource metadata")?;
    Ok(meta)
}

async fn fetch_auth_metadata(http: &Client, issuer: &Url) -> Result<AuthServerMetadata> {
    let candidates = auth_metadata_urls(issuer);
    let mut errors = Vec::new();
    for url in candidates {
        match fetch_metadata_url(http, &url).await {
            Ok(meta) => return Ok(meta),
            Err(err) => errors.push(format!("{url}: {err}")),
        }
    }
    Err(anyhow!(
        "No metadata endpoint succeeded: {}",
        errors.join(" | ")
    ))
}

fn auth_metadata_urls(issuer: &Url) -> Vec<Url> {
    if issuer.path().contains("/.well-known/") {
        return vec![issuer.clone()];
    }

    let mut urls = Vec::new();
    // Prefer OAuth Authorization Server metadata (MCP standard) over OIDC.
    // The OAuth AS metadata typically includes registration_endpoint,
    // while the OIDC metadata often omits it.
    if let Ok(url) = oauth_metadata_url(issuer) {
        urls.push(url);
    }
    if let Ok(url) = oidc_metadata_url(issuer) {
        urls.push(url);
    }
    urls
}

fn oidc_metadata_url(issuer: &Url) -> Result<Url> {
    let mut url = issuer.clone();
    let path = issuer.path().trim_end_matches('/');
    let well_known = if path.is_empty() || path == "/" {
        "/.well-known/openid-configuration".to_string()
    } else {
        format!("{path}/.well-known/openid-configuration")
    };
    url.set_path(&well_known);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn oauth_metadata_url(issuer: &Url) -> Result<Url> {
    let issuer_path = issuer.path().trim_end_matches('/');
    let mut origin = issuer.clone();
    origin.set_path("");
    origin.set_query(None);
    origin.set_fragment(None);
    let well_known = if issuer_path.is_empty() || issuer_path == "/" {
        "/.well-known/oauth-authorization-server".to_string()
    } else {
        format!("/.well-known/oauth-authorization-server{issuer_path}")
    };
    origin.set_path(&well_known);
    Ok(origin)
}

fn default_registration_endpoint(authorization_endpoint: &str) -> Option<String> {
    let Ok(mut url) = Url::parse(authorization_endpoint) else {
        return None;
    };
    url.set_path("/register");
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

async fn fetch_metadata_url(http: &Client, url: &Url) -> Result<AuthServerMetadata> {
    let response = http
        .get(url.clone())
        .header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION)
        .send()
        .await
        .context("fetch oauth metadata")?;
    let status = response.status();
    let text = response.text().await.context("read oauth metadata")?;
    if !status.is_success() {
        return Err(anyhow!("HTTP {status}: {text}"));
    }
    let metadata: AuthServerMetadata =
        serde_json::from_str(&text).context("parse oauth metadata")?;
    Ok(metadata)
}

#[derive(Debug)]
struct AuthChallenge {
    resource_metadata: Option<String>,
    scope: Option<String>,
    authorization_server: Option<String>,
    resource: Option<String>,
}

async fn probe_www_authenticate(resource: &Url) -> Result<Option<AuthChallenge>> {
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build probe client")?;
    let response = client.get(resource.clone()).send().await;
    let Ok(resp) = response else {
        return Ok(None);
    };

    let mut challenge = None;
    // Check both standard and CloudFront-remapped WWW-Authenticate headers.
    let header_names = [
        reqwest::header::WWW_AUTHENTICATE,
        reqwest::header::HeaderName::from_static("x-amzn-remapped-www-authenticate"),
    ];
    for name in &header_names {
        for header_value in resp.headers().get_all(name) {
            if let Ok(value) = header_value.to_str() {
                if let Some(parsed) = parse_bearer_challenge(value) {
                    challenge = Some(parsed);
                    break;
                }
            }
        }
        if challenge.is_some() {
            break;
        }
    }
    Ok(challenge)
}

fn parse_bearer_challenge(value: &str) -> Option<AuthChallenge> {
    let lower = value.to_ascii_lowercase();
    let bearer_pos = lower.find("bearer")?;
    let params = value[bearer_pos + "bearer".len()..].trim();
    let params = params.trim_start_matches(',');

    let mut pairs = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in params.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ',' if !in_quotes => {
                if !current.trim().is_empty() {
                    pairs.push(current.trim().to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        pairs.push(current.trim().to_string());
    }

    let mut resource_metadata = None;
    let mut scope = None;
    let mut authorization_server = None;
    let mut resource = None;

    for pair in pairs {
        let Some((key, raw_value)) = pair.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let mut value = raw_value.trim();
        if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
            value = &value[1..value.len() - 1];
        }
        match key {
            "resource_metadata" => resource_metadata = Some(value.to_string()),
            "scope" => scope = Some(value.to_string()),
            "authorization_server" => authorization_server = Some(value.to_string()),
            "resource" => resource = Some(value.to_string()),
            _ => {}
        }
    }

    if resource_metadata.is_none()
        && scope.is_none()
        && authorization_server.is_none()
        && resource.is_none()
    {
        None
    } else {
        Some(AuthChallenge {
            resource_metadata,
            scope,
            authorization_server,
            resource,
        })
    }
}

fn wait_for_callback(server: Server) -> Result<OAuthCallback> {
    for request in server.incoming_requests() {
        let url = format!("http://localhost{}", request.url());
        let parsed = Url::parse(&url).context("parse callback url")?;
        let mut code = None;
        let mut state = None;
        let mut error = None;

        for (key, value) in parsed.query_pairs() {
            match key.as_ref() {
                "code" => code = Some(value.to_string()),
                "state" => state = Some(value.to_string()),
                "error" => error = Some(value.to_string()),
                _ => {}
            }
        }

        let response = Response::from_string(
            "OAuth complete. You can close this window and return to the terminal.",
        );
        let _ = request.respond(response);

        return Ok(OAuthCallback { code, state, error });
    }

    Err(anyhow!("No OAuth callback received"))
}

async fn register_client(
    http: &Client,
    registration_endpoint: &str,
    redirect_uris: &[String],
) -> Result<(String, Option<String>)> {
    let mut errors = Vec::new();

    // Prefer public-client registration for native apps (PKCE + loopback redirect).
    // Some IdPs also support confidential registration; fall back if needed.
    for method in ["none", "client_secret_post"] {
        match register_client_with_method(http, registration_endpoint, redirect_uris, method).await
        {
            Ok(result) => return Ok(result),
            Err(err) => errors.push(format!("{method}: {err:#}")),
        }
    }

    Err(anyhow!(
        "Client registration failed. Tried: {}",
        errors.join(" | ")
    ))
}

async fn register_client_with_method(
    http: &Client,
    registration_endpoint: &str,
    redirect_uris: &[String],
    token_endpoint_auth_method: &str,
) -> Result<(String, Option<String>)> {
    let body = serde_json::json!({
        "client_name": APP_NAME,
        "redirect_uris": redirect_uris,
        "token_endpoint_auth_method": token_endpoint_auth_method,
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "application_type": "native",
    });

    let response = http
        .post(registration_endpoint)
        .json(&body)
        .send()
        .await
        .context("register client")?;

    let status = response.status();
    let text = response
        .text()
        .await
        .context("read registration response")?;
    if !status.is_success() {
        return Err(anyhow!("HTTP {status}: {text}"));
    }

    let parsed: RegistrationResponse = serde_json::from_str(&text).context("parse registration")?;
    let client_secret = parsed.client_secret.filter(|s| !s.is_empty());
    Ok((parsed.client_id, client_secret))
}

async fn exchange_code(
    http: &Client,
    token_endpoint: &str,
    client_id: &str,
    client_secret: Option<&str>,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
    resource: &str,
) -> Result<TokenResponse> {
    let mut params = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("client_id", client_id.to_string()),
        ("code_verifier", code_verifier.to_string()),
        ("resource", resource.to_string()),
    ];
    if let Some(secret) = client_secret {
        params.push(("client_secret", secret.to_string()));
    }

    let response = http
        .post(token_endpoint)
        .form(&params)
        .send()
        .await
        .context("exchange token")?;

    let status = response.status();
    let text = response.text().await.context("read token response")?;
    if !status.is_success() {
        return Err(anyhow!("Token exchange failed: {text}"));
    }

    let token: TokenResponse = serde_json::from_str(&text).context("parse token response")?;
    Ok(token)
}

/// Extract an authorization code from a URL or raw code string.
fn extract_code_from_input(input: &str) -> String {
    // If it looks like a URL, try to parse the `code` query parameter.
    if input.starts_with("http://") || input.starts_with("https://") {
        if let Ok(url) = Url::parse(input) {
            for (key, value) in url.query_pairs() {
                if key == "code" {
                    return value.to_string();
                }
            }
        }
    }
    // Otherwise treat the whole input as the code.
    input.trim().to_string()
}

/// Complete an OAuth flow using a manually provided authorization code or URL.
pub async fn exchange_manual_code_with_input(
    http: &Client,
    pending: &PendingOAuth,
    raw_input: &str,
) -> Result<OAuthToken> {
    let code = extract_code_from_input(raw_input);
    if code.is_empty() {
        return Err(anyhow!("No authorization code found in input"));
    }

    let token = exchange_code(
        http,
        &pending.token_endpoint,
        &pending.client_id,
        pending.client_secret.as_deref(),
        &code,
        &pending.redirect_uri,
        &pending.code_verifier,
        &pending.resource_value,
    )
    .await?;

    Ok(OAuthToken {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_in: token.expires_in,
        scope: token.scope,
        token_type: token.token_type,
        client_id: Some(pending.client_id.clone()),
    })
}

/// Wait for the OAuth callback on a local server using the redirect_uri from
/// the pending OAuth state. Returns the token if successful.
pub async fn wait_for_oauth_callback(
    pending: &PendingOAuth,
    timeout: Duration,
) -> Result<OAuthToken> {
    let url = Url::parse(&pending.redirect_uri).context("parse pending redirect_uri")?;
    let port = url
        .port()
        .ok_or_else(|| anyhow!("redirect_uri missing port"))?;

    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("bind 127.0.0.1:{port} for OAuth callback"))?;
    let server = Server::from_listener(listener, None)
        .map_err(|err| anyhow!("start callback server: {err}"))?;
    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        if let Ok(callback) = wait_for_callback(server) {
            let _ = sender.send(callback);
        }
    });

    let callback = receiver.recv_timeout(timeout).map_err(|_| {
        anyhow!(
            "Timed out waiting for OAuth callback ({}s)",
            timeout.as_secs()
        )
    })?;

    if let Some(error) = callback.error {
        return Err(anyhow!("OAuth error: {error}"));
    }

    let code = callback
        .code
        .ok_or_else(|| anyhow!("OAuth callback missing code"))?;

    if let Some(callback_state) = &callback.state {
        if callback_state != &pending.state {
            return Err(anyhow!("OAuth state mismatch"));
        }
    }

    let http = Client::new();
    let token = exchange_code(
        &http,
        &pending.token_endpoint,
        &pending.client_id,
        pending.client_secret.as_deref(),
        &code,
        &pending.redirect_uri,
        &pending.code_verifier,
        &pending.resource_value,
    )
    .await?;

    Ok(OAuthToken {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_in: token.expires_in,
        scope: token.scope,
        token_type: token.token_type,
        client_id: Some(pending.client_id.clone()),
    })
}

/// Prepare an OAuth flow: discover endpoints, register client, build the authorize URL,
/// and return the pending state along with the URL to open.
pub async fn prepare_auth<F>(
    http: &Client,
    server: &McpServer,
    auth: &McpAuth,
    client_id_hint: Option<String>,
    client_secret_hint: Option<String>,
    mut log: F,
) -> Result<(String, PendingOAuth)>
where
    F: FnMut(String),
{
    let resource = resource_identifier(server)?;
    log(format!("OAuth resource: {resource}"));

    let discovery = discover_resource_metadata(http, &resource).await?;
    if let Some(meta) = &discovery.metadata {
        log(format!(
            "Resource metadata: resource={:?} auth_servers={:?}",
            meta.resource, meta.authorization_servers
        ));
    }

    let auth_issuers = resolve_auth_issuers(
        &resource,
        discovery.metadata.as_ref(),
        discovery.auth_server_hint.as_deref(),
    );

    let metadata = discover_auth_metadata(http, auth, &auth_issuers).await?;
    let authorization_endpoint = auth
        .authorization_endpoint
        .clone()
        .unwrap_or_else(|| metadata.authorization_endpoint.clone());
    let token_endpoint = auth
        .token_endpoint
        .clone()
        .unwrap_or_else(|| metadata.token_endpoint.clone());
    let registration_endpoint = auth
        .registration_endpoint
        .clone()
        .or_else(|| metadata.registration_endpoint.clone())
        .or_else(|| default_registration_endpoint(&authorization_endpoint));

    let scopes = resolve_scopes(
        auth,
        &metadata,
        discovery.metadata.as_ref(),
        discovery.scope_hint.as_deref(),
    )?;
    let resource_value = discovery
        .metadata
        .as_ref()
        .and_then(|meta| meta.resource.clone())
        .or_else(|| discovery.resource_hint.clone())
        .unwrap_or_else(|| resource.to_string());

    // Use a fixed port so the redirect_uri registered with the auth server
    // stays consistent across retries.
    let listener = TcpListener::bind("127.0.0.1:0").context("bind localhost for OAuth")?;
    let port = listener.local_addr().context("get listener port")?.port();
    drop(listener); // Release so the callback server can rebind later.
    let redirect_uri = format!("http://localhost:{port}/callback");
    let redirect_uris = vec![redirect_uri.clone()];

    let (code_verifier, code_challenge) = pkce_pair();
    let state = random_state();

    let (client_id, client_secret) = match client_id_hint {
        Some(client_id) => (client_id, client_secret_hint),
        None => {
            if let Some(endpoint) = registration_endpoint {
                let (client_id, client_secret) =
                    register_client(http, &endpoint, &redirect_uris).await?;
                log("Registered OAuth client dynamically.".to_string());
                (client_id, client_secret.or(client_secret_hint))
            } else {
                return Err(anyhow!(
                    "No client_id available and dynamic registration not supported."
                ));
            }
        }
    };

    let mut auth_url =
        Url::parse(&authorization_endpoint).context("parse authorization endpoint")?;
    auth_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("resource", &resource_value);
    if let Some(scope) = &scopes {
        if !scope.is_empty() {
            auth_url.query_pairs_mut().append_pair("scope", scope);
        }
    }

    let pending = PendingOAuth {
        client_id,
        client_secret,
        redirect_uri,
        code_verifier,
        state,
        token_endpoint,
        resource_value,
    };

    Ok((auth_url.to_string(), pending))
}

fn pkce_pair() -> (String, String) {
    let mut verifier_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut verifier_bytes);
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);

    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(digest);

    (verifier, challenge)
}

fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
