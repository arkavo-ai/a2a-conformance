// Shared identity fixture loading for the arkavo-ext-rust harnesses.
//
// The committed P-256 test keys and `manifest.json` live in
// `adapters/shared-fixtures/identity/` (see its README). Both harnesses load
// them at startup; the dir is located via $A2A_IDENTITY_FIXTURES, then
// cwd-relative (the runner runs from the repo root), then relative to the
// harness executable (adapters/arkavo-ext-rust/target/release/<bin>).

use std::path::PathBuf;

use p256::SecretKey;
use p256::ecdsa::{SigningKey, VerifyingKey};
use p256::pkcs8::DecodePublicKey;
use serde_json::Value;

/// The aia-identity-v1 extension URI (spec §1).
pub const EXTENSION_URI: &str = "https://arkavo.social/ext/a2a/aia-identity/v1";

pub struct IdentityFixtures {
    /// Spec-pinned issuer string (`iss`).
    pub iss: String,
    pub client_did: String,
    pub server_did: String,
    /// AIA issuer signing key (harness-local mint path; trust root for peers).
    pub issuer_signing: SigningKey,
    /// AIA issuer public key (the verifier trust root).
    pub issuer_verifying: VerifyingKey,
    /// Target agent key (card signing).
    pub server_signing: SigningKey,
    /// Presenting agent public key (the `cnf` confirmation key).
    pub client_verifying: VerifyingKey,
}

fn fixtures_dir() -> Result<PathBuf, String> {
    if let Ok(dir) = std::env::var("A2A_IDENTITY_FIXTURES") {
        return Ok(PathBuf::from(dir));
    }
    let cwd_relative = PathBuf::from("adapters/shared-fixtures/identity");
    if cwd_relative.is_dir() {
        return Ok(cwd_relative);
    }
    // <repo>/adapters/arkavo-ext-rust/target/release/<bin> -> <repo>/adapters
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe;
        for _ in 0..4 {
            if !dir.pop() {
                break;
            }
        }
        let exe_relative = dir.join("shared-fixtures/identity");
        if exe_relative.is_dir() {
            return Ok(exe_relative);
        }
    }
    Err("cannot locate adapters/shared-fixtures/identity (set A2A_IDENTITY_FIXTURES)".into())
}

fn load_signing_key(dir: &std::path::Path, name: &str) -> Result<SigningKey, String> {
    let path = dir.join(name);
    let pem = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let secret = SecretKey::from_sec1_pem(&pem)
        .map_err(|e| format!("{}: not a SEC1 EC private key: {e}", path.display()))?;
    Ok(SigningKey::from(secret))
}

fn load_verifying_key(dir: &std::path::Path, name: &str) -> Result<VerifyingKey, String> {
    let path = dir.join(name);
    let pem = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    VerifyingKey::from_public_key_pem(&pem)
        .map_err(|e| format!("{}: not an SPKI public key: {e}", path.display()))
}

impl IdentityFixtures {
    pub fn load() -> Result<Self, String> {
        let dir = fixtures_dir()?;
        let manifest_path = dir.join("manifest.json");
        let manifest_text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let manifest: Value = serde_json::from_str(&manifest_text)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let field = |name: &str| -> Result<String, String> {
            manifest
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("manifest.json missing {name:?}"))
        };
        Ok(Self {
            iss: field("iss")?,
            client_did: field("clientDid")?,
            server_did: field("serverDid")?,
            issuer_signing: load_signing_key(&dir, "test-issuer.key.pem")?,
            issuer_verifying: load_verifying_key(&dir, "test-issuer.pub.pem")?,
            server_signing: load_signing_key(&dir, "test-server-agent.key.pem")?,
            client_verifying: load_verifying_key(&dir, "test-client-agent.pub.pem")?,
        })
    }
}
