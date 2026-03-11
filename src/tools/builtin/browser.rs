//! Built-in browser tool.
//!
//! This is an MVP port of OpenClaw's browser tool surface. Instead of bundling
//! a browser automation runtime into IronClaw, it proxies requests to an
//! OpenClaw-compatible browser control server when
//! `IRONCLAW_BROWSER_BASE_URL` is configured. For simple open/navigate flows it
//! can also fall back to the host browser via `open::that`.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Map, Value, json};
use tempfile::Builder;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::context::JobContext;
use crate::tools::tool::{ApprovalRequirement, Tool, ToolError, ToolOutput, require_str};

const USER_AGENT: &str = concat!(
    "IronClaw-Agent/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/nearai/ironclaw)"
);
const ENV_BROWSER_BASE_URL: &str = "IRONCLAW_BROWSER_BASE_URL";
const ENV_BROWSER_RUNTIME_MANAGED: &str = "IRONCLAW_BROWSER_RUNTIME_MANAGED";
const ENV_BROWSER_RUNTIME_HOST: &str = "IRONCLAW_BROWSER_RUNTIME_HOST";
const ENV_BROWSER_RUNTIME_PORT: &str = "IRONCLAW_BROWSER_RUNTIME_PORT";
const ENV_BROWSER_RUNTIME_HEADLESS: &str = "IRONCLAW_BROWSER_RUNTIME_HEADLESS";
const ENV_BROWSER_NODE_BASE_URL: &str = "IRONCLAW_BROWSER_NODE_BASE_URL";
const ENV_BROWSER_NODE_MAP: &str = "IRONCLAW_BROWSER_NODE_MAP";
const DEFAULT_BROWSER_RUNTIME_HOST: &str = "127.0.0.1";
const DEFAULT_BROWSER_RUNTIME_PORT: u16 = 24242;
const BROWSER_RUNTIME_START_TIMEOUT: Duration = Duration::from_secs(20);
const BROWSER_RUNTIME_IDENTITY: &str = "IronClaw/browser-runtime";

struct ManagedRuntime {
    base_url: String,
    child: Child,
    reused_existing: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserTarget {
    Host,
    Sandbox,
    Node,
}

#[derive(Clone, Debug)]
struct BrowserRoute {
    target: BrowserTarget,
    profile: Option<String>,
    base_url: Option<String>,
}

struct BinaryTempRequest<'a> {
    path: &'a str,
    query: &'a str,
    body: Value,
    extension: &'a str,
    content_type_prefix: &'a str,
}

pub struct BrowserTool {
    client: Client,
}

impl BrowserTool {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(USER_AGENT)
            .build()
            .expect("Failed to create browser tool HTTP client");

