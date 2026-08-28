//! `reng config provider` — git-config-like management of `[[llm]]` provider
//! entries in review-engine config files.
//!
//! Scope semantics mirror `git config`:
//! - default scope for `set`/`remove` is the project file
//!   `.code-audit-config.toml` in the current directory;
//! - `--global` targets the user-level file
//!   `~/.config/review-engine/.code-audit-config.toml` (same path logic as
//!   `config::resolver::user_fallback`);
//! - `--project` selects the project file explicitly (useful for scripts)
//!   and conflicts with `--global` (enforced by clap).
//!
//! All edits are surgical: they go through `toml_edit` so every section,
//! comment, and formatting choice outside the touched `[[llm]]` entry
//! survives byte-for-byte.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use review_engine::models::{default_max_tokens, default_temperature, LLMConfig, API_KEY_MASK};

use crate::cli::commands::ProviderAction;

/// Test-only capture of every warning written to stderr, so tests can
/// assert on terminal-visible output (same pattern as
/// `config::resolver::resolve::FALLBACK_WARNINGS`).
#[cfg(test)]
pub(crate) static STDERR_CAPTURE: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Print a warning to stderr. main.rs routes `tracing` into an in-memory
/// ring buffer plus logs.ndjson, so `tracing::warn!` never reaches the
/// terminal — CLI warnings must use `eprintln!` (the same reason the
/// review path does in `config::resolver::resolve`). Under `cfg(test)` the
/// message is also recorded in [`STDERR_CAPTURE`] for assertions.
fn warn_stderr(msg: String) {
    eprintln!("{msg}");
    #[cfg(test)]
    STDERR_CAPTURE.lock().unwrap_or_else(|e| e.into_inner()).push(msg);
}

// ─── Scope & source ────────────────────────────────────────────────

/// The concrete file a scoped command targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `.code-audit-config.toml` in the current directory (default).
    Project,
    /// `~/.config/review-engine/.code-audit-config.toml` (`--global`).
    Global,
}

/// Where a listed / resolved provider entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Project-level `.code-audit-config.toml`.
    Project,
    /// User-level `~/.config/review-engine/.code-audit-config.toml`.
    User,
    /// The `LLM_CONFIG` environment variable.
    Env,
}

impl Source {
    /// Annotation printed in `list` / `test` output (e.g. `[project]`).
    pub fn label(self) -> &'static str {
        match self {
            Source::Project => "[project]",
            Source::User => "[user]",
            Source::Env => "[env]",
        }
    }
}

impl Scope {
    fn source(self) -> Source {
        match self {
            Scope::Project => Source::Project,
            Scope::Global => Source::User,
        }
    }
}

/// Map the `--global`/`--project` flags to a scope; `None` = resolved chain
/// (only meaningful for `list` and `test`).
fn scope_from_flags(global: bool, project: bool) -> Option<Scope> {
    if global {
        Some(Scope::Global)
    } else if project {
        Some(Scope::Project)
    } else {
        None
    }
}

/// The project config file: `.code-audit-config.toml` in the cwd.
pub fn project_config_path() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("failed to resolve the current directory")?
        .join(".code-audit-config.toml"))
}

/// The user-level config file, resolved with the same home-dir logic as
/// `config::resolver::user_fallback`.
pub fn user_config_path() -> Result<PathBuf> {
    home::home_dir()
        .map(|p| p.join(".config").join("review-engine").join(".code-audit-config.toml"))
        .ok_or_else(|| anyhow::anyhow!("cannot determine the home directory to resolve the --global config path"))
}

fn scope_path(scope: Scope) -> Result<PathBuf> {
    match scope {
        Scope::Project => project_config_path(),
        Scope::Global => user_config_path(),
    }
}

// ─── Reading entries ───────────────────────────────────────────────

/// A provider entry annotated with the source it was resolved from.
#[derive(Debug, Clone)]
pub struct ListedProvider {
    pub config: LLMConfig,
    pub source: Source,
}

/// Parse the raw `[[llm]]` entries out of a config file. A missing file
/// yields an empty list; an unparsable file or an invalid `llm` section
/// warns on stderr and yields an empty list (mirroring the resolver's
/// `take_llm` semantics, so `list` agrees with what a review would use).
pub fn read_llm_entries(path: &Path) -> Result<Vec<LLMConfig>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content =
        std::fs::read_to_string(path).with_context(|| format!("failed to read config file {}", path.display()))?;
    Ok(parse_llm_entries(&content, path))
}

