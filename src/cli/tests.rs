use super::app::cli_command;
use super::app::resolve_validate_target;
use super::app::spawn_progress_if_needed;
use super::commands::{Cli, Commands};
use anyhow::Result;
use clap::FromArgMatches;
use review_engine::progress::new_progress_map;

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

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    let mut full = vec!["review-engine"];
    full.extend_from_slice(args);
    // `try_get_matches_from` returns the error instead of printing it to
    // stderr and exiting (which `get_matches_from` would do on bad args).
    let matches = cli_command().try_get_matches_from(full)?;
    Cli::from_arg_matches(&matches)
}

fn parse_ok(args: &[&str]) -> Cli {
    parse(args).unwrap_or_else(|e| panic!("args should parse: {e}"))
}

#[test]
fn serve_uses_documented_defaults() {
    let cli = parse_ok(&["serve"]);
    match cli.command {
        Some(Commands::Serve {
            port, bind, tls_port, ..
        }) => {
            assert_eq!(port, 8080);
            assert_eq!(bind, "127.0.0.1");
            assert_eq!(tls_port, 8443);
        }
        other => panic!("expected Serve, got {other:?}"),
    }
}

#[test]
fn serve_accepts_overrides() {
    let cli = parse_ok(&["serve", "--port", "9000", "--bind", "0.0.0.0", "--api-token", "tok"]);
    match cli.command {
        Some(Commands::Serve {
            port, bind, api_token, ..
        }) => {
            assert_eq!(port, 9000);
            assert_eq!(bind, "0.0.0.0");
            assert_eq!(api_token.as_deref(), Some("tok"));
        }
        other => panic!("expected Serve, got {other:?}"),
    }
}

#[test]
fn serve_tls_requires_both_cert_and_key() {
    // Only --tls-cert → clap `requires` rejects it.
    assert!(parse(&["serve", "--tls-cert", "/tmp/cert.pem"]).is_err());
    assert!(parse(&["serve", "--tls-key", "/tmp/key.pem"]).is_err());
    // Both → ok.
    assert!(parse(&["serve", "--tls-cert", "/tmp/c.pem", "--tls-key", "/tmp/k.pem"]).is_ok());
}

/// RENG-37 (a): `serve --data-dir` parses, defaults to `None` (unchanged
/// behaviour), and the parsed root repoints **every** artifact of the state
/// layout — not just one.
#[test]
fn serve_data_dir_parses_and_repoints_the_whole_state_layout() {
    match parse_ok(&["serve"]).command {
        Some(Commands::Serve { data_dir, .. }) => assert_eq!(data_dir, None, "no flag means today's defaults"),
        other => panic!("expected Serve, got {other:?}"),
    }

    let root = "/srv/reng-instance-a";
    let cli = parse_ok(&["serve", "--data-dir", root]);
    let Some(Commands::Serve {
        data_dir: Some(data_dir),
        ..
    }) = cli.command
    else {
        panic!("serve --data-dir must capture the path");
    };
    let resolved_root = review_engine::paths::state_dir_from(
        Some(data_dir),
        Some("/app/config"),
        Some(std::path::PathBuf::from("/home/alice")),
    );
    assert_eq!(resolved_root.as_deref(), Some(std::path::Path::new(root)));
    for file in review_engine::paths::STATE_FILES {
        let path = review_engine::paths::resolve_artifact_at(None, file.name, resolved_root.clone())
            .unwrap_or_else(|| panic!("{} must resolve under the data dir", file.name));
        assert_eq!(path, std::path::Path::new(root).join(file.name));
    }

    // The flag is `serve`-only: other subcommands must reject it.
    assert!(parse(&["review", "--data-dir", root]).is_err());
    assert!(parse(&["--data-dir", root, "serve"]).is_err());
}

