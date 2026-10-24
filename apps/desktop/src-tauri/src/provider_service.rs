//! Provider profiles and their tests (S-020, S-023). Same rules as hosts: the
//! key goes to the OS store before the profile is committed, and nothing returns
//! the key to the webview.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use selai_domain::{
    BaseUrl, ErrorCode, MaxOutputTokens, ModelId, Page, PageRequest, ProviderId, ProviderName,
    ProviderProtocol, ProviderSettings, SelaiError, TimeoutSecs,
};
use selai_llm::{probe_tools, test_connectivity, TEMPLATES};
use selai_secrets::{SecretKey, SecretValue};
use selai_storage::ProviderRecord;
use tokio_util::sync::CancellationToken;

use crate::{
    dto::{
        CommandError, ModelsView, ProviderInput, ProviderModelsInput, ProviderTestView,
        ProviderView, TemplateView, ToolCallingView,
    },
    service::Services,
};

const MODELS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

fn invalid(err: impl SelaiError + std::fmt::Display) -> CommandError {
    CommandError::new(err.code(), err.to_string())
}

fn parse_provider_id(raw: &str) -> Result<ProviderId, CommandError> {
    raw.parse()
        .map_err(|_| CommandError::new(ErrorCode::InvalidInput, "invalid provider id"))
}

fn settings_from_input(input: &ProviderInput) -> Result<ProviderSettings, CommandError> {
    let protocol = ProviderProtocol::parse(&input.protocol)
        .ok_or_else(|| CommandError::new(ErrorCode::InvalidInput, "unknown provider protocol"))?;
    let allow_insecure_http = input.allow_insecure_http.unwrap_or(false);
    Ok(ProviderSettings {
        name: ProviderName::parse(&input.name).map_err(invalid)?,
        protocol,
        base_url: BaseUrl::parse_with(&input.base_url, allow_insecure_http).map_err(invalid)?,
        model_id: ModelId::parse(&input.model_id).map_err(invalid)?,
        timeout: TimeoutSecs::new(input.timeout_seconds).map_err(invalid)?,
        max_output_tokens: MaxOutputTokens::new(input.max_output_tokens).map_err(invalid)?,
        allow_insecure_http,
    })
}

/// Rebuilds settings from a stored row, through the same validation as input.
pub(crate) fn settings_from_record(rec: &ProviderRecord) -> Result<ProviderSettings, CommandError> {
    Ok(ProviderSettings {
        name: ProviderName::parse(&rec.name).map_err(invalid)?,
        protocol: rec.protocol,
        base_url: BaseUrl::parse_with(&rec.base_url, rec.allow_insecure_http).map_err(invalid)?,
        model_id: ModelId::parse(&rec.model_id).map_err(invalid)?,
        timeout: TimeoutSecs::new(rec.timeout_seconds).map_err(invalid)?,
        max_output_tokens: MaxOutputTokens::new(rec.max_output_tokens).map_err(invalid)?,
        allow_insecure_http: rec.allow_insecure_http,
    })
}

pub(crate) fn tool_calling_view(rec: &ProviderRecord) -> ToolCallingView {
    let current = format!(
        "{}|{}|{}",
        rec.protocol.as_str(),
        rec.base_url,
        rec.model_id
    );
    let same_identity = rec.tool_probe_identity.as_deref() == Some(current.as_str());
    let status = match (
        same_identity,
        rec.tool_probe_passed,
        rec.last_tool_probe_at_ms,
    ) {
        (true, true, _) => "verified",
        (true, false, Some(_)) => "failed",
        _ => "not_verified",
    };
    ToolCallingView {
        status,
        last_checked_at_ms: rec.last_tool_probe_at_ms,
    }
}

