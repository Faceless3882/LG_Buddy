# Release Process

LG Buddy release automation is triggered by pushing a version tag that starts
with `v` followed by a digit, such as `v1.0.0` or `v1.0.0-beta.1`.

## What the release workflow does

1. Runs the same validation as CI:
   - `cargo test -p lg-buddy --lib`
   - `cargo test -p lg-buddy --test cucumber`
   - `cargo clippy -p lg-buddy --all-targets --all-features -- -D warnings`
   - `bash -n install.sh uninstall.sh configure.sh bin/LG_Buddy_Common scripts/build-release-bundle.sh scripts/test-release-bundle.sh scripts/publish-release-assets.sh`
2. Builds a static Linux binary for `x86_64-unknown-linux-musl`.
3. Packages a release bundle that contains:
   - `lg-buddy`
   - `install.sh`
   - `configure.sh`
   - `uninstall.sh`
   - `bin/LG_Buddy_Common`
   - `systemd/`
   - `docs/`
   - `README.md`
   - `LICENSE`
4. Smoke tests the generated release bundle by unpacking it, running a non-interactive install into a temporary root, checking both TV platform modes and the lifecycle service plus NetworkManager pre-down hook topology, and then uninstalling from that temporary install.
5. Generates and verifies `sha256sums.txt` for the release archive.
6. Publishes the release assets through `scripts/publish-release-assets.sh`.

`install.sh` is only an installer. It does not build the runtime.

## Creating a release

1. Make sure the branch you want to release has passed CI.
2. Create a tag such as `v0.7.0`.
3. Push the tag.

```bash
git tag v0.7.0
git push origin v0.7.0
```

## Installing from a release bundle

End users can extract the release archive and run:

```bash
./install.sh
```

That path uses the bundled `lg-buddy` binary and does not require a Rust toolchain.

## Installing a locally built binary

If you build `lg-buddy` yourself, install it by passing the binary path explicitly:

```bash
./install.sh --runtime-binary ./target/release/lg-buddy
```
