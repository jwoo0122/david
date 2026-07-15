# Architecture decisions

This directory is a living map of architectural intent, not a chronological decision log.

- Read `index.yaml` first and load only relevant records.
- Preserve `.adr-system.yaml`; it identifies the owning schema and version.
- Name records by stable decision question: `records/<scope>/<question>.md`.
- Improve or revise an existing record when its question is unchanged.
- Create a record only for a new architectural question.
- Treat accepted decisions as current but revisable intent.
- Keep supersession links bidirectional.
- Rebuild and validate the index with the ADR maintainer script after edits.

Records capture intent that code alone does not reliably reveal. Local implementation details and routine changes do not belong here.