fn provider_view(rec: &ProviderRecord) -> ProviderView {
    ProviderView {
        id: rec.id.to_string(),
        name: rec.name.clone(),
        protocol: rec.protocol.as_str(),
        base_url: rec.base_url.clone(),
        model_id: rec.model_id.clone(),
        timeout_seconds: rec.timeout_seconds,
        max_output_tokens: rec.max_output_tokens,
        key_configured: rec.key_configured,
        is_loopback: BaseUrl::parse_with(&rec.base_url, rec.allow_insecure_http)
            .is_ok_and(|b| b.is_loopback()),
        allow_insecure_http: rec.allow_insecure_http,
        updated_at_ms: rec.updated_at_ms,
        tool_calling: tool_calling_view(rec),
    }
}

fn llm_failure(
    kind: &'static str,
    err: &selai_llm::LlmError,
    started: Instant,
) -> ProviderTestView {
    ProviderTestView {
        kind,
        ok: false,
        verified: false,
        code: Some(err.code),
        message: err.message.clone(),
        latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        reply_preview: None,
    }
}

impl Services {
    pub fn provider_templates() -> Vec<TemplateView> {
        TEMPLATES
            .iter()
            .map(|t| TemplateView {
                id: t.id,
                display_name: t.display_name,
                protocol: t.protocol.as_str(),
                default_base_url: t.default_base_url,
                note: t.note,
            })
            .collect()
    }

    pub fn provider_list(
        &self,
        cursor: Option<String>,
    ) -> Result<Page<ProviderView>, CommandError> {
        let db = self.lock_db_pub()?;
        let page = db
            .list_providers(&PageRequest { cursor, limit: 50 })
            .map_err(|e| CommandError::new(e.code(), e.to_string()))?;
        Ok(Page {
            items: page.items.iter().map(provider_view).collect(),
            next_cursor: page.next_cursor,
        })
    }

