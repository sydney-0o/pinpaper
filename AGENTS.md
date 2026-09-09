# Project agent instructions

These instructions apply to future work in this repository.

- The primary Codex agent plans and orchestrates only. Delegate execution to a subagent using `gpt-5.6-luna` with maximum reasoning effort.
- Do not poll a delegated subagent every 30 seconds. Wait for the subagent to return a completion message, or wake at a 25-minute interval (`1500000` ms).
- Keep delegation scoped to the task and avoid unnecessary multi-agent fanout.
