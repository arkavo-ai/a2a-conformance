// Shared TDF (tdf-parts-v1) harness support for the arkavo-ext-rust harnesses.
//
// Phase 5 (TDF-HARNESS.md): the committed test DEKs live in
// `adapters/shared-fixtures/tdf/`. Both harnesses load them at startup; the dir
// is located via $A2A_TDF_FIXTURES, then cwd-relative (the runner runs from the
// repo root), then relative to the harness executable
// (adapters/arkavo-ext-rust/target/release/<bin>).
//
// The scenario-keyed behavior (which scenario uses which DEK, what plaintext is
// encrypted, the b3 mutate leg) lives in the harnesses; the SDK crate
// (arkavo-a2a-tdf) stays scenario-blind.

use std::path::PathBuf;

use arkavo_a2a_tdf::Dek;

/// The pinned roundtrip plaintext (TDF-HARNESS.md / shared fixtures `hello-tdf`).
pub const KNOWN_PLAINTEXT: &str = "hello tdf";
/// The pinned sibling plaintext part value (proves mixed messages, §5).
pub const SIBLING_PLAINTEXT: &str = "sibling plaintext";
/// Fixed 12-byte nonce for live harness runs (both peers read it from the
/// manifest, so a fixed nonce keeps the wire deterministic).
pub const NONCE: [u8; 12] = [0xaa; 12];
/// The KAS URL pinned into the manifest (spec §1 default).
pub const KAS_URL: &str = arkavo_a2a_tdf::DEFAULT_KAS_URL;

fn fixtures_dir() -> Result<PathBuf, String> {
    if let Ok(dir) = std::env::var("A2A_TDF_FIXTURES") {
        return Ok(PathBuf::from(dir));
    }
    let cwd_relative = PathBuf::from("adapters/shared-fixtures/tdf");
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
        let exe_relative = dir.join("shared-fixtures/tdf");
        if exe_relative.is_dir() {
            return Ok(exe_relative);
        }
    }
    Err("cannot locate adapters/shared-fixtures/tdf (set A2A_TDF_FIXTURES)".into())
}

fn load_dek(dir: &std::path::Path, name: &str) -> Result<Dek, String> {
    let path = dir.join(name);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Dek::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// The committed test DEKs: `test-dek.bin` (the correct unwrapped key both peers
/// hold) and `wrong-dek.bin` (the KAS-denial leg's wrong key).
pub struct TdfFixtures {
    pub test_dek: Dek,
    pub wrong_dek: Dek,
}

impl TdfFixtures {
    pub fn load() -> Result<Self, String> {
        let dir = fixtures_dir()?;
        Ok(Self {
            test_dek: load_dek(&dir, "test-dek.bin")?,
            wrong_dek: load_dek(&dir, "wrong-dek.bin")?,
        })
    }
}
