# Owner acceptance and distribution controls

The verified owner is **@x-aero**, GitHub user ID **167432977**. Contributors
submit suggestions through fork PRs. They need no upstream write/admin access.
Only the owner accepts/merges changes and initiates official distribution runs.

## GitHub configuration required before opening contributions

The tracked files prepare the policy; they do not themselves activate GitHub
protections. Keep @x-aero as the sole repository administrator and upstream
writer. Review organization owners, teams, installed GitHub Apps and deploy keys
too: an administrator can change the policy itself. Do not grant contributors
admin/maintain/write merely to let them open a PR.

Apply `distribution/github-main-protection.json` to `main` with the owner's
administration access, after verifying the actual new CI matrix contexts. Its
required `CI acceptance` check must come from the GitHub Actions app (ID 15368),
require an up-to-date branch, and block failed checks. This stable check requires
all four native platforms for code/build/fixture changes; proven documentation
changes run tooling checks only. It rejects missing platforms and stale PR
head/base results. Old macOS 14 success is not evidence for the current matrix.

Require code-owner reviews, dismiss changed approvals and restrict review
dismissal and main pushes to @x-aero. Disable force pushes and deletion. The
single rule in `.github/CODEOWNERS` owns every file, including itself and all
workflow/fixture files. Listing another code owner would allow that person's
approval to satisfy the rule and would change the requested policy.

The owner has a deliberate bypass to accept contributions and push maintenance changes;
GitHub cannot let a PR author approve their own PR. This privilege belongs only
to the owner. Contributors cannot merge even a green, approved PR themselves.
Keep the existing auto-merge setting disabled. Document and inspect each owner
acceptance; do not grant another administrator a general bypass.

Protect `v*` tags against creation outside the owner-controlled release path,
updates and deletion. The release workflow's token needs permission to reserve
the exact tag; grant any necessary integration exception only with reviewed
workflows and read-only ordinary PR tokens. Do not give contributors a tag or
release bypass. Verify the resulting rules, not just their configuration file.

Apply the `github-main-owner-ruleset.json` and `github-main-integrity-ruleset.json`
files from `distribution/` as active rulesets. Main updates have only the verified
owner User bypass; force pushes and deletion have no bypass. The owner can merge
or push without another person's review or a mandatory check run.
Apply `github-tag-owner-ruleset.json` and `github-tag-integrity-ruleset.json` too:
the owner and the gated release workflow's GitHub Actions integration can create
version tags, but neither can update or delete them. Keep ordinary workflow
tokens read-only. `github-selected-actions.json` and
`github-workflow-permissions.json` supply the allowed action pins and token policy.

## Distribution approval

Both Production packaging workflows are manual `workflow_dispatch` workflows,
with no tag-push distribution trigger and no Preview option. Every job checks
the initiating owner's immutable user ID, the owner rerun account and
`refs/heads/main`; preflight also checks the canonical repository ID. A fork,
another account, a tag/feature branch or an outsider rerun does not pass.
Validation CI still runs for PR suggestions.
Preview installers are built and verified on trusted local native machines,
then published by the authenticated maintainer to the verified private archive as an immutable prerelease; no
Actions release environment or hosted Preview build secret is used.

## Paid CI policy

Main pushes do not automatically repeat CI. The owner runs full main CI when
needed, or a release performs it once. Both release workflows accept an optional
`validation_run_id`: completed canonical main CI for the exact source and all
four platforms. Failed, PR, fork, stale-source and policy-only runs cannot be
reused. A verified run skips duplicate full validation, without bypassing owner
release approval or artifact verification.

Dependabot proposals do not start native CI automatically. The owner may rerun
their skipped workflow after reviewing the proposal; the owner rerun account is
required by the planning job. Security alerts remain available independently.
PR updates cancel obsolete tests for that PR. A trusted base checkout plans
checks using GitHub metadata, including both paths of a rename. Incomplete or
large diffs run native tests; unknown files are never treated as documentation.
Planning and final acceptance verify the current PR head/base. Test jobs have
45-minute timeouts; small planning/acceptance jobs have five-minute timeouts.
The owner's manual `scope=policy` checks workflow/tooling only and is never
usable as full release validation. It does not build an installer.