fn parse_llm_entries(content: &str, path: &Path) -> Vec<LLMConfig> {
    let val: toml::Value = match toml::from_str(content) {
        Ok(val) => val,
        Err(e) => {
            warn_stderr(format!(
                "warning: failed to parse config file {} as TOML: {e}; treating its provider list as empty",
                path.display()
            ));
            return Vec::new();
        }
    };
    let Some(llm) = val.as_table().and_then(|t| t.get("llm")) else {
        return Vec::new();
    };
    match Vec::<LLMConfig>::deserialize(llm.clone()) {
        Ok(entries) => entries,
        Err(e) => {
            warn_stderr(format!(
                "warning: failed to parse [[llm]] array in {}: {e}; treating its provider list as empty",
                path.display()
            ));
            Vec::new()
        }
    }
}

fn annotate(configs: Vec<LLMConfig>, source: Source) -> Vec<ListedProvider> {
    configs
        .into_iter()
        .map(|config| ListedProvider { config, source })
        .collect()
}

/// The resolved effective provider list — the same resolution the review
/// path uses: project `[[llm]]` → user-level fallback → `LLM_CONFIG` env.
pub fn resolved_providers() -> Result<Vec<ListedProvider>> {
    let project = read_llm_entries(&project_config_path()?)?;
    if !project.is_empty() {
        return Ok(annotate(project, Source::Project));
    }
    let user = read_llm_entries(&user_config_path()?)?;
    if !user.is_empty() {
        return Ok(annotate(user, Source::User));
    }
    let env = review_engine::config::llm_configs_from_env();
    if !env.is_empty() {
        return Ok(annotate(env, Source::Env));
    }
    Ok(Vec::new())
}

/// Resolve a single provider by name. With an explicit scope only that file
/// is searched; otherwise the chain project → user → env is searched in
/// order and the first match wins.
fn find_provider(name: &str, scope: Option<Scope>) -> Result<(LLMConfig, Source)> {
    match scope {
        Some(scope) => {
            let path = scope_path(scope)?;
            let source = scope.source();
            read_llm_entries(&path)?
                .into_iter()
                .find(|c| c.provider == name)
                .map(|c| (c, source))
                .ok_or_else(|| {
                    anyhow::anyhow!("provider \"{name}\" not found in {} {}", path.display(), source.label())
                })
        }
        None => {
            let project_path = project_config_path()?;
            if let Some(c) = read_llm_entries(&project_path)?
                .into_iter()
                .find(|c| c.provider == name)
            {
                return Ok((c, Source::Project));
            }
            let user_path = user_config_path()?;
            if let Some(c) = read_llm_entries(&user_path)?.into_iter().find(|c| c.provider == name) {
                return Ok((c, Source::User));
            }
            if let Some(c) = review_engine::config::llm_configs_from_env()
                .into_iter()
                .find(|c| c.provider == name)
            {
                return Ok((c, Source::Env));
            }
            anyhow::bail!(
                "provider \"{name}\" not found (searched {} [project], {} [user], and the LLM_CONFIG env)",
                project_path.display(),
                user_path.display(),
            )
        }
    }
}

// ─── Rendering ─────────────────────────────────────────────────────

fn dash_if_empty(s: &str) -> String {
    if s.is_empty() {
        "-".to_string()
    } else {
        s.to_string()
    }
}

/// The displayed API-key cell, selected by CONTROL FLOW on the key's
/// emptiness bit alone: `-` when no key is stored, the [`API_KEY_MASK`]
/// constant when one is. The raw key never enters any render/print path —
/// every string this produces is a compile-time constant, so no printed
/// output is secret-derived (CodeQL `rust/cleartext-logging`).
fn key_cell(key_empty: bool) -> String {
    if key_empty {
        "-".to_string()
    } else {
        API_KEY_MASK.to_string()
    }
}

/// Render the human-readable provider table (one line per entry, API keys
/// masked). An empty list renders a friendly hint pointing at `set`.
pub fn render_provider_table(entries: &[ListedProvider]) -> String {
    if entries.is_empty() {
        return "no providers configured\n\nadd one with:\n  reng config provider set <name> --model <model> --api-base <url> [--api-key <key>]".to_string();
    }
    let mut rows: Vec<[String; 5]> = Vec::with_capacity(entries.len() + 1);
    rows.push([
        "PROVIDER".to_string(),
        "MODEL".to_string(),
        "API_BASE".to_string(),
        "API_KEY".to_string(),
        "SOURCE".to_string(),
    ]);
    for e in entries {
        rows.push([
            e.config.provider.clone(),
            dash_if_empty(&e.config.model),
            dash_if_empty(&e.config.api_base),
            key_cell(e.config.api_key.is_empty()),
            e.source.label().to_string(),
        ]);
    }
    let mut widths = [0usize; 5];
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }
    let mut out = String::new();
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            if i + 1 < row.len() {
                out.push_str(cell);
                out.push_str(&" ".repeat(widths[i] - cell.len() + 2));
            } else {
                out.push_str(cell);
            }
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

