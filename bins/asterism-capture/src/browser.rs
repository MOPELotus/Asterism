use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use anyhow::{Context, bail};
use asterism_provider_api::{
    BrowserSessionSpec, CaptureReadiness, CaptureRecipe, CaptureScalarSource, CaptureValueSource,
};
use asterism_secrets::SecretString;
use reqwest::{Client, StatusCode, Url, redirect::Policy};
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use zeroize::Zeroizing;

use crate::{CaptureResolution, CaptureSnapshot, cdp_websocket::CdpWebSocket};

const DEVTOOLS_FILE_LIMIT: u64 = 1024;
const DEVTOOLS_LIST_LIMIT: usize = 64 * 1024;
const MAX_TRACKED_REQUESTS: usize = 512;
const BROWSER_START_TIMEOUT: Duration = Duration::from_secs(30);

/// A visible, isolated Chromium process that executes one frozen Capture
/// recipe through an origin-allowlisted `DevTools` session.
pub struct ChromiumCapture {
    recipe: CaptureRecipe,
    process: IsolatedBrowserProcess,
    target_id: String,
    cdp: CdpSession,
}

/// One stable top-level browser document observed under a frozen
/// `BrowserSessionSpec` origin allowlist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserBridgeDocumentBinding {
    pub observed_origin: String,
    pub frame_id: String,
}

/// An isolated Chromium process prepared for typed `BrowserBridge` commands.
/// This boundary launches and binds the document only; it does not interpret
/// opaque Provider command artifacts.
pub struct ChromiumBrowserBridge {
    spec: BrowserSessionSpec,
    process: IsolatedBrowserProcess,
    target_id: String,
    cdp: CdpSession,
}

impl std::fmt::Debug for ChromiumBrowserBridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChromiumBrowserBridge")
            .field("spec", &self.spec)
            .field("target_id", &self.target_id)
            .field("browser", &"running")
            .field("cdp", &"attached")
            .finish_non_exhaustive()
    }
}

impl ChromiumBrowserBridge {
    /// Launches the exact frozen `BrowserBridge` start route in an isolated
    /// profile and attaches only to an allowlisted page target.
    ///
    /// # Errors
    ///
    /// Rejects an invalid specification/browser path, failed process launch,
    /// unsafe `DevTools` endpoint, target drift or CDP initialization failure.
    pub async fn launch(
        spec: BrowserSessionSpec,
        browser_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        spec.validate().context("BrowserBridge policy is invalid")?;
        let profile = tempfile::Builder::new()
            .prefix("asterism-browser-bridge-")
            .tempdir()
            .context("failed to create the isolated BrowserBridge profile")?;
        let browser = launch_browser(&spec.start_url, profile.path(), browser_path, spec.headless)?;
        let mut process = IsolatedBrowserProcess::new(browser, profile);
        let profile_path = process.profile_path().to_path_buf();
        let port = wait_for_devtools_port(process.browser_mut(), &profile_path).await?;
        let target = wait_for_allowed_target(
            process.browser_mut(),
            port,
            &spec.allowed_origins,
            "BrowserBridge",
        )
        .await?;
        let socket = CdpWebSocket::connect(&target.web_socket_debugger_url).await?;
        let mut cdp = CdpSession::new(socket, BTreeSet::new());
        initialize_cdp(&mut cdp).await?;
        let mut bridge = Self {
            spec,
            process,
            target_id: target.id,
            cdp,
        };
        bridge.wait_for_initial_document().await?;
        Ok(bridge)
    }

    /// Returns the current stable top-level origin/frame binding after
    /// repeating the frozen navigation allowlist check.
    ///
    /// # Errors
    ///
    /// Returns an error when the browser exited, the document is missing or
    /// navigation escaped the frozen policy.
    pub async fn document_binding(&mut self) -> anyhow::Result<BrowserBridgeDocumentBinding> {
        if self.process.browser_mut().try_wait()?.is_some() {
            bail!("isolated BrowserBridge browser exited before document binding");
        }
        let document = current_document(&mut self.cdp, &self.spec.allowed_origins).await?;
        Ok(BrowserBridgeDocumentBinding {
            observed_origin: document.origin,
            frame_id: document.frame_id,
        })
    }

    /// Closes the isolated browser and reclaims its process tree/profile.
    ///
    /// # Errors
    ///
    /// Returns an error when the browser process tree or profile cannot be
    /// reclaimed.
    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            self.cdp.command("Browser.close", json!({})),
        )
        .await;
        self.process.shutdown().await
    }

    async fn wait_for_initial_document(&mut self) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + BROWSER_START_TIMEOUT;
        loop {
            if self.process.browser_mut().try_wait()?.is_some() {
                bail!("isolated BrowserBridge browser exited before its document was ready");
            }
            if current_document_candidate(&mut self.cdp, &self.spec.allowed_origins)
                .await?
                .is_some()
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("isolated BrowserBridge document did not become ready in time");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl std::fmt::Debug for ChromiumCapture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChromiumCapture")
            .field("recipe", &self.recipe)
            .field("target_id", &self.target_id)
            .field("browser", &"running")
            .field("cdp", &"attached")
            .finish_non_exhaustive()
    }
}