    /// Creates or updates a provider profile. A new key is written to the OS store
    /// before the row is committed. A failed commit removes a key written for a
    /// new profile.
    pub fn provider_save(&self, input: ProviderInput) -> Result<ProviderView, CommandError> {
        let settings = settings_from_input(&input)?;
        let now = now_ms();
        let previous = match input.id.as_deref() {
            Some(raw) => {
                let id = parse_provider_id(raw)?;
                Some(
                    self.lock_db_pub()?
                        .get_provider(id)
                        .map_err(|e| CommandError::new(e.code(), e.to_string()))?
                        .ok_or_else(|| {
                            CommandError::new(ErrorCode::NotFound, "provider not found")
                        })?,
                )
            }
            None => None,
        };
        let id = previous.as_ref().map_or_else(ProviderId::new, |p| p.id);
        let key = SecretKey::provider_api_key(id);
        let mut configured = previous.as_ref().is_some_and(|p| p.key_configured);
        let mut wrote_key = false;

        if input.clear_api_key == Some(true) {
            self.secrets
                .delete(&key)
                .map_err(|e| CommandError::new(e.code(), e.to_string()))?;
            configured = false;
        } else if let Some(k) = input.api_key.as_deref().filter(|k| !k.is_empty()) {
            self.secrets
                .put(&key, &SecretValue::new(k))
                .map_err(|e| CommandError::new(e.code(), e.to_string()))?;
            configured = true;
            wrote_key = true;
        }

        let saved = {
            let mut db = self.lock_db_pub()?;
            let result = match previous {
                Some(ref p) => db.update_provider(
                    id,
                    &settings,
                    input.expected_updated_at_ms.unwrap_or(p.updated_at_ms),
                    now,
                ),
                None => db.create_provider(id, &settings, now),
            };
            match result {
                Ok(rec) => rec,
                Err(e) => {
                    if previous.is_none() && wrote_key {
                        let _ = self.secrets.delete(&key);
                    }
                    return Err(CommandError::new(e.code(), e.to_string()));
                }
            }
        };
        self.lock_db_pub()?
            .set_provider_key_configured(id, configured, now)
            .map_err(|e| CommandError::new(e.code(), e.to_string()))?;
        let rec = self
            .lock_db_pub()?
            .get_provider(saved.id)
            .map_err(|e| CommandError::new(e.code(), e.to_string()))?
            .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "provider not found"))?;
        Ok(provider_view(&rec))
    }

    pub fn provider_delete(&self, id: &str) -> Result<(), CommandError> {
        let id = parse_provider_id(id)?;
        self.lock_db_pub()?
            .delete_provider(id)
            .map_err(|e| CommandError::new(e.code(), e.to_string()))?;
        self.secrets
            .delete(&SecretKey::provider_api_key(id))
            .map_err(|e| CommandError::new(e.code(), e.to_string()))?;
        Ok(())
    }

    /// Runs `connectivity` or `tool_calling`. Provider failures come back as a
    /// result with `ok = false`. Only a broken local store is an error. A tool
    /// probe that runs to completion is recorded for the current endpoint and model.
    pub async fn provider_test(
        &self,
        id: &str,
        kind: &str,
    ) -> Result<ProviderTestView, CommandError> {
        let id = parse_provider_id(id)?;
        let rec = self
            .lock_db_pub()?
            .get_provider(id)
            .map_err(|e| CommandError::new(e.code(), e.to_string()))?
            .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "provider not found"))?;
        let settings = settings_from_record(&rec)?;
        let key = self
            .secrets
            .get(&SecretKey::provider_api_key(id))
            .map_err(|e| CommandError::new(e.code(), e.to_string()))?;
        let cancel = CancellationToken::new();
        let started = Instant::now();

        match kind {
            "connectivity" => Ok(
                match test_connectivity(&self.llm, &settings, key.as_ref(), &cancel).await {
                    Ok(t) => ProviderTestView {
                        kind: "connectivity",
                        ok: true,
                        verified: false,
                        code: None,
                        message: "the provider replied".to_owned(),
                        latency_ms: t.latency_ms,
                        reply_preview: Some(t.reply_preview),
                    },
                    Err(e) => llm_failure("connectivity", &e, started),
                },
            ),
            "tool_calling" => {
                match probe_tools(&self.llm, &settings, key.as_ref(), &cancel).await {
                    Ok(p) => {
                        self.lock_db_pub()?
                            .record_tool_probe(id, p.verified, &settings.probe_identity(), now_ms())
                            .map_err(|e| CommandError::new(e.code(), e.to_string()))?;
                        Ok(ProviderTestView {
                            kind: "tool_calling",
                            ok: p.verified,
                            verified: p.verified,
                            code: p.code,
                            message: p.detail,
                            latency_ms: p.latency_ms,
                            reply_preview: None,
                        })
                    }
                    Err(e) => Ok(llm_failure("tool_calling", &e, started)),
                }
            }
            _ => Err(CommandError::new(
                ErrorCode::InvalidInput,
                "test kind must be connectivity or tool_calling",
            )),
        }
    }

    /// Lists an endpoint's models without verifying access to inference. Works
    /// on an unsaved draft. The typed key is used once and dropped;
    /// with no key typed, the key saved for `id` is used.
    pub async fn provider_models(
        &self,
        input: ProviderModelsInput,
    ) -> Result<ModelsView, CommandError> {
        let protocol = ProviderProtocol::parse(&input.protocol).ok_or_else(|| {
            CommandError::new(ErrorCode::InvalidInput, "unknown provider protocol")
        })?;
        let base_url =
            BaseUrl::parse_with(&input.base_url, input.allow_insecure_http.unwrap_or(false))
                .map_err(invalid)?;
        let key = match input.api_key.as_deref().filter(|k| !k.is_empty()) {
            Some(k) => Some(SecretValue::new(k)),
            None => match input.id.as_deref() {
                Some(id) => self
                    .secrets
                    .get(&SecretKey::provider_api_key(parse_provider_id(id)?))
                    .map_err(|e| CommandError::new(e.code(), e.to_string()))?,
                None => None,
            },
        };
        let started = std::time::Instant::now();
        let models = self
            .llm
            .list_models(protocol, &base_url, key.as_ref(), MODELS_TIMEOUT)
            .await
            .map_err(|e| CommandError::new(e.code, e.message))?;
        Ok(ModelsView {
            models,
            latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        })
    }
}