// ─── Surgical TOML editing ─────────────────────────────────────────

/// Fields explicitly passed to `set`; `None` means "not given" — keep the
/// stored value on update, or fall back to the LLMConfig default for a new
/// entry. An empty `--api-key` is filtered to `None` by the dispatcher
/// (blank = keep, same semantic as the web UI).
#[derive(Debug, Default)]
pub struct ProviderPatch {
    pub model: Option<String>,
    pub api_base: Option<String>,
    pub api_key: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub disable_thinking: bool,
}

/// The effective values of an entry after a `set`, used for the
/// confirmation echo (kept separate from [`LLMConfig`] so a hand-written
/// entry missing optional keys still echoes cleanly).
///
/// Defense in depth against cleartext logging: the raw `api_key` never
/// leaves [`echo_of`] — the outcome carries only the key's emptiness bit,
/// and the confirmation print selects between compile-time-constant strings
/// (`-` / `***`) via [`key_cell`], so no printed output is secret-derived.
#[derive(Debug)]
pub struct SetOutcome {
    /// True when a new `[[llm]]` entry was appended.
    pub created: bool,
    pub model: String,
    /// True when the stored api_key is empty (drives the "created without a
    /// key" warning in `run_set` and the `-`/`***` display select).
    pub key_empty: bool,
    pub api_base: String,
    pub max_tokens: i64,
    pub temperature: f64,
}

/// Upsert the `[[llm]]` entry with `provider == name` in `path`, preserving
/// every other section, comment, and formatting choice byte-for-byte.
/// Creates the file (and parent directories, for `--global`) when missing.
pub fn upsert_provider(path: &Path, name: &str, patch: &ProviderPatch) -> Result<SetOutcome> {
    let content = if path.exists() {
        std::fs::read_to_string(path).with_context(|| format!("failed to read config file {}", path.display()))?
    } else {
        String::new()
    };
    let mut doc = content.parse::<toml_edit::DocumentMut>().with_context(|| {
        format!(
            "failed to parse {} as TOML; fix it manually before editing providers",
            path.display()
        )
    })?;
    let outcome = upsert_in_document(&mut doc, name, patch)?;
    write_config(path, &doc.to_string())?;
    Ok(outcome)
}

/// Remove the `[[llm]]` entry with `provider == name` from `path`. Errors
/// when the file or the entry does not exist.
pub fn remove_provider(path: &Path, name: &str) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("config file not found: {}", path.display());
    }
    let content =
        std::fs::read_to_string(path).with_context(|| format!("failed to read config file {}", path.display()))?;
    let mut doc = content.parse::<toml_edit::DocumentMut>().with_context(|| {
        format!(
            "failed to parse {} as TOML; fix it manually before editing providers",
            path.display()
        )
    })?;
    if !remove_from_document(&mut doc, name) {
        anyhow::bail!("provider \"{name}\" not found in {}", path.display());
    }
    write_config(path, &doc.to_string())?;
    Ok(())
}

/// Set `key` on `table`, preserving the original decor (surrounding
/// whitespace and any trailing comment) when the key already exists, so the
/// edit stays surgical. Missing keys are appended with default formatting.
fn set_table_value(table: &mut toml_edit::Table, key: &str, new_value: toml_edit::Value) {
    match table.get_mut(key).and_then(|item| item.as_value_mut()) {
        Some(existing) => {
            let mut new_value = new_value;
            *new_value.decor_mut() = existing.decor().clone();
            *existing = new_value;
        }
        None => {
            table[key] = toml_edit::Item::Value(new_value);
        }
    }
}

fn table_str(table: &toml_edit::Table, key: &str) -> String {
    table.get(key).and_then(|i| i.as_str()).unwrap_or_default().to_string()
}

fn table_i64(table: &toml_edit::Table, key: &str, default: i64) -> i64 {
    table.get(key).and_then(|i| i.as_integer()).unwrap_or(default)
}

fn table_f64(table: &toml_edit::Table, key: &str, default: f64) -> f64 {
    table.get(key).and_then(|i| i.as_float()).unwrap_or(default)
}