        Self { client }
    }

    fn configured_base_url() -> Option<String> {
        std::env::var(ENV_BROWSER_BASE_URL)
            .ok()
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
    }

    fn configured_node_base_url() -> Option<String> {
        std::env::var(ENV_BROWSER_NODE_BASE_URL)
            .ok()
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
    }

    fn configured_node_map() -> Result<Map<String, Value>, ToolError> {
        let Some(raw) = std::env::var(ENV_BROWSER_NODE_MAP)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        else {
            return Ok(Map::new());
        };

        let parsed: Value = serde_json::from_str(&raw).map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "{ENV_BROWSER_NODE_MAP} must be a JSON object mapping node ids to browser server URLs: {e}"
            ))
        })?;
        let map = parsed.as_object().ok_or_else(|| {
            ToolError::ExecutionFailed(format!(
                "{ENV_BROWSER_NODE_MAP} must be a JSON object mapping node ids to browser server URLs"
            ))
        })?;
        Ok(map.clone())
    }

    fn resolve_node_base_url(node: Option<&str>) -> Result<Option<String>, ToolError> {
        if let Some(requested) = node.map(str::trim).filter(|v| !v.is_empty()) {
            if requested.starts_with("http://") || requested.starts_with("https://") {
                return Ok(Some(requested.trim_end_matches('/').to_string()));
            }

            let node_map = Self::configured_node_map()?;
            if let Some(url) = node_map
                .get(requested)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                return Ok(Some(url.trim_end_matches('/').to_string()));
            }

            return Err(ToolError::ExecutionFailed(format!(
                "browser node '{requested}' is not configured. Set {ENV_BROWSER_NODE_MAP} or pass node as a full browser server URL"
            )));
        }

        Ok(Self::configured_node_base_url())
    }

    fn managed_runtime_enabled() -> bool {
        !matches!(
            std::env::var(ENV_BROWSER_RUNTIME_MANAGED)
                .ok()
                .as_deref()
                .map(str::trim),
            Some("0") | Some("false") | Some("False") | Some("FALSE")
        )
    }

    fn runtime_host() -> String {
        std::env::var(ENV_BROWSER_RUNTIME_HOST)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_BROWSER_RUNTIME_HOST.to_string())
    }

    fn runtime_port() -> u16 {
        std::env::var(ENV_BROWSER_RUNTIME_PORT)
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(DEFAULT_BROWSER_RUNTIME_PORT)
    }

    fn managed_runtime_base_url() -> String {
        format!("http://{}:{}", Self::runtime_host(), Self::runtime_port())
    }

    fn runtime_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("browser-runtime")
    }

    fn runtime_script() -> PathBuf {
        Self::runtime_dir().join("server.mjs")
    }

    fn runtime_state() -> &'static Mutex<Option<ManagedRuntime>> {
        static STATE: OnceLock<Mutex<Option<ManagedRuntime>>> = OnceLock::new();
        STATE.get_or_init(|| Mutex::new(None))
    }

    async fn resolve_route(&self, params: &Value) -> Result<BrowserRoute, ToolError> {
        let profile = Self::extract_optional_string(params, "profile");
        let requested_target = Self::extract_optional_string(params, "target");
        let requested_node = Self::extract_optional_string(params, "node");

        if requested_node.is_some() && requested_target.as_deref() != Some("node") {
            return Err(ToolError::InvalidParameters(
                "browser node is only valid with target='node'".to_string(),
            ));
        }
        let target = match requested_target.as_deref() {
            Some("host") => BrowserTarget::Host,
            Some("sandbox") => BrowserTarget::Sandbox,
            Some("node") => BrowserTarget::Node,
            Some(other) => {
                return Err(ToolError::InvalidParameters(format!(
                    "unsupported browser target '{other}'"
                )));
            }
            None => match profile.as_deref() {
                Some("chrome") => BrowserTarget::Host,
                Some("openclaw") => BrowserTarget::Sandbox,
                _ if Self::configured_base_url().is_some() => BrowserTarget::Host,
                _ => BrowserTarget::Sandbox,
            },
        };

        let base_url = match target {
            BrowserTarget::Host => {
                if profile.as_deref() == Some("chrome") && Self::managed_runtime_enabled() {
                    Some(self.ensure_managed_runtime().await?)
                } else {
                    Self::configured_base_url()
                }
            }
            BrowserTarget::Sandbox => {
                if Self::managed_runtime_enabled() {
                    Some(self.ensure_managed_runtime().await?)
                } else {
                    None
                }
            }
            BrowserTarget::Node => Self::resolve_node_base_url(requested_node.as_deref())?,
        };

        Ok(BrowserRoute {
            target,
            profile,
            base_url,
        })
    }

    async fn ensure_managed_runtime(&self) -> Result<String, ToolError> {
        let base_url = Self::managed_runtime_base_url();

        if self.server_healthy(&base_url).await {
            return Ok(base_url);
        }

        let state = Self::runtime_state();
        let existing_base_url = {
            let guard = state.lock().await;
            guard.as_ref().map(|runtime| runtime.base_url.clone())
        };

        if let Some(runtime_base_url) = existing_base_url {
            if self.server_healthy(&runtime_base_url).await {
                return Ok(runtime_base_url);
            }
            let mut guard = state.lock().await;
            if let Some(runtime) = guard.as_mut()
                && runtime.base_url == runtime_base_url
            {
                let _ = runtime.child.start_kill();
                *guard = None;
            }
        }

        let runtime_dir = Self::runtime_dir();
        let runtime_script = Self::runtime_script();
        if !runtime_script.exists() {
            return Err(ToolError::ExecutionFailed(format!(
                "managed browser runtime is missing at {}",
                runtime_script.display()
            )));
        }

        let mut command = Command::new("node");
        command
            .arg(runtime_script)
            .current_dir(&runtime_dir)
            .env(ENV_BROWSER_RUNTIME_HOST, Self::runtime_host())
            .env(ENV_BROWSER_RUNTIME_PORT, Self::runtime_port().to_string())
            .env(
                ENV_BROWSER_RUNTIME_HEADLESS,
                std::env::var(ENV_BROWSER_RUNTIME_HEADLESS).unwrap_or_else(|_| "1".to_string()),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = command.spawn().map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "failed to start managed browser runtime via `node`: {}. Install Node.js and run `cd {} && npm install && npx playwright install chromium`",
                e,
                runtime_dir.display()
            ))
        })?;

        {
            let mut guard = state.lock().await;
            *guard = Some(ManagedRuntime {
                base_url: base_url.clone(),
                child,
                reused_existing: false,
            });
        }

        let start = std::time::Instant::now();
        while start.elapsed() < BROWSER_RUNTIME_START_TIMEOUT {
            let runtime_status = self.runtime_status(&base_url).await;
            if runtime_status.state == BrowserRuntimeState::Healthy {
                let mut guard = state.lock().await;
                if let Some(runtime) = guard.as_mut() {
                    runtime.reused_existing = runtime_status.reused_existing;
                }
                return Ok(base_url);
            }

            let mut guard = state.lock().await;
            if let Some(runtime) = guard.as_mut()
                && let Ok(Some(status)) = runtime.child.try_wait()
            {
                *guard = None;
                return Err(ToolError::ExecutionFailed(format!(
                    "managed browser runtime exited early with status {}. Run `cd {} && npm install && npx playwright install chromium`",
                    status,
                    runtime_dir.display()
                )));
            }
            drop(guard);
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        Err(ToolError::ExecutionFailed(format!(
            "{}. Run `cd {} && npm install && npx playwright install chromium`",
            self.managed_runtime_timeout_message(&base_url).await,
            runtime_dir.display()
        )))
    }

    async fn managed_runtime_timeout_message(&self, base_url: &str) -> String {
        match self.runtime_status(base_url).await.state {
            BrowserRuntimeState::Healthy => format!(
                "managed browser runtime health check eventually succeeded at {} but readiness confirmation was delayed",
                base_url
            ),
            BrowserRuntimeState::WrongService => format!(
                "managed browser runtime did not become ready at {} within {:?}; the port is serving a non-IronClaw HTTP service",
                base_url, BROWSER_RUNTIME_START_TIMEOUT
            ),
            BrowserRuntimeState::Unreachable => format!(
                "managed browser runtime did not become ready at {} within {:?}",
                base_url, BROWSER_RUNTIME_START_TIMEOUT
            ),
        }
    }

    async fn server_healthy(&self, base_url: &str) -> bool {
        matches!(
            self.runtime_status(base_url).await.state,
            BrowserRuntimeState::Healthy
        )
    }

    async fn runtime_status(&self, base_url: &str) -> BrowserRuntimeStatus {
        let response = match self
            .client
            .get(format!("{base_url}/_ironclaw/runtime"))
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(_) => {
                return BrowserRuntimeStatus {
                    state: BrowserRuntimeState::Unreachable,
                    reused_existing: false,
                };
            }
        };
        if !response.status().is_success() {
            return BrowserRuntimeStatus {
                state: BrowserRuntimeState::Unreachable,
                reused_existing: false,
            };
        }
        let body: Value = match response.json().await {
            Ok(body) => body,
            Err(_) => {
                return BrowserRuntimeStatus {
                    state: BrowserRuntimeState::WrongService,
                    reused_existing: false,
                };
            }
        };
        let reused_existing = body
            .get("startup")
            .and_then(Value::as_object)
            .and_then(|startup| startup.get("reused"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match body.get("identity").and_then(Value::as_str) {
            Some(BROWSER_RUNTIME_IDENTITY) => BrowserRuntimeStatus {
                state: BrowserRuntimeState::Healthy,
                reused_existing,
            },
            _ => BrowserRuntimeStatus {
                state: BrowserRuntimeState::WrongService,
                reused_existing: false,
            },
        }
    }

    fn annotate_managed_runtime_status(status: &mut Value, runtime: Option<&ManagedRuntime>) {
        let Some(obj) = status.as_object_mut() else {
            return;
        };
        let Some(runtime) = runtime else {
            return;
        };
        obj.insert(
            "managedRuntimeReusedExisting".to_string(),
            Value::Bool(runtime.reused_existing),
        );
        obj.insert(
            "managedRuntimeBaseUrl".to_string(),
            Value::String(runtime.base_url.clone()),
        );
        obj.insert(
            "managedRuntimeStartupMode".to_string(),
            Value::String(if runtime.reused_existing {
                "reused-existing".to_string()
            } else {
                "listening".to_string()
            }),
        );
    }

    fn profile_suffix(profile: Option<&str>) -> String {
        match profile.map(str::trim).filter(|v| !v.is_empty()) {
            Some(profile) => format!("?profile={}", urlencoding::encode(profile)),
            None => String::new(),
        }
    }

    fn query_string(params: &[(&str, Option<String>)]) -> String {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in params {
            if let Some(value) = value.as_deref()
                && !value.is_empty()
            {
                serializer.append_pair(key, value);
            }
        }
        let query = serializer.finish();
        if query.is_empty() {
            String::new()
        } else {
            format!("?{query}")
        }
    }

    fn extract_optional_string(params: &Value, key: &str) -> Option<String> {
        params
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
    }

    fn extract_optional_bool(params: &Value, key: &str) -> Option<bool> {
        params.get(key).and_then(Value::as_bool)
    }

    fn extract_optional_u64(params: &Value, key: &str) -> Option<u64> {
        params.get(key).and_then(Value::as_u64)
    }

    fn extract_paths(params: &Value) -> Result<Vec<String>, ToolError> {
        let Some(value) = params.get("paths") else {
            return Ok(Vec::new());
        };
        let items = value
            .as_array()
            .ok_or_else(|| ToolError::InvalidParameters("'paths' must be an array".to_string()))?;
        let mut out = Vec::with_capacity(items.len());
        for (idx, item) in items.iter().enumerate() {
            let path = item.as_str().ok_or_else(|| {
                ToolError::InvalidParameters(format!("paths[{idx}] must be a string"))
            })?;
            out.push(path.to_string());
        }
        Ok(out)
    }

    async fn proxy_json(
        &self,
        route: &BrowserRoute,
        method: reqwest::Method,
        path: &str,
        query: &str,
        body: Option<Value>,
    ) -> Result<Value, ToolError> {
        let base_url = route.base_url.as_ref().ok_or_else(|| match route.target {
            BrowserTarget::Host => ToolError::ExecutionFailed(format!(
                "host browser control is unavailable; set {ENV_BROWSER_BASE_URL} to a host browser server"
            )),
            BrowserTarget::Sandbox => ToolError::ExecutionFailed(
                "sandbox browser control is unavailable; enable managed runtime".to_string(),
            ),
            BrowserTarget::Node => ToolError::ExecutionFailed(format!(
                "node browser control is unavailable; set {ENV_BROWSER_NODE_BASE_URL}, {ENV_BROWSER_NODE_MAP}, or pass node as a full browser server URL"
            )),
        })?;
        let url = format!("{base_url}{path}{query}");
        let mut req = self.client.request(method, &url);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let response = req.send().await.map_err(|e| {
            ToolError::ExternalService(format!("browser server request failed: {e}"))
        })?;
        let status = response.status();
        let text = response.text().await.map_err(|e| {
            ToolError::ExternalService(format!("failed to read browser server response: {e}"))
        })?;
        if !status.is_success() {
            return Err(ToolError::ExternalService(format!(
                "browser server returned {}: {}",
                status, text
            )));
        }
        serde_json::from_str(&text).map_err(|e| {
            ToolError::ExternalService(format!("browser server returned invalid JSON: {e}"))
        })
    }

    async fn proxy_json_guided(
        &self,
        route: &BrowserRoute,
        params: &Value,
        method: reqwest::Method,
        path: &str,
        query: &str,
        body: Option<Value>,
    ) -> Result<Value, ToolError> {
        match self.proxy_json(route, method, path, query, body).await {
            Ok(value) => Ok(value),
            Err(err) => Err(self.map_chrome_relay_error(route, params, err).await),
        }
    }

    async fn proxy_binary_to_temp(
        &self,
        route: &BrowserRoute,
        request: BinaryTempRequest<'_>,
    ) -> Result<Value, ToolError> {
        let base_url = route.base_url.as_ref().ok_or_else(|| match route.target {
            BrowserTarget::Host => ToolError::ExecutionFailed(format!(
                "host browser control is unavailable; set {ENV_BROWSER_BASE_URL} to a host browser server"
            )),
            BrowserTarget::Sandbox => ToolError::ExecutionFailed(
                "sandbox browser control is unavailable; enable managed runtime".to_string(),
            ),
            BrowserTarget::Node => ToolError::ExecutionFailed(format!(
                "node browser control is unavailable; set {ENV_BROWSER_NODE_BASE_URL}, {ENV_BROWSER_NODE_MAP}, or pass node as a full browser server URL"
            )),
        })?;
        let url = format!("{base_url}{}{query}", request.path, query = request.query);
        let response = self
            .client
            .post(&url)
            .json(&request.body)
            .send()
            .await
            .map_err(|e| {
                ToolError::ExternalService(format!("browser server request failed: {e}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ToolError::ExternalService(format!(
                "browser server returned {}: {}",
                status, text
            )));
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        if !content_type.starts_with(request.content_type_prefix) {
            return Err(ToolError::ExternalService(format!(
                "browser server returned unexpected content type '{content_type}'"
            )));
        }

        let target_id = response
            .headers()
            .get("x-browser-target-id")
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);
        let source_url = response
            .headers()
            .get("x-browser-url")
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);

        let bytes = response.bytes().await.map_err(|e| {
            ToolError::ExternalService(format!("failed to read browser media response: {e}"))
        })?;
        let path = persist_temp_file(request.extension, bytes.as_ref()).map_err(|e| {
            ToolError::ExecutionFailed(format!("failed to persist browser media: {e}"))
        })?;

        Ok(json!({
            "ok": true,
            "path": path.to_string_lossy(),
            "bytes": bytes.len(),
            "content_type": content_type,
            "targetId": target_id,
            "url": source_url,
        }))
    }

    async fn proxy_binary_to_temp_guided(
        &self,
        route: &BrowserRoute,
        params: &Value,
        request: BinaryTempRequest<'_>,
    ) -> Result<Value, ToolError> {
        match self.proxy_binary_to_temp(route, request).await {
            Ok(value) => Ok(value),
            Err(err) => Err(self.map_chrome_relay_error(route, params, err).await),
        }
    }

    async fn execute_open_or_navigate(
        &self,
        route: &BrowserRoute,
        action: &str,
        params: &Value,
    ) -> Result<Value, ToolError> {
        let url = require_str(params, "url")?;
        if route.base_url.is_none() {
            match route.target {
                BrowserTarget::Host => {}
                BrowserTarget::Sandbox => {
                    return Err(ToolError::ExecutionFailed(
                        "sandbox browser control is unavailable; enable managed runtime for target='sandbox'"
                            .to_string(),
                    ));
                }
                BrowserTarget::Node => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "node browser control is unavailable; set {ENV_BROWSER_NODE_BASE_URL}, {ENV_BROWSER_NODE_MAP}, or pass node as a full browser server URL"
                    )));
                }
            }
            open::that(url).map_err(|e| {
                ToolError::ExecutionFailed(format!("failed to open host browser: {e}"))
            })?;
            return Ok(json!({
                "ok": true,
                "mode": "host_open",
                "action": action,
                "url": url,
            }));
        }

        if action == "open" {
            self.proxy_json_guided(
                route,
                params,
                reqwest::Method::POST,
                "/tabs/open",
                &Self::profile_suffix(route.profile.as_deref()),
                Some(json!({ "url": url })),
            )
            .await
        } else {
            self.proxy_json_guided(
                route,
                params,
                reqwest::Method::POST,
                "/navigate",
                &Self::profile_suffix(route.profile.as_deref()),
                Some(json!({
                    "url": url,
                    "targetId": Self::extract_optional_string(params, "targetId"),
                })),
            )
            .await
        }
    }

    fn is_chrome_profile(route: &BrowserRoute) -> bool {
        route.profile.as_deref() == Some("chrome")
    }

    fn error_text(err: &ToolError) -> String {
        format!("{err}")
    }

    fn is_chrome_relay_not_connected(err: &ToolError) -> bool {
        let msg = Self::error_text(err).to_lowercase();
        msg.contains("chrome relay extension not connected")
            || msg.contains("extension disconnected")
            || msg.contains("browser server returned 500") && msg.contains("not connected")
    }

    fn is_chrome_stale_target(err: &ToolError) -> bool {
        let msg = Self::error_text(err).to_lowercase();
        (msg.contains("target not found")
            || msg.contains("tab not found")
            || msg.contains("session not found")
            || msg.contains("cannot find session"))
            && (msg.contains("browser server returned 404")
                || msg.contains("browser server returned 500")
                || msg.contains("external service"))
    }

    fn is_chrome_ambiguous_target(err: &ToolError) -> bool {
        let msg = Self::error_text(err).to_lowercase();
        msg.contains("ambiguous target id prefix")
            && (msg.contains("browser server returned 409") || msg.contains("external service"))
    }

    async fn map_chrome_relay_error(
        &self,
        route: &BrowserRoute,
        params: &Value,
        err: ToolError,
    ) -> ToolError {
        if !Self::is_chrome_profile(route) {
            return err;
        }
        if Self::is_chrome_relay_not_connected(&err) {
            return ToolError::ExecutionFailed(
                "Chrome relay is not connected. Click the IronClaw Browser Relay toolbar icon on the tab you want to control (badge ON), then retry.".to_string(),
            );
        }
        if Self::is_chrome_ambiguous_target(&err) {
            return ToolError::ExecutionFailed(
                "Chrome targetId prefix is ambiguous. Run action=tabs profile=\"chrome\" and use a longer or full targetId.".to_string(),
            );
        }
        if Self::is_chrome_stale_target(&err)
            && Self::extract_optional_string(params, "targetId").is_some()
        {
            let tabs_count = self.chrome_tabs_count(route).await;
            if tabs_count == 0 {
                return ToolError::ExecutionFailed(
                    "No Chrome tabs are attached via the IronClaw Browser Relay extension. Click the toolbar icon on the tab you want to control (badge ON), then retry.".to_string(),
                );
            }
            return ToolError::ExecutionFailed(
                "Chrome tab not found (stale targetId?). Run action=tabs profile=\"chrome\" and use one of the returned targetIds.".to_string(),
            );
        }
        err
    }

    fn strip_target_id_from_act_request(request: &Value) -> Option<Value> {
        let mut cloned = request.as_object()?.clone();
        let had_target_id = cloned.remove("targetId").is_some();
        if had_target_id {
            Some(Value::Object(cloned))
        } else {
            None
        }
    }

    async fn chrome_tabs_count(&self, route: &BrowserRoute) -> usize {
        let Ok(result) = self
            .proxy_json(
                route,
                reqwest::Method::GET,
                "/tabs",
                &Self::profile_suffix(route.profile.as_deref()),
                None,
            )
            .await
        else {
            return 0;
        };

        result
            .get("tabs")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_else(|| result.as_array().map(Vec::len).unwrap_or(0))
    }

    async fn execute_act(&self, route: &BrowserRoute, params: &Value) -> Result<Value, ToolError> {
        let request = build_act_request(params)?;
        let query = &Self::profile_suffix(route.profile.as_deref());
        let first = self
            .proxy_json(
                route,
                reqwest::Method::POST,
                "/act",
                query,
                Some(request.clone()),
            )
            .await;

        match first {
            Ok(value) => Ok(value),
            Err(err) if Self::is_chrome_profile(route) && Self::is_chrome_stale_target(&err) => {
                if let Some(retry_request) = Self::strip_target_id_from_act_request(&request)
                    && let Ok(value) = self
                        .proxy_json(
                            route,
                            reqwest::Method::POST,
                            "/act",
                            query,
                            Some(retry_request),
                        )
                        .await
                {
                    return Ok(value);
                }

                let tabs_count = self.chrome_tabs_count(route).await;
                if tabs_count == 0 {
                    return Err(ToolError::ExecutionFailed(
                        "No Chrome tabs are attached via the IronClaw Browser Relay extension. Click the toolbar icon on the tab you want to control (badge ON), then retry.".to_string(),
                    ));
                }

                Err(ToolError::ExecutionFailed(
                    "Chrome tab not found (stale targetId?). Run action=tabs profile=\"chrome\" and use one of the returned targetIds.".to_string(),
                ))
            }
            Err(err)
                if Self::is_chrome_profile(route) && Self::is_chrome_relay_not_connected(&err) =>
            {
                Err(self.map_chrome_relay_error(route, params, err).await)
            }
            Err(err) => Err(err),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserRuntimeState {
    Healthy,
    WrongService,
    Unreachable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BrowserRuntimeStatus {
    state: BrowserRuntimeState,
    reused_existing: bool,
}

impl Default for BrowserTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        "Control the browser through an OpenClaw-compatible browser control server. Supports \
         status, start, stop, profiles, tabs, open, focus, close, snapshot, screenshot, \
         navigate, console, pdf, dialog, upload, and act. Profiles: use profile=\"chrome\" for \
         Chrome extension relay takeover of your existing Chrome tabs. Use profile=\"openclaw\" \
         for the isolated IronClaw-managed browser. If the user mentions the Chrome extension / \
         Browser Relay / toolbar button / attach tab, ALWAYS use profile=\"chrome\" (do not ask \
         which profile). When a node-hosted browser proxy is available, the tool may auto-route \
         to it. Pin a node with node=<id|url> or target=\"node\". Chrome extension relay needs \
         an attached tab: user must click the IronClaw Browser Relay toolbar icon on the tab \
         (badge ON). If no tab is connected, ask them to attach it. When using refs from \
         snapshot such as e12, keep the same tab by passing targetId from the snapshot response \
         into later actions. If you omit targetId, the runtime prefers the last selected tab for \
         that profile. If a stale targetId is provided but only one tab remains, the runtime may \
         recover by using that tab. For stable self-resolving refs across calls, prefer snapshot \
         with refs=\"aria\"; default refs=\"role\" are lighter but more snapshot-local. Use \
         snapshot plus act for UI automation. For act, prefer ref from snapshot; selector is only \
         a fallback. element is accepted as a legacy alias for ref in act requests. Avoid \
         act:wait by default; use it only when no reliable UI state exists. target selects \
         browser location (sandbox|host|node). Stop once you have the required artifact path or \
         the required page state. Set IRONCLAW_BROWSER_BASE_URL to use an external host browser \
         server. Set IRONCLAW_BROWSER_NODE_BASE_URL or IRONCLAW_BROWSER_NODE_MAP to enable node \
         browser routing. Without a browser server, open and navigate can fall back to opening a \
         URL in the host browser only for target=\"host\"."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::from_str(
            r#"{
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "start", "stop", "profiles", "tabs", "open", "focus", "close", "snapshot", "screenshot", "navigate", "console", "pdf", "upload", "dialog", "act"],
                        "description": "Browser action to perform"
                    },
                    "target": { "type": "string", "enum": ["sandbox", "host", "node"], "description": "Browser location. sandbox uses the managed browser runtime, host uses the external host browser server, node routes to a configured remote browser server." },
                    "node": { "type": "string", "description": "Node browser proxy identifier or full browser server URL. Requires target=node. If omitted, IRONCLAW_BROWSER_NODE_BASE_URL is used." },
                    "profile": { "type": "string", "description": "Optional browser profile. profile=chrome prefers host behavior; profile=openclaw prefers isolated managed browser behavior." },
                    "url": { "type": "string", "description": "Target URL for open/navigate actions" },
                    "targetId": { "type": "string", "description": "Browser tab target ID" },
                    "targetUrl": { "type": "string", "description": "Alias used by some callers for URL-based browser actions" },
                    "limit": { "type": "integer" },
                    "maxChars": { "type": "integer" },
                    "mode": { "type": "string", "enum": ["efficient"] },
                    "snapshotFormat": { "type": "string", "enum": ["aria", "ai"], "description": "Snapshot output style. ai is best for agent navigation; aria is tree-shaped." },
                    "refs": { "type": "string", "enum": ["role", "aria"], "description": "Ref strategy for snapshot. Prefer aria for stable refs across calls; role is lighter but more local to that snapshot." },
                    "interactive": { "type": "boolean" },
                    "compact": { "type": "boolean" },
                    "depth": { "type": "integer" },
                    "selector": { "type": "string", "description": "CSS selector fallback. For act, prefer ref from snapshot." },
                    "frame": { "type": "string" },
                    "labels": { "type": "boolean" },
                    "fullPage": { "type": "boolean" },
                    "ref": { "type": "string", "description": "Snapshot ref such as e12. Preferred for act and element-targeted actions." },
                    "element": { "type": "string", "description": "Legacy alias for ref in act, or element identifier for screenshot/upload." },
                    "type": { "type": "string", "enum": ["png", "jpeg"] },
                    "level": { "type": "string" },
                    "paths": { "type": "array", "items": { "type": "string" } },
                    "inputRef": { "type": "string" },
                    "timeoutMs": { "type": "integer" },
                    "accept": { "type": "boolean" },
                    "promptText": { "type": "string" },
                    "request": { "type": "object", "description": "Raw request object for act action", "properties": {}, "additionalProperties": true },
                    "kind": {
                        "type": "string",
                        "enum": ["click", "type", "press", "hover", "drag", "select", "fill", "resize", "wait", "evaluate", "close"]
                    },
                    "doubleClick": { "type": "boolean" },
                    "button": { "type": "string" },
                    "modifiers": { "type": "array", "items": { "type": "string" } },
                    "text": { "type": "string" },
                    "submit": { "type": "boolean" },
                    "slowly": { "type": "boolean" },
                    "key": { "type": "string" },
                    "delayMs": { "type": "integer" },
                    "startRef": { "type": "string" },
                    "endRef": { "type": "string" },
                    "values": { "type": "array", "items": { "type": "string" } },
                    "fields": { "type": "array", "items": { "type": "object", "properties": {}, "additionalProperties": true } },
                    "width": { "type": "integer" },
                    "height": { "type": "integer" },
                    "timeMs": { "type": "integer" },
                    "textGone": { "type": "string" },
                    "loadState": { "type": "string" },
                    "fn": { "type": "string" }
                },
                "required": ["action"]
            }"#,
        )
        .expect("browser schema must be valid JSON")
    }

    async fn execute(&self, params: Value, _ctx: &JobContext) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        let action = require_str(&params, "action")?;
        let route = self.resolve_route(&params).await?;

        let result = match action {
            "status" => {
                let query = Self::profile_suffix(route.profile.as_deref());
                if route.base_url.is_some() {
                    let mut status = self
                        .proxy_json_guided(
                            &route,
                            &params,
                            reqwest::Method::GET,
                            "/_ironclaw/runtime",
                            &query,
                            None,
                        )
                        .await?;
                    if route.target == BrowserTarget::Sandbox {
                        let guard = Self::runtime_state().lock().await;
                        Self::annotate_managed_runtime_status(&mut status, guard.as_ref());
                    }
                    status
                } else {
                    json!({
                        "enabled": false,
                        "running": false,
                        "configured": false,
                        "fallback_open": true,
                        "target": match route.target {
                            BrowserTarget::Host => "host",
                            BrowserTarget::Sandbox => "sandbox",
                            BrowserTarget::Node => "node",
                        }
                    })
                }
            }
            "start" => {
                self.proxy_json_guided(
                    &route,
                    &params,
                    reqwest::Method::POST,
                    "/start",
                    &Self::profile_suffix(route.profile.as_deref()),
                    None,
                )
                .await?
            }
            "stop" => {
                self.proxy_json_guided(
                    &route,
                    &params,
                    reqwest::Method::POST,
                    "/stop",
                    &Self::profile_suffix(route.profile.as_deref()),
                    None,
                )
                .await?
            }
            "profiles" => {
                self.proxy_json_guided(&route, &params, reqwest::Method::GET, "/profiles", "", None)
                    .await?
            }
            "tabs" => {
                self.proxy_json_guided(
                    &route,
                    &params,
                    reqwest::Method::GET,
                    "/tabs",
                    &Self::profile_suffix(route.profile.as_deref()),
                    None,
                )
                .await?
            }
            "open" => {
                self.execute_open_or_navigate(&route, "open", &normalize_url_alias(params))
                    .await?
            }
            "navigate" => {
                self.execute_open_or_navigate(&route, "navigate", &normalize_url_alias(params))
                    .await?
            }
            "focus" => {
                self.proxy_json_guided(
                    &route,
                    &params,
                    reqwest::Method::POST,
                    "/tabs/focus",
                    &Self::profile_suffix(route.profile.as_deref()),
                    Some(json!({ "targetId": require_str(&params, "targetId")? })),
                )
                .await?
            }
            "close" => {
                self.proxy_json_guided(
                    &route,
                    &params,
                    reqwest::Method::DELETE,
                    &format!(
                        "/tabs/{}",
                        urlencoding::encode(require_str(&params, "targetId")?)
                    ),
                    &Self::profile_suffix(route.profile.as_deref()),
                    None,
                )
                .await?
            }
            "snapshot" => {
                let query = Self::query_string(&[
                    (
                        "format",
                        Self::extract_optional_string(&params, "snapshotFormat"),
                    ),
                    (
                        "targetId",
                        Self::extract_optional_string(&params, "targetId"),
                    ),
                    (
                        "limit",
                        Self::extract_optional_u64(&params, "limit").map(|v| v.to_string()),
                    ),
                    (
                        "maxChars",
                        Self::extract_optional_u64(&params, "maxChars").map(|v| v.to_string()),
                    ),
                    ("refs", Self::extract_optional_string(&params, "refs")),
                    (
                        "interactive",
                        Self::extract_optional_bool(&params, "interactive").map(|v| v.to_string()),
                    ),
                    (
                        "compact",
                        Self::extract_optional_bool(&params, "compact").map(|v| v.to_string()),
                    ),
                    (
                        "depth",
                        Self::extract_optional_u64(&params, "depth").map(|v| v.to_string()),
                    ),
                    (
                        "selector",
                        Self::extract_optional_string(&params, "selector"),
                    ),
                    ("frame", Self::extract_optional_string(&params, "frame")),
                    (
                        "labels",
                        Self::extract_optional_bool(&params, "labels")
                            .map(|v| if v { "1" } else { "0" }.to_string()),
                    ),
                    ("mode", Self::extract_optional_string(&params, "mode")),
                    ("profile", route.profile.clone()),
                ]);
                self.proxy_json_guided(
                    &route,
                    &params,
                    reqwest::Method::GET,
                    "/snapshot",
                    &query,
                    None,
                )
                .await?
            }
            "screenshot" => {
                let image_type = Self::extract_optional_string(&params, "type")
                    .unwrap_or_else(|| "png".to_string());
                let content_prefix = if image_type == "jpeg" {
                    "image/jpeg"
                } else {
                    "image/png"
                };
                self.proxy_binary_to_temp_guided(
                    &route,
                    &params,
                    BinaryTempRequest {
                        path: "/screenshot",
                        query: &Self::profile_suffix(route.profile.as_deref()),
                        body: json!({
                            "targetId": Self::extract_optional_string(&params, "targetId"),
                            "fullPage": Self::extract_optional_bool(&params, "fullPage"),
                            "ref": Self::extract_optional_string(&params, "ref"),
                            "element": Self::extract_optional_string(&params, "element"),
                            "type": image_type,
                        }),
                        extension: if image_type == "jpeg" { "jpg" } else { "png" },
                        content_type_prefix: content_prefix,
                    },
                )
                .await?
            }
            "console" => {
                let query = Self::query_string(&[
                    ("level", Self::extract_optional_string(&params, "level")),
                    (
                        "targetId",
                        Self::extract_optional_string(&params, "targetId"),
                    ),
                    ("profile", route.profile.clone()),
                ]);
                self.proxy_json_guided(
                    &route,
                    &params,
                    reqwest::Method::GET,
                    "/console",
                    &query,
                    None,
                )
                .await?
            }
            "pdf" => {
                self.proxy_binary_to_temp_guided(
                    &route,
                    &params,
                    BinaryTempRequest {
                        path: "/pdf",
                        query: &Self::profile_suffix(route.profile.as_deref()),
                        body: json!({
                            "targetId": Self::extract_optional_string(&params, "targetId"),
                        }),
                        extension: "pdf",
                        content_type_prefix: "application/pdf",
                    },
                )
                .await?
            }
            "dialog" => {
                self.proxy_json_guided(
                    &route,
                    &params,
                    reqwest::Method::POST,
                    "/hooks/dialog",
                    &Self::profile_suffix(route.profile.as_deref()),
                    Some(json!({
                        "accept": Self::extract_optional_bool(&params, "accept").unwrap_or(true),
                        "promptText": Self::extract_optional_string(&params, "promptText"),
                        "targetId": Self::extract_optional_string(&params, "targetId"),
                        "timeoutMs": Self::extract_optional_u64(&params, "timeoutMs"),
                    })),
                )
                .await?
            }
            "upload" => {
                self.proxy_json_guided(
                    &route,
                    &params,
                    reqwest::Method::POST,
                    "/hooks/file-chooser",
                    &Self::profile_suffix(route.profile.as_deref()),
                    Some(json!({
                        "paths": Self::extract_paths(&params)?,
                        "ref": Self::extract_optional_string(&params, "ref"),
                        "inputRef": Self::extract_optional_string(&params, "inputRef"),
                        "element": Self::extract_optional_string(&params, "element"),
                        "targetId": Self::extract_optional_string(&params, "targetId"),
                        "timeoutMs": Self::extract_optional_u64(&params, "timeoutMs"),
                    })),
                )
                .await?
            }
            "act" => self.execute_act(&route, &params).await?,
            other => {
                return Err(ToolError::InvalidParameters(format!(
                    "unsupported browser action '{other}'"
                )));
            }
        };

        Ok(ToolOutput::success(result, start.elapsed()))
    }

    fn requires_approval(&self, params: &Value) -> ApprovalRequirement {
        match params.get("action").and_then(Value::as_str) {
            Some("status") | Some("profiles") | Some("tabs") | Some("snapshot")
            | Some("console") => ApprovalRequirement::Never,
            Some(_) => ApprovalRequirement::UnlessAutoApproved,
            None => ApprovalRequirement::Never,
        }
    }
}