impl ChromiumCapture {
    /// Launches an isolated browser profile, opens the recipe start URL and
    /// attaches only to the matching page target.
    ///
    /// # Errors
    ///
    /// Rejects an invalid recipe/browser path, failed process launch, unsafe
    /// `DevTools` endpoints, target mismatch or CDP initialization failure.
    pub async fn launch(
        recipe: CaptureRecipe,
        browser_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        recipe.validate().context("Capture recipe is invalid")?;
        let profile = tempfile::Builder::new()
            .prefix("asterism-capture-")
            .tempdir()
            .context("failed to create the isolated browser profile")?;
        let browser = launch_browser(&recipe.start_url, profile.path(), browser_path, false)?;
        // Install the process-tree guard immediately after spawn. Startup can
        // fail before CDP exists, and `Child` alone does not terminate its
        // Chromium subprocesses when dropped.
        let mut process = IsolatedBrowserProcess::new(browser, profile);
        let profile_path = process.profile_path().to_path_buf();
        let port = wait_for_devtools_port(process.browser_mut(), &profile_path).await?;
        let target = wait_for_allowed_target(
            process.browser_mut(),
            port,
            &recipe.navigation_origins,
            "Capture recipe",
        )
        .await?;
        let socket = CdpWebSocket::connect(&target.web_socket_debugger_url).await?;
        let mut cdp = CdpSession::new(socket, declared_headers(&recipe));
        initialize_cdp(&mut cdp).await?;
        let mut capture = Self {
            recipe,
            process,
            target_id: target.id,
            cdp,
        };
        capture.wait_for_initial_document().await?;
        Ok(capture)
    }

    /// Polls the current page until every required recipe output can be
    /// resolved from one stable target/document observation.
    ///
    /// # Errors
    ///
    /// Fails if the browser exits, the deadline expires, CDP drifts, or an
    /// unsafe/oversized browser value is observed.
    pub async fn capture_until(
        &mut self,
        deadline: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<Vec<crate::CaptureCredentialField>> {
        loop {
            if self.process.browser_mut().try_wait()?.is_some() {
                bail!("isolated Capture browser exited before credentials were available");
            }
            if chrono::Utc::now() >= deadline {
                bail!("Capture pairing session expired before the browser recipe completed");
            }
            let (snapshot, ready) = self.capture_snapshot().await?;
            match snapshot.resolve()? {
                CaptureResolution::Ready(fields) if ready => return Ok(fields),
                CaptureResolution::Ready(_) | CaptureResolution::Incomplete { .. } => {}
            }
            tokio::time::sleep(Duration::from_millis(self.recipe.poll_interval_millis)).await;
        }
    }

    /// Closes the isolated browser instance and removes its temporary profile.
    /// A process-tree termination is used only when the normal CDP close path
    /// does not finish within the bounded grace period.
    ///
    /// # Errors
    ///
    /// Returns an error when the process tree or its temporary profile cannot
    /// be reclaimed.
    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            self.cdp.command("Browser.close", json!({})),
        )
        .await;
        self.process.shutdown().await
    }

    async fn capture_snapshot(&mut self) -> anyhow::Result<(CaptureSnapshot, bool)> {
        let document = self.current_document().await?;
        let mut snapshot = CaptureSnapshot::new(
            self.recipe.clone(),
            self.target_id.clone(),
            document.loader_id.clone(),
        )?;
        self.cdp
            .copy_observed_headers(&document.loader_id, &mut snapshot)?;
        self.capture_storage(&document, &mut snapshot).await?;
        self.capture_cookies(&mut snapshot).await?;
        let confirmation = self.current_document().await?;
        if confirmation != document {
            bail!("browser document changed during one Capture snapshot");
        }
        let ready = self
            .cdp
            .readiness_satisfied(&self.recipe.readiness, &document.loader_id);
        Ok((snapshot, ready))
    }

    async fn wait_for_initial_document(&mut self) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + BROWSER_START_TIMEOUT;
        loop {
            if self.process.browser_mut().try_wait()?.is_some() {
                bail!("isolated Capture browser exited before its document was ready");
            }
            if self.current_document_candidate().await?.is_some() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("isolated Capture browser document did not become ready in time");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn current_document(&mut self) -> anyhow::Result<DocumentBinding> {
        current_document(&mut self.cdp, &self.recipe.navigation_origins).await
    }

    async fn current_document_candidate(&mut self) -> anyhow::Result<Option<DocumentBinding>> {
        current_document_candidate(&mut self.cdp, &self.recipe.navigation_origins).await
    }

    async fn capture_storage(
        &mut self,
        document: &DocumentBinding,
        snapshot: &mut CaptureSnapshot,
    ) -> anyhow::Result<()> {
        for (local, key) in storage_sources(&self.recipe, &document.origin) {
            let storage = if local {
                "window.localStorage"
            } else {
                "window.sessionStorage"
            };
            let encoded_key = serde_json::to_string(&key)?;
            let expression = format!("{storage}.getItem({encoded_key})");
            let response = self
                .cdp
                .command(
                    "Runtime.evaluate",
                    json!({
                        "expression": expression,
                        "returnByValue": true,
                        "awaitPromise": false,
                        "userGesture": false
                    }),
                )
                .await?;
            if response
                .value()
                .pointer("/result/exceptionDetails")
                .is_some()
            {
                continue;
            }
            let Some(value) = response
                .value()
                .pointer("/result/result/value")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let value = SecretString::new(value.to_owned());
            if local {
                snapshot.insert_local_storage(&document.origin, &key, value)?;
            } else {
                snapshot.insert_session_storage(&document.origin, &key, value)?;
            }
        }
        Ok(())
    }

    async fn capture_cookies(&mut self, snapshot: &mut CaptureSnapshot) -> anyhow::Result<()> {
        for origin in cookie_origins(&self.recipe) {
            let response = self
                .cdp
                .command(
                    "Network.getCookies",
                    json!({"urls": [format!("{origin}/")]}),
                )
                .await?;
            let Some(cookies) = response
                .value()
                .pointer("/result/cookies")
                .and_then(Value::as_array)
            else {
                bail!("DevTools cookie response is invalid");
            };
            let mut fields = Zeroizing::new(Vec::new());
            for cookie in cookies {
                let Some(name) = bounded_text(cookie.get("name"), 256) else {
                    bail!("DevTools returned an invalid Cookie name");
                };
                let Some(value) = bounded_text(cookie.get("value"), 64 * 1024) else {
                    bail!("DevTools returned an invalid Cookie value");
                };
                if name
                    .bytes()
                    .any(|byte| matches!(byte, b'=' | b';' | b'\r' | b'\n'))
                    || value
                        .bytes()
                        .any(|byte| matches!(byte, b';' | b'\r' | b'\n'))
                {
                    bail!("DevTools returned a malformed Cookie field");
                }
                fields.push(format!("{name}={value}"));
            }
            if !fields.is_empty() {
                let mut header = fields.join("; ");
                snapshot.insert_cookie_header(
                    &origin,
                    SecretString::new(std::mem::take(&mut header)),
                )?;
            }
        }
        Ok(())
    }
}

