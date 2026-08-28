//! Tests for `reng config provider` — tempdir-backed, serialized through
//! [`FS_LOCK`] because they mutate process-global state (`$HOME`, the
//! current directory, `LLM_CONFIG`), following the same guard pattern as
//! `config::resolver::tests`.

use super::provider::*;
use std::sync::{Mutex, MutexGuard};

/// Serializes every test in this module (they all touch process-global
/// state). Tolerates poisoning: guards restore state on drop even when a
/// test panics.
static FS_LOCK: Mutex<()> = Mutex::new(());

fn fs_lock() -> MutexGuard<'static, ()> {
    FS_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Guard that temporarily sets `$HOME` and restores it on drop.
struct HomeGuard {
    original: Option<String>,
}

impl HomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let original = std::env::var("HOME").ok();
        std::env::set_var("HOME", path.as_os_str());
        Self { original }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.original.take() {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// Guard that temporarily changes the current directory and restores it on drop.
struct CwdGuard {
    original: std::path::PathBuf,
}

impl CwdGuard {
    fn set(path: &std::path::Path) -> Self {
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(path).unwrap();
        Self { original }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.original).unwrap();
    }
}

/// Guard that captures and clears an env var, restoring it on drop.
struct EnvGuard {
    key: &'static str,
    original: Option<String>,
}

impl EnvGuard {
    fn unset(key: &'static str) -> Self {
        let original = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, original }
    }

    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.original.take() {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

fn patch_full() -> ProviderPatch {
    ProviderPatch {
        model: Some("gpt-4o".to_string()),
        api_base: Some("https://api.openai.com/v1".to_string()),
        api_key: Some("sk-test-123".to_string()),
        max_tokens: Some(8192),
        temperature: Some(0.7),
        disable_thinking: true,
    }
}

/// Drain the test-only stderr capture buffer (`provider::STDERR_CAPTURE`),
/// which `warn_stderr` records into alongside `eprintln!`.
fn take_captured_stderr() -> Vec<String> {
    STDERR_CAPTURE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect()
}

// ─── 1. set creates a new project file ─────────────────────────────

#[test]
fn set_creates_new_project_file_with_one_llm_entry() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(&tmp.path().join("home"));
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    run_set("openai", patch_full(), false, false).unwrap();

    let path = tmp.path().join(".code-audit-config.toml");
    assert!(path.exists(), "project config file must be created");
    let entries = read_llm_entries(&path).unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.provider, "openai");
    assert_eq!(e.model, "gpt-4o");
    assert_eq!(e.api_key, "sk-test-123");
    assert_eq!(e.api_base, "https://api.openai.com/v1");
    assert_eq!(e.max_tokens, 8192);
    assert!((e.temperature - 0.7).abs() < f32::EPSILON);
    assert_eq!(e.disable_thinking, Some(true));
}

#[test]
fn set_new_entry_falls_back_to_llmconfig_defaults() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(&tmp.path().join("home"));
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    run_set("ollama", ProviderPatch::default(), false, false).unwrap();

    let entries = read_llm_entries(&tmp.path().join(".code-audit-config.toml")).unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.provider, "ollama");
    assert_eq!(e.api_key, "", "omitted api_key is stored as an empty string");
    assert_eq!(e.max_tokens, 4096, "serde default_max_tokens");
    assert!((e.temperature - 0.3).abs() < f32::EPSILON, "serde default_temperature");
    assert_eq!(e.disable_thinking, None, "flag omitted → key not written");
}

// ─── 2. set preserves other sections and comments ──────────────────

#[test]
fn set_preserves_other_sections_and_comments_byte_for_byte() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(&tmp.path().join("home"));
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    let original = r#"# review-engine config — keep this comment

[gitlab]
base_url = "https://gitlab.example.com" # trailing comment stays too

[[llm]]
provider = "openai"
model = "gpt-4"
api_key = "sk-old"
api_base = "https://api.openai.com/v1"
max_tokens = 4096
temperature = 0.3

[report]
format = "markdown"
"#;
    let path = tmp.path().join(".code-audit-config.toml");
    std::fs::write(&path, original).unwrap();

    let patch = ProviderPatch {
        temperature: Some(0.9),
        ..ProviderPatch::default()
    };
    run_set("openai", patch, false, false).unwrap();

    let after = std::fs::read_to_string(&path).unwrap();
    // The ONLY change anywhere in the file is the temperature literal
    // inside the touched [[llm]] entry.
    let expected = original.replace("temperature = 0.3", "temperature = 0.9");
    assert_eq!(
        after, expected,
        "everything outside the touched entry must survive byte-for-byte"
    );
    assert!(after.contains("# review-engine config — keep this comment"));
    assert!(after.contains("[gitlab]"));
    assert!(after.contains("[report]"));
}

