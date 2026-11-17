use crate::secrets::KeychainSecrets;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use gm_core::config::ProviderPreset;
use gm_core::error::{GmError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthTokenBundle {
    pub provider_id: String,
    pub account_id: String,
    pub account_label: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<String>,
    pub token_endpoint: String,
    pub client_id: String,
}

impl OAuthTokenBundle {
    pub fn is_expiring_soon(&self) -> bool {
        let Some(ref raw) = self.expires_at else {
            return false;
        };
        let Ok(dt) = DateTime::parse_from_rfc3339(raw) else {
            return false;
        };
        dt.with_timezone(&Utc) <= Utc::now() + Duration::seconds(60)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkceAuthRequest {
    pub authorize_url: String,
    pub state: String,
    pub code_verifier: String,
    pub redirect_uri: String,
}

pub fn keychain_account_for_oauth(preset: ProviderPreset, account_id: &str) -> Result<String> {
    let clean: String = account_id
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(64)
        .collect();
    if clean.is_empty() {
        return Err(GmError::Config("OAuth account_id cannot be empty".into()));
    }
    match preset {
        ProviderPreset::ChatGpt => Ok(format!("gm_chatgpt_oauth_{clean}")),
        ProviderPreset::NousPortal => Ok(format!("gm_nous_portal_{clean}")),
        other => Err(GmError::Config(format!(
            "Provider preset {other:?} does not use isolated OAuth grant storage"
        ))),
    }
}

pub fn generate_pkce_challenge() -> (String, String) {
    let raw = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_bytes());
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

fn percent_encode_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

/// Official open-source Sign in with ChatGPT & Nous Portal OAuth PKCE authorization URL builder.
pub fn build_oauth_authorize_url(
    preset: ProviderPreset,
    client_id: &str,
    redirect_uri: &str,
    authorize_endpoint: Option<&str>,
) -> Result<PkceAuthRequest> {
    let (default_auth, scope) = match preset {
        ProviderPreset::ChatGpt => (
            "https://auth.openai.com/oauth/authorize",
            "openid profile email offline_access",
        ),
        ProviderPreset::NousPortal => (
            "https://portal.nousresearch.com/oauth/authorize",
            "openid profile inference offline_access",
        ),
        _ => {
            return Err(GmError::Config(
                "OAuth PKCE is supported for ChatGPT and Nous Portal".into(),
            ));
        }
    };
    let base = authorize_endpoint.unwrap_or(default_auth);
    let state = uuid::Uuid::new_v4().simple().to_string();
    let (verifier, challenge) = generate_pkce_challenge();
    let authorize_url = format!(
        "{base}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        percent_encode_component(client_id),
        percent_encode_component(redirect_uri),
        percent_encode_component(scope),
        percent_encode_component(&state),
        percent_encode_component(&challenge),
    );
    Ok(PkceAuthRequest {
        authorize_url,
        state,
        code_verifier: verifier,
        redirect_uri: redirect_uri.to_string(),
    })
}

pub fn store_oauth_bundle(
    secrets: &KeychainSecrets,
    preset: ProviderPreset,
    bundle: &OAuthTokenBundle,
) -> Result<String> {
    let account = keychain_account_for_oauth(preset, &bundle.account_id)?;
    let json = serde_json::to_string(bundle)?;
    secrets.set_secret(&account, &json)?;
    Ok(format!("secret://keychain/{account}"))
}

pub fn load_oauth_bundle(secrets: &KeychainSecrets, secret_ref: &str) -> Result<OAuthTokenBundle> {
    let account = secret_ref
        .strip_prefix("secret://keychain/")
        .ok_or_else(|| GmError::Config(format!("Invalid OAuth secret_ref '{secret_ref}'")))?;
    let raw = secrets.get_secret(account)?;
    serde_json::from_str::<OAuthTokenBundle>(&raw)
        .map_err(|e| GmError::Config(format!("Corrupt OAuth token bundle in keychain: {e}")))
}

pub fn disconnect_oauth_account(secrets: &KeychainSecrets, secret_ref: &str) -> Result<()> {
    let account = secret_ref
        .strip_prefix("secret://keychain/")
        .ok_or_else(|| GmError::Config(format!("Invalid OAuth secret_ref '{secret_ref}'")))?;
    secrets.delete_secret(account)
}

#[allow(clippy::too_many_arguments)]
pub async fn exchange_oauth_code(
    client: &reqwest::Client,
    preset: ProviderPreset,
    token_endpoint: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
    account_id: &str,
    account_label: &str,
) -> Result<OAuthTokenBundle> {
    let resp = client
        .post(token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .map_err(|e| GmError::Provider(format!("OAuth code exchange failed: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(GmError::Provider(format!(
            "OAuth token endpoint returned {status}: {text}"
        )));
    }
    let val: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| GmError::Provider(format!("Invalid OAuth token JSON: {e}")))?;
    let access_token = val["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GmError::Provider("OAuth response missing access_token".into()))?
        .to_string();
    let refresh_token = val["refresh_token"].as_str().map(str::to_string);
    let expires_at = val["expires_in"]
        .as_i64()
        .map(|secs| (Utc::now() + Duration::seconds(secs)).to_rfc3339());
    let provider_id = match preset {
        ProviderPreset::ChatGpt => "chatgpt",
        ProviderPreset::NousPortal => "nous_portal",
        _ => "oauth",
    }
    .to_string();
    Ok(OAuthTokenBundle {
        provider_id,
        account_id: account_id.to_string(),
        account_label: account_label.to_string(),
        access_token,
        refresh_token,
        expires_at,
        token_endpoint: token_endpoint.to_string(),
        client_id: client_id.to_string(),
    })
}

pub async fn refresh_oauth_bundle(
    client: &reqwest::Client,
    secrets: &KeychainSecrets,
    preset: ProviderPreset,
    secret_ref: &str,
) -> Result<OAuthTokenBundle> {
    let mut bundle = load_oauth_bundle(secrets, secret_ref)?;
    let refresh_tok = bundle.refresh_token.clone().ok_or_else(|| {
        GmError::Config("OAuth account has no refresh_token; please sign in again".into())
    })?;
    let resp = client
        .post(&bundle.token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", bundle.client_id.as_str()),
            ("refresh_token", refresh_tok.as_str()),
        ])
        .send()
        .await
        .map_err(|e| GmError::Provider(format!("OAuth refresh failed: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(GmError::Provider(format!(
            "OAuth refresh returned {status}: {text}"
        )));
    }
    let val: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| GmError::Provider(format!("Invalid OAuth refresh JSON: {e}")))?;
    bundle.access_token = val["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GmError::Provider("OAuth refresh missing access_token".into()))?
        .to_string();
    if let Some(new_rt) = val["refresh_token"].as_str() {
        bundle.refresh_token = Some(new_rt.to_string());
    }
    if let Some(secs) = val["expires_in"].as_i64() {
        bundle.expires_at = Some((Utc::now() + Duration::seconds(secs)).to_rfc3339());
    }
    let _ = store_oauth_bundle(secrets, preset, &bundle)?;
    Ok(bundle)
}
