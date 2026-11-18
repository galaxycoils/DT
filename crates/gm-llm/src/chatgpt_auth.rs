//! OpenAI's public-client Sign in with ChatGPT registration and identity validation.
use crate::{OAuthTokenBundle, PkceAuthRequest, generate_pkce_challenge};
use gm_core::{GmError, Result};
use serde::{Deserialize, Serialize};

const ISSUER: &str = "https://auth.openai.com";
pub const CHATGPT_TOKEN_ENDPOINT: &str = "https://auth.openai.com/api/accounts/oauth/token";

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthIdentity {
    pub subject: String,
    pub email: Option<String>,
    pub client_id: String,
    pub id_token: String,
    pub scopes: Vec<String>,
    pub host_id: String,
}

impl std::fmt::Debug for OAuthIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthIdentity")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

pub struct ChatGptSignIn {
    pub request: PkceAuthRequest,
    pub nonce: String,
    pub client_id: String,
    pub host_id: String,
    pub expected_subject: Option<String>,
}

pub fn begin_chatgpt_sign_in(
    host_id: &str,
    saved: Option<&OAuthIdentity>,
    redirect_uri: &str,
) -> Result<ChatGptSignIn> {
    let state = uuid::Uuid::new_v4().simple().to_string();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let (verifier, challenge) = generate_pkce_challenge();
    let client_id = saved
        .map(|identity| identity.client_id.as_str())
        .unwrap_or("dynamic_agent_client");
    let mut url = reqwest::Url::parse("https://auth.openai.com/api/accounts/authorize")
        .map_err(|_| GmError::Config("Invalid ChatGPT authorization endpoint".into()))?;
    {
        let mut query = url.query_pairs_mut();
        query.extend_pairs([
            ("client_id", client_id),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri),
            (
                "scope",
                "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
            ),
            ("resource", "https://api.openai.com/v1"),
            ("ext_agent_host_id", host_id),
            ("state", &state),
            ("nonce", &nonce),
            ("code_challenge_method", "S256"),
            ("code_challenge", &challenge),
        ]);
        if let Some(identity) = saved {
            query.append_pair("id_token_hint", &identity.id_token);
        } else {
            query.append_pair("agent_name_hint", "Grok Muse");
        }
    }
    Ok(ChatGptSignIn {
        request: PkceAuthRequest {
            authorize_url: url.into(),
            state,
            code_verifier: verifier,
            redirect_uri: redirect_uri.into(),
        },
        nonce,
        client_id: client_id.into(),
        host_id: host_id.into(),
        expected_subject: saved.map(|identity| identity.subject.clone()),
    })
}

#[derive(Deserialize)]
struct IdentityClaims {
    sub: String,
    email: Option<String>,
    nonce: Option<String>,
    aud: serde_json::Value,
    azp: Option<String>,
}

pub async fn validate_chatgpt_identity(
    client: &reqwest::Client,
    id_token: &str,
    client_id: &str,
    nonce: Option<&str>,
    expected_subject: Option<&str>,
) -> Result<(String, Option<String>)> {
    let header = jsonwebtoken::decode_header(id_token)
        .map_err(|_| GmError::Provider("ChatGPT returned an invalid identity token".into()))?;
    if header.alg != jsonwebtoken::Algorithm::RS256 {
        return Err(GmError::Provider(
            "ChatGPT identity uses an unsupported signing algorithm".into(),
        ));
    }
    let response = client
        .get(format!("{ISSUER}/.well-known/jwks.json"))
        .send()
        .await
        .map_err(|_| GmError::Provider("Unable to verify the ChatGPT signing keys".into()))?
        .error_for_status()
        .map_err(|_| GmError::Provider("ChatGPT signing keys are unavailable".into()))?;
    let keys: jsonwebtoken::jwk::JwkSet = response
        .json()
        .await
        .map_err(|_| GmError::Provider("Invalid ChatGPT signing keys".into()))?;
    validate_identity_with_keys(id_token, client_id, nonce, expected_subject, &keys)
}

