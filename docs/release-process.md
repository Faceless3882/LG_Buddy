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

The promotion PR is the code and release-identity review surface. Required checks
gate its merge, and merging the PR authorizes preparation of a verified release
draft. The merged target-branch commit is the release commit. Publishing that
draft is a separate explicit approval after its release notes and assets have
been reviewed.

The release App is not a substitute for merging. After the merged commit has
passed the release build and smoke test, the App performs protected tag and
draft-release writes and keeps the persistent streams aligned. A prerelease merge
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
- normal CI and the release-bundle smoke test pass; the bundle smoke includes a
  pinned cross-version upgrade from `v1.4.0-beta.2`

Requiring every candidate to advance both release-channel heads keeps release
publication globally monotonic. The newest published release is therefore also
the highest semantic version and prerelease-channel clients do not scan release
history to determine ordering.

The tag, binary, archive, and GitHub release all use the Cargo package version.
There is no separate version input.

## Creating a release

1. Prepare the exact release version in `Cargo.toml` and `Cargo.lock` on `dev`.
2. Open a PR from `dev` to `prerelease` or `main`.
3. Wait for `verify`, `bundle-smoke-test`, and `validate-promotion` to pass.
4. Merge the promotion PR. This authorizes preparation of the release draft.
5. The resulting push starts the serialized release workflow, which builds and
   smoke-tests the merged release commit without write credentials, including
   an archive-driven upgrade from the pinned public baseline.
6. The staging job obtains a short-lived token from the dedicated repository-only
   GitHub App, aligns the remaining release streams, creates or resumes the exact
   release draft, uploads any missing assets, and verifies the complete asset set.
7. Fill in the draft's release-specific notes and review its title, classification,
   tag, and assets. Then approve the waiting `release-publication` environment.
   The approval does not accept an independent version, tag, commit, artifact, or
   workflow-run identifier.
8. The publication job downloads the same workflow artifact again, revalidates the
   merged refs, tag, checksums, manifest, draft classification, complete remote
   asset set, and non-placeholder notes, then makes only that draft public. It
   preserves the reviewed title and notes. Repository release immutability then
   locks the tag and assets.
9. A published prerelease then runs `v1.4.0-beta.2` through the real production
   `lg-buddy updates install` path. The newly installed candidate performs a
   cold-cache production update check, and the canary records sanitized GitHub
   response evidence for the deterministic mock. This is supplemental
   post-publication evidence, not a prerequisite for stable promotion.

Promotion through `prerelease` is optional. A direct `dev -> main` promotion
runs the same required PR validation and post-merge release build as a
`dev -> prerelease` promotion. During stable draft staging, the release App
fast-forwards both `prerelease` and `dev` to the reviewed stable commit.

Replace the draft's placeholder with concise, release-specific notes before
approving publication. The workflow does not impose a notes template, but it
refuses to publish empty, whitespace-only, or unchanged placeholder notes.

Do not push version tags manually. Protected `v*` tags and stream-alignment
writes permit bypass only to the dedicated release App. A failed post-merge
release run can be rerun safely: an incomplete draft remains private and is
resumed only when its tag, classification, and existing assets match the merged
release commit. If publication was attempted before the notes were ready, update
the draft, rerun the failed jobs, and approve the environment again. The staged
workflow artifact is retained for 35 days, leaving a margin beyond GitHub's
30-day maximum wait for an environment approval; after that, rerun the whole
workflow to rebuild it. A published release is accepted only when its expected
asset set is complete and byte-for-byte identical.

Repository release immutability must remain enabled. GitHub applies it only when
a draft is published, so the publisher uploads and verifies every expected asset
before making the release visible. Each stored asset must report the expected
name, uploaded state, byte size, and server-computed SHA-256 digest. Unexpected
draft assets block publication; published releases are verification-only and are
never repaired in place.

## What the release workflow validates

The workflow:

