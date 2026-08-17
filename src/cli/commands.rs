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
}
