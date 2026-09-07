//! Authenticated Code-control client used by the review-fix bridge.

use std::{path::Path, time::Duration};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use url::Url;
use uuid::Uuid;

use super::{
    ReviewFixBridgeError,
    fix_protocol::{ReviewFixInteractionResponse, ReviewFixSessionSnapshot},
};
use crate::command::{
    code_control::{apply_control_headers, ensure_loopback_control_url, read_control_token},
    code_control_files::discover_control_stdio_endpoint,
};

/// A stale control sidecar must not make the foreground CLI wait forever.
const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct ReviewFixControlClient {
    client: Client,
    base_url: Url,
    control_token: String,
    controller_token: RwLock<String>,
    client_id: String,
}

impl ReviewFixControlClient {
    pub(super) async fn connect(working_dir: &Path) -> Result<Self, ReviewFixBridgeError> {
        let endpoint = discover_control_stdio_endpoint(working_dir, None, None, None)
            .await
            .map_err(|_| ReviewFixBridgeError::Unavailable)?;
        let base_url =
            Url::parse(&endpoint.base_url).map_err(|_| ReviewFixBridgeError::Unavailable)?;
        ensure_loopback_control_url(&base_url).map_err(|_| ReviewFixBridgeError::Unavailable)?;
        let control_token = read_control_token(&endpoint.token_file)
            .map_err(|_| ReviewFixBridgeError::Unavailable)?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(CONTROL_REQUEST_TIMEOUT)
            .build()
            .map_err(|_| ReviewFixBridgeError::Unavailable)?;
        let client_id = format!("review-fix-{}", Uuid::new_v4());
        let controller_token =
            attach_controller(&client, &base_url, &control_token, &client_id).await?;
        Ok(Self {
            client,
            base_url,
            control_token,
            controller_token: RwLock::new(controller_token),
            client_id,
        })
    }

    pub(super) async fn submit_admission(
        &self,
        admission_message: &'static str,
    ) -> Result<(), ReviewFixBridgeError> {
        let request = CodeMessageRequest {
            text: admission_message,
        };
        self.post_json("/api/code/messages", &request).await
    }

    pub(super) async fn snapshot(&self) -> Result<ReviewFixSessionSnapshot, ReviewFixBridgeError> {
        let controller_token = self.controller_token().await;
        let response = apply_control_headers(
            self.client
                .get(control_endpoint(&self.base_url, "/api/code/session")),
            &self.control_token,
            Some(&controller_token),
        )
        .send()
        .await
        .map_err(|_| ReviewFixBridgeError::Unavailable)?;
        if !response.status().is_success() {
            return Err(ReviewFixBridgeError::Unavailable);
        }
        response
            .json::<ReviewFixSessionSnapshot>()
            .await
            .map_err(|_| ReviewFixBridgeError::ExecutionFailed)
    }

    pub(super) async fn respond_interaction(
        &self,
        interaction_id: &str,
        response_body: &ReviewFixInteractionResponse,
    ) -> Result<(), ReviewFixBridgeError> {
        let endpoint = interaction_endpoint(&self.base_url, interaction_id)?;
        let controller_token = self.controller_token().await;
        let response = apply_control_headers(
            self.client.post(endpoint).json(response_body),
            &self.control_token,
            Some(&controller_token),
        )
        .send()
        .await
        .map_err(|_| ReviewFixBridgeError::Unavailable)?;
        ensure_success(response.status().is_success())
    }

    pub(super) async fn detach(&self) -> Result<(), ReviewFixBridgeError> {
        let request = ControllerDetachRequest {
            client_id: &self.client_id,
        };
        self.post_json("/api/code/controller/detach", &request)
            .await
    }

    /// Renew the automation lease while planning or terminal input is pending.
    /// A replacement token means the old lease expired and a takeover already
    /// invalidated its approvals, so execution must stop fail-closed.
    pub(super) async fn renew_controller(&self) -> Result<(), ReviewFixBridgeError> {
        let renewed = attach_controller(
            &self.client,
            &self.base_url,
            &self.control_token,
            &self.client_id,
        )
        .await
        .map_err(|_| ReviewFixBridgeError::ExecutionFailed)?;
        let mut controller_token = self.controller_token.write().await;
        if *controller_token != renewed {
            *controller_token = renewed;
            return Err(ReviewFixBridgeError::ExecutionFailed);
        }
        Ok(())
    }

    async fn post_json<T: Serialize + ?Sized>(
        &self,
        endpoint: &str,
        body: &T,
    ) -> Result<(), ReviewFixBridgeError> {
        let controller_token = self.controller_token().await;
        let response = apply_control_headers(
            self.client
                .post(control_endpoint(&self.base_url, endpoint))
                .json(body),
            &self.control_token,
            Some(&controller_token),
        )
        .send()
        .await
        .map_err(|_| ReviewFixBridgeError::Unavailable)?;
        ensure_success(response.status().is_success())
    }

    async fn controller_token(&self) -> String {
        self.controller_token.read().await.clone()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ControllerAttachRequest<'a> {
    client_id: &'a str,
    kind: &'static str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControllerAttachResponse {
    controller_token: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ControllerDetachRequest<'a> {
    client_id: &'a str,
}

#[derive(Serialize)]
struct CodeMessageRequest<'a> {
    text: &'a str,
}

async fn attach_controller(
    client: &Client,
    base_url: &Url,
    control_token: &str,
    client_id: &str,
) -> Result<String, ReviewFixBridgeError> {
    let request = ControllerAttachRequest {
        client_id,
        kind: "automation",
    };
    let response = apply_control_headers(
        client
            .post(control_endpoint(base_url, "/api/code/controller/attach"))
            .json(&request),
        control_token,
        None,
    )
    .send()
    .await
    .map_err(|_| ReviewFixBridgeError::Unavailable)?;
    if !response.status().is_success() {
        return Err(ReviewFixBridgeError::Unavailable);
    }
    let payload = response
        .json::<ControllerAttachResponse>()
        .await
        .map_err(|_| ReviewFixBridgeError::Unavailable)?;
    let token = payload.controller_token.trim();
    if token.is_empty() {
        return Err(ReviewFixBridgeError::Unavailable);
    }
    Ok(token.to_string())
}

fn ensure_success(success: bool) -> Result<(), ReviewFixBridgeError> {
    success
        .then_some(())
        .ok_or(ReviewFixBridgeError::Unavailable)
}

fn control_endpoint(base_url: &Url, endpoint: &str) -> Url {
    let mut url = base_url.clone();
    let base_path = url.path().trim_end_matches('/');
    let endpoint = endpoint.trim_start_matches('/');
    url.set_path(&format!("{base_path}/{endpoint}"));
    url.set_query(None);
    url.set_fragment(None);
    url
}

fn interaction_endpoint(base_url: &Url, interaction_id: &str) -> Result<Url, ReviewFixBridgeError> {
    let mut url = control_endpoint(base_url, "/api/code/interactions");
    url.path_segments_mut()
        .map_err(|_| ReviewFixBridgeError::Unavailable)?
        .push(interaction_id);
    Ok(url)
}

#[cfg(test)]
#[path = "fix_control_tests.rs"]
mod tests;
