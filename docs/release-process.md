# Release Process

LG Buddy uses three persistent branches as source channels:

- `main` points to the current stable release.
- `prerelease` is equal to `main` or ahead of it and points to the current
  prerelease when ahead.
- `dev` is the ordinary integration branch and may contain unreleased work.

The intended ancestry is `main <= prerelease <= dev`. Ordinary changes merge
into `dev`. An official release requires a promotion PR whose head is the
same-repository `dev` branch and whose base is `main` or `prerelease`.

## Promotion contract

The promotion PR is the review and approval surface. Required checks gate its
merge, and merging the PR is the release authorization. The merged target-branch
commit is the release commit; there is no separate approval label or publish
action.

The release App is not a substitute for merging. After the merged commit has
passed the release build and smoke test, the App performs protected tag and
release writes and keeps the persistent streams aligned. A prerelease merge
advances `dev` to the merged prerelease commit. A stable merge advances both
`prerelease` and `dev` to the merged stable commit.

Required promotion checks prove that:

- the PR is `dev -> main` or `dev -> prerelease`
- `Cargo.toml` and `Cargo.lock` declare the same `lg-buddy` version
- a stable target has a stable SemVer and a prerelease target has a prerelease
  SemVer
- the version advances both existing release-channel heads
- the persistent branches have not moved or diverged before merge
- the derived `v<crate-version>` tag is absent before merge
- normal CI and the release-bundle smoke test pass

The tag, binary, archive, and GitHub release all use the Cargo package version.
There is no separate version input.

## Creating a release

1. Prepare the exact release version in `Cargo.toml` and `Cargo.lock` on `dev`.
2. Open a PR from `dev` to `prerelease` or `main`.
3. Wait for `verify`, `bundle-smoke-test`, and `validate-promotion` to pass.
4. Merge the promotion PR. This is the release authorization.
5. The resulting push starts the serialized release workflow, which builds and
   smoke-tests the merged release commit without write credentials.
6. The final job obtains a short-lived token from the dedicated repository-only
   GitHub App, aligns the remaining release streams, publishes the immutable tag
   and release, and verifies the published checksums.

Do not push version tags manually. Protected `v*` tags and stream-alignment
writes permit bypass only to the dedicated release App. A failed post-merge
release run can be rerun safely, but an existing tag or asset is accepted only
when it is byte-for-byte consistent with the merged release commit.

## What the release workflow validates

The workflow:

1. Validates the merged target-branch commit, Cargo version, and tag state.
2. Builds a static `x86_64-unknown-linux-musl` binary with exact version and
   commit identity.
3. Generates a versioned identity manifest and packages the release bundle.
4. Validates the manifest and installs the bundle in an isolated smoke-test root.
5. Verifies the built and installed binary's exact version, channel, and commit.
6. Generates and verifies `sha256sums.txt`.
7. Keeps `main`, `prerelease`, and `dev` aligned for the next promotion.
8. Publishes the tag and GitHub release without replacing conflicting assets.
9. Downloads the published assets and verifies their checksums independently.

`install.sh` is only an installer. It does not build the runtime.

## Release bundle identity

Every release archive contains `release-manifest.json` at the bundle root. The
schema-versioned JSON records the exact release tag, semantic version, release
channel, Rust target triple, and full lowercase commit SHA. Version, tag, and
channel must agree: stable SemVer maps to `stable`, prerelease SemVer maps to
`prerelease`, and the tag is exactly `v<version>`.

Schema 1 marks all five identity fields as critical. Validators reject an
unsupported schema, duplicate JSON fields, missing identity fields, and unknown
critical fields. Unknown non-critical fields may be ignored for compatible
schema evolution. Official manifests use a deterministic field order and JSON
rendering.

The bundle builder derives version, channel, and commit from `lg-buddy
--version`; it cannot package a binary whose identity disagrees with its tag.
Smoke validation checks the manifest before executing installer code and then
compares it with both the bundled and installed binary. Publishing validates
the manifest directly from each archive without extracting or executing archive
content.

## Upgrade compatibility preflight

Release-bundle replacement is guarded by observed capability rather than a
distribution allowlist or provenance receipt. The initial runtime preflight
checks the installed mutable FHS topology, config discovery, ownership, path
types and mount boundaries, trusted system containment, integration override
state, and system/user service-manager availability before an updater performs
release or privilege-related effects. A verified candidate's binary performs a
second pass for its own installer requirements and trusted external ancestor
chain before privileged mutation.
The checker assigns each target an installer-operation policy so replacement,
directory mutation, recursive repair, exact drop-in, and candidate-input
requirements cannot silently lose their operation-specific safeguards.

The extracted candidate exposes this second pass through the hidden
`upgrade-preflight` installer entrypoint. `install.sh --upgrade` invokes it
before sudo or installation writes, loads the existing config pointer and
settings without rewriting them, and never runs configuration, discovery, or
pairing. Native and healthy compatibility installations preserve their Python
environment; an unhealthy compatibility environment must pass the additional
recursive-repair checks before it is rebuilt. After replacing owned runtime and
integration files, the installer reloads system integrations before user
integrations and verifies that the installed binary matches the candidate.

This is intentionally a conservative and evolving refusal boundary. It does
not migrate legacy layouts, declare broad host support, or guarantee that a
later privileged operation cannot fail.

## Nix source selection

Nix configurations may select `main`, `prerelease`, or `dev` as the upstream
source according to the desired stability. The Nix lock file must continue to
pin an exact commit: the branch selects the update stream, not an implicitly
moving deployment.

This source selection is separate from LG Buddy's runtime `updates.channel`
setting, which controls GitHub release discovery for installed release bundles.

## Installing from a release bundle

End users can extract the release archive and run:

```bash
./install.sh
```

That path uses the bundled `lg-buddy` binary and does not require a Rust toolchain.

To update an existing compatible release-bundle installation from an already
verified and extracted newer bundle, run as the installed user:

```bash
./install.sh --upgrade
```

An incompatible or legacy layout is refused rather than migrated. If a failure
occurs after installation writes begin, correct the reported cause and rerun the
same verified bundle with `--upgrade`.

## Installing a locally built binary

If you build `lg-buddy` yourself, install it by passing the binary path explicitly:

```bash
./install.sh --runtime-binary ./target/release/lg-buddy
```
