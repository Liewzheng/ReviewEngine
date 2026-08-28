use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    about = "Rust driven Code Review Engine",
    disable_version_flag = true,
    subcommand_required = false
)]
pub struct Cli {
    /// Show version
    #[arg(short = 'V', long = "version")]
    pub version: bool,

    /// Show progress bar
    #[arg(long, global = true)]
    pub progress: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run a single review
    Review {
        /// Merge request URL
        #[arg(long)]
        mr_url: Option<String>,

        /// Path to local diff file
        #[arg(long)]
        diff: Option<String>,

        /// Read request JSON from stdin
        #[arg(long)]
        stdin: bool,

        /// Path to local git repository
        #[arg(long)]
        local_path: Option<String>,

        /// Directory (relative to --local-path) to review in full (every controlled file, not just a diff)
        ///
        /// 按目录全量审查：把该目录下所有受控文件的当前内容当作新增代码，逐一完整审查，
        /// 复用 review 专家团队与 large-PR 覆盖保证。需配合 --local-path 指定仓库根目录；
        /// 与 --mr-url / --diff / --stdin / --base / --head / --since / --until / --staged 互斥。
        #[arg(
            long,
            requires = "local_path",
            conflicts_with_all = ["mr_url", "diff", "stdin", "base", "head", "since", "until", "staged"]
        )]
        path: Option<String>,

        /// Base ref for local diff (default: main)
        #[arg(long)]
        base: Option<String>,

        /// Head ref for local diff
        #[arg(long)]
        head: Option<String>,

        /// Review staged changes
        #[arg(long)]
        staged: bool,

        /// Since commit range
        #[arg(long)]
        since: Option<String>,

        /// Until commit range
        #[arg(long)]
        until: Option<String>,

        /// Path to .code-audit-config.toml config
        #[arg(long)]
        config: Option<String>,

        /// GitLab personal access token
        #[arg(long)]
        gitlab_token: Option<String>,

        /// GitHub personal access token
        #[arg(long)]
        github_token: Option<String>,

        /// LLM config JSON (can be repeated)
        #[arg(long, name = "llm-config")]
        llm_config: Vec<String>,

        /// Output format
        #[arg(long, default_value = "json")]
        format: String,

        /// Output file (default: stdout)
        #[arg(long)]
        output: Option<String>,

        /// Dump each expert's raw LLM prompt and response to `<output>.raw/`
        /// (or `<output_dir>/review-raw/`) for debugging zero-finding or
        /// mis-parsed reviews. File paths are printed to stderr and referenced
        /// in the report.
        #[arg(long)]
        verbose: bool,

        /// Publish results back to the MR/PR discussion
        #[arg(long)]
        publish: bool,
    },

    /// Validate a .code-audit-config.toml file
    Validate {
        /// Path to config file
        #[arg(long)]
        config: Option<String>,
    },

    /// Print the default config
    Default,

    /// Start the health check and webhook server
    Serve {
        /// Port to listen on
        #[arg(long, default_value = "8080")]
        port: u16,

        /// Bind address (127.0.0.1 for local only, 0.0.0.0 for network)
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,

        /// API token for authentication (required when bind != 127.0.0.1)
        #[arg(long)]
        api_token: Option<String>,

        /// One-time bootstrap key for first-run setup: lets a non-loopback
        /// bind start without an API token and accept the FIRST token via the
        /// web UI (`PUT /api/v1/system/token` with `X-Bootstrap-Key`).
        /// Env: REVIEW_BOOTSTRAP_KEY.
        #[arg(long)]
        bootstrap_key: Option<String>,

        /// GitHub personal access token
        #[arg(long)]
        github_token: Option<String>,

        /// GitHub webhook secret
        #[arg(long)]
        github_webhook_secret: Option<String>,

        /// GitLab personal access token
        #[arg(long)]
        gitlab_token: Option<String>,

        /// GitLab webhook secret (legacy X-Gitlab-Token)
        #[arg(long)]
        gitlab_webhook_secret: Option<String>,

        /// GitLab webhook signing secret (HMAC-SHA256 body signature, GitLab 19.0+)
        #[arg(long)]
        gitlab_webhook_signing_secret: Option<String>,

        /// PEM certificate chain path for TLS (HTTPS); requires --tls-key.
        /// When set, the server also listens on --tls-port with HTTPS.
        #[arg(long, requires = "tls_key")]
        tls_cert: Option<String>,

        /// PEM private key path for TLS (HTTPS); requires --tls-cert.
        #[arg(long, requires = "tls_cert")]
        tls_key: Option<String>,

        /// TLS (HTTPS) listen port; used only when --tls-cert/--tls-key are set
        #[arg(long, default_value = "8443")]
        tls_port: u16,
    },

    /// Generate a random API token
    GenerateToken,

    /// Interactive project initialization.
    ///
    /// Scans the current directory, detects project language / CI / test
    /// framework, then prompts the user to choose commands, experts, and
    /// LLM settings before writing a `.code-audit-config.toml`.
    Init {
        /// Skip interactive prompts and print the built-in default config.
        #[arg(long)]
        default: bool,
    },

    /// Generate code improvement suggestions for an MR
    Improve {
        /// Merge request URL
        #[arg(long)]
        mr_url: Option<String>,

        /// Path to local git repository
        #[arg(long)]
        local_path: Option<String>,

        /// Path to local diff file
        #[arg(long)]
        diff: Option<String>,

        /// Review staged changes
        #[arg(long)]
        staged: bool,

        /// Base ref for local diff
        #[arg(long)]
        base: Option<String>,

        /// Head ref for local diff
        #[arg(long)]
        head: Option<String>,

        /// Since commit range
        #[arg(long)]
        since: Option<String>,

        /// Until commit range
        #[arg(long)]
        until: Option<String>,

        /// Path to .code-audit-config.toml config
        #[arg(long)]
        config: Option<String>,

        /// GitLab personal access token
        #[arg(long)]
        gitlab_token: Option<String>,

        /// GitHub personal access token
        #[arg(long)]
        github_token: Option<String>,

        /// LLM config JSON (can be repeated)
        #[arg(long, name = "llm-config")]
        llm_config: Vec<String>,

        /// Output format
        #[arg(long, default_value = "json")]
        format: String,

        /// Output file (default: stdout)
        #[arg(long)]
        output: Option<String>,

        /// Publish results back to the MR/PR discussion
        #[arg(long)]
        publish: bool,
    },

    /// Generate a PR description / summary for an MR
    Describe {
        /// Merge request URL
        #[arg(long)]
        mr_url: Option<String>,

        /// Path to local git repository
        #[arg(long)]
        local_path: Option<String>,

        /// Path to local diff file
        #[arg(long)]
        diff: Option<String>,

        /// Review staged changes
        #[arg(long)]
        staged: bool,

        /// Base ref for local diff
        #[arg(long)]
        base: Option<String>,

        /// Head ref for local diff
        #[arg(long)]
        head: Option<String>,

        /// Since commit range
        #[arg(long)]
        since: Option<String>,

        /// Until commit range
        #[arg(long)]
        until: Option<String>,

        /// Path to .code-audit-config.toml config
        #[arg(long)]
        config: Option<String>,

        /// GitLab personal access token
        #[arg(long)]
        gitlab_token: Option<String>,

        /// GitHub personal access token
        #[arg(long)]
        github_token: Option<String>,

        /// LLM config JSON (can be repeated)
        #[arg(long, name = "llm-config")]
        llm_config: Vec<String>,

        /// Output format
        #[arg(long, default_value = "json")]
        format: String,

        /// Output file (default: stdout)
        #[arg(long)]
        output: Option<String>,

        /// Publish results back to the MR/PR discussion
        #[arg(long)]
        publish: bool,
    },

    /// Ask a question about the code changes
    Ask {
        /// Question to ask
        #[arg(long)]
        question: Option<String>,

        /// Merge request URL
        #[arg(long)]
        mr_url: Option<String>,

        /// Path to local git repository
        #[arg(long)]
        local_path: Option<String>,

        /// Path to local diff file
        #[arg(long)]
        diff: Option<String>,

        /// Read diff from stdin
        #[arg(long)]
        stdin: bool,

        /// Path to .code-audit-config.toml config
        #[arg(long)]
        config: Option<String>,

        /// GitLab personal access token
        #[arg(long)]
        gitlab_token: Option<String>,

        /// GitHub personal access token
        #[arg(long)]
        github_token: Option<String>,

        /// LLM config JSON (can be repeated)
        #[arg(long, name = "llm-config")]
        llm_config: Vec<String>,

        /// Output format
        #[arg(long, default_value = "json")]
        format: String,

        /// Output file (default: stdout)
        #[arg(long)]
        output: Option<String>,
    },

    /// Update CHANGELOG from commit history
    UpdateChangelog {
        /// Path to local git repository
        #[arg(long)]
        local_path: Option<String>,

        /// Since commit range
        #[arg(long)]
        since: Option<String>,

        /// Until commit range
        #[arg(long)]
        until: Option<String>,

        /// Path to .code-audit-config.toml config
        #[arg(long)]
        config: Option<String>,

        /// LLM config JSON (can be repeated)
        #[arg(long, name = "llm-config")]
        llm_config: Vec<String>,

        /// Output format
        #[arg(long, default_value = "json")]
        format: String,

        /// Output file (default: stdout)
        #[arg(long)]
        output: Option<String>,
    },

    /// Run a full repository health review
    #[command(visible_alias = "audit")]
    RepoReview {
        /// Path to local git repository
        #[arg(long)]
        local_path: Option<String>,

        /// Path to .code-audit-config.toml config
        #[arg(long)]
        config: Option<String>,

        /// LLM config JSON (can be repeated). When provided, the repo review
        /// is enhanced with LLM analysis. Otherwise runs local-only analysis.
        #[arg(long, name = "llm-config")]
        llm_config: Vec<String>,

        /// Output format (markdown, json)
        #[arg(long, default_value = "markdown")]
        format: String,

        /// Output file (default: stdout)
        #[arg(long)]
        output: Option<String>,
    },

    /// Check for and apply updates to review-engine itself
    Upgrade {
        /// Check only: report the latest version without downloading or installing
        #[arg(long)]
        check: bool,

        /// Non-interactive: apply the update without prompting for confirmation
        #[arg(long)]
        yes: bool,

        /// Target version to upgrade to (e.g. 0.9.0); only the latest release is auto-installable
        #[arg(long, value_name = "TAG")]
        version: Option<String>,

        /// Roll back to the previous binary (review-engine.bak)
        #[arg(long)]
        rollback: bool,
    },

    /// View and edit review-engine configuration (git-config-like)
    Config {
        #[command(subcommand)]
        noun: ConfigNoun,
    },
}

