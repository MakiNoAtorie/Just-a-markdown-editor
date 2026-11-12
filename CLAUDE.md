# Just a markdown editor

Native Rust Markdown editor for Windows (winit + softbuffer + cosmic-text, CPU drawing, no webview). Offline only; Markdown files are the only storage. Cloud sync was discarded.

## Layout
- `src/core`: buffer, commands, config, markdown, storage, workspace (no UI).
- `src/ui`: app, editor, gui, layout, menu, overlay, tab.
- `src/render`: drawing and theme. `src/platform`: Windows calls.
- `src/bin/performance-check.rs`: performance gate.
- `markdown-editor-spec.md`: product spec. `README.md`: user-facing behavior.

## Checks
- Full check (same as CI): `./build.ps1 -Test -Benchmark`. Run before each push.
- Tests only: `cargo test --locked`.
- Output goes to `dist/`, which is git-ignored.

## Rules
- Keep dependencies minimal; prefer std and existing crates.
- Update `README.md` when user-visible behavior changes.
- Settings and session files (`settings.toml`, `session.toml`) live beside the executable.
- Use simple, technical english for the readme and comments.
- Use ASD-STE100 guidelines for the manuals.
- The code should be readable by humans.
- Development is done in sprints so each milestone should be shippable and add value.


## Sub agent Rules
You can be the orchestrator and may use sub-agents for different tasks. Always ask the agents to review their work before handing it over. 
Use claude sonnet (high) for:
- Clear and scoped tasks.
- Simple code reviews.
- General codebase exploration.
- Explore opportunities.
- Non-critical ambiguous tasks.
Sonnet and Haiku are the only models you're allowed to send more than 3 agents in parallel.

Use claude opus (medium or high) for:
- Code, optimization, architecture reviews.
- When tasks have uncertainty.
- Changes require touching more than one file.
- Wide codebase changes.

Only I, the user, can use fable models. For cases not in this instruction, use your own judgement.

## Git
- Commit and push on `main`. No branches unless tasks run in parallel.