// ─── 3. set keeps / replaces the stored api key ────────────────────

#[test]
fn set_keeps_stored_key_when_api_key_omitted_and_replaces_when_passed() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(&tmp.path().join("home"));
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    run_set("openai", patch_full(), false, false).unwrap();

    // Update without --api-key: the stored key must survive.
    let patch = ProviderPatch {
        model: Some("gpt-4o-mini".to_string()),
        ..ProviderPatch::default()
    };
    run_set("openai", patch, false, false).unwrap();
    let path = tmp.path().join(".code-audit-config.toml");
    let entries = read_llm_entries(&path).unwrap();
    assert_eq!(entries.len(), 1, "update must not duplicate the entry");
    assert_eq!(entries[0].model, "gpt-4o-mini");
    assert_eq!(
        entries[0].api_key, "sk-test-123",
        "omitted --api-key keeps the stored key"
    );
    assert_eq!(
        entries[0].disable_thinking,
        Some(true),
        "omitted --disable-thinking leaves the existing value"
    );

    // An explicitly blank --api-key is filtered to None by the dispatcher
    // (blank = keep, same semantic as the web UI).
    let patch = ProviderPatch {
        api_key: Some("".to_string()).filter(|k| !k.is_empty()),
        ..ProviderPatch::default()
    };
    run_set("openai", patch, false, false).unwrap();
    let entries = read_llm_entries(&path).unwrap();
    assert_eq!(
        entries[0].api_key, "sk-test-123",
        "blank --api-key keeps the stored key"
    );

    // Update with --api-key: the key is replaced.
    let patch = ProviderPatch {
        api_key: Some("sk-replaced".to_string()),
        ..ProviderPatch::default()
    };
    run_set("openai", patch, false, false).unwrap();
    let entries = read_llm_entries(&path).unwrap();
    assert_eq!(entries[0].api_key, "sk-replaced");
}

// ─── 4. remove deletes only the named entry ────────────────────────

#[test]
fn remove_deletes_only_the_named_entry() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(&tmp.path().join("home"));
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    run_set("openai", patch_full(), false, false).unwrap();
    run_set(
        "deepseek",
        ProviderPatch {
            model: Some("deepseek-chat".to_string()),
            api_base: Some("https://api.deepseek.com".to_string()),
            api_key: Some("sk-deep".to_string()),
            ..ProviderPatch::default()
        },
        false,
        false,
    )
    .unwrap();

    let path = tmp.path().join(".code-audit-config.toml");
    assert_eq!(read_llm_entries(&path).unwrap().len(), 2);

    run_remove("openai", false, false).unwrap();

    let entries = read_llm_entries(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].provider, "deepseek");
    assert_eq!(entries[0].api_key, "sk-deep", "the surviving entry is untouched");
}

#[test]
fn remove_missing_name_or_file_errors() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(&tmp.path().join("home"));
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    // No file at all → clean error.
    let err = run_remove("openai", false, false).unwrap_err();
    assert!(err.to_string().contains("config file not found"), "got: {err}");

    // File exists, entry does not → clean error naming the provider.
    run_set("openai", patch_full(), false, false).unwrap();
    let err = run_remove("nonexistent", false, false).unwrap_err();
    assert!(err.to_string().contains("\"nonexistent\""), "got: {err}");
    assert!(err.to_string().contains("not found"), "got: {err}");

    // The failed remove left the existing entry alone.
    let entries = read_llm_entries(&tmp.path().join(".code-audit-config.toml")).unwrap();
    assert_eq!(entries.len(), 1);
}

// ─── 5. list masks keys and annotates sources ──────────────────────

#[test]
fn list_masks_keys_and_annotates_sources() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(&tmp.path().join("home"));
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    // Project scope: the resolved list comes from the project file.
    run_set("openai", patch_full(), false, false).unwrap();
    let entries = resolved_providers().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].source, Source::Project);
    let out = render_provider_table(&entries);
    assert!(
        !out.contains("sk-test-123"),
        "the raw key must never be printed, got:\n{out}"
    );
    assert!(out.contains("***"), "the mask sentinel must be printed, got:\n{out}");
    assert!(out.contains("[project]"), "source annotation missing, got:\n{out}");

    // User scope: project file gone → user-level fallback wins.
    std::fs::create_dir_all(tmp.path().join("empty-dir")).unwrap();
    let _cwd2 = CwdGuard::set(&tmp.path().join("empty-dir"));
    let user_dir = tmp.path().join("home").join(".config").join("review-engine");
    std::fs::create_dir_all(&user_dir).unwrap();
    std::fs::write(
        user_dir.join(".code-audit-config.toml"),
        "[[llm]]\nprovider = \"user-prov\"\nmodel = \"m\"\napi_key = \"sk-user-secret\"\napi_base = \"https://u\"\n",
    )
    .unwrap();
    let entries = resolved_providers().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].source, Source::User);
    let out = render_provider_table(&entries);
    assert!(!out.contains("sk-user-secret"), "got:\n{out}");
    assert!(out.contains("[user]"), "got:\n{out}");

    // Env scope: no files at all → LLM_CONFIG is the last resort.
    let _home2 = HomeGuard::set(&tmp.path().join("home-without-config"));
    let _env2 = EnvGuard::set(
        "LLM_CONFIG",
        r#"[{"provider":"env-prov","model":"m","api_base":"https://e","api_key":"sk-env-secret"}]"#,
    );
    let entries = resolved_providers().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].source, Source::Env);
    let out = render_provider_table(&entries);
    assert!(!out.contains("sk-env-secret"), "got:\n{out}");
    assert!(out.contains("***"), "got:\n{out}");
    assert!(out.contains("[env]"), "got:\n{out}");
}

