# Domain Docs

This repo uses a single-context layout: `GLOSSARY.md` at the repo root and ADRs in `docs/adr/`.

## Before exploring, read these

- Read the root `GLOSSARY.md`.
- Read ADRs in `docs/adr/` that touch the area you are about to work in.

If either is absent, proceed silently. The `domain-modeling` skill creates domain documents lazily when terms or decisions are resolved.

## Use the glossary's vocabulary

When naming domain concepts in issue titles, refactor proposals, hypotheses, or tests, use the terms defined in `GLOSSARY.md` and respect its explicit synonyms to avoid.

If a needed concept is absent, reconsider whether it belongs to the project's language; record a real gap for `domain-modeling`.

## Flag ADR conflicts

If a proposal contradicts an existing ADR, identify the ADR and explain why its decision should be reopened rather than silently overriding it.