fn normalize_url_alias(mut params: Value) -> Value {
    if params.get("url").is_none()
        && let Some(target_url) = params.get("targetUrl").cloned()
        && let Some(obj) = params.as_object_mut()
    {
        obj.insert("url".to_string(), target_url);
    }
    params
}

fn build_act_request(params: &Value) -> Result<Value, ToolError> {
    if let Some(request) = params.get("request") {
        return Ok(normalize_act_request(request.clone()));
    }

    let kind = require_str(params, "kind")?;
    let mut request = Map::new();
    request.insert("kind".to_string(), Value::String(kind.to_string()));

    for key in [
        "targetId",
        "ref",
        "doubleClick",
        "button",
        "modifiers",
        "text",
        "submit",
        "slowly",
        "key",
        "delayMs",
        "startRef",
        "endRef",
        "values",
        "fields",
        "width",
        "height",
        "timeMs",
        "selector",
        "url",
        "loadState",
        "textGone",
        "timeoutMs",
        "fn",
    ] {
        if let Some(value) = params.get(key) {
            request.insert(key.to_string(), value.clone());
        }
    }

    if !request.contains_key("ref")
        && let Some(element) = params.get("element")
    {
        request.insert("ref".to_string(), element.clone());
    }

    Ok(normalize_act_request(Value::Object(request)))
}

