# Contributing

Thanks for helping improve LG Buddy. This project touches desktop sessions,
systemd units, shell installers, and real TVs, so small changes are easiest to
review when they stay focused and explicit.

## Mandatory Reading

Before opening a PR, read:

- [Development](docs/development.md)
- [Testing strategy](docs/testing-strategy.md)

For changes in these areas, also read the matching design docs:

- runtime architecture: [Architecture overview](docs/architecture-overview.md)
- session backend behavior: [Session backend model](docs/session-backend-model.md)
- release packaging: [Release process](docs/release-process.md)

## Pull Request Scope

Keep one concern per pull request.

Base ordinary contribution PRs on `dev` and target `dev`. The persistent
`main` and `prerelease` branches accept only release promotion PRs whose head is
the exact same-repository `dev` branch; see the
[release process](docs/release-process.md).

Good PRs usually fit one of these shapes:

- a single confirmed bug fix
- one documented behavior change
- one refactor with no behavior change
- one documentation update
- one focused test improvement

Do not bundle changes with different acceptance thresholds. A low-risk bug fix,
a user-facing behavior change, a setup/configuration change, and a broad
refactor are different review decisions with different regression risks.

Avoid drive-by formatting, cleanup, dependency changes, or unrelated test edits
inside a behavior PR. If they are worth doing, open a separate PR.

Maintainers may ask for a PR to be split before review continues.

## When To Discuss First

Open an issue or comment on an existing issue before investing in changes that
affect:

- Wake-on-LAN targeting, networking assumptions, or cross-subnet behavior
- installer prompts, config file shape, defaults, or migration behavior
- systemd units, shutdown/sleep/wake ordering, or privilege boundaries
- GNOME, `swayidle`, or other desktop-session backend semantics
- release packaging, installed file layout, or dependency policy
- broad architecture, large refactors, or new external dependencies

Small, obvious bug fixes can go straight to a PR, but the PR should still
explain the bug and why the fix is safe.

## Pull Request Description

Every PR should explain:

- what problem it fixes
- what changed
- what user-visible behavior changed, if any
- what validation was run
- which issue it fixes or relates to, if applicable

If the change intentionally does not handle a case, say so. Clear limits are
better than reviewers having to infer them from the patch.

## Tests And Validation

New behavior must include tests at all three levels described in
[Testing strategy](docs/testing-strategy.md):

- module behavior
- module interoperability
- user needs

The full test suite should pass before a PR is marked ready for review. Mention
the validation you ran in the PR description, including any relevant manual or
hardware smoke checks.

## Style

Match the existing code and documentation style. Prefer small, boring patches
over clever rewrites.

Rust changes should:

- keep behavior in the module that owns it
- add focused tests near the changed behavior when practical
- avoid adding public API or configuration surface unless the use case is clear

Shell changes should:

- stay POSIX-shell compatible where the existing script expects it
- quote variables deliberately
- keep installer and configuration behavior non-destructive by default

Documentation changes should:

- describe current behavior
- avoid promising behavior that is not implemented
- link to deeper docs instead of duplicating large sections