/// Top-level nouns for `reng config`.
#[derive(Subcommand, Debug)]
pub enum ConfigNoun {
    /// Manage LLM providers (the `[[llm]]` entries) in config files
    Provider {
        #[command(subcommand)]
        action: ProviderAction,
    },
}

/// Actions for `reng config provider`.
///
/// Scope semantics mirror `git config`: the default scope for `set`/`remove`
/// is the project file `.code-audit-config.toml` in the current directory;
/// `--global` targets the user-level file
/// `~/.config/review-engine/.code-audit-config.toml`; `--project` selects the
/// project file explicitly (useful in scripts) and conflicts with `--global`.
#[derive(Subcommand, Debug)]
pub enum ProviderAction {
    /// List configured providers.
    ///
    /// Without a scope flag, shows the RESOLVED effective provider list —
    /// the same resolution the review path uses (project `[[llm]]` →
    /// user-level fallback → `LLM_CONFIG` env) — with each entry annotated
    /// by source (`[project]` / `[user]` / `[env]`). With `--global` or
    /// `--project`, shows only that file's raw `[[llm]]` entries. API keys
    /// are always masked.
    List {
        /// Show only the user-level config file
        #[arg(long, conflicts_with = "project")]
        global: bool,

        /// Show only the project config file (.code-audit-config.toml in cwd)
        #[arg(long)]
        project: bool,
    },