#[test]
fn list_empty_renders_friendly_hint() {
    let out = render_provider_table(&[]);
    assert!(out.contains("no providers configured"), "got:\n{out}");
    assert!(
        out.contains("config provider set"),
        "the hint must point at `set`, got:\n{out}"
    );
}

// ─── 6. --global writes to the user-level path ─────────────────────

#[test]
fn set_global_writes_to_user_level_path_and_creates_parent_dirs() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    // HOME points at a not-yet-created directory: set must create it.
    let home = tmp.path().join("fresh-home");
    let _home = HomeGuard::set(&home);
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    run_set("openai", patch_full(), true, false).unwrap();

    let user_path = home
        .join(".config")
        .join("review-engine")
        .join(".code-audit-config.toml");
    assert!(
        user_path.exists(),
        "user-level config must be created (with parent dirs)"
    );
    let entries = read_llm_entries(&user_path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].provider, "openai");
    assert_eq!(entries[0].api_key, "sk-test-123");

    // The project directory must NOT have gained a config file.
    assert!(
        !tmp.path().join(".code-audit-config.toml").exists(),
        "--global must not write the project file"
    );

    // remove --global targets the same file.
    run_remove("openai", true, false).unwrap();
    assert!(read_llm_entries(&user_path).unwrap().is_empty());
}

// ─── 7. test probes connectivity and fails cleanly ─────────────────

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_unreachable_api_base_fails_nonzero() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(&tmp.path().join("home"));
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    run_set(
        "openai",
        ProviderPatch {
            // 127.0.0.1:1 — connection refused, no real network needed.
            api_base: Some("http://127.0.0.1:1".to_string()),
            api_key: Some("sk-test-123".to_string()),
            ..ProviderPatch::default()
        },
        false,
        false,
    )
    .unwrap();

    let err = run_test("openai", false, false).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("connectivity test failed"), "got: {msg}");
    assert!(msg.contains("\"openai\""), "got: {msg}");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_unknown_provider_errors_without_probing() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(&tmp.path().join("home"));
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    let err = run_test("ghost", false, false).await.unwrap_err();
    assert!(err.to_string().contains("\"ghost\""), "got: {err}");
    assert!(err.to_string().contains("not found"), "got: {err}");
}

// ─── 8. unknown provider + empty api_base fails fast, no probe ─────

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_unknown_provider_empty_api_base_fails_fast_without_probing() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(&tmp.path().join("home"));
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    // The classic misconfiguration: `set mimo --api-key sk-real` with no
    // --api-base. The stored key must NOT be silently sent to
    // api.openai.com — the test must fail fast, before any network call.
    run_set(
        "mimo",
        ProviderPatch {
            model: Some("mimo-v1".to_string()),
            api_key: Some("sk-real".to_string()),
            ..ProviderPatch::default()
        },
        false,
        false,
    )
    .unwrap();

    let err = run_test("mimo", false, false).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("api_base is required for provider \"mimo\" (no well-known default)"),
        "got: {msg}"
    );
    // The error is the fail-fast resolution error itself — no connectivity
    // wrapper, proving no probe was ever attempted.
    assert!(
        !msg.contains("connectivity test failed"),
        "no request may be made for an unknown provider, got: {msg}"
    );
}

// ─── 9. known provider + empty api_base uses the well-known default ─

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_known_provider_empty_api_base_surfaces_resolved_default() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(&tmp.path().join("home"));
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    run_set(
        "openai",
        ProviderPatch {
            model: Some("gpt-4o".to_string()),
            api_key: Some("sk-test-123".to_string()),
            ..ProviderPatch::default()
        },
        false,
        false,
    )
    .unwrap();

    // Bogus key against the real OpenAI default: HTTP 401 with network, a
    // transport error without — either way the probe IS attempted and the
    // resolved default URL must surface in the failure output.
    let err = run_test("openai", false, false).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("connectivity test failed"), "got: {msg}");
    assert!(
        msg.contains("https://api.openai.com/v1"),
        "the resolved well-known default must be visible in the failure, got: {msg}"
    );
}