/// RENG-106: `reng doctor [--fix] [--quiet]` — both flags optional, neither
/// implied, and `--fix` is not the default (a bare `reng doctor` must never
/// mutate anything).
#[test]
fn doctor_parses_fix_and_quiet_independently() {
    match parse_ok(&["doctor"]).command {
        Some(Commands::Doctor { fix, quiet }) => {
            assert!(!fix, "a bare `reng doctor` only diagnoses");
            assert!(!quiet);
        }
        other => panic!("expected Doctor, got {other:?}"),
    }
    match parse_ok(&["doctor", "--fix"]).command {
        Some(Commands::Doctor { fix, quiet }) => {
            assert!(fix);
            assert!(!quiet);
        }
        other => panic!("expected Doctor, got {other:?}"),
    }
    match parse_ok(&["doctor", "--fix", "--quiet"]).command {
        Some(Commands::Doctor { fix, quiet }) => {
            assert!(fix);
            assert!(quiet);
        }
        other => panic!("expected Doctor, got {other:?}"),
    }
    // The flags belong to `doctor` alone.
    assert!(parse(&["validate", "--fix"]).is_err());
}

#[test]
fn validate_accepts_config_path() {
    let cli = parse_ok(&["validate", "--config", "/tmp/.code-audit-config.toml"]);
    match cli.command {
        Some(Commands::Validate { config, file }) => {
            assert_eq!(config.as_deref(), Some("/tmp/.code-audit-config.toml"));
            assert_eq!(file, None, "--config alone leaves the positional empty");
        }
        other => panic!("expected Validate, got {other:?}"),
    }
}

/// RENG-81: `reng validate <file>` was rejected with "unexpected argument" even
/// though the help text said the command takes a config file. Both spellings
/// must now name the same file, and asking for both at once is a contradiction
/// rather than a silent winner.
#[test]
fn validate_positional_path_equals_the_config_flag() {
    let target = |cli: Cli| match cli.command {
        Some(Commands::Validate { config, file }) => config.or(file),
        other => panic!("expected Validate, got {other:?}"),
    };
    let positional = target(parse_ok(&["validate", "/tmp/.code-audit-config.toml"]));
    let flagged = target(parse_ok(&["validate", "--config", "/tmp/.code-audit-config.toml"]));
    assert_eq!(positional.as_deref(), Some("/tmp/.code-audit-config.toml"));
    assert_eq!(positional, flagged, "both forms must resolve to the same file");

    assert!(
        parse(&["validate", "/tmp/a.toml", "--config", "/tmp/a.toml"]).is_err(),
        "one file named twice is rejected, not silently preferred"
    );
}

/// The `validate` file resolution: an explicit path (from either spelling)
/// wins, then the current directory's `.code-audit-config.toml`, then the
/// user-level file — and with none of them the error names both spellings.
#[test]
fn resolve_validate_target_prefers_explicit_then_cwd_then_user_config() {
    let dir = tempfile::tempdir().ok().expect("tempdir");
    let local = dir.path().join(".code-audit-config.toml");
    std::fs::write(&local, "llm = []\n").expect("write local config");
    let user = dir.path().join("user-code-audit-config.toml");
    std::fs::write(&user, "llm = []\n").expect("write user config");

    assert_eq!(
        resolve_validate_target(
            Some("/tmp/explicit.toml".to_string()),
            Some(dir.path()),
            Some(user.clone())
        )
        .expect("an explicit path always resolves"),
        "/tmp/explicit.toml",
        "an explicit path is used even when it does not exist (the read reports it)"
    );
    assert_eq!(
        resolve_validate_target(None, Some(dir.path()), Some(user.clone())).expect("local config exists"),
        local.to_string_lossy(),
        "the current directory's config wins over the user-level one"
    );

    let empty = tempfile::tempdir().ok().expect("tempdir");
    assert_eq!(
        resolve_validate_target(None, Some(empty.path()), Some(user.clone())).expect("user config exists"),
        user.to_string_lossy(),
        "without a local file the user-level config is validated"
    );

    let err = resolve_validate_target(None, Some(empty.path()), None).expect_err("no config anywhere");
    let message = err.to_string();
    assert!(message.contains("reng validate <file>"), "got: {message}");
    assert!(message.contains("--config"), "got: {message}");
}

