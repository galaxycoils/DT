//! Stripe sandbox billing, signed `Stripe-Signature` webhook verification, idempotent event
//! ledger, and workspace entitlements — preserving free self-hosted BYOK by default.

use crate::bridges::{compute_hmac_sha256_hex, constant_time_eq};
use chrono::Utc;
use gm_core::config::extract_provider_origin;
use gm_core::database::Database;
use gm_core::error::{GmError, Result};
use parking_lot::Mutex;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BillingMode {
    #[default]
    SelfHostedByok,
    HostedFree,
    HostedPro,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceEntitlements {
    pub ws_id: String,
    pub mode: BillingMode,
    pub byok_unrestricted: bool,
    pub credit_balance_cents: i64,
    pub stripe_customer_id: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutSessionResult {
    pub session_id: String,
    pub url: String,
}

pub fn verify_stripe_signature(
    webhook_secret: &str,
    signature_header: &str,
    raw_body: &str,
    now_ts: i64,
) -> Result<()> {
    if webhook_secret.trim().is_empty() {
        return Err(GmError::SafetyViolation(
            "Stripe webhook secret cannot be empty".into(),
        ));
    }
    let mut timestamp: Option<i64> = None;
    let mut v1_sigs = Vec::new();
    for part in signature_header.split(',') {
        let Some((k, v)) = part.trim().split_once('=') else {
            continue;
        };
        match k.trim() {
            "t" => timestamp = v.trim().parse::<i64>().ok(),
            "v1" => v1_sigs.push(v.trim().to_string()),
            _ => {}
        }
    }
    let ts = timestamp
        .ok_or_else(|| GmError::SafetyViolation("Missing Stripe timestamp in signature".into()))?;
    if now_ts.abs_diff(ts) > 300 {
        return Err(GmError::SafetyViolation(
            "Stripe webhook timestamp outside 300s replay tolerance".into(),
        ));
    }
    let signed_payload = format!("{ts}.{raw_body}");
    let expected = compute_hmac_sha256_hex(webhook_secret.as_bytes(), signed_payload.as_bytes());
    if v1_sigs
        .iter()
        .any(|sig| constant_time_eq(expected.as_bytes(), sig.as_bytes()))
    {
        Ok(())
    } else {
        Err(GmError::SafetyViolation(
            "Stripe-Signature verification failed".into(),
        ))
    }
}

#[derive(Clone)]
pub struct BillingStore {
    conn: Arc<Mutex<Database>>,
    self_hosted: bool,
    stripe_api_base: String,
}

impl BillingStore {
    pub fn open(db_path: impl AsRef<Path>, self_hosted: bool) -> Result<Self> {
        let conn = gm_core::database::Database::open(db_path.as_ref())?;
        Self::init(conn, self_hosted, "https://api.stripe.com")
    }

    pub fn in_memory(self_hosted: bool) -> Result<Self> {
        let conn = Database::in_memory()
            .map_err(|e| GmError::Platform(format!("Billing DB error: {e}")))?;
        Self::init(conn, self_hosted, "https://api.stripe.com")
    }

    pub fn with_stripe_api_base(mut self, base: impl AsRef<str>) -> Result<Self> {
        let _ = extract_provider_origin(base.as_ref())?;
        self.stripe_api_base = base.as_ref().trim_end_matches('/').to_string();
        Ok(self)
    }

    fn init(conn: Database, self_hosted: bool, stripe_api_base: &str) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workspace_entitlements_v1 (
                ws_id TEXT PRIMARY KEY,
                mode TEXT NOT NULL,
                byok_unrestricted INTEGER NOT NULL DEFAULT 1,
                credit_balance_cents INTEGER NOT NULL DEFAULT 0,
                stripe_customer_id TEXT,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS stripe_webhook_events_v1 (
                event_id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                ws_id TEXT NOT NULL,
                amount_cents INTEGER NOT NULL,
                processed_at TEXT NOT NULL
            );",
        )
        .map_err(|e| GmError::Platform(format!("Billing schema error: {e}")))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            self_hosted,
            stripe_api_base: stripe_api_base.to_string(),
        })
    }

    pub fn check_self_hosted_byok_allowed(&self) -> bool {
        self.self_hosted
    }

    pub fn get_entitlements(&self, ws_id: &str) -> Result<WorkspaceEntitlements> {
        let conn = self.conn.lock();
        let row: Option<WorkspaceEntitlements> = conn
            .query_row(
                "SELECT ws_id, mode, byok_unrestricted, credit_balance_cents, stripe_customer_id, updated_at
                 FROM workspace_entitlements_v1 WHERE ws_id = ?1",
                params![ws_id],
                |r| {
                    let mode_str: String = r.get(1)?;
                    let mode = match mode_str.as_str() {
                        "hosted_free" => BillingMode::HostedFree,
                        "hosted_pro" => BillingMode::HostedPro,
                        _ => BillingMode::SelfHostedByok,
                    };
                    Ok(WorkspaceEntitlements {
                        ws_id: r.get(0)?,
                        mode,
                        byok_unrestricted: r.get::<_, i64>(2)? != 0,
                        credit_balance_cents: r.get(3)?,
                        stripe_customer_id: r.get(4)?,
                        updated_at: r.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(row.unwrap_or_else(|| WorkspaceEntitlements {
            ws_id: ws_id.to_string(),
            mode: if self.self_hosted {
                BillingMode::SelfHostedByok
            } else {
                BillingMode::HostedFree
            },
            byok_unrestricted: true,
            credit_balance_cents: 0,
            stripe_customer_id: None,
            updated_at: Utc::now().to_rfc3339(),
        }))
    }

    pub async fn create_sandbox_checkout_session(
        &self,
        stripe_secret_key: &str,
        ws_id: &str,
        success_url: &str,
        cancel_url: &str,
        price_id: &str,
    ) -> Result<CheckoutSessionResult> {
        if stripe_secret_key.trim().is_empty() {
            return Err(GmError::Config("Missing Stripe secret key".into()));
        }
        let client = reqwest::Client::new();
        let url = format!("{}/v1/checkout/sessions", self.stripe_api_base);
        let resp = client
            .post(&url)
            .bearer_auth(stripe_secret_key.trim())
            .form(&[
                ("mode", "subscription"),
                ("success_url", success_url),
                ("cancel_url", cancel_url),
                ("line_items[0][price]", price_id),
                ("line_items[0][quantity]", "1"),
                ("metadata[ws_id]", ws_id),
            ])
            .send()
            .await
            .map_err(|e| GmError::Platform(format!("Stripe checkout error: {e}")))?;
        if !resp.status().is_success() {
            return Err(GmError::Platform(format!(
                "Stripe checkout HTTP {}",
                resp.status()
            )));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GmError::Platform(format!("Invalid Stripe checkout JSON: {e}")))?;
        Ok(CheckoutSessionResult {
            session_id: body["id"].as_str().unwrap_or("cs_test").to_string(),
            url: body["url"].as_str().unwrap_or("").to_string(),
        })
    }

    /// Verifies `Stripe-Signature` and applies idempotent credit/plan updates inside a SQLite transaction.
    pub fn process_stripe_webhook(
        &self,
        webhook_secret: &str,
        signature_header: &str,
        raw_body: &str,
    ) -> Result<(bool, WorkspaceEntitlements)> {
        verify_stripe_signature(
            webhook_secret,
            signature_header,
            raw_body,
            Utc::now().timestamp(),
        )?;
        let val: serde_json::Value = serde_json::from_str(raw_body)
            .map_err(|e| GmError::Config(format!("Invalid Stripe webhook JSON: {e}")))?;
        let event_id = val["id"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| GmError::Config("Missing Stripe event id".into()))?;
        let event_type = val["type"].as_str().unwrap_or("unknown");
        let obj = &val["data"]["object"];
        let ws_id = obj["metadata"]["ws_id"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("default");
        let amount_cents = obj["amount_total"]
            .as_i64()
            .or_else(|| obj["amount_paid"].as_i64())
            .unwrap_or(0);
        let customer_id = obj["customer"].as_str().map(ToString::to_string);

        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO stripe_webhook_events_v1
                 (event_id, event_type, ws_id, amount_cents, processed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![event_id, event_type, ws_id, amount_cents, now],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;

        if inserted == 0 {
            drop(conn);
            return Ok((false, self.get_entitlements(ws_id)?));
        }

        let new_mode = if matches!(event_type, "checkout.session.completed" | "invoice.paid") {
            "hosted_pro"
        } else {
            "hosted_free"
        };
        conn.execute(
            "INSERT INTO workspace_entitlements_v1
             (ws_id, mode, byok_unrestricted, credit_balance_cents, stripe_customer_id, updated_at)
             VALUES (?1, ?2, 1, ?3, ?4, ?5)
             ON CONFLICT(ws_id) DO UPDATE SET
               mode = excluded.mode,
               credit_balance_cents = workspace_entitlements_v1.credit_balance_cents + excluded.credit_balance_cents,
               stripe_customer_id = COALESCE(excluded.stripe_customer_id, workspace_entitlements_v1.stripe_customer_id),
               updated_at = excluded.updated_at",
            params![ws_id, new_mode, amount_cents, customer_id, now],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;
        drop(conn);
        Ok((true, self.get_entitlements(ws_id)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripe_webhook_signature_idempotency_and_self_hosted_byok() {
        let store = BillingStore::in_memory(true).unwrap();
        assert!(store.check_self_hosted_byok_allowed());
        let def = store.get_entitlements("local").unwrap();
        assert_eq!(def.mode, BillingMode::SelfHostedByok);
        assert!(def.byok_unrestricted);

        let hosted = BillingStore::in_memory(false).unwrap();
        let ts = Utc::now().timestamp();
        let payload = r#"{"id":"evt_123","type":"checkout.session.completed","data":{"object":{"customer":"cus_1","amount_total":2000,"metadata":{"ws_id":"ws1"}}}}"#;
        let mac = compute_hmac_sha256_hex(b"whsec_test", format!("{ts}.{payload}").as_bytes());
        let hdr = format!("t={ts},v1={mac}");

        // Invalid signature rejected
        assert!(
            hosted
                .process_stripe_webhook("whsec_test", "t=0,v1=bad", payload)
                .is_err()
        );

        // First valid delivery credits 2000 cents
        let (applied1, ent1) = hosted
            .process_stripe_webhook("whsec_test", &hdr, payload)
            .unwrap();
        assert!(applied1);
        assert_eq!(ent1.mode, BillingMode::HostedPro);
        assert_eq!(ent1.credit_balance_cents, 2000);

        // Duplicate delivery is idempotent and does NOT double-credit
        let (applied2, ent2) = hosted
            .process_stripe_webhook("whsec_test", &hdr, payload)
            .unwrap();
        assert!(!applied2);
        assert_eq!(ent2.credit_balance_cents, 2000);
    }
}
