use std::env;
use std::fs;
use std::path::PathBuf;

const PREFIX: &str = "LG_BUDDY_RELEASE_IDENTITY_V1\0";
const SUFFIX: &str = "\0LG_BUDDY_RELEASE_IDENTITY_END\0";

fn main() {
    println!("cargo:rerun-if-env-changed=LG_BUDDY_RELEASE_VERSION");
    println!("cargo:rerun-if-env-changed=LG_BUDDY_BUILD_COMMIT");

    let package_version = env::var("CARGO_PKG_VERSION").expect("Cargo package version");
    let release_version = env::var("LG_BUDDY_RELEASE_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let version = release_version.as_deref().unwrap_or(&package_version);
    let channel = match release_version.as_deref() {
        None => "dev",
        Some(value) if value.contains('-') => "prerelease",
        Some(_) => "stable",
    };
    let commit = env::var("LG_BUDDY_BUILD_COMMIT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let target = env::var("TARGET").expect("Cargo target triple");
    for (name, value) in [
        ("version", version),
        ("channel", channel),
        ("target", target.as_str()),
        ("commit", commit.as_str()),
    ] {
        assert!(
            value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')),
            "{name} contains unsupported release-identity characters"
        );
    }

    let manifest = format!(
        "{{\"schema_version\":1,\"critical\":[\"release_tag\",\"version\",\"channel\",\"target\",\"commit\"],\"release_tag\":\"v{version}\",\"version\":\"{version}\",\"channel\":\"{channel}\",\"target\":\"{target}\",\"commit\":\"{commit}\"}}"
    );
    let record = format!("{PREFIX}{manifest}{SUFFIX}").into_bytes();
    let bytes = record
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let generated = format!(
        "#[used]\n#[link_section = \".lg_buddy.identity\"]\nstatic LG_BUDDY_EMBEDDED_RELEASE_IDENTITY: [u8; {}] = [{bytes}];\n",
        record.len()
    );
    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR")).join("release_identity.rs");
    fs::write(output, generated).expect("write embedded release identity");
}