#[test]
fn init_supports_default_flag() {
    let cli = parse_ok(&["init", "--default"]);
    match cli.command {
        Some(Commands::Init { default }) => assert!(default),
        other => panic!("expected Init, got {other:?}"),
    }
    let cli = parse_ok(&["init"]);
    match cli.command {
        Some(Commands::Init { default }) => assert!(!default),
        other => panic!("expected Init, got {other:?}"),
    }
}

#[test]
fn repo_review_parses_local_path_and_format() {
    let cli = parse_ok(&["repo-review", "--local-path", ".", "--format", "json"]);
    match cli.command {
        Some(Commands::RepoReview {
            local_path,
            format,
            output,
            ..
        }) => {
            assert_eq!(local_path.as_deref(), Some("."));
            assert_eq!(format, "json");
            assert_eq!(output, None);
        }
        other => panic!("expected RepoReview, got {other:?}"),
    }
}

#[test]
fn repo_review_defaults_to_markdown_and_accepts_repeated_llm_config() {
    let cli = parse_ok(&["repo-review", "--llm-config", "a", "--llm-config", "b"]);
    match cli.command {
        Some(Commands::RepoReview { format, llm_config, .. }) => {
            assert_eq!(format, "markdown");
            assert_eq!(llm_config, vec!["a".to_string(), "b".to_string()]);
        }
        other => panic!("expected RepoReview, got {other:?}"),
    }
}

#[test]
fn upgrade_parses_check_version_and_rollback() {
    let cli = parse_ok(&["upgrade", "--check", "--version", "0.9.0"]);
    match cli.command {
        Some(Commands::Upgrade {
            check,
            yes,
            version,
            rollback,
        }) => {
            assert!(check);
            assert!(!yes);
            assert_eq!(version.as_deref(), Some("0.9.0"));
            assert!(!rollback);
        }
        other => panic!("expected Upgrade, got {other:?}"),
    }
}

#[test]
fn generate_token_and_default_commands_have_no_fields() {
    let cli = parse_ok(&["generate-token"]);
    assert!(matches!(cli.command, Some(Commands::GenerateToken)));
    let cli = parse_ok(&["default"]);
    assert!(matches!(cli.command, Some(Commands::Default)));
}

#[test]
fn no_subcommand_yields_none() {
    let cli = parse_ok(&[]);
    assert!(cli.command.is_none());
    assert!(!cli.version);
}

#[test]
fn global_progress_and_version_flags_are_recognized() {
    let cli = parse_ok(&["--progress", "review", "--diff", "/tmp/x.diff"]);
    assert!(cli.progress);
    let cli = parse_ok(&["--version"]);
    assert!(cli.version);
    let cli = parse_ok(&["-V"]);
    assert!(cli.version);
}

#[test]
fn unknown_subcommand_is_rejected() {
    assert!(parse(&["bogus-command"]).is_err());
}

/// The full-content directory review is spelled `--path` (the stale
/// `--review-dir` name this test used made it pass on an unknown-argument
/// error, asserting nothing): it requires `--local-path` and is mutually
/// exclusive with `--diff`.
#[test]
fn review_path_requires_local_path_and_conflicts_with_diff() {
    assert!(
        parse(&["review", "--path", "src"]).is_err(),
        "--path without --local-path has no repository to read"
    );
    assert!(
        parse(&["review", "--diff", "/tmp/x.diff", "--path", "src", "--local-path", "."]).is_err(),
        "--path and --diff are mutually exclusive"
    );
    assert!(
        parse(&["review", "--path", "src", "--local-path", "."]).is_ok(),
        "the documented combination must parse"
    );
}

