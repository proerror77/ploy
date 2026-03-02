# Lessons

## 2026-03-02

- Pattern: Some links (including but not limited to X) fail with plain `curl` because content is JS-rendered, anti-bot protected, or behind login walls.
- Rule: If `curl` output is mostly boilerplate/error shell and no meaningful body text, switch to the `agent-browser` skill first instead of spending time on mirror/curl scraping.
- Reusable flow:
  - `agent-browser --session <name> open <target-url>`
  - `agent-browser --session <name> snapshot -i`
  - `agent-browser --session <name> get text body`
  - Extract article body from the first real article sentence onward; ignore boilerplate/error shell text.
  - Validate key numbers with a quick local calculation (Node/JS) before final analysis.
  - `agent-browser --session <name> close`
- Output standard:
  - Provide a structured analysis (core thesis, what is correct, what is inconsistent, actionable use).
  - Include relevant source URL(s) in the final answer.
  - If user asks to "save the article", default archive format should contain only two sections: `内文` and `分析` (omit capture metadata unless explicitly requested).

- Pattern: Deploying a locally-built macOS binary (`target/release/ploy` from Darwin) to a Linux server causes immediate runtime failure (`Exec format error` / binary gibberish in shell).
- Rule: All production releases must be built on Linux CI runners and validated as ELF before deploy. Never SCP a locally-built macOS binary to Linux.
- Release guardrail checklist:
  - Build on `ubuntu-latest` only for production release artifacts.
  - Use explicit production features: `claimer_daemon,api,pm_ctf,tokio/io-std`.
  - Run `file target/release/ploy` and require `ELF 64-bit LSB` in CI before deployment.
  - Deploy only the CI-built artifact, not local `target/release/ploy`.

- Pattern: On macOS, `cargo build --target x86_64-unknown-linux-gnu` can fail for crates with C build steps (for example `ring`) when `x86_64-linux-gnu-gcc` is missing.
- Rule: For local Linux artifacts from macOS, default to `cargo zigbuild --target x86_64-unknown-linux-gnu` and always verify with `file` before deploy.
- Local Linux build checklist:
  - `cargo zigbuild --release --target x86_64-unknown-linux-gnu --features "claimer_daemon,api,pm_ctf"`
  - `file target/x86_64-unknown-linux-gnu/release/ploy` must contain `ELF 64-bit LSB`.