/// Build a TOML float from an `f32` through its shortest round-trip repr, so
/// `--temperature 0.7` lands in the file as `0.7` — not the f64-widened
/// `0.699999988079071`.
fn f32_value(v: f32) -> toml_edit::Value {
    match v.to_string().parse::<f64>() {
        Ok(shortest) => toml_edit::Value::from(shortest),
        Err(_) => toml_edit::Value::from(f64::from(v)),
    }
}

fn echo_of(table: &toml_edit::Table, created: bool) -> SetOutcome {
    let raw_key = table_str(table, "api_key");
    SetOutcome {
        created,
        model: table_str(table, "model"),
        // Keep only the emptiness bit and let the raw value drop at the end
        // of this function: `SetOutcome` carries nothing secret-derived.
        key_empty: raw_key.is_empty(),
        api_base: table_str(table, "api_base"),
        max_tokens: table_i64(table, "max_tokens", default_max_tokens() as i64),
        temperature: table_f64(table, "temperature", default_temperature() as f64),
    }
}

fn upsert_in_document(doc: &mut toml_edit::DocumentMut, name: &str, patch: &ProviderPatch) -> Result<SetOutcome> {
    use toml_edit::{ArrayOfTables, Item, Table, Value};

    let root = doc.as_table_mut();
    if !root.contains_key("llm") {
        root["llm"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    let entries = root["llm"].as_array_of_tables_mut().ok_or_else(|| {
        anyhow::anyhow!("`llm` is not an array of tables; expected [[llm]] entries — fix the file manually")
    })?;

    for table in entries.iter_mut() {
        if table.get("provider").and_then(|p| p.as_str()) == Some(name) {
            // Update only the explicitly passed fields; everything else —
            // including a stored api_key when --api-key was omitted — stays.
            if let Some(model) = &patch.model {
                set_table_value(table, "model", Value::from(model.as_str()));
            }
            if let Some(api_base) = &patch.api_base {
                set_table_value(table, "api_base", Value::from(api_base.as_str()));
            }
            if let Some(api_key) = &patch.api_key {
                set_table_value(table, "api_key", Value::from(api_key.as_str()));
            }
            if let Some(max_tokens) = patch.max_tokens {
                set_table_value(table, "max_tokens", Value::from(i64::from(max_tokens)));
            }
            if let Some(temperature) = patch.temperature {
                set_table_value(table, "temperature", f32_value(temperature));
            }
            if patch.disable_thinking {
                set_table_value(table, "disable_thinking", Value::from(true));
            }
            return Ok(echo_of(table, false));
        }
    }

    // New entry: omitted optionals fall back to the LLMConfig serde
    // defaults; an omitted api_key is stored as an empty string (the caller
    // prints the warning).
    let mut table = Table::new();
    table["provider"] = toml_edit::value(name);
    table["model"] = toml_edit::value(patch.model.as_deref().unwrap_or(""));
    table["api_key"] = toml_edit::value(patch.api_key.as_deref().unwrap_or(""));
    table["api_base"] = toml_edit::value(patch.api_base.as_deref().unwrap_or(""));
    table["max_tokens"] = toml_edit::value(i64::from(patch.max_tokens.unwrap_or_else(default_max_tokens)));
    table["temperature"] = toml_edit::Item::Value(f32_value(patch.temperature.unwrap_or_else(default_temperature)));
    if patch.disable_thinking {
        table["disable_thinking"] = toml_edit::value(true);
    }
    let outcome = echo_of(&table, true);
    entries.push(table);
    Ok(outcome)
}

fn remove_from_document(doc: &mut toml_edit::DocumentMut, name: &str) -> bool {
    let Some(entries) = doc
        .as_table_mut()
        .get_mut("llm")
        .and_then(|i| i.as_array_of_tables_mut())
    else {
        return false;
    };
    let Some(idx) = entries
        .iter()
        .position(|t| t.get("provider").and_then(|p| p.as_str()) == Some(name))
    else {
        return false;
    };
    entries.remove(idx);
    true
}

/// Write the config file, creating parent directories first (the `--global`
/// path may not exist yet).
fn write_config(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create config directory {}", parent.display()))?;
        }
    }
    std::fs::write(path, content).with_context(|| format!("failed to write config file {}", path.display()))?;
    // The file can hold plaintext API keys, so tighten it to owner-only —
    // on create (where umask would typically leave 0644) and on update of
    // an existing file that already contains key material. Windows: no-op.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set permissions on config file {}", path.display()))?;
    }
    Ok(())
}

// ─── Command handlers ──────────────────────────────────────────────

