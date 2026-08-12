# ReviewEngine Enterprise

ReviewEngine is free and open source at its core, with a commercial enterprise edition available separately.

## Free and open-source core

The core engine is licensed under the Apache License 2.0 and is free for individuals and teams. It is developed in this public repository and includes everything you need to run automated reviews in your own environment:

- The `review-engine` CLI.
- Local repository review (`--local-path`, `--base`, `--staged`, commit ranges).
- GitLab MR and GitHub PR review via CLI or webhook.
- The REST API and webhook server (`review-engine serve`).
- Configurable CodeReview Board experts and weighted scoring.
- The default expert team shipped in `docs/code-audit-default.toml`.

## Enterprise edition

Enterprise features are developed separately and available under a commercial license. They are designed for organizations that need centralized management, compliance, and support:

- **Single Sign-On (SSO)** — integration with your identity provider.
- **Audit logs and compliance reporting** — track reviews, decisions, and data access.
- **Custom expert templates and fine-tuning** — tailor expert prompts and weights for your organization.
- **Advanced analytics and dashboards** — review trends, risk metrics, and team insights.
- **Dedicated support and SLAs** — priority help from the ReviewEngine team.
- **Private deployment assistance** — on-premise or isolated cloud setup.

## Contact

For enterprise licensing, pricing, or a private demo, contact us at **isletspace@outlook.com** or reach out through the maintainers listed in the repository.

## Related

- Read the core project overview: [`README.md`](../README.md)
- Learn how to contribute: [`CONTRIBUTING.md`](../CONTRIBUTING.md)