fn validate_identity_with_keys(
    id_token: &str,
    client_id: &str,
    nonce: Option<&str>,
    expected_subject: Option<&str>,
    keys: &jsonwebtoken::jwk::JwkSet,
) -> Result<(String, Option<String>)> {
    let header = jsonwebtoken::decode_header(id_token)
        .map_err(|_| GmError::Provider("Invalid ChatGPT identity header".into()))?;
    if header.alg != jsonwebtoken::Algorithm::RS256 {
        return Err(GmError::Provider(
            "Unsupported ChatGPT identity signing algorithm".into(),
        ));
    }
    let key = header
        .kid
        .as_deref()
        .and_then(|id| keys.find(id))
        .ok_or_else(|| GmError::Provider("ChatGPT identity signing key was not found".into()))?;
    let decoding_key = jsonwebtoken::DecodingKey::from_jwk(key)
        .map_err(|_| GmError::Provider("Invalid ChatGPT identity signing key".into()))?;
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[client_id]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let claims = jsonwebtoken::decode::<IdentityClaims>(id_token, &decoding_key, &validation)
        .map_err(|_| {
            GmError::Provider("ChatGPT identity signature, audience, or expiry is invalid".into())
        })?
        .claims;
    if claims
        .azp
        .as_deref()
        .is_some_and(|party| party != client_id)
        || claims.aud.as_array().is_some_and(|audiences| {
            audiences.len() > 1 && claims.azp.as_deref() != Some(client_id)
        })
        || claims.sub.is_empty()
        || nonce.is_some_and(|expected| claims.nonce.as_deref() != Some(expected))
        || expected_subject.is_some_and(|expected| expected != claims.sub)
    {
        return Err(GmError::Provider(
            "ChatGPT identity does not match this sign-in attempt".into(),
        ));
    }
    Ok((claims.sub, claims.email))
}