/// Dispatch entry point for `reng config provider <action>`.
pub async fn run(action: ProviderAction) -> Result<()> {
    match action {
        ProviderAction::List { global, project } => run_list(global, project),
        ProviderAction::Set {
            name,
            model,
            api_base,
            api_key,
            max_tokens,
            temperature,
            disable_thinking,
            global,
            project,
        } => {
            let patch = ProviderPatch {
                model,
                api_base,
                // Blank = keep (same semantic as the web UI): an explicitly
                // empty --api-key must not wipe the stored key.
                api_key: api_key.filter(|k| !k.is_empty()),
                max_tokens,
                temperature,
                disable_thinking,
            };
            run_set(&name, patch, global, project)
        }
        ProviderAction::Remove { name, global, project } => run_remove(&name, global, project),
        ProviderAction::Test { name, global, project } => run_test(&name, global, project).await,
    }
}

pub fn run_list(global: bool, project: bool) -> Result<()> {
    let entries = match scope_from_flags(global, project) {
        Some(scope) => annotate(read_llm_entries(&scope_path(scope)?)?, scope.source()),
        None => resolved_providers()?,
    };
    println!("{}", render_provider_table(&entries));
    Ok(())
}

pub fn run_set(name: &str, patch: ProviderPatch, global: bool, project: bool) -> Result<()> {
    let scope = scope_from_flags(global, project).unwrap_or(Scope::Project);
    let path = scope_path(scope)?;
    let outcome = upsert_provider(&path, name, &patch)?;
    if outcome.created && outcome.key_empty {
        eprintln!(
            "warning: no --api-key given; provider \"{name}\" was saved with an empty api_key — most providers require one"
        );
    }
    // The echo selects the displayed key from compile-time constants via
    // `key_cell(outcome.key_empty)` — the raw secret is never in scope here
    // (see `SetOutcome`).
    println!(
        "✓ {} provider \"{}\" in {} {}: model={} api_base={} api_key={} max_tokens={} temperature={}",
        if outcome.created { "created" } else { "updated" },
        name,
        path.display(),
        scope.source().label(),
        dash_if_empty(&outcome.model),
        dash_if_empty(&outcome.api_base),
        key_cell(outcome.key_empty),
        outcome.max_tokens,
        outcome.temperature,
    );
    Ok(())
}

pub fn run_remove(name: &str, global: bool, project: bool) -> Result<()> {
    let scope = scope_from_flags(global, project).unwrap_or(Scope::Project);
    let path = scope_path(scope)?;
    remove_provider(&path, name)?;
    println!(
        "✓ removed provider \"{name}\" from {} {}",
        path.display(),
        scope.source().label()
    );
    Ok(())
}

/// `Some(warning)` when the effective probe URL is cleartext `http://` to a
/// non-loopback host — the bearer key would cross the wire unencrypted.
/// Loopback targets (localhost, 127.0.0.0/8, ::1) stay quiet so local
/// providers like Ollama don't trigger false alarms.
pub(crate) fn cleartext_key_warning(resolved_base: &str) -> Option<String> {
    let rest = resolved_base.strip_prefix("http://")?;
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    // Bracketed IPv6 literals keep their colons inside the brackets.
    let host = match authority.strip_prefix('[') {
        Some(inner) => inner.split(']').next().unwrap_or(""),
        None => authority.split(':').next().unwrap_or(""),
    };
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".localhost")
        || host.starts_with("127.")
        || host == "::1";
    if loopback {
        None
    } else {
        Some(format!(
            "warning: api_base {resolved_base} uses cleartext HTTP on a non-localhost host — the API key will be sent unencrypted"
        ))
    }
}

pub async fn run_test(name: &str, global: bool, project: bool) -> Result<()> {
    let (cfg, source) = find_provider(name, scope_from_flags(global, project))?;
    // Resolve before probing: for an unknown provider with an empty
    // api_base this fails fast with the api_base-required error, before any
    // network call could leak the stored key.
    let resolved_base = review_engine::llm::probe::resolve_api_base(&cfg)?;
    if let Some(warning) = cleartext_key_warning(&resolved_base) {
        warn_stderr(warning);
    }
    let start = std::time::Instant::now();
    match review_engine::llm::probe::probe_llm_connectivity(&cfg).await {
        Ok(outcome) => {
            println!(
                "✓ provider \"{name}\" {} is reachable via {} ({} ms)",
                source.label(),
                outcome.resolved_base,
                start.elapsed().as_millis()
            );
            Ok(())
        }
        Err(e) => Err(e).with_context(|| {
            format!(
                "provider \"{name}\" {} connectivity test failed (via {resolved_base})",
                source.label()
            )
        }),
    }
}
