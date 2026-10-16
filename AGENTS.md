# Codex instructions for youtube-extractor-rs

## Main thread is CTO / lead agent

The main Codex thread is responsible for analysis, architecture, technology evaluation, task decomposition, delegation, integration, and review. **It must not directly edit production code** unless the user explicitly overrides this rule. Delegate implementation to available coding agents, select smaller agents for focused work and more capable agents for complex work, and inspect/test their changes before integration.

Decisions must follow the needs of this project, without bias toward the user's usual tech stack.

## Project constraints

This is a standalone Rust YouTube media extraction microservice, independent of the video-automation repository, intended for Docker/Railway and an authenticated HTTP API. Implement our own extraction logic; do not add yt-dlp or pre-built YouTube extraction engines. General-purpose Rust crates and FFmpeg for media processing are allowed.

Avoid unauthorized downloading and circumvention of DRM, CAPTCHA, authentication requirements, or platform access controls. Distinguish extraction errors from network-level restrictions, and do not claim reliable Railway access unless end-to-end tests verify it.

## Local skills

Use `cto-advisor`, `senior-backend`, `docker-development`, `senior-devops`, and `adversarial-reviewer` when appropriate. Actual copies are in `.codex/skills`; Codex-discoverable symlinks are in `.agents/skills`. See `.codex/SKILLS.md`.

## Git workflow

Create feature branches from the latest `main`. Preserve a deployable main branch, open pull requests for review, and verify relevant tests before merging. Do not claim tests, extraction, or deployment succeeded without running them.
