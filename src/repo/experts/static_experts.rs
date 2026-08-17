//! Static (non-LLM) repo experts: code organization, security, documentation,
//! dependency health and code style. Each expert lives in its own submodule;
//! this facade re-exports them so `repo::experts::static_experts::*` paths in
//! callers stay unchanged.

mod code_organization;
mod code_style;
mod dependency;
mod documentation;
mod security;

pub use code_organization::CodeOrganization;
pub use code_style::CodeStyle;
pub use dependency::Dependency;
pub use documentation::Documentation;
pub use security::Security;

// ─── CodeStyle ────────────────────────────────

/// Normalise a style-config file name to a per-tool key, so that aliases
/// configuring the same tool collapse into one check: `rustfmt.toml` and
/// `.rustfmt.toml` both map to `rustfmt`, `.eslintrc` and `.eslintrc.json`
/// to `eslint`. Leading dots are stripped, then the part before the first
/// remaining dot is taken, lower-cased; finally the legacy `eslintrc*` and
/// `prettierrc*` families are folded onto their modern flat-config key
/// (`eslint`, `prettier`) so a repo shipping only `eslint.config.js` is
/// recognised as configuring the same tool as one shipping `.eslintrc`.
/// Kept on the facade so the shared test module can reach it directly.
fn style_tool_key(config_file: &str) -> String {
    let key = config_file
        .trim_start_matches('.')
        .split('.')
        .next()
        .unwrap_or(config_file)
        .to_ascii_lowercase();
    match key.as_str() {
        "eslintrc" => "eslint".to_string(),
        "prettierrc" => "prettier".to_string(),
        _ => key,
    }
}

#[cfg(test)]
mod tests;