// ─── 10. malformed project TOML warns on stderr, user row listed ────

#[test]
fn list_warns_on_malformed_project_toml_and_shows_user_row() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(&tmp.path().join("home"));
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    // Malformed project file (written directly — `set` would refuse it).
    let project_path = tmp.path().join(".code-audit-config.toml");
    std::fs::write(&project_path, "this is not = = valid toml").unwrap();

    // A valid user-level entry that must win the fallback.
    let user_dir = tmp.path().join("home").join(".config").join("review-engine");
    std::fs::create_dir_all(&user_dir).unwrap();
    std::fs::write(
        user_dir.join(".code-audit-config.toml"),
        "[[llm]]\nprovider = \"user-prov\"\nmodel = \"m\"\napi_key = \"sk-user-secret\"\napi_base = \"https://u\"\n",
    )
    .unwrap();

    let _ = take_captured_stderr();
    let entries = resolved_providers().unwrap();

    // The malformed project file fell back to the user-level entry…
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].source, Source::User);
    let out = render_provider_table(&entries);
    assert!(out.contains("user-prov"), "got:\n{out}");
    assert!(out.contains("[user]"), "got:\n{out}");

    // …and the fallback was announced on stderr, naming the file.
    let captured = take_captured_stderr().join("\n");
    assert!(
        captured.contains(&project_path.display().to_string()),
        "the warning must name the malformed file, got:\n{captured}"
    );
    assert!(
        captured.contains("treating its provider list as empty"),
        "got:\n{captured}"
    );
}

// ─── 11. config files holding keys are 0600 on unix ─────────────────

#[cfg(unix)]
#[test]
fn set_tightens_config_file_permissions_to_0600_on_create_and_update() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(&tmp.path().join("home"));
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    use std::os::unix::fs::PermissionsExt;
    let path = tmp.path().join(".code-audit-config.toml");

    // Create: even under a permissive umask the file must end up 0600.
    run_set("openai", patch_full(), false, false).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "newly created config file must be 0600, got {mode:#o}");

    // Update of an existing 0644 file containing key material tightens it.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    run_set(
        "openai",
        ProviderPatch {
            model: Some("gpt-4o-mini".to_string()),
            ..ProviderPatch::default()
        },
        false,
        false,
    )
    .unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "updating a 0644 config file must tighten it to 0600, got {mode:#o}"
    );
}

// ─── 12. cleartext http:// non-localhost warns but still probes ─────

#[test]
fn cleartext_key_warning_predicate_loopback_vs_remote() {
    assert!(cleartext_key_warning("https://api.openai.com/v1").is_none());
    assert!(cleartext_key_warning("http://localhost:11434").is_none());
    assert!(cleartext_key_warning("http://127.0.0.1:8080").is_none());
    assert!(cleartext_key_warning("http://[::1]:8080").is_none());
    assert!(cleartext_key_warning("http://foo.localhost:8000").is_none());
    assert!(cleartext_key_warning("http://192.168.1.10:8000").is_some());
    assert!(cleartext_key_warning("http://llm.internal:8000").is_some());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_cleartext_http_non_loopback_warns_and_still_probes() {
    let _lock = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(&tmp.path().join("home"));
    let _cwd = CwdGuard::set(tmp.path());
    let _env = EnvGuard::unset("LLM_CONFIG");

    run_set(
        "custom",
        ProviderPatch {
            // Port 1 on a public DNS name: refused fast with or without
            // network, and unambiguously a non-loopback cleartext target.
            api_base: Some("http://example.com:1".to_string()),
            api_key: Some("sk-test-123".to_string()),
            ..ProviderPatch::default()
        },
        false,
        false,
    )
    .unwrap();

    let _ = take_captured_stderr();
    let err = run_test("custom", false, false).await.unwrap_err();

    // The cleartext warning reached stderr, naming the URL…
    let captured = take_captured_stderr().join("\n");
    assert!(
        captured.contains("cleartext HTTP"),
        "the cleartext-key warning must reach stderr, got:\n{captured}"
    );
    assert!(captured.contains("http://example.com:1"), "got:\n{captured}");

    // …and the probe was still attempted: the failure is a connectivity
    // error against that URL, not the api_base-required fail-fast.
    let msg = format!("{err:#}");
    assert!(msg.contains("connectivity test failed"), "got: {msg}");
    assert!(msg.contains("http://example.com:1"), "got: {msg}");
}