    /// Add a new provider or update an existing one (matched by name).
    ///
    /// Only the fields explicitly passed are changed; an omitted `--api-key`
    /// KEEPS the stored key (blank = keep, same semantic as the web UI). For
    /// a new entry, omitted options fall back to the LLMConfig defaults and
    /// an omitted `--api-key` is stored as an empty string with a warning.
    Set {
        /// Provider name (e.g. "openai", "anthropic", "ollama", "deepseek")
        name: String,

        /// Model identifier (e.g. "gpt-4o", "claude-3-opus")
        #[arg(long)]
        model: Option<String>,

        /// Base URL for the provider API (e.g. "https://api.openai.com/v1")
        #[arg(long)]
        api_base: Option<String>,

        /// API key / authentication token (omit to keep the stored key)
        #[arg(long)]
        api_key: Option<String>,

        /// Maximum number of tokens in the LLM response
        #[arg(long)]
        max_tokens: Option<u32>,

        /// Sampling temperature (0.0–1.0; lower = more deterministic)
        #[arg(long)]
        temperature: Option<f32>,

        /// Disable chain-of-thought reasoning (sends "thinking": {"type": "disabled"})
        #[arg(long)]
        disable_thinking: bool,

        /// Write to the user-level config file (~/.config/review-engine)
        #[arg(long, conflicts_with = "project")]
        global: bool,

        /// Write to the project config file explicitly (this is the default)
        #[arg(long)]
        project: bool,
    },

    /// Remove a provider entry (matched by name) from the chosen scope file
    Remove {
        /// Provider name to remove
        name: String,

        /// Remove from the user-level config file (~/.config/review-engine)
        #[arg(long, conflicts_with = "project")]
        global: bool,

        /// Remove from the project config file explicitly (this is the default)
        #[arg(long)]
        project: bool,
    },

    /// Probe a provider's connectivity with its stored API key.
    ///
    /// Without a scope flag the entry is resolved through the chain
    /// project → user → env; with `--global`/`--project` only that file is
    /// searched. Prints the probe latency on success; exits non-zero on
    /// failure so scripts can rely on it.
    Test {
        /// Provider name to test
        name: String,

        /// Test the entry from the user-level config file only
        #[arg(long, conflicts_with = "project")]
        global: bool,

        /// Test the entry from the project config file only
        #[arg(long)]
        project: bool,
    },
}
