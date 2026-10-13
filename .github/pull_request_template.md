## Change and supported format

Describe the source structure, expected behavior and any output/version change.

## Required acceptance evidence

- [ ] Synthetic positive examples cover every new subtype and extension alias.
- [ ] Expected JSONL and metadata are committed; I reviewed their full content.
- [ ] Malformed, ambiguous and resource cases are registered and tested.
- [ ] Archive/container changes cover expansion, paths, members and unsupported nesting.
- [ ] The public schema and citation/manifest validator pass.
- [ ] Parser version and documentation match any deliberate output change.
- [ ] The documented applicable checks pass; commands/results are included below.
- [ ] Dependency/lockfile changes include refreshed license notices (`npm run licenses`).
- [ ] No real exports, credentials, account/persona IDs or private data are included.

All changes, including workflows and expected outputs, require @x-aero's final
acceptance. A PR is a suggestion; it grants no merge, deployment or release right.

## Checks run

List exact commands and results. Explain any platform checks the owner must run.