#[tokio::test]
async fn spawn_progress_if_needed_toggles_on_cli_progress() {
    let map = new_progress_map();
    let (pm, id) = spawn_progress_if_needed(&map, false);
    assert!(pm.is_none());
    assert!(!id.is_empty());

    let (pm, id2) = spawn_progress_if_needed(&map, true);
    assert!(pm.is_some());
    assert!(!id2.is_empty());
    // Give the spawned display task a tick so it does not leak a pending
    // task past the test.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
}

#[cfg(test)]
mod config_provider_parse {
    use super::super::commands::{Cli, Commands, ConfigNoun, ProviderAction};
    use super::{parse, parse_ok};

    #[test]
    fn config_provider_set_parses_all_options() {
        let cli = parse_ok(&[
            "config",
            "provider",
            "set",
            "openai",
            "--model",
            "gpt-4o",
            "--api-base",
            "https://api.openai.com/v1",
            "--api-key",
            "sk-x",
            "--max-tokens",
            "8192",
            "--temperature",
            "0.7",
            "--disable-thinking",
            "--global",
        ]);
        match cli.command {
            Some(Commands::Config {
                noun:
                    ConfigNoun::Provider {
                        action:
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
                            },
                    },
            }) => {
                assert_eq!(name, "openai");
                assert_eq!(model.as_deref(), Some("gpt-4o"));
                assert_eq!(api_base.as_deref(), Some("https://api.openai.com/v1"));
                assert_eq!(api_key.as_deref(), Some("sk-x"));
                assert_eq!(max_tokens, Some(8192));
                assert_eq!(temperature, Some(0.7));
                assert!(disable_thinking);
                assert!(global);
                assert!(!project);
            }
            other => panic!("expected Config provider Set, got {other:?}"),
        }
    }

    #[test]
    fn config_provider_list_remove_test_parse() {
        let cli = parse_ok(&["config", "provider", "list"]);
        match cli.command {
            Some(Commands::Config {
                noun:
                    ConfigNoun::Provider {
                        action: ProviderAction::List { global, project },
                    },
            }) => {
                assert!(!global);
                assert!(!project);
            }
            other => panic!("expected Config provider List, got {other:?}"),
        }

        let cli = parse_ok(&["config", "provider", "remove", "openai", "--project"]);
        match cli.command {
            Some(Commands::Config {
                noun:
                    ConfigNoun::Provider {
                        action: ProviderAction::Remove { name, global, project },
                    },
            }) => {
                assert_eq!(name, "openai");
                assert!(!global);
                assert!(project);
            }
            other => panic!("expected Config provider Remove, got {other:?}"),
        }

        let cli = parse_ok(&["config", "provider", "test", "openai"]);
        match cli.command {
            Some(Commands::Config {
                noun:
                    ConfigNoun::Provider {
                        action: ProviderAction::Test { name, .. },
                    },
            }) => assert_eq!(name, "openai"),
            other => panic!("expected Config provider Test, got {other:?}"),
        }
    }

    #[test]
    fn config_provider_scope_flags_are_mutually_exclusive() {
        assert!(parse(&["config", "provider", "list", "--global", "--project"]).is_err());
        assert!(parse(&["config", "provider", "set", "x", "--global", "--project"]).is_err());
        assert!(parse(&["config", "provider", "remove", "x", "--global", "--project"]).is_err());
        assert!(parse(&["config", "provider", "test", "x", "--global", "--project"]).is_err());
    }

    #[test]
    fn config_provider_set_requires_name() {
        assert!(parse(&["config", "provider", "set"]).is_err());
        assert!(parse(&["config", "provider"]).is_err());
        let cli: Cli = parse_ok(&["config", "provider", "set", "openai"]);
        match cli.command {
            Some(Commands::Config {
                noun:
                    ConfigNoun::Provider {
                        action:
                            ProviderAction::Set {
                                name,
                                model,
                                api_key,
                                disable_thinking,
                                ..
                            },
                    },
            }) => {
                assert_eq!(name, "openai");
                assert_eq!(model, None);
                assert_eq!(api_key, None);
                assert!(!disable_thinking);
            }
            other => panic!("expected Config provider Set, got {other:?}"),
        }
    }
}
