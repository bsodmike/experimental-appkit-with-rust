# CLAUDE.md

Guidance for Claude when working in this repo.

## Context

- `docs/PRD.md` at the root of this project is the Product Requirements Document (PRD) and serves as the blueprint for all code in `crates/`
- `docs/PRD-mac.md` is the blueprint for the native macOS frontend in `native/macos/`, and `docs/PRD-mac-01-concepts.md` explains the macOS concepts its decisions rest on
- _Grilling_ and/or _Planing_ (plan mode): at the end, record any appropriate changes in the PRD document, so that it is always a valid document. You may create ADRs (Architecture Decision Record) as appropriate in `docs/adrs`; all adrs should have the format `YYYY-MM-DD.adr-<DESC>.md`, where `<DESC>` is a short description that is (i) lowercase and (ii) separated by single dashes only.

## Writing code

- Do not use emojis anywhere in code or docs.