struct IsolatedBrowserProcess {
    browser: Child,
    profile: Option<TempDir>,
}

impl IsolatedBrowserProcess {
    fn new(browser: Child, profile: TempDir) -> Self {
        Self {
            browser,
            profile: Some(profile),
        }
    }

    fn browser_mut(&mut self) -> &mut Child {
        &mut self.browser
    }

    fn profile_path(&self) -> &Path {
        self.profile
            .as_ref()
            .expect("browser profile remains present until shutdown")
            .path()
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while self.browser.try_wait()?.is_none() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if self.browser.try_wait()?.is_none() {
            terminate_browser_tree(&mut self.browser)?;
        }
        let profile = self
            .profile
            .take()
            .context("isolated browser profile was already reclaimed")?;
        profile
            .close()
            .context("failed to remove the isolated browser profile")
    }
}

impl Drop for IsolatedBrowserProcess {
    fn drop(&mut self) {
        let _ = terminate_browser_tree(&mut self.browser);
        if let Some(profile) = self.profile.take() {
            let _ = profile.close();
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DocumentBinding {
    loader_id: String,
    frame_id: String,
    origin: String,
}

async fn initialize_cdp(cdp: &mut CdpSession) -> anyhow::Result<()> {
    cdp.command("Page.enable", json!({})).await?;
    cdp.command(
        "Network.enable",
        json!({
            "maxTotalBufferSize": 0,
            "maxResourceBufferSize": 0,
            "maxPostDataSize": 0
        }),
    )
    .await?;
    cdp.command("Runtime.enable", json!({})).await?;
    Ok(())
}

async fn current_document(
    cdp: &mut CdpSession,
    allowed_origins: &[String],
) -> anyhow::Result<DocumentBinding> {
    current_document_candidate(cdp, allowed_origins)
        .await?
        .context("DevTools frame-tree document is temporarily empty")
}

async fn current_document_candidate(
    cdp: &mut CdpSession,
    allowed_origins: &[String],
) -> anyhow::Result<Option<DocumentBinding>> {
    let response = cdp.command("Page.getFrameTree", json!({})).await?;
    let frame = response
        .value()
        .pointer("/result/frameTree/frame")
        .and_then(Value::as_object)
        .context("DevTools frame-tree response is invalid")?;
    let loader_id = frame
        .get("loaderId")
        .and_then(Value::as_str)
        .context("DevTools frame-tree has no loader ID")?;
    if loader_id.is_empty() {
        return Ok(None);
    }
    let loader_id = bounded_text(frame.get("loaderId"), 256)
        .context("DevTools frame-tree has no bounded loader ID")?;
    let frame_id = bounded_text(frame.get("id"), 256)
        .context("DevTools frame-tree has no bounded frame ID")?;
    let raw_url = frame
        .get("url")
        .and_then(Value::as_str)
        .context("DevTools frame-tree has no URL")?;
    if raw_url.is_empty() {
        return Ok(None);
    }
    let url =
        bounded_text(frame.get("url"), 2_048).context("DevTools frame-tree has no bounded URL")?;
    let origin = canonical_origin(&url).context("browser page URL has no safe HTTPS origin")?;
    if !allowed_origins.iter().any(|allowed| allowed == &origin) {
        bail!("browser navigated outside the frozen origin allowlist");
    }
    Ok(Some(DocumentBinding {
        loader_id,
        frame_id,
        origin,
    }))
}

struct CdpSession {
    socket: CdpWebSocket,
    next_id: u64,
    declared_headers: BTreeSet<(String, String)>,
    requests: BTreeMap<String, RequestBinding>,
    pending_headers: BTreeMap<String, BTreeMap<String, SecretString>>,
    observed_headers: BTreeMap<(String, String, String), SecretString>,
    observed_requests: BTreeSet<(String, String, String, String)>,
    observed_responses: BTreeSet<(String, String, String, String, u16, String)>,
}

impl CdpSession {
    fn new(socket: CdpWebSocket, declared_headers: BTreeSet<(String, String)>) -> Self {
        Self {
            socket,
            next_id: 1,
            declared_headers,
            requests: BTreeMap::new(),
            pending_headers: BTreeMap::new(),
            observed_headers: BTreeMap::new(),
            observed_requests: BTreeSet::new(),
            observed_responses: BTreeSet::new(),
        }
    }

    async fn command(
        &mut self,
        method: &str,
        params: Value,
    ) -> anyhow::Result<crate::cdp_websocket::ZeroizingJson> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .context("DevTools command ID exhausted")?;
        self.socket
            .send_json(&json!({"id": id, "method": method, "params": params}))
            .await?;
        loop {
            let response = self.socket.receive_json().await?;
            if response.value().get("id").and_then(Value::as_u64) == Some(id) {
                if response.value().get("error").is_some() {
                    bail!("DevTools rejected the {method} command");
                }
                return Ok(response);
            }
            self.observe_event(response.value())?;
        }
    }

    fn observe_event(&mut self, event: &Value) -> anyhow::Result<()> {
        match event.get("method").and_then(Value::as_str) {
            Some("Network.requestWillBeSent") => self.observe_request(event)?,
            Some("Network.requestWillBeSentExtraInfo") => self.observe_request_headers(event)?,
            Some("Network.responseReceived") => self.observe_response(event)?,
            Some("Page.frameNavigated") => self.observe_navigation(event),
            _ => {}
        }
        Ok(())
    }

    fn observe_request(&mut self, event: &Value) -> anyhow::Result<()> {
        let Some(request_id) = bounded_text(event.pointer("/params/requestId"), 256) else {
            return Ok(());
        };
        let Some(loader_id) = bounded_text(event.pointer("/params/loaderId"), 256) else {
            return Ok(());
        };
        let Some(url) = event.pointer("/params/request/url").and_then(Value::as_str) else {
            return Ok(());
        };
        let Some(origin) = canonical_origin(url) else {
            return Ok(());
        };
        let request_route = if let (Some(method), Some(path_and_query)) = (
            event
                .pointer("/params/request/method")
                .and_then(Value::as_str),
            canonical_path_and_query(url),
        ) && matches!(method, "GET" | "POST")
        {
            if self.observed_requests.len() >= MAX_TRACKED_REQUESTS {
                bail!("DevTools request readiness observations exceed their safety limit");
            }
            self.observed_requests.insert((
                loader_id.clone(),
                origin.clone(),
                method.to_owned(),
                path_and_query.clone(),
            ));
            Some((method.to_owned(), path_and_query))
        } else {
            None
        };
        if self.requests.len() >= MAX_TRACKED_REQUESTS {
            self.requests.clear();
            self.pending_headers.clear();
        }
        let pending_headers = self.pending_headers.remove(&request_id);
        self.requests.insert(
            request_id,
            RequestBinding {
                loader_id: loader_id.clone(),
                origin: origin.clone(),
                method: request_route.as_ref().map(|(method, _)| method.clone()),
                path_and_query: request_route.map(|(_, path)| path),
            },
        );
        self.observe_headers(
            &loader_id,
            &origin,
            event.pointer("/params/request/headers"),
        )?;
        if let Some(headers) = pending_headers {
            self.observe_header_values(&loader_id, &origin, headers);
        }
        Ok(())
    }

    fn observe_request_headers(&mut self, event: &Value) -> anyhow::Result<()> {
        let Some(request_id) = event.pointer("/params/requestId").and_then(Value::as_str) else {
            return Ok(());
        };
        let Some(binding) = self.requests.get(request_id).cloned() else {
            self.remember_pending_headers(request_id, event.pointer("/params/headers"))?;
            return Ok(());
        };
        self.observe_headers(
            &binding.loader_id,
            &binding.origin,
            event.pointer("/params/headers"),
        )
    }

    fn observe_response(&mut self, event: &Value) -> anyhow::Result<()> {
        let Some(request_id) = event.pointer("/params/requestId").and_then(Value::as_str) else {
            return Ok(());
        };
        let Some(binding) = self.requests.get(request_id) else {
            return Ok(());
        };
        let (Some(method), Some(path_and_query)) = (&binding.method, &binding.path_and_query)
        else {
            return Ok(());
        };
        let Some(status) = event
            .pointer("/params/response/status")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
        else {
            return Ok(());
        };
        let Some(mime_type) = bounded_text(event.pointer("/params/response/mimeType"), 128) else {
            return Ok(());
        };
        if self.observed_responses.len() >= MAX_TRACKED_REQUESTS {
            bail!("DevTools response readiness observations exceed their safety limit");
        }
        self.observed_responses.insert((
            binding.loader_id.clone(),
            binding.origin.clone(),
            method.clone(),
            path_and_query.clone(),
            status,
            mime_type.to_ascii_lowercase(),
        ));
        Ok(())
    }

    fn observe_navigation(&mut self, event: &Value) {
        let Some(loader_id) = top_level_loader_id(event) else {
            return;
        };
        self.requests
            .retain(|_, binding| binding.loader_id == loader_id);
        self.pending_headers.clear();
        self.observed_headers
            .retain(|(observed_loader, _, _), _| observed_loader == loader_id);
        self.observed_requests
            .retain(|(observed_loader, _, _, _)| observed_loader == loader_id);
        self.observed_responses
            .retain(|(observed_loader, _, _, _, _, _)| observed_loader == loader_id);
    }

    fn observe_headers(
        &mut self,
        loader_id: &str,
        origin: &str,
        headers: Option<&Value>,
    ) -> anyhow::Result<()> {
        let Some(headers) = headers.and_then(Value::as_object) else {
            return Ok(());
        };
        let mut values = BTreeMap::new();
        for (name, value) in headers {
            let normalized = name.to_ascii_lowercase();
            if !self
                .declared_headers
                .contains(&(origin.to_owned(), normalized.clone()))
            {
                continue;
            }
            let Some(value) = value.as_str() else {
                bail!("DevTools returned a non-string declared request header");
            };
            if value.is_empty() || value.len() > 1024 * 1024 {
                bail!("DevTools returned an empty or oversized declared request header");
            }
            values.insert(normalized, SecretString::new(value.to_owned()));
        }
        self.observe_header_values(loader_id, origin, values);
        Ok(())
    }

    fn remember_pending_headers(
        &mut self,
        request_id: &str,
        headers: Option<&Value>,
    ) -> anyhow::Result<()> {
        let Some(headers) = headers.and_then(Value::as_object) else {
            return Ok(());
        };
        if self.pending_headers.len() >= MAX_TRACKED_REQUESTS {
            self.pending_headers.clear();
        }
        let declared_names = self
            .declared_headers
            .iter()
            .map(|(_, name)| name.as_str())
            .collect::<BTreeSet<_>>();
        let mut values = BTreeMap::new();
        for (name, value) in headers {
            let normalized = name.to_ascii_lowercase();
            if !declared_names.contains(normalized.as_str()) {
                continue;
            }
            let Some(value) = value.as_str() else {
                bail!("DevTools returned a non-string declared request header");
            };
            if value.is_empty() || value.len() > 1024 * 1024 {
                bail!("DevTools returned an empty or oversized declared request header");
            }
            values.insert(normalized, SecretString::new(value.to_owned()));
        }
        if !values.is_empty() {
            self.pending_headers.insert(request_id.to_owned(), values);
        }
        Ok(())
    }

    fn observe_header_values(
        &mut self,
        loader_id: &str,
        origin: &str,
        values: BTreeMap<String, SecretString>,
    ) {
        for (name, value) in values {
            if self
                .declared_headers
                .contains(&(origin.to_owned(), name.clone()))
            {
                self.observed_headers
                    .insert((loader_id.to_owned(), origin.to_owned(), name), value);
            }
        }
    }

    fn copy_observed_headers(
        &self,
        loader_id: &str,
        snapshot: &mut CaptureSnapshot,
    ) -> anyhow::Result<()> {
        for ((observed_loader, origin, name), value) in &self.observed_headers {
            if observed_loader == loader_id {
                snapshot.insert_request_header(
                    origin,
                    name,
                    SecretString::new(value.expose_secret().to_owned()),
                )?;
            }
        }
        Ok(())
    }

    fn readiness_satisfied(&self, readiness: &CaptureReadiness, loader_id: &str) -> bool {
        readiness_satisfied(
            readiness,
            loader_id,
            &self.observed_requests,
            &self.observed_responses,
        )
    }
}

fn readiness_satisfied(
    readiness: &CaptureReadiness,
    loader_id: &str,
    observed_requests: &BTreeSet<(String, String, String, String)>,
    observed_responses: &BTreeSet<(String, String, String, String, u16, String)>,
) -> bool {
    match readiness {
        CaptureReadiness::OutputsComplete => true,
        CaptureReadiness::RequestObserved {
            origin,
            method,
            path_and_query,
        } => observed_requests.contains(&(
            loader_id.to_owned(),
            origin.clone(),
            method.clone(),
            path_and_query.clone(),
        )),
        CaptureReadiness::ResponseObserved {
            origin,
            method,
            path_and_query,
            status,
            mime_type,
        } => observed_responses.contains(&(
            loader_id.to_owned(),
            origin.clone(),
            method.clone(),
            path_and_query.clone(),
            *status,
            mime_type.clone(),
        )),
    }
}

#[derive(Clone, Debug)]
struct RequestBinding {
    loader_id: String,
    origin: String,
    method: Option<String>,
    path_and_query: Option<String>,
}

fn top_level_loader_id(event: &Value) -> Option<&str> {
    if event.pointer("/params/frame/parentId").is_some() {
        return None;
    }
    event
        .pointer("/params/frame/loaderId")
        .and_then(Value::as_str)
}

fn canonical_path_and_query(raw_url: &str) -> Option<String> {
    let url = Url::parse(raw_url).ok()?;
    if url.scheme() != "https" || url.fragment().is_some() {
        return None;
    }
    let mut value = url.path().to_owned();
    if let Some(query) = url.query() {
        value.push('?');
        value.push_str(query);
    }
    (value.len() <= 2_048).then_some(value)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevToolsTarget {
    id: String,
    #[serde(rename = "type")]
    target_type: String,
    url: String,
    web_socket_debugger_url: String,
}

fn launch_browser(
    start_url: &str,
    profile: &Path,
    browser_path: Option<&Path>,
    headless: bool,
) -> anyhow::Result<Child> {
    let browser = browser_path
        .map(Path::to_path_buf)
        .or_else(find_browser)
        .context("no supported Chromium browser was found; pass --browser-path")?;
    if browser_path.is_some() && !browser.is_file() {
        bail!("the explicit browser path is not a file");
    }
    let mut command = Command::new(browser);
    command.args([
        "--remote-debugging-port=0",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-sync",
        "--new-window",
    ]);
    if headless {
        command.arg("--headless=new");
    }
    command
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(start_url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to launch the isolated Capture browser")
}

fn find_browser() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    let mut fallback_candidates = Vec::new();
    for variable in ["PROGRAMFILES", "PROGRAMFILES(X86)"] {
        if let Some(root) = env::var_os(variable) {
            let root = PathBuf::from(root);
            candidates.push(root.join("Microsoft/Edge/Application/msedge.exe"));
            candidates.push(root.join("Google/Chrome/Application/chrome.exe"));
            fallback_candidates.push(root.join("Microsoft/EdgeCore/Optimized/msedge.exe"));
            extend_versioned_browsers(&mut fallback_candidates, &root.join("Microsoft/EdgeCore"));
        }
    }
    if let Some(root) = env::var_os("LOCALAPPDATA") {
        let root = PathBuf::from(root);
        candidates.push(root.join("Microsoft/Edge/Application/msedge.exe"));
        candidates.push(root.join("Google/Chrome/Application/chrome.exe"));
        if let Ok(entries) = fs::read_dir(root.join("ms-playwright")) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("chromium-") {
                    candidates.push(entry.path().join("chrome-win64/chrome.exe"));
                }
            }
        }
    }
    candidates
        .into_iter()
        .chain(fallback_candidates)
        .find(|candidate| candidate.is_file())
}

fn extend_versioned_browsers(candidates: &mut Vec<PathBuf>, root: &Path) {
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            candidates.push(entry.path().join("msedge.exe"));
        }
    }
}

