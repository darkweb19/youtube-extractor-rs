# Project Codex Skills

These five skills were copied from [darkweb19/skills](https://github.com/darkweb19/skills) at source commit `02e3bc090208bebb8cbf0ddc1985c51235c67b8f`. Their instructions, references, profiles, and attribution are retained. The eleven optional Python helper scripts were removed because this Rust service does not use them.

| Skill | Intended use |
| --- | --- |
| `cto-advisor` | Architecture and technical decision support |
| `senior-backend` | Backend APIs, security, and the `go-or-rust-microservice` profile |
| `docker-development` | Multi-stage Docker builds and container hardening |
| `senior-devops` | CI/CD and deployment |
| `adversarial-reviewer` | Critical reviews of changes before merge |

Each skill lives directly in `.agents/skills/<skill>/`, with its `SKILL.md` entry point and any supporting references or profiles. This is the standard Codex repository skill discovery path and the single canonical location for these skills.

These are guidance-only skill copies. Upstream examples that mention Python helper scripts describe tools available in the linked upstream repository, not this checkout. Do not run missing helpers or reinstall them automatically. Apply the documented checks directly using this project's Rust tools, Cargo tests, Docker build, and maintained smoke scripts. Backend decisions should record assumptions and tradeoffs directly rather than invoking the removed decision engine.

The maintained `scripts/smoke-*.sh` helpers still use inline Python for JSON validation and test-token generation. They are active test tooling; Python is not an application or production Docker runtime dependency.

Source attribution and upstream guidance are preserved, with local adaptation notices documenting the omitted tools. Consult individual skill metadata and the upstream repository's licensing information for applicable notices. Updates should be reviewed against the pinned source revision rather than pulled automatically; do not restore the omitted scripts as part of an update.
