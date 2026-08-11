use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use review_engine::models::*;
use review_engine::progress::{new_progress_map, ProgressMap, ProgressStatus};
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "cli")]
pub mod handlers;

#[derive(Parser)]
#[command(
    about = "Rust driven Code Review Engine",
    disable_version_flag = true,
    subcommand_required = false
)]
struct Cli {
    /// Show version
    #[arg(short = 'V', long = "version")]
    version: bool,

    /// Show progress bar
    #[arg(long, global = true)]
    progress: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
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

/// Build the clap command whose displayed program name is derived from
/// argv[0]'s basename, so symlinked invocations (e.g. `reng`) show their own
/// name in --help / usage output instead of the hardcoded `review-engine`.
fn cli_command() -> clap::Command {
    let bin_name = std::env::args_os()
        .next()
        .as_deref()
        .and_then(|arg| std::path::Path::new(arg).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| Cli::command().get_name().to_string());
    Cli::command().bin_name(bin_name)
}

/// Parse CLI args from the environment, applying the argv[0]-derived program
/// name. Equivalent to `Cli::parse()` except the displayed name is dynamic.
fn parse_cli() -> Cli {
    let matches = cli_command().get_matches();
    Cli::from_arg_matches(&matches).unwrap_or_else(|err| err.exit())
}

pub async fn run() -> Result<()> {
    let cli = parse_cli();

    if cli.version {
        println!("Review Engine v{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let progress_map: ProgressMap = new_progress_map();

    let cmd = cli.command.unwrap_or_else(|| {
        let mut out = std::io::stdout();
        writeln!(out, "{}", cli_command().render_help()).ok();
        std::process::exit(0);
    });

    match cmd {
        Commands::Review {
            path: Some(dir),
            local_path: Some(repo),
            config,
            llm_config,
            format,
            output,
            ..
        } => {
            let (pm, review_id) = spawn_progress_if_needed(&progress_map, cli.progress);
            handlers::run_local_path(&dir, &repo, config, llm_config, &format, &output, pm, &review_id).await?;
        }
        Commands::Review { path: Some(_), .. } => {
            anyhow::bail!("review --path requires --local-path <repo>");
        }
        Commands::Review {
            mr_url: Some(url),
            config,
            gitlab_token,
            github_token,
            llm_config,
            format,
            output,
            publish,
            ..
        } => {
            let (pm, review_id) = spawn_progress_if_needed(&progress_map, cli.progress);
            handlers::run_mr(
                &url,
                config,
                gitlab_token,
                github_token,
                llm_config,
                &format,
                &output,
                publish,
                pm,
                &review_id,
            )
            .await?;
        }
        Commands::Review {
            diff: Some(diff_path),
            local_path,
            config,
            llm_config,
            format,
            output,
            ..
        } => {
            let (pm, review_id) = spawn_progress_if_needed(&progress_map, cli.progress);
            handlers::run_local(
                &diff_path,
                local_path.as_deref(),
                config,
                llm_config,
                &format,
                &output,
                pm,
                &review_id,
            )
            .await?;
        }
        Commands::Review {
            local_path: Some(path),
            base,
            head,
            staged,
            since,
            until,
            config,
            format,
            output,
            llm_config,
            ..
        } => {
            let (pm, review_id) = spawn_progress_if_needed(&progress_map, cli.progress);
            handlers::run_local_repo(
                &path,
                base.as_deref(),
                head.as_deref(),
                staged,
                since.as_deref(),
                until.as_deref(),
                config,
                llm_config,
                &format,
                &output,
                pm,
                &review_id,
            )
            .await?;
        }
        Commands::Review {
            stdin: true,
            format,
            output,
            ..
        } => {
            handlers::run_stdin(&format, &output).await?;
        }
        Commands::Review { .. } => {
            anyhow::bail!("Please specify --mr-url, --diff, --stdin, --local-path, or --path");
        }
        Commands::Validate { config } => {
            let config = match config {
                Some(path) => path,
                None => {
                    let candidates = [
                        std::env::current_dir().ok().map(|p| p.join(".code-audit-config.toml")),
                        home::home_dir()
                            .map(|p| p.join(".config").join("review-engine").join(".code-audit-config.toml")),
                    ];
                    candidates
                        .into_iter()
                        .flatten()
                        .find(|p| p.exists())
                        .ok_or_else(|| {
                            anyhow::anyhow!("No config file found. Use --config or run review-engine init.")
                        })?
                        .to_string_lossy()
                        .to_string()
                }
            };
            let content = tokio::fs::read_to_string(&config).await?;
            let parsed = review_engine::config::load_and_apply(&content)?;
            println!("✓ Valid config: {} experts defined", parsed.review_experts.len());
        }
        Commands::Default => {
            let default = review_engine::config::default_config()?;
            println!("{}", toml::to_string_pretty(&default)?);
        }
        Commands::Serve {
            port,
            bind,
            api_token,
            bootstrap_key,
            github_token,
            github_webhook_secret,
            gitlab_token,
            gitlab_webhook_secret,
            gitlab_webhook_signing_secret,
            tls_cert,
            tls_key,
            tls_port,
        } => {
            // clap's `requires` already enforces that --tls-cert and --tls-key
            // come as a pair; the fall-through arm is defense-in-depth in case
            // the constraint ever changes, so a half-configured TLS request
            // fails loudly instead of silently serving plain HTTP.
            let tls = match (tls_cert, tls_key) {
                (Some(cert), Some(key)) => Some(review_engine::server::TlsConfig::new(
                    std::path::PathBuf::from(cert),
                    std::path::PathBuf::from(key),
                    tls_port,
                )),
                (None, None) => None,
                _ => {
                    return Err(anyhow::anyhow!("--tls-cert and --tls-key must be provided together"));
                }
            };
            // Resolve API token precedence: CLI arg > env var > persisted auth
            // file (loaded inside `AuthConfig::resolve`). `None` on a loopback
            // bind enters first-run bootstrap mode; a non-loopback bind requires
            // either a token or a one-time bootstrap key.
            let explicit_token = api_token.or_else(|| std::env::var("REVIEW_API_TOKEN").ok());
            let bootstrap_key = bootstrap_key.or_else(|| std::env::var("REVIEW_BOOTSTRAP_KEY").ok());
            let auth = Arc::new(review_engine::server::auth::AuthConfig::resolve(
                explicit_token,
                &bind,
                None,
                bootstrap_key,
            )?);

            let mut config = review_engine::config::resolve_config(None).await?;
            // LLM_CONFIG env is a fallback for the provider list only: a
            // non-empty [[llm]] from config files always wins (same
            // precedence as webhook-triggered reviews).
            review_engine::config::apply_llm_env_fallback(&mut config);
            let mut app_state = review_engine::server::AppState::new(config.llm.clone());
            app_state.task_store = Some(Arc::new(review_engine::server::task_queue::TaskStore::new()));
            app_state.app_config = std::sync::RwLock::new(Some(Arc::new(config.clone())));
            app_state.registry = Some(review_engine::metrics::REGISTRY.clone());
            app_state.progress_map = Some(progress_map.clone());
            app_state.log_collector = Some(
                review_engine::server::log_collector::get_global_collector()
                    .unwrap_or_else(review_engine::server::log_collector::init_global_collector),
            );
            app_state.feedback_store = Some(Arc::new(review_engine::feedback::FeedbackStore::persistent()));
            app_state.ui_config =
                std::sync::RwLock::new(review_engine::server::api::config::UiConfig::from_app_config(&config));
            let state = Arc::new(app_state);
            let dispatcher = review_engine::server::dispatcher::MrDispatcher::persistent();
            let mut handlers: Vec<Arc<dyn review_engine::server::webhook::WebhookHandler>> = vec![];
            let gitlab_token = gitlab_token
                .or_else(|| std::env::var("GITLAB_TOKEN").ok())
                .unwrap_or_default();
            let signing_secret = gitlab_webhook_signing_secret
                .or_else(|| std::env::var("GITLAB_WEBHOOK_SIGNING_SECRET").ok())
                .or_else(|| {
                    let ui = match state.ui_config.read() {
                        Ok(guard) => guard,
                        Err(_) => {
                            tracing::warn!("UI config lock is poisoned; skipping UI signing secret");
                            return None;
                        }
                    };
                    Some(ui.gitlab.webhook_signing_secret.clone()).filter(|s| !s.is_empty())
                });
            let webhook_secret = gitlab_webhook_secret
                .or_else(|| std::env::var("GITLAB_WEBHOOK_SECRET").ok())
                .unwrap_or_default();
            if !webhook_secret.is_empty() || signing_secret.is_some() {
                handlers.push(Arc::new(review_engine::server::gitlab::GitLabWebhookHandler::new(
                    webhook_secret.clone(),
                    signing_secret.clone(),
                    dispatcher.clone(),
                    gitlab_token.clone(),
                )));
                // Initialise the global GitLab runtime config so UI changes
                // propagate to the running handler without restart.
                review_engine::server::gitlab::init_gitlab_runtime(
                    &review_engine::server::gitlab::GitLabWebhookHandler::new(
                        webhook_secret,
                        signing_secret,
                        dispatcher.clone(),
                        gitlab_token,
                    ),
                );
            }
            if let Some((tok, secret)) = github_token
                .or_else(|| std::env::var("GITHUB_TOKEN").ok())
                .and_then(|tok| {
                    let secret = github_webhook_secret.or_else(|| std::env::var("GITHUB_WEBHOOK_SECRET").ok())?;
                    if secret.is_empty() {
                        tracing::warn!("GITHUB_WEBHOOK_SECRET is empty — webhook will reject all requests");
                        return None;
                    }
                    Some((tok, secret))
                })
            {
                handlers.push(Arc::new(review_engine::server::github::GitHubWebhookHandler::new(
                    secret,
                    dispatcher.clone(),
                    tok,
                )));
            }

            // Config file watching for hot-reload (server only). Spawned last,
            // right before serving: the watcher parks a `spawn_blocking` task
            // on a never-ready `mpsc::recv`, which prevents the tokio runtime
            // from ever dropping — so it must only exist once every fallible
            // startup step above has already succeeded.
            let config_candidates = [
                std::env::current_dir().ok().map(|p| p.join(".code-audit-config.toml")),
                home::home_dir().map(|p| p.join(".config").join("review-engine").join(".code-audit-config.toml")),
            ];
            for candidate in config_candidates.into_iter().flatten() {
                if candidate.exists() {
                    let path = candidate;
                    tokio::spawn(async move {
                        handlers::watch_config_file(path).await;
                    });
                }
            }

            // Fail fast on startup errors (e.g. port already in use):
            // report on stderr and exit non-zero via `process::exit`, which
            // skips the tokio runtime teardown. That teardown would
            // otherwise block forever waiting for the config-file watcher's
            // spawn_blocking task (parked on a never-ready `mpsc::recv`),
            // turning a clear bind error into a silent hang with no output.
            if let Err(e) = review_engine::server::serve(port, &bind, tls, state, auth, handlers).await {
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        }
        Commands::GenerateToken => {
            let token = review_engine::server::auth::generate_token();
            println!("{}", token);
        }
        Commands::Init { default } => {
            if default {
                review_engine::actions::init::run_default()?;
            } else {
                review_engine::actions::init::run_interactive(".")?;
            }
        }
        Commands::Improve {
            mr_url: Some(url),
            config,
            gitlab_token,
            github_token,
            llm_config,
            format,
            output,
            publish,
            ..
        } => {
            handlers::run_improve(
                &url,
                config,
                gitlab_token,
                github_token,
                llm_config,
                &format,
                &output,
                publish,
            )
            .await?;
        }
        Commands::Improve {
            diff: Some(diff_path),
            config,
            llm_config,
            format,
            output,
            ..
        } => {
            handlers::run_improve_local_diff(&diff_path, config, llm_config, &format, &output).await?;
        }
        Commands::Improve {
            local_path: Some(path),
            base,
            head,
            staged,
            since,
            until,
            config,
            llm_config,
            format,
            output,
            ..
        } => {
            handlers::run_improve_local_repo(
                &path,
                base.as_deref(),
                head.as_deref(),
                staged,
                since.as_deref(),
                until.as_deref(),
                config,
                llm_config,
                &format,
                &output,
            )
            .await?;
        }
        Commands::Improve { .. } => {
            anyhow::bail!("Please specify --mr-url, --diff, or --local-path");
        }
        Commands::Describe {
            mr_url: Some(url),
            config,
            gitlab_token,
            github_token,
            llm_config,
            format,
            output,
            publish,
            ..
        } => {
            handlers::run_describe(
                &url,
                config,
                gitlab_token,
                github_token,
                llm_config,
                &format,
                &output,
                publish,
            )
            .await?;
        }
        Commands::Describe {
            diff: Some(diff_path),
            config,
            llm_config,
            format,
            output,
            ..
        } => {
            handlers::run_describe_local_diff(&diff_path, config, llm_config, &format, &output).await?;
        }
        Commands::Describe {
            local_path: Some(path),
            base,
            head,
            staged,
            since,
            until,
            config,
            llm_config,
            format,
            output,
            ..
        } => {
            handlers::run_describe_local_repo(
                &path,
                base.as_deref(),
                head.as_deref(),
                staged,
                since.as_deref(),
                until.as_deref(),
                config,
                llm_config,
                &format,
                &output,
            )
            .await?;
        }
        Commands::Describe { .. } => {
            anyhow::bail!("Please specify --mr-url, --diff, or --local-path");
        }
        Commands::Ask {
            question,
            mr_url: Some(url),
            config,
            gitlab_token,
            github_token,
            llm_config,
            format,
            output,
            ..
        } => {
            let q = question.unwrap_or_default();
            handlers::run_ask(
                &q,
                &url,
                config,
                gitlab_token,
                github_token,
                llm_config,
                &format,
                &output,
            )
            .await?;
        }
        Commands::Ask {
            question,
            diff: Some(diff_path),
            config,
            llm_config,
            format,
            output,
            ..
        } => {
            let q = question.unwrap_or_default();
            handlers::run_ask_local_diff(&q, &diff_path, config, llm_config, &format, &output).await?;
        }
        Commands::Ask {
            question,
            local_path: Some(path),
            config,
            llm_config,
            format,
            output,
            ..
        } => {
            let q = question.unwrap_or_default();
            handlers::run_ask_local_repo(
                &q, &path, None, None, false, None, None, config, llm_config, &format, &output,
            )
            .await?;
        }
        Commands::Ask {
            question,
            stdin: true,
            config,
            llm_config,
            format,
            output,
            ..
        } => {
            let q = question.unwrap_or_default();
            handlers::run_ask_stdin(&q, config, llm_config, &format, &output).await?;
        }
        Commands::Ask { .. } => {
            anyhow::bail!("Please specify --mr-url, --diff, --local-path, or --stdin");
        }
        Commands::UpdateChangelog {
            local_path: Some(path),
            since,
            until,
            config,
            llm_config,
            format,
            output,
            ..
        } => {
            handlers::run_update_changelog(
                &path,
                since.as_deref(),
                until.as_deref(),
                config,
                llm_config,
                &format,
                &output,
            )
            .await?;
        }
        Commands::UpdateChangelog { .. } => {
            anyhow::bail!("Please specify --local-path");
        }
        Commands::RepoReview {
            local_path: Some(path),
            config: config_path,
            llm_config,
            format,
            output,
        } => {
            let (pm, review_id) = spawn_progress_if_needed(&progress_map, cli.progress);

            // Resolve LLM config
            let config_source = config_path.clone().map(ConfigSource::Path);
            let config = review_engine::config::resolve_config(config_source).await?;
            let llm_configs = handlers::resolve_llm_configs(&llm_config, &config)?;

            let has_llm = !llm_configs.is_empty() || std::env::var("LLM_CONFIG").is_ok() || !config.llm.is_empty();
            let llm_configs = if has_llm { llm_configs } else { Vec::new() };

            handlers::run_repo_review_local_or_enhanced(&path, &llm_configs, &format, &output, pm, &review_id, &config)
                .await?;
        }
        Commands::RepoReview { .. } => {
            anyhow::bail!("Please specify --local-path");
        }
        Commands::Upgrade {
            check,
            yes,
            version,
            rollback,
        } => {
            handlers::run_upgrade(check, yes, version.as_deref(), rollback).await?;
        }
    }

    // Give the progress bar display task one last polling cycle (500 ms) so
    // it can render the final "100%" state before the runtime shuts down and
    // cancels all spawned tasks.
    if cli.progress {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    }

    Ok(())
}

/// Spawn the progress bar display task if `--progress` is enabled.
/// Returns `(progress_map_for_callee, review_id)`.
fn spawn_progress_if_needed(progress_map: &ProgressMap, cli_progress: bool) -> (Option<ProgressMap>, String) {
    let review_id = uuid::Uuid::new_v4().to_string();
    let pm = if cli_progress { Some(progress_map.clone()) } else { None };
    if cli_progress {
        let pm_display = progress_map.clone();
        let rid_display = review_id.clone();
        // Use a background task.  The runtime keeps it alive until `run()`
        // returns; the sleep at the end of `run()` gives it one last
        // polling cycle so it can render the final "100%" state.
        tokio::spawn(async move {
            display_progress_bar(pm_display, rid_display).await;
        });
    }
    (pm, review_id)
}

/// Display a progress bar in the terminal by polling the progress map.
async fn display_progress_bar(map: ProgressMap, review_id: String) {
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let snapshot = {
            let Ok(map) = map.read() else { break };
            map.get(&review_id).cloned()
        };
        if let Some(p) = snapshot {
            let bar_width: usize = 20;
            let filled = (p.overall_percent / 100.0 * bar_width as f64) as usize;
            let bar: String = "▓".repeat(filled) + &"░".repeat(bar_width.saturating_sub(filled));
            let current_stage = p
                .stages
                .iter()
                .find(|s| s.status == ProgressStatus::Running)
                .map(|s| format!("{}: {}", s.label, s.detail))
                .unwrap_or_default();
            // Pad to 80 chars to clear terminal residuals from previous line
            print!("\r[{}] {:.0}%  {:<80}", bar, p.overall_percent, current_stage);
            std::io::stdout().flush().ok();
            if p.status != ProgressStatus::Running {
                println!();
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Root cause C: the `review --diff` branch must thread `--local-path`
    /// through instead of dropping it in the `..` catch-all, otherwise the
    /// full-file contents injection reads from a non-existent "local" path.
    #[test]
    fn test_review_diff_branch_captures_local_path() {
        let matches = cli_command().get_matches_from([
            "review-engine",
            "review",
            "--diff",
            "/tmp/x.diff",
            "--local-path",
            "/repo",
            "--format",
            "json",
        ]);
        let cli = match Cli::from_arg_matches(&matches) {
            Ok(cli) => cli,
            Err(e) => panic!("cli args should parse: {e}"),
        };
        match cli.command {
            Some(Commands::Review { diff, local_path, .. }) => {
                assert_eq!(diff.as_deref(), Some("/tmp/x.diff"));
                assert_eq!(local_path.as_deref(), Some("/repo"));
            }
            other => panic!("expected Review command, got {other:?}"),
        }
    }

    #[test]
    fn test_review_diff_branch_local_path_optional() {
        let matches = cli_command().get_matches_from(["review-engine", "review", "--diff", "/tmp/x.diff"]);
        let cli = match Cli::from_arg_matches(&matches) {
            Ok(cli) => cli,
            Err(e) => panic!("cli args should parse: {e}"),
        };
        match cli.command {
            Some(Commands::Review { local_path, .. }) => {
                assert_eq!(local_path, None);
            }
            other => panic!("expected Review command, got {other:?}"),
        }
    }
}
