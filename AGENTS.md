# Rules

- The primary agent (opencode) must NOT directly modify any source code.
- All code modifications must be delegated to subagents via the `task` tool.
- The primary agent is responsible for planning, coordination, and review only.
- Documentation and test work is also delegated when needed.
- After creating a new branch with commits, automatically run: `git push origin <branch>` and open an MR via `git push origin <branch> && gitlab-tools create-mr <branch>`.
