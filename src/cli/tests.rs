use super::app::cli_command;
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

#[test]
fn validate_accepts_config_path() {
    let cli = parse_ok(&["validate", "--config", "/tmp/.code-audit-config.toml"]);
    match cli.command {
        Some(Commands::Validate { config }) => {
            assert_eq!(config.as_deref(), Some("/tmp/.code-audit-config.toml"))
        }
        other => panic!("expected Validate, got {other:?}"),
    }
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

#[test]
fn review_rejects_conflicting_directory_and_diff_flags() {
    // `--review-dir` conflicts with `--diff` (clap conflicts_with_all).
    assert!(parse(&["review", "--diff", "/tmp/x.diff", "--review-dir", "src"]).is_err());
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