fn terminate_browser_tree(browser: &mut Child) -> anyhow::Result<()> {
    if browser.try_wait()?.is_some() {
        return Ok(());
    }
    #[cfg(windows)]
    {
        let status = Command::new("taskkill")
            .args(["/PID", &browser.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("failed to start exact browser process-tree termination")?;
        if !status.success() && browser.try_wait()?.is_none() {
            browser
                .kill()
                .context("failed to terminate isolated Capture browser")?;
        }
    }
    #[cfg(not(windows))]
    browser
        .kill()
        .context("failed to terminate isolated Capture browser")?;
    browser
        .wait()
        .context("failed to reap isolated Capture browser")?;
    Ok(())
}

async fn wait_for_devtools_port(browser: &mut Child, profile: &Path) -> anyhow::Result<u16> {
    let file = profile.join("DevToolsActivePort");
    let deadline = tokio::time::Instant::now() + BROWSER_START_TIMEOUT;
    loop {
        if browser.try_wait()?.is_some() {
            bail!("isolated Capture browser exited during startup");
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("isolated Capture browser did not expose DevTools in time");
        }
        if let Ok(metadata) = fs::metadata(&file)
            && metadata.len() <= DEVTOOLS_FILE_LIMIT
            && let Ok(contents) = fs::read_to_string(&file)
            && let Some(port) = contents.lines().next().and_then(|line| line.parse().ok())
        {
            return Ok(port);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_allowed_target(
    browser: &mut Child,
    port: u16,
    allowed_origins: &[String],
    boundary: &str,
) -> anyhow::Result<DevToolsTarget> {
    let client = Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;
    let url = format!("http://127.0.0.1:{port}/json/list");
    let deadline = tokio::time::Instant::now() + BROWSER_START_TIMEOUT;
    loop {
        if browser.try_wait()?.is_some() {
            bail!("isolated Capture browser exited before its page target was ready");
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("isolated browser did not expose the {boundary} page in time");
        }
        if let Ok(response) = client.get(&url).send().await
            && response.status() == StatusCode::OK
            && let Ok(mut targets) = read_target_list(response).await
            && let Some(target) = targets.drain(..).find(|target| {
                target.target_type == "page"
                    && canonical_origin(&target.url).is_some_and(|origin| {
                        allowed_origins.iter().any(|allowed| allowed == &origin)
                    })
                    && !target.id.is_empty()
                    && !target.web_socket_debugger_url.is_empty()
            })
        {
            return Ok(target);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn read_target_list(mut response: reqwest::Response) -> anyhow::Result<Vec<DevToolsTarget>> {
    if response
        .content_length()
        .is_some_and(|length| length > u64::try_from(DEVTOOLS_LIST_LIMIT).unwrap_or(u64::MAX))
    {
        bail!("DevTools target list exceeds its safety limit");
    }
    let mut document = Zeroizing::new(Vec::new());
    while let Some(chunk) = response.chunk().await? {
        if document.len() + chunk.len() > DEVTOOLS_LIST_LIMIT {
            bail!("DevTools target list exceeds its safety limit");
        }
        document.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&document).context("DevTools target list is invalid")
}

fn declared_headers(recipe: &CaptureRecipe) -> BTreeSet<(String, String)> {
    let mut headers = BTreeSet::new();
    for output in &recipe.outputs {
        for source in &output.sources {
            match source {
                CaptureValueSource::RequestHeader { origin, name } => {
                    headers.insert((origin.clone(), name.to_ascii_lowercase()));
                }
                CaptureValueSource::JsonObject { fields } => {
                    for field in fields {
                        for source in &field.sources {
                            if let CaptureScalarSource::RequestHeader { origin, name } = source {
                                headers.insert((origin.clone(), name.to_ascii_lowercase()));
                            }
                        }
                    }
                }
                CaptureValueSource::LocalStorage { .. }
                | CaptureValueSource::SessionStorage { .. }
                | CaptureValueSource::CookieHeader { .. } => {}
            }
        }
    }
    headers
}

fn storage_sources(recipe: &CaptureRecipe, origin: &str) -> BTreeSet<(bool, String)> {
    let mut sources = BTreeSet::new();
    for output in &recipe.outputs {
        for source in &output.sources {
            collect_storage_source(source, origin, &mut sources);
            if let CaptureValueSource::JsonObject { fields } = source {
                for field in fields {
                    for source in &field.sources {
                        match source {
                            CaptureScalarSource::LocalStorage {
                                origin: expected,
                                key,
                            } if expected == origin => {
                                sources.insert((true, key.clone()));
                            }
                            CaptureScalarSource::SessionStorage {
                                origin: expected,
                                key,
                            } if expected == origin => {
                                sources.insert((false, key.clone()));
                            }
                            CaptureScalarSource::RequestHeader { .. }
                            | CaptureScalarSource::LocalStorage { .. }
                            | CaptureScalarSource::SessionStorage { .. } => {}
                        }
                    }
                }
            }
        }
    }
    sources
}

fn collect_storage_source(
    source: &CaptureValueSource,
    origin: &str,
    sources: &mut BTreeSet<(bool, String)>,
) {
    match source {
        CaptureValueSource::LocalStorage {
            origin: expected,
            key,
        } if expected == origin => {
            sources.insert((true, key.clone()));
        }
        CaptureValueSource::SessionStorage {
            origin: expected,
            key,
        } if expected == origin => {
            sources.insert((false, key.clone()));
        }
        CaptureValueSource::RequestHeader { .. }
        | CaptureValueSource::CookieHeader { .. }
        | CaptureValueSource::JsonObject { .. }
        | CaptureValueSource::LocalStorage { .. }
        | CaptureValueSource::SessionStorage { .. } => {}
    }
}

fn cookie_origins(recipe: &CaptureRecipe) -> BTreeSet<String> {
    recipe
        .outputs
        .iter()
        .flat_map(|output| &output.sources)
        .filter_map(|source| match source {
            CaptureValueSource::CookieHeader { origin } => Some(origin.clone()),
            CaptureValueSource::RequestHeader { .. }
            | CaptureValueSource::LocalStorage { .. }
            | CaptureValueSource::SessionStorage { .. }
            | CaptureValueSource::JsonObject { .. } => None,
        })
        .collect()
}

fn bounded_text(value: Option<&Value>, maximum: usize) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| {
            !text.is_empty()
                && text.len() <= maximum
                && text.trim() == *text
                && !text.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

fn canonical_origin(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?;
    let port = url.port();
    Some(match port {
        Some(port) => format!("https://{host}:{port}"),
        None => format!("https://{host}"),
    })
}

#[cfg(test)]
mod tests {
    use asterism_domain::{AuthMethod, SessionKind};
    use asterism_provider_api::{CaptureCredentialOutput, CaptureJsonField};
    use asterism_secrets::SecretPurpose;

    use super::*;

    #[test]
    fn source_collection_remains_exact_and_origin_bound() {
        let recipe = CaptureRecipe {
            version: 1,
            start_url: "https://provider.example/login".to_owned(),
            navigation_origins: vec!["https://provider.example".to_owned()],
            read_origins: vec!["https://provider.example".to_owned()],
            poll_interval_millis: 500,
            auth_method: AuthMethod::AssistedSession,
            session_kind: SessionKind::Composite,
            readiness: asterism_provider_api::CaptureReadiness::OutputsComplete,
            outputs: vec![CaptureCredentialOutput {
                purpose: SecretPurpose::ProviderCompositeSession,
                required: true,
                sources: vec![CaptureValueSource::JsonObject {
                    fields: vec![CaptureJsonField {
                        name: "token".to_owned(),
                        sources: vec![CaptureScalarSource::RequestHeader {
                            origin: "https://provider.example".to_owned(),
                            name: "Authorization".to_owned(),
                        }],
                    }],
                }],
            }],
        };
        assert_eq!(
            declared_headers(&recipe),
            BTreeSet::from([(
                "https://provider.example".to_owned(),
                "authorization".to_owned()
            )])
        );
        assert!(storage_sources(&recipe, "https://provider.example").is_empty());
        assert!(cookie_origins(&recipe).is_empty());
    }

    #[test]
    fn canonical_origin_rejects_non_https_and_credentials() {
        assert_eq!(
            canonical_origin("https://provider.example/path?q=1"),
            Some("https://provider.example".to_owned())
        );
        assert!(canonical_origin("http://provider.example/path").is_none());
        assert!(canonical_origin("https://user@provider.example/path").is_none());
    }

    #[test]
    fn only_top_level_navigation_rebinds_the_document_loader() {
        let top_level = json!({
            "params": {"frame": {"loaderId": "main-loader"}}
        });
        let child = json!({
            "params": {"frame": {"loaderId": "child-loader", "parentId": "main"}}
        });
        assert_eq!(top_level_loader_id(&top_level), Some("main-loader"));
        assert_eq!(top_level_loader_id(&child), None);
    }

    #[test]
    fn request_readiness_is_exact_and_bound_to_the_current_document_loader() {
        let readiness = CaptureReadiness::RequestObserved {
            origin: "https://provider.example".to_owned(),
            method: "GET".to_owned(),
            path_and_query: "/api/account?action=current".to_owned(),
        };
        let observations = BTreeSet::from([(
            "authenticated-loader".to_owned(),
            "https://provider.example".to_owned(),
            "GET".to_owned(),
            "/api/account?action=current".to_owned(),
        )]);
        let responses = BTreeSet::new();
        assert!(readiness_satisfied(
            &readiness,
            "authenticated-loader",
            &observations,
            &responses,
        ));
        assert!(!readiness_satisfied(
            &readiness,
            "anonymous-loader",
            &observations,
            &responses,
        ));
        assert_eq!(
            canonical_path_and_query("https://provider.example/api/account?action=current"),
            Some("/api/account?action=current".to_owned())
        );
        assert!(canonical_path_and_query("http://provider.example/api/account").is_none());
    }

    #[test]
    fn response_readiness_rejects_login_html_with_the_same_successful_route() {
        let readiness = CaptureReadiness::ResponseObserved {
            origin: "https://provider.example".to_owned(),
            method: "GET".to_owned(),
            path_and_query: "/api/account".to_owned(),
            status: 200,
            mime_type: "application/json".to_owned(),
        };
        let requests = BTreeSet::from([(
            "loader".to_owned(),
            "https://provider.example".to_owned(),
            "GET".to_owned(),
            "/api/account".to_owned(),
        )]);
        let login_html = BTreeSet::from([(
            "loader".to_owned(),
            "https://provider.example".to_owned(),
            "GET".to_owned(),
            "/api/account".to_owned(),
            200,
            "text/html".to_owned(),
        )]);
        assert!(!readiness_satisfied(
            &readiness,
            "loader",
            &requests,
            &login_html,
        ));
        let authenticated_json = BTreeSet::from([(
            "loader".to_owned(),
            "https://provider.example".to_owned(),
            "GET".to_owned(),
            "/api/account".to_owned(),
            200,
            "application/json".to_owned(),
        )]);
        assert!(readiness_satisfied(
            &readiness,
            "loader",
            &requests,
            &authenticated_json,
        ));
    }

    #[tokio::test]
    #[ignore = "requires a locally installed Chromium browser and network access"]
    async fn isolated_browser_attaches_only_to_the_allowlisted_page() {
        let recipe = CaptureRecipe {
            version: 1,
            start_url: "https://example.com/".to_owned(),
            navigation_origins: vec!["https://example.com".to_owned()],
            read_origins: vec!["https://example.com".to_owned()],
            poll_interval_millis: 500,
            auth_method: AuthMethod::AssistedSession,
            session_kind: SessionKind::Cookie,
            readiness: asterism_provider_api::CaptureReadiness::OutputsComplete,
            outputs: vec![CaptureCredentialOutput {
                purpose: SecretPurpose::ProviderCookie,
                required: true,
                sources: vec![CaptureValueSource::CookieHeader {
                    origin: "https://example.com".to_owned(),
                }],
            }],
        };
        let mut browser = ChromiumCapture::launch(recipe, None).await.unwrap();
        let document = browser.current_document().await.unwrap();
        assert_eq!(document.origin, "https://example.com");
        assert!(!document.loader_id.is_empty());
        browser.shutdown().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires a locally installed Chromium browser and network access"]
    async fn isolated_browser_bridge_returns_only_the_frozen_document_binding() {
        let spec = BrowserSessionSpec {
            version: 1,
            start_url: "https://example.com/".to_owned(),
            isolation_key: "browser-bridge-example".to_owned(),
            allowed_origins: vec!["https://example.com".to_owned()],
            headless: true,
        };
        let mut browser = ChromiumBrowserBridge::launch(spec, None).await.unwrap();
        let binding = browser.document_binding().await.unwrap();
        assert_eq!(binding.observed_origin, "https://example.com");
        assert!(!binding.frame_id.is_empty());
        browser.shutdown().await.unwrap();
    }
}