1. Validates the merged target-branch commit, Cargo version, and tag state.
2. Builds a static `x86_64-unknown-linux-musl` binary with exact version and
   commit identity.
3. Generates a versioned identity manifest and packages the release bundle.
4. Validates the manifest and installs the bundle in an isolated smoke-test root.
5. Verifies the built and installed binary's exact version, channel, and commit.
6. Upgrades a pinned real previous archive to the candidate and verifies
   preserved user state plus replaced owned integration files.
7. Generates and verifies `sha256sums.txt`.
8. Keeps `main`, `prerelease`, and `dev` aligned for the next promotion.
9. Stages and verifies the exact release assets privately.
10. Waits for release-note review and explicit publication approval.
11. Revalidates and publishes only the staged draft, preserving its reviewed
    title and notes.
12. Downloads the published assets and verifies their checksums independently.
13. For prereleases, exercises production GitHub discovery, acquisition,
    confirmation, installation, and final identity from the pinned baseline.

`install.sh` is only an installer. It does not build the runtime or GUI.

## Release bundle identity

Every release archive contains `release-manifest.json` at the bundle root. The
schema-versioned JSON records the exact release tag, semantic version, release
channel, headless Rust target triple, GTK GUI target triple, and full lowercase commit SHA. Version, tag, and
channel must agree: stable SemVer maps to `stable`, prerelease SemVer maps to
`prerelease`, and the tag is exactly `v<version>`.

Schema 1 keeps the original five identity fields critical and adds
`gui_target` as a required extension for GUI-bearing bundles. Keeping it
non-critical lets the public v1.4 updater read the new manifest. Validators reject an
unsupported schema, duplicate JSON fields, missing identity fields, and unknown
critical fields. Unknown non-critical fields may be ignored for compatible
schema evolution. Official manifests use a deterministic field order and JSON
rendering.

The bundle builder derives version, channel, and commit from both executables;
it cannot package binaries whose embedded identities, targets, or `--version`
output disagree. The archive remains named for the static
`x86_64-unknown-linux-musl` runtime. The dynamic
`x86_64-unknown-linux-gnu` GUI is stored as
`docs/lg-buddy-gui-x86_64-unknown-linux-gnu`: that established namespace is
understood by the v1.4 updater. The named application icon is likewise carried
under `docs/` for compatibility. The installer publishes them as
`/usr/bin/lg-buddy-gui` and under the hicolor application-icon directory, and
safely restores GUI executable mode when an older updater extracted it as data.
Smoke validation checks the manifest before executing installer code and then
compares it with both the bundled and installed binary. Publishing validates
the manifest directly from each archive without extracting or executing archive
content. CI builds on Ubuntu 24.04, establishing GTK 4.14, libadwaita 1.5, and
a maximum GLIBC 2.39 symbol baseline. Fedora 43 and current Arch lanes install
and launch the same artifact under Xvfb, drive it without a pointer, inspect its
AT-SPI tree, and compare its light/dark and 1x/2x rendering; Ubuntu remains the
ABI baseline.

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

## Cross-version upgrade baseline

`v1.4.0-beta.2` is the first public updater-capable release and is the pinned
previous archive for the initial cross-version contract. Its
`x86_64-unknown-linux-musl` archive SHA-256 is
`883e6cb869cbe60988a195acac2e15864d904797edfefbb7d90052eff9a17d32`.
CI verifies that digest and the full release identity before extracting or
executing the baseline.

Versions before `v1.4.0-beta.2` do not contain `updates install`. They require
one normal manual installation of an updater-capable release before assisted
upgrades become available. Arbitrary historical upgrade support is not implied.

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

## Installing locally built binaries

If you build `lg-buddy` and `lg-buddy-gui` yourself, install them by passing both
binary paths explicitly:

```bash
./install.sh \
  --runtime-binary ./target/x86_64-unknown-linux-musl/release/lg-buddy \
  --gui-binary ./target/x86_64-unknown-linux-gnu/release/lg-buddy-gui
```