pub async fn complete_chatgpt_sign_in(
    client: &reqwest::Client,
    attempt: &ChatGptSignIn,
    issued_client_id: &str,
    code: &str,
) -> Result<OAuthTokenBundle> {
    if issued_client_id.is_empty()
        || issued_client_id == "dynamic_agent_client"
        || (attempt.client_id != "dynamic_agent_client" && issued_client_id != attempt.client_id)
    {
        return Err(GmError::Provider(
            "ChatGPT registration returned a mismatched client ID".into(),
        ));
    }
    let response = client
        .post(CHATGPT_TOKEN_ENDPOINT)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", issued_client_id),
            ("code", code),
            ("code_verifier", &attempt.request.code_verifier),
            ("redirect_uri", &attempt.request.redirect_uri),
            ("resource", "https://api.openai.com/v1"),
        ])
        .send()
        .await
        .map_err(|_| GmError::Provider("ChatGPT token exchange could not reach OpenAI".into()))?;
    if !response.status().is_success() {
        return Err(GmError::Provider(format!(
            "ChatGPT sign-in returned HTTP {}. Start a fresh sign-in.",
            response.status()
        )));
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|_| GmError::Provider("Invalid ChatGPT token response".into()))?;
    let access_token = value["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GmError::Provider("ChatGPT did not return an access token".into()))?;
    let id_token = value["id_token"]
        .as_str()
        .ok_or_else(|| GmError::Provider("ChatGPT did not return an identity token".into()))?;
    let (subject, email) = validate_chatgpt_identity(
        client,
        id_token,
        issued_client_id,
        Some(&attempt.nonce),
        attempt.expected_subject.as_deref(),
    )
    .await?;
    let scopes: Vec<String> = value["scope"]
        .as_str()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    // Include the registration identity so accounts/workspaces cannot overwrite one another.
    let account_id =
        gm_core::hash::compute_sha256(format!("{issued_client_id}:{subject}").as_bytes());
    Ok(OAuthTokenBundle {
        provider_id: "chatgpt".into(),
        account_id,
        account_label: email.clone().unwrap_or_else(|| "ChatGPT account".into()),
        access_token: access_token.into(),
        refresh_token: value["refresh_token"].as_str().map(str::to_owned),
        expires_at: value["expires_in"]
            .as_i64()
            .map(|seconds| (chrono::Utc::now() + chrono::Duration::seconds(seconds)).to_rfc3339()),
        token_endpoint: CHATGPT_TOKEN_ENDPOINT.into(),
        client_id: issued_client_id.into(),
        inference_base_url: None,
        identity: Some(OAuthIdentity {
            subject,
            email,
            client_id: issued_client_id.into(),
            id_token: id_token.into(),
            scopes,
            host_id: attempt.host_id.clone(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registration_owns_its_identity_and_keeps_pkce_private() {
        let first =
            begin_chatgpt_sign_in("host-1", None, "http://127.0.0.1:1234/auth/callback").unwrap();
        let second =
            begin_chatgpt_sign_in("host-1", None, "http://127.0.0.1:1234/auth/callback").unwrap();
        let url = reqwest::Url::parse(&first.request.authorize_url).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["client_id"], "dynamic_agent_client");
        assert_eq!(query["agent_name_hint"], "Grok Muse");
        assert_eq!(query["ext_agent_host_id"], "host-1");
        assert!(query["scope"].contains("chatgpt.tokens.use.direct"));
        assert!(
            !first
                .request
                .authorize_url
                .contains(&first.request.code_verifier)
        );
        assert_ne!(first.request.state, second.request.state);
        assert_ne!(first.nonce, second.nonce);
    }
}

#[cfg(test)]
mod signed_identity_tests {
    use super::*;
    use serde_json::json;
    fn token(mut claims: serde_json::Value) -> String {
        if claims.get("exp").is_none() {
            claims["exp"] = json!(chrono::Utc::now().timestamp() + 600);
        }
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = Some("fixture".into());
        jsonwebtoken::encode(
            &header,
            &claims,
            &jsonwebtoken::EncodingKey::from_rsa_der(include_bytes!(
                "../tests/fixtures/identity-test-key.der"
            )),
        )
        .unwrap()
    }
    #[test]
    fn signature_issuer_audience_expiry_nonce_and_account_are_checked() {
        let keys = serde_json::from_str(include_str!("../tests/fixtures/identity-test-jwks.json"))
            .unwrap();
        let claims = json!({"iss":ISSUER,"aud":"own-client","sub":"account-a","nonce":"attempt","email":"person@example.test"});
        let signed = token(claims.clone());
        assert!(
            validate_identity_with_keys(
                &signed,
                "own-client",
                Some("attempt"),
                Some("account-a"),
                &keys
            )
            .is_ok()
        );
        for (field, value) in [
            ("iss", json!("https://other.invalid")),
            ("aud", json!("another-client")),
            ("exp", json!(1)),
            ("nonce", json!("other-attempt")),
            ("sub", json!("account-b")),
        ] {
            let mut altered = claims.clone();
            altered[field] = value;
            assert!(
                validate_identity_with_keys(
                    &token(altered),
                    "own-client",
                    Some("attempt"),
                    Some("account-a"),
                    &keys
                )
                .is_err(),
                "accepted {field}"
            );
        }
        let mut multiple = claims.clone();
        multiple["aud"] = json!(["own-client", "other-client"]);
        assert!(
            validate_identity_with_keys(
                &token(multiple.clone()),
                "own-client",
                Some("attempt"),
                None,
                &keys
            )
            .is_err()
        );
        multiple["azp"] = json!("own-client");
        assert!(
            validate_identity_with_keys(
                &token(multiple),
                "own-client",
                Some("attempt"),
                None,
                &keys
            )
            .is_ok()
        );
        let mut tampered = signed.into_bytes();
        let index = tampered.iter().position(|b| *b == b'.').unwrap() + 3;
        tampered[index] = if tampered[index] == b'A' { b'B' } else { b'A' };
        assert!(
            validate_identity_with_keys(
                std::str::from_utf8(&tampered).unwrap(),
                "own-client",
                Some("attempt"),
                None,
                &keys
            )
            .is_err()
        );
    }
}
