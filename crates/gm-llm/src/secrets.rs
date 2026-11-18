use gm_core::error::{GmError, Result};
use tracing::debug;
#[cfg(target_os = "macos")]
use tracing::info;

pub const DEFAULT_KEYCHAIN_SERVICE: &str = "co.anysphere.grok-muse";
pub const META_KEY_ACCOUNT: &str = "meta_api_key";
pub const XAI_KEY_ACCOUNT: &str = "xai_api_key";
pub const OPENBOT_WEB_TOKEN_ACCOUNT: &str = "openbot_web_token";
pub const TELEGRAM_TOKEN_ACCOUNT: &str = "telegram_bot_token";
pub const SLACK_TOKEN_ACCOUNT: &str = "slack_bot_token";
pub const DISCORD_TOKEN_ACCOUNT: &str = "discord_bot_token";

#[derive(Debug, Clone)]
pub struct LeasedSecret {
    pub secret_ref: String,
    pub value: String,
}

/// Resolves opaque vault references (`secret://keychain/<account>` or `secret://1password/<vault>/<item>/<field>`)
/// without exposing plaintext credentials in LLM prompts or transcripts.
#[derive(Debug, Clone, Default)]
pub struct VaultSecretResolver {
    keychain: KeychainSecrets,
}

impl VaultSecretResolver {
    pub fn new(keychain: KeychainSecrets) -> Self {
        Self { keychain }
    }

    pub fn keychain(&self) -> &KeychainSecrets {
        &self.keychain
    }

    pub fn resolve(&self, secret_ref: &str) -> Result<LeasedSecret> {
        if let Some(account) = secret_ref.strip_prefix("secret://keychain/") {
            let clean = account.trim();
            if clean.is_empty() {
                return Err(GmError::Config(
                    "Empty keychain account in secret URI".into(),
                ));
            }
            let value = self.keychain.get_secret(clean)?;
            return Ok(LeasedSecret {
                secret_ref: secret_ref.to_string(),
                value,
            });
        }

        if let Some(op_path) = secret_ref.strip_prefix("secret://1password/") {
            let clean = op_path.trim();
            if clean.is_empty() {
                return Err(GmError::Config("Empty 1Password path in secret URI".into()));
            }
            let op_uri = format!("op://{clean}");
            let output = std::process::Command::new("op")
                .args(["read", &op_uri, "--no-newline"])
                .output()
                .map_err(|e| {
                    GmError::Config(format!("Failed to invoke 1Password CLI (`op`): {e}"))
                })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(GmError::Config(format!(
                    "1Password CLI (`op read`) failed for '{secret_ref}': {stderr}"
                )));
            }
            let value = String::from_utf8(output.stdout).map_err(|e| {
                GmError::Config(format!("1Password secret is not valid UTF-8: {e}"))
            })?;
            return Ok(LeasedSecret {
                secret_ref: secret_ref.to_string(),
                value,
            });
        }

        Err(GmError::Config(format!(
            "Unsupported secret URI scheme '{secret_ref}'. Expected 'secret://keychain/<account>' or 'secret://1password/<vault>/<item>/<field>'"
        )))
    }
}

/// macOS Keychain Secret Manager for Meta and xAI API Keys.
#[derive(Debug, Clone)]
pub struct KeychainSecrets {
    #[allow(dead_code)]
    service: String,
    #[cfg(test)]
    memory: Option<std::sync::Arc<parking_lot::Mutex<std::collections::BTreeMap<String, String>>>>,
}

impl Default for KeychainSecrets {
    fn default() -> Self {
        Self::new(DEFAULT_KEYCHAIN_SERVICE)
    }
}