Fork PR tokens stay read-only and receive no release secrets. Before public
Actions are enabled, apply
`distribution/github-public-fork-approval.json` to the repository's
`actions/permissions/fork-pr-contributor-approval` endpoint and read back
`all_external_contributors`. Only the owner has upstream write access and may
approve runs. CI approval authorizes testing, not merging or distribution.
Review proposed workflow changes before approving an external run; keep
`pull_request_target` code execution disabled.

Require full commit pins for remote actions with the repository Actions setting
`sha_pinning_required=true`. All current remote actions already use full reviewed
SHAs. A pin update requires owner review, just like other workflow changes;
ordinary workflow tokens remain read-only and cannot approve PRs.
[GitHub Actions permissions](https://docs.github.com/en/rest/actions/permissions).

Create the **owner-release** environment using
`distribution/github-release-environment.json`. Its only reviewer is @x-aero;
allow this owner to approve their own run so a sole-owner release does not
deadlock. Limit deployments to protected branches and disable administrator
bypass of approval where available. Credential-bearing build and write-token
jobs reference that environment, so approval precedes those operations.

Production builders load the public client key from
`distribution/public-client/production.json` and pass it as
`SYNTHIUS_SUPABASE_PUBLIC_KEY`. Preview builders require a maintainer's external
`SYNTHIUS_PREVIEW_CONFIG_FILE` containing the private endpoints/public key.
Never commit actual Preview configuration. No channel has a hardcoded Rust key fallback. Public client keys are not release authority;
self-built clients may use the corresponding hosted app with their own account.
Never put a service-role key, secret key or user session token in client configuration.
Standalone contributor tests/builds use `scripts/offline-dev.cjs` with a
nonfunctional key and do not require the private webapp or a Supabase instance.

Preflight requires the exact protected `main` SHA, an owner-only CODEOWNERS file
and an owner-reviewer environment. Missing/empty protection fails before any
installer build. The workflow token reads contents/actions only; full branch
review/check-rule readback requires the owner's administration access. Validate
the supplied main policy during setup, not with a broader workflow token.

Desktop releases execute the full parser/worker workspace suite and fixture
matrix as well as desktop and JavaScript checks. Stable releases call full CI.
Source/tag/provenance/checksum safeguards remain in place. Upload-only failures
reuse verified original artifacts; owner-only reruns do not grant a new source
identity to an old artifact.

## Telegram application identity and consent

Official desktop distribution includes Telegram import. Put
`SYNTHIUS_TELEGRAM_API_ID` and `SYNTHIUS_TELEGRAM_API_HASH` in the protected
owner-release environment. The builder requires a valid identity and preserves
it in the compiler environment. No feature-disable switch or policy-approval
variable is used. Owner-only dispatch, code acceptance and release approval
controls still apply.

Never put user session credentials into Actions or source. The application
identity is intentionally recoverable from a distributed native binary;
account authorization remains separate and memory-only. Credential-free local
builds remain available for offline development and tests.

The [content-licensing terms](https://telegram.org/tos/content-licensing) include
a scoped consent exception: each relevant user must individually give explicit,
informed, affirmative and continued consent for the specific content/context.
Consent is not transferable to another chat or broader platform data. Keep the
one-time account explanation and each import's explicit chat/topic/date selection.
Feature acceptance does not preselect or broaden that content scope. This documents consent scope; it is
not an automatic feature or publication restriction.

## Private reporting and launch verification

Enable GitHub private vulnerability reporting, then verify that an outside
GitHub account sees **Security Ã¢â€ â€™ Report a vulnerability** and the owner receives
the private report. No email route is part of this repository's policy.

During the owner-authorized publication transition, keep Actions disabled
until public protections are installed and read back. Old repository history,
releases and logs remain private in `synthius-dendrite-private-archive`; the public
repository has its own immutable identity and clean source history.

Verify a harmless external PR cannot merge without owner approval, stale
approval is dismissed, failed checks block acceptance, direct contributor main
pushes are denied, and non-owner distribution dispatch/reruns are skipped.
Then validate the exact final publication commit and existing public surfaces.
