# CI Integration Examples

You can run review-engine in CI to catch issues before merging. The examples below install the binary, run a local review against `main`, and store the report as an artifact.

## GitLab CI

```yaml
review-engine:
  image: rust:latest
  variables:
    LLM_CONFIG: '[{"provider":"openai","model":"gpt-4o","api_key":"$OPENAI_API_KEY","api_base":"https://api.openai.com/v1","max_tokens":4096,"temperature":0.3}]'
  script:
    - curl -fsSL https://raw.githubusercontent.com/Liewzheng/ReviewEngine/master/install.sh | bash
    - export PATH="$HOME/.local/bin:$PATH"
    - review-engine review --local-path . --base main --format markdown --output review-report.md
  artifacts:
    paths:
      - review-report.md
    when: always
```

Store `OPENAI_API_KEY` as a GitLab CI/CD variable and mark it as masked.

## GitHub Actions

```yaml
name: review-engine

on:
  pull_request:

jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Install review-engine
        run: |
          curl -fsSL https://raw.githubusercontent.com/Liewzheng/ReviewEngine/master/install.sh | bash
          echo "$HOME/.local/bin" >> "$GITHUB_PATH"

      - name: Run review-engine
        env:
          LLM_CONFIG: '[{"provider":"openai","model":"gpt-4o","api_key":"${{ secrets.OPENAI_API_KEY }}","api_base":"https://api.openai.com/v1","max_tokens":4096,"temperature":0.3}]'
        run: |
          review-engine review --local-path . --base main --format markdown --output review-report.md

      - name: Upload report
        uses: actions/upload-artifact@v4
        with:
          name: review-report
          path: review-report.md
```

## Failing a pipeline on risk level

review-engine does not currently have a `--fail-on-risk-level` flag. You can script around the JSON output instead:

```bash
review-engine review --local-path . --base main --format json --output report.json
risk=$(jq -r '.consolidated.assessment.risk_level // empty' report.json)

if [ "$risk" = "High" ] || [ "$risk" = "Critical" ]; then
  echo "Risk level $risk detected. Blocking merge."
  exit 1
fi
```

> **JSON 路径说明**：`reng review`（团队评审）的总体风险在
> `consolidated.assessment.risk_level`（如 `"Medium"` / `"High"` / `"Critical"`）。
> 顶层 `aggregated` 字段默认是 `null`——它对应可选的 aggregator 专家，默认关闭
> （需在配置中启用 `[report] aggregated = true` 且团队包含 aggregator 专家）；
> 未启用时请勿从 `aggregated` 取风险。先 `jq . report.json` 核对实际结构再接入 CI。

The exact JSON path depends on the report structure; inspect `report.json` to find the field that matches your config.