impl KeychainSecrets {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            #[cfg(test)]
            memory: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self {
            service: String::new(),
            memory: Some(Default::default()),
        }
    }

    /// Retrieve secret from OS keychain or environment, returning Option<String>.
    pub fn get(&self, account: &str) -> Option<String> {
        self.get_secret(account).ok()
    }

    /// Retrieve secret from OS keychain, falling back to environment variable if absent.
    pub fn get_secret(&self, account: &str) -> Result<String> {
        #[cfg(test)]
        if let Some(memory) = &self.memory {
            return memory
                .lock()
                .get(account)
                .cloned()
                .ok_or_else(|| GmError::Config(format!("Test secret '{account}' not found")));
        }

        #[cfg(target_os = "macos")]
        {
            match security_framework::passwords::get_generic_password(&self.service, account) {
                Ok(bytes) => {
                    let secret = String::from_utf8(bytes).map_err(|e| {
                        GmError::Config(format!("Secret for {account} is invalid UTF-8: {e}"))
                    })?;
                    debug!("Loaded secret '{account}' from macOS Keychain");
                    return Ok(secret);
                }
                Err(e) => {
                    debug!("Keychain lookup for '{account}' failed: {e}");
                }
            }
        }

        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if let Ok(entry) = keyring::Entry::new(&self.service, account)
            && let Ok(secret) = entry.get_password()
        {
            return Ok(secret);
        }

        // Fallback to environment variables
        let env_var_name = account.to_uppercase();
        if let Ok(val) = std::env::var(&env_var_name) {
            debug!("Loaded secret '{account}' from environment variable {env_var_name}");
            return Ok(val);
        }

        // Check provider-specific aliases (e.g. GROK_API_KEY, MUSE_API_KEY)
        let aliases: &[&str] = match account {
            META_KEY_ACCOUNT => &["MODEL_API_KEY", "MUSE_API_KEY"],
            XAI_KEY_ACCOUNT => &["GROK_API_KEY"],
            _ => &[],
        };
        for alias in aliases {
            if let Ok(value) = std::env::var(alias) {
                return Ok(value);
            }
        }

        Err(GmError::Config(format!(
            "Secret '{account}' not found in macOS Keychain or environment"
        )))
    }

    /// Save secret to macOS Keychain (overwrites existing key if present).
    pub fn set_secret(&self, account: &str, secret: &str) -> Result<()> {
        #[cfg(test)]
        if let Some(memory) = &self.memory {
            memory.lock().insert(account.to_owned(), secret.to_owned());
            return Ok(());
        }

        #[cfg(target_os = "macos")]
        {
            // The platform API updates an existing item atomically. Preserve the old key if saving fails.
            security_framework::passwords::set_generic_password(
                &self.service,
                account,
                secret.as_bytes(),
            )
            .map_err(|e| {
                GmError::Config(format!("Failed to save '{account}' to macOS Keychain: {e}"))
            })?;
            info!(
                "Stored secret '{account}' in macOS Keychain (service: {})",
                self.service
            );
            Ok(())
        }

        #[cfg(not(target_os = "macos"))]
        {
            keyring::Entry::new(&self.service, account)
                .and_then(|entry| entry.set_password(secret))
                .map_err(|e| GmError::Config(format!("OS credential store unavailable: {e}")))
        }
    }

    /// Delete secret from macOS Keychain.
    pub fn delete_secret(&self, account: &str) -> Result<()> {
        #[cfg(test)]
        if let Some(memory) = &self.memory {
            memory.lock().remove(account);
            return Ok(());
        }

        #[cfg(target_os = "macos")]
        {
            if let Err(error) =
                security_framework::passwords::delete_generic_password(&self.service, account)
                && error.code() != -25300
            {
                return Err(GmError::Config(format!(
                    "Failed to delete '{account}' from macOS Keychain: {error}"
                )));
            }
            info!("Deleted secret '{account}' from macOS Keychain");
            Ok(())
        }

        #[cfg(not(target_os = "macos"))]
        {
            match keyring::Entry::new(&self.service, account)
                .and_then(|entry| entry.delete_credential())
            {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(GmError::Config(format!(
                    "OS credential store unavailable: {error}"
                ))),
            }
        }
    }

    pub fn get_meta_key(&self) -> Result<String> {
        self.get_secret(META_KEY_ACCOUNT)
    }

    pub fn set_meta_key(&self, key: &str) -> Result<()> {
        self.set_secret(META_KEY_ACCOUNT, key)
    }

    pub fn get_xai_key(&self) -> Result<String> {
        self.get_secret(XAI_KEY_ACCOUNT)
    }

    pub fn set_xai_key(&self, key: &str) -> Result<()> {
        self.set_secret(XAI_KEY_ACCOUNT, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_secrets_are_shared_and_ignore_environment() {
        let secrets = KeychainSecrets::in_memory();
        let clone = secrets.clone();
        let account = "GM_TEST_MEMORY_ONLY_SECRET";
        unsafe { std::env::set_var(account, "environment-secret") };
        assert!(secrets.get_secret(account).is_err());
        secrets.set_secret(account, "first").unwrap();
        assert_eq!(clone.get_secret(account).unwrap(), "first");
        clone.set_secret(account, "second").unwrap();
        assert_eq!(secrets.get_secret(account).unwrap(), "second");
        secrets.delete_secret(account).unwrap();
        assert!(clone.get_secret(account).is_err());
        assert!(secrets.delete_secret(account).is_ok());
        assert!(KeychainSecrets::in_memory().get_secret(account).is_err());
        unsafe { std::env::remove_var(account) };
    }

    #[test]
    fn test_keychain_env_fallback() {
        let test_account = "TEST_TEMP_API_KEY";
        unsafe {
            std::env::set_var(test_account, "test-key-val-123");
        }

        let secrets = KeychainSecrets::new("co.anysphere.grok-muse.test");
        let val = secrets.get_secret(test_account).unwrap();
        assert_eq!(val, "test-key-val-123");

        unsafe {
            std::env::remove_var(test_account);
        }
    }

    #[test]
    fn test_keychain_env_alias_fallback() {
        unsafe {
            std::env::set_var("GROK_API_KEY", "grok-secret-abc");
        }

        let secrets = KeychainSecrets::new("co.anysphere.grok-muse.test-alias");
        let val = secrets.get_xai_key().unwrap();
        assert_eq!(val, "grok-secret-abc");

        unsafe {
            std::env::remove_var("GROK_API_KEY");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_keychain_roundtrip_macos() {
        let secrets = KeychainSecrets::new("co.anysphere.grok-muse.unit-test");
        let account = "unit_test_key";
        let secret_val1 = "sk-test-super-secret-12345";
        let secret_val2 = "sk-test-updated-secret-67890";

        // Cleanup any stale key from previous run
        let _ = secrets.delete_secret(account);

        // Store first value
        secrets
            .set_secret(account, secret_val1)
            .expect("store secret");

        // Retrieve
        let retrieved = secrets.get_secret(account).expect("retrieve secret");
        assert_eq!(retrieved, secret_val1);

        // Update (verify overwriting existing key works without errSecDuplicateItem)
        secrets
            .set_secret(account, secret_val2)
            .expect("overwrite secret");

        let retrieved_updated = secrets.get_secret(account).expect("retrieve updated");
        assert_eq!(retrieved_updated, secret_val2);

        // Delete
        secrets.delete_secret(account).expect("delete secret");

        // Verify gone (when env var not set)
        assert!(secrets.get_secret(account).is_err());
    }

    #[test]
    fn test_vault_secret_resolver_keychain_and_invalid_scheme() {
        unsafe {
            std::env::set_var("VAULT_TEST_TOKEN", "s3cr3t_vault_val_42");
        }
        let resolver =
            VaultSecretResolver::new(KeychainSecrets::new("co.anysphere.grok-muse.vault-test"));
        let leased = resolver
            .resolve("secret://keychain/vault_test_token")
            .unwrap();
        assert_eq!(leased.secret_ref, "secret://keychain/vault_test_token");
        assert_eq!(leased.value, "s3cr3t_vault_val_42");

        assert!(resolver.resolve("http://not-a-secret").is_err());
        unsafe {
            std::env::remove_var("VAULT_TEST_TOKEN");
        }
    }
}
