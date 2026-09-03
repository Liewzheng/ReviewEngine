use anyhow::Result;
use clap::{CommandFactory, FromArgMatches};
use review_engine::models::*;
use review_engine::progress::{new_progress_map, ProgressMap, ProgressStatus};
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use super::commands::{Cli, Commands};

#[cfg(feature = "cli")]
use super::handlers;

/// Build the clap command whose displayed program name is derived from
/// argv[0]'s basename, so symlinked invocations (e.g. `reng`) show their own
/// name in --help / usage output instead of the hardcoded `review-engine`.
pub fn cli_command() -> clap::Command {
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
            verbose,
            ..
        } => {
            let (pm, review_id) = spawn_progress_if_needed(&progress_map, cli.progress);
            handlers::run_local_path(
                &dir, &repo, config, llm_config, &format, &output, pm, &review_id, verbose,
            )
            .await?;
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
            verbose,
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
                verbose,
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
            verbose,
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
                verbose,
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
            verbose,
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
                verbose,
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
            // precedence as webhook-triggered reviews). Track the entries env
            // ACTUALLY seeded: they win at runtime but must never be
            // persisted into ui-state.toml (see the persist module docs).
            let env_llm_entries = if config.llm.is_empty() {
                review_engine::config::llm_configs_from_env()
            } else {
                Vec::new()
            };
            review_engine::config::apply_llm_env_fallback(&mut config);
            // Track which GitLab credentials came from CLI flags / env vars.
            // Since the persistence-file priority inversion these are
            // FALLBACK-ONLY: they take effect only when ui-state.toml holds
            // no value for the field, and each such use logs a deprecation
            // warning (configure the credential in the Web UI instead). They
            // are passed as overrides to the replay below and never saved to
            // the file.
            let gitlab_token_opt = gitlab_token
                .or_else(|| std::env::var("GITLAB_TOKEN").ok())
                .filter(|s| !s.is_empty());
            let webhook_secret_opt = gitlab_webhook_secret
                .or_else(|| std::env::var("GITLAB_WEBHOOK_SECRET").ok())
                .filter(|s| !s.is_empty());
            let signing_secret = gitlab_webhook_signing_secret
                .or_else(|| std::env::var("GITLAB_WEBHOOK_SIGNING_SECRET").ok())
                .filter(|s| !s.is_empty());
            let mut app_state = review_engine::server::AppState::new(config.llm.clone());
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
            app_state.ui_state_path = review_engine::server::api::config::persist::resolve_ui_state_path();
            app_state.ui_state_env = Some(review_engine::server::api::config::persist::UiStateEnvOverrides {
                gitlab_token: gitlab_token_opt.clone(),
                gitlab_webhook_secret: webhook_secret_opt.clone(),
                gitlab_webhook_signing_secret: signing_secret.clone(),
                llm_from_env: !env_llm_entries.is_empty(),
                llm_entries: env_llm_entries,
            });
            // 0.10.0 persistence (design/persistence.md §6.1, strict order):
            // 1) resolve DB URL → pool → migrate (failure aborts startup;
            //    REVIEW_DISABLE_DB=1 bypasses to 0.9 behaviour);
            // 2) TODO(梁序, step 4): §5.3 interrupted sweep — UPDATE reviews
            //    SET state='failed', error='interrupted: server restarted',
            //    completed_at=? WHERE state IN ('pending','running') goes
            //    here, after migrate and before the config replay;
            // 3) one-shot ui-state.toml import (single transaction; failure
            //    keeps the file and falls back to the file replay below);
            // 4) replay the DB state through the same apply_ui_config path.
            app_state.db = review_engine::server::api::config::persist::bootstrap_database()
                .await?
                .map(Arc::new);
            let state = Arc::new(app_state);
            let mut config_replayed = false;
            if let Some(store) = state.db.clone() {
                let overrides = state.ui_state_env.clone().unwrap_or_default();
                if let Some(path) = state.ui_state_path.clone() {
                    match review_engine::server::api::config::persist::import_ui_state_into_db(&store, &path).await {
                        Ok(true) => {}
                        Ok(false) => {}
                        Err(e) => tracing::error!(
                            "ui-state.toml import failed: {e:#}; the file is untouched, \
                             falling back to the file replay path"
                        ),
                    }
                }
                match review_engine::server::api::config::persist::load_and_apply_ui_state_from_db(
                    &state, &store, &overrides,
                )
                .await
                {
                    Ok(applied) => {
                        config_replayed = applied;
                        if applied {
                            tracing::info!("applied UI state from the database");
                        }
                    }
                    Err(e) => tracing::warn!("failed to replay UI state from the database: {e:#}"),
                }
            }
            let dispatcher = review_engine::server::dispatcher::MrDispatcher::persistent();
            let mut handlers: Vec<Arc<dyn review_engine::server::webhook::WebhookHandler>> = vec![];
            let gitlab_token = gitlab_token_opt.clone().unwrap_or_default();
            let webhook_secret = webhook_secret_opt.clone().unwrap_or_default();
            // Construct the GitLab webhook handler once: the global runtime
            // config below and the mounted route share this instance.
            let gitlab_handler = review_engine::server::gitlab::GitLabWebhookHandler::new(
                webhook_secret,
                signing_secret,
                dispatcher.clone(),
                gitlab_token,
            )
            .with_app_state(&state);
            // Always initialise the global GitLab runtime config from the
            // startup CLI/env values — even when no webhook secret is
            // configured: the REST API's `gitlab_mr` credential fallback
            // (docs/rest-api.md §1, used when the request carries no
            // `X-Gitlab-Token` header) reads the token from the runtime, so
            // `--gitlab-token` / `GITLAB_TOKEN` must land there regardless
            // of webhook setup.
            review_engine::server::gitlab::init_gitlab_runtime(&gitlab_handler);
            // Load the persisted UI state and apply it through the same code
            // path as PUT /config, so hot-apply and cold-start semantics are
            // identical. When the DB replay above already applied (or the DB
            // is active but empty after a failed import), the file replay is
            // the fallback. A missing/corrupt file never blocks startup — it
            // just reverts to config.toml/env.
            if !config_replayed {
                if let Some(path) = state.ui_state_path.clone() {
                    let overrides = state.ui_state_env.clone().unwrap_or_default();
                    match review_engine::server::api::config::persist::load_and_apply_ui_state(
                        &state, &path, &overrides,
                    ) {
                        Ok(true) => tracing::info!("applied persisted UI state from {}", path.display()),
                        Ok(false) => {}
                        Err(e) => tracing::warn!("failed to load persisted UI state from {}: {e:#}", path.display()),
                    }
                }
            }
            // Mount /webhook/gitlab unconditionally: verification is resolved
            // per-request — `verify` matches the payload's instance URL against
            // the hot-configured `state.git_platforms` (UI「Git 平台」), so a
            // webhook secret added through the UI takes effect without a
            // restart. With no verification configured anywhere the handler
            // itself rejects the request with 403 "no verification
            // configured"; gating the mount on startup env secrets would
            // instead drop such requests to the static-file fallback (405).
            handlers.push(Arc::new(gitlab_handler));
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
                handlers.push(Arc::new(
                    review_engine::server::github::GitHubWebhookHandler::new(secret, dispatcher.clone(), tok)
                        .with_app_state(&state),
                ));
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
                review_engine::actions::init::run_interactive(".").await?;
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
        Commands::Config { noun } => match noun {
            super::commands::ConfigNoun::Provider { action } => {
                handlers::run_config_provider(action).await?;
            }
        },
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
pub(crate) fn spawn_progress_if_needed(
    progress_map: &ProgressMap,
    cli_progress: bool,
) -> (Option<ProgressMap>, String) {
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