fn normalize_act_request(mut request: Value) -> Value {
    if let Some(obj) = request.as_object_mut()
        && !obj.contains_key("ref")
        && let Some(element) = obj.get("element").cloned()
    {
        obj.insert("ref".to_string(), element);
    }
    request
}

fn persist_temp_file(extension: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let mut file = Builder::new()
        .prefix("ironclaw-browser-")
        .suffix(&format!("-{}.{}", Uuid::new_v4(), extension))
        .tempfile_in(std::env::temp_dir())?;
    use std::io::Write as _;
    file.write_all(bytes)?;
    let (_file, path) = file.keep()?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    async fn env_lock() -> tokio::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }

    #[test]
    fn normalize_url_alias_copies_target_url() {
        let params = json!({
            "action": "open",
            "targetUrl": "https://example.com"
        });
        let normalized = normalize_url_alias(params);
        assert_eq!(
            normalized.get("url").and_then(Value::as_str),
            Some("https://example.com")
        );
    }

    #[test]
    fn build_act_request_uses_raw_request_when_present() {
        let request = json!({"kind": "click", "ref": "e12"});
        let params = json!({"action": "act", "request": request});
        assert_eq!(
            build_act_request(&params).unwrap(),
            json!({"kind": "click", "ref": "e12"})
        );
    }

    #[test]
    fn build_act_request_flattens_legacy_fields() {
        let params = json!({
            "action": "act",
            "kind": "type",
            "ref": "e7",
            "text": "hello"
        });
        assert_eq!(
            build_act_request(&params).unwrap(),
            json!({"kind": "type", "ref": "e7", "text": "hello"})
        );
    }

    #[test]
    fn build_act_request_maps_element_alias() {
        let params = json!({
            "action": "act",
            "kind": "click",
            "element": "e12"
        });
        assert_eq!(
            build_act_request(&params).unwrap(),
            json!({"kind": "click", "ref": "e12"})
        );
    }

    #[test]
    fn build_act_request_normalizes_raw_element_alias() {
        let params = json!({
            "action": "act",
            "request": {
                "kind": "click",
                "element": "e12"
            }
        });
        assert_eq!(
            build_act_request(&params).unwrap(),
            json!({"kind": "click", "element": "e12", "ref": "e12"})
        );
    }

    #[tokio::test]
    async fn annotate_managed_runtime_status_adds_runtime_metadata() {
        let mut status = json!({
            "enabled": true,
            "running": true
        });
        let runtime = ManagedRuntime {
            base_url: "http://127.0.0.1:24242".to_string(),
            child: Command::new("true")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("should spawn true for test runtime"),
            reused_existing: true,
        };
        BrowserTool::annotate_managed_runtime_status(&mut status, Some(&runtime));
        assert_eq!(
            status
                .get("managedRuntimeReusedExisting")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            status.get("managedRuntimeBaseUrl").and_then(Value::as_str),
            Some("http://127.0.0.1:24242")
        );
        assert_eq!(
            status
                .get("managedRuntimeStartupMode")
                .and_then(Value::as_str),
            Some("reused-existing")
        );
    }

    #[test]
    fn strip_target_id_from_act_request_removes_only_target_id() {
        let request = json!({
            "kind": "click",
            "targetId": "tab-1",
            "ref": "e12"
        });
        assert_eq!(
            BrowserTool::strip_target_id_from_act_request(&request).unwrap(),
            json!({
                "kind": "click",
                "ref": "e12"
            })
        );
    }

    #[test]
    fn chrome_relay_error_detection_matches_known_messages() {
        let disconnected = ToolError::ExternalService(
            "browser server returned 500: Chrome relay extension not connected".to_string(),
        );
        let stale =
            ToolError::ExternalService("browser server returned 404: target not found".to_string());
        let ambiguous = ToolError::ExternalService(
            "browser server returned 409: ambiguous target id prefix".to_string(),
        );

        assert!(BrowserTool::is_chrome_relay_not_connected(&disconnected));
        assert!(BrowserTool::is_chrome_stale_target(&stale));
        assert!(BrowserTool::is_chrome_ambiguous_target(&ambiguous));
    }

    #[tokio::test]
    async fn map_chrome_relay_error_rewrites_not_connected_message() {
        let tool = BrowserTool::new();
        let route = BrowserRoute {
            target: BrowserTarget::Host,
            profile: Some("chrome".to_string()),
            base_url: Some("http://127.0.0.1:1".to_string()),
        };
        let err = ToolError::ExternalService(
            "browser server returned 500: Chrome relay extension not connected".to_string(),
        );

        let mapped = tool
            .map_chrome_relay_error(&route, &json!({ "action": "tabs" }), err)
            .await;
        assert_eq!(
            format!("{mapped}"),
            "Execution failed: Chrome relay is not connected. Click the IronClaw Browser Relay toolbar icon on the tab you want to control (badge ON), then retry."
        );
    }

    #[tokio::test]
    async fn map_chrome_relay_error_rewrites_ambiguous_target_message() {
        let tool = BrowserTool::new();
        let route = BrowserRoute {
            target: BrowserTarget::Host,
            profile: Some("chrome".to_string()),
            base_url: Some("http://127.0.0.1:1".to_string()),
        };
        let err = ToolError::ExternalService(
            "browser server returned 409: ambiguous target id prefix".to_string(),
        );

        let mapped = tool
            .map_chrome_relay_error(&route, &json!({ "action": "tabs", "targetId": "abc" }), err)
            .await;
        assert_eq!(
            format!("{mapped}"),
            "Execution failed: Chrome targetId prefix is ambiguous. Run action=tabs profile=\"chrome\" and use a longer or full targetId."
        );
    }

    #[test]
    fn description_mentions_chrome_relay_and_node_routing() {
        let tool = BrowserTool::new();
        let description = tool.description();
        assert!(
            description.contains("Chrome extension relay"),
            "browser description should mention Chrome extension relay"
        );
        assert!(
            description.contains("profile=\"chrome\""),
            "browser description should mention chrome profile guidance"
        );
        assert!(
            description.contains("target=\"node\""),
            "browser description should mention node routing guidance"
        );
        assert!(
            description.contains("last selected tab"),
            "browser description should mention last-target reuse guidance"
        );
    }

    #[tokio::test]
    async fn resolve_route_prefers_host_for_chrome_profile() {
        let _guard = env_lock().await;
        let tool = BrowserTool::new();
        unsafe {
            std::env::set_var(ENV_BROWSER_BASE_URL, "http://127.0.0.1:12345");
            std::env::set_var(ENV_BROWSER_RUNTIME_MANAGED, "0");
        }
        let route = tool
            .resolve_route(&json!({
                "action": "status",
                "profile": "chrome"
            }))
            .await
            .unwrap();
        assert_eq!(route.target, BrowserTarget::Host);
        assert_eq!(route.base_url.as_deref(), Some("http://127.0.0.1:12345"));
        unsafe {
            std::env::remove_var(ENV_BROWSER_BASE_URL);
            std::env::remove_var(ENV_BROWSER_RUNTIME_MANAGED);
        }
    }

    #[tokio::test]
    async fn resolve_route_rejects_node_without_target() {
        let _guard = env_lock().await;
        let tool = BrowserTool::new();
        let err = tool
            .resolve_route(&json!({
                "action": "status",
                "node": "worker-1"
            }))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("only valid with target='node'"));
    }

    #[tokio::test]
    async fn resolve_route_uses_default_node_base_url() {
        let _guard = env_lock().await;
        let tool = BrowserTool::new();
        unsafe {
            std::env::set_var(ENV_BROWSER_NODE_BASE_URL, "http://10.0.0.5:24242");
        }
        let route = tool
            .resolve_route(&json!({
                "action": "status",
                "target": "node"
            }))
            .await
            .unwrap();
        assert_eq!(route.target, BrowserTarget::Node);
        assert_eq!(route.base_url.as_deref(), Some("http://10.0.0.5:24242"));
        unsafe {
            std::env::remove_var(ENV_BROWSER_NODE_BASE_URL);
        }
    }

    #[tokio::test]
    async fn resolve_route_uses_named_node_from_map() {
        let _guard = env_lock().await;
        let tool = BrowserTool::new();
        unsafe {
            std::env::set_var(
                ENV_BROWSER_NODE_MAP,
                r#"{"office-mac":"http://192.168.50.7:24242","worker-2":"http://10.0.0.2:24242"}"#,
            );
        }
        let route = tool
            .resolve_route(&json!({
                "action": "status",
                "target": "node",
                "node": "office-mac"
            }))
            .await
            .unwrap();
        assert_eq!(route.target, BrowserTarget::Node);
        assert_eq!(route.base_url.as_deref(), Some("http://192.168.50.7:24242"));
        unsafe {
            std::env::remove_var(ENV_BROWSER_NODE_MAP);
        }
    }

    #[tokio::test]
    async fn resolve_route_accepts_node_url_directly() {
        let _guard = env_lock().await;
        let tool = BrowserTool::new();
        let route = tool
            .resolve_route(&json!({
                "action": "status",
                "target": "node",
                "node": "http://127.0.0.1:31234"
            }))
            .await
            .unwrap();
        assert_eq!(route.target, BrowserTarget::Node);
        assert_eq!(route.base_url.as_deref(), Some("http://127.0.0.1:31234"));
    }

    #[test]
    fn approval_is_read_only_for_snapshot() {
        let tool = BrowserTool::new();
        assert_eq!(
            tool.requires_approval(&json!({"action": "snapshot"})),
            ApprovalRequirement::Never
        );
        assert_eq!(
            tool.requires_approval(&json!({"action": "open"})),
            ApprovalRequirement::UnlessAutoApproved
        );
    }

    #[tokio::test]
    #[ignore = "requires Node.js + playwright chromium installed locally"]
    async fn managed_runtime_can_autostart() {
        let _guard = env_lock().await;
        let tool = BrowserTool::new();
        let port = reserve_local_test_port();
        unsafe {
            std::env::remove_var(ENV_BROWSER_BASE_URL);
            std::env::set_var(ENV_BROWSER_RUNTIME_MANAGED, "1");
            std::env::set_var(ENV_BROWSER_RUNTIME_PORT, port.to_string());
            std::env::set_var(ENV_BROWSER_RUNTIME_HEADLESS, "1");
        }

        let output = tool
            .execute(json!({ "action": "status" }), &JobContext::default())
            .await
            .expect("managed runtime should start");
        assert_eq!(
            output.result.get("enabled").and_then(Value::as_bool),
            Some(true)
        );

        if let Some(runtime) = BrowserTool::runtime_state().lock().await.as_mut() {
            let _ = runtime.child.start_kill();
        }
        unsafe {
            std::env::remove_var(ENV_BROWSER_RUNTIME_PORT);
        }
    }

    fn reserve_local_test_port() -> u16 {
        std::net::TcpListener::bind(("127.0.0.1", 0))
            .expect("should reserve ephemeral test port")
            .local_addr()
            .expect("listener should have local addr")
            .port()
    }
}
