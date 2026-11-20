@AGENTS.md

# Claude Code notes

`AGENTS.md` (imported above) is the project's rulebook: read it first. Two rules are easy to forget,
so they are repeated here:

- **Document everything, exhaustively, in the same change**: every configuration option and every
  user-visible behaviour, each with a usage example (`docs/config/`, `docs/usage.md`,
  `docs/design/plugin-protocol.md`, `examples/config.yaml`). See the first rule in AGENTS.md.
- **Regenerate `THIRD-PARTY-NOTICES.md`** whenever dependencies change.

Commits: the user has pre-authorised committing once fmt, clippy, tests and the notices check pass;
never push without being asked.
