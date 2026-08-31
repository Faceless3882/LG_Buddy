include!(concat!(env!("OUT_DIR"), "/release_identity.rs"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionInfo {
    version: &'static str,
    channel: ReleaseChannel,
    commit: Option<&'static str>,
}

impl VersionInfo {
    pub fn current() -> Self {
        let release_version = non_empty(option_env!("LG_BUDDY_RELEASE_VERSION"));

        Self {
            version: release_version.unwrap_or(env!("CARGO_PKG_VERSION")),
            channel: ReleaseChannel::from_release_version(release_version),
            commit: option_env!("LG_BUDDY_BUILD_COMMIT"),
        }
    }

    pub fn version(self) -> &'static str {
        self.version
    }

    pub fn commit(self) -> Option<&'static str> {
        non_empty(self.commit)
    }

    pub fn channel(self) -> ReleaseChannel {
        self.channel
    }

    pub fn is_release_build(self) -> bool {
        self.channel() != ReleaseChannel::Dev
    }

    pub fn render(self) -> String {
        format!(
            "lg-buddy {}\nversion: {}\nchannel: {}\ncommit: {}\n",
            self.version(),
            self.version(),
            self.channel().as_str(),
            self.commit().unwrap_or("unknown")
        )
    }

    #[cfg(test)]
    pub(crate) fn for_testing(
        version: &'static str,
        channel: ReleaseChannel,
        commit: Option<&'static str>,
    ) -> Self {
        Self {
            version,
            channel,
            commit,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseChannel {
    Dev,
    Prerelease,
    Stable,
}

impl ReleaseChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Prerelease => "prerelease",
            Self::Stable => "stable",
        }
    }

    fn from_release_version(version: Option<&str>) -> Self {
        match version {
            None => Self::Dev,
            Some(version) if is_prerelease_version(version) => Self::Prerelease,
            Some(_) => Self::Stable,
        }
    }
}

pub fn version_text() -> String {
    std::hint::black_box(&LG_BUDDY_EMBEDDED_RELEASE_IDENTITY);
    VersionInfo::current().render()
}

fn non_empty(value: Option<&'static str>) -> Option<&'static str> {
    value.and_then(|value| {
        let value = value.trim();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    })
}

fn is_prerelease_version(version: &str) -> bool {
    version.contains('-')
}

#[cfg(test)]
mod tests {
    use super::{version_text, ReleaseChannel, VersionInfo, LG_BUDDY_EMBEDDED_RELEASE_IDENTITY};

    #[test]
    fn dev_version_output_reports_package_version_channel_and_unknown_commit() {
        let info = VersionInfo {
            version: "1.1.0",
            channel: ReleaseChannel::Dev,
            commit: None,
        };

        assert_eq!(
            info.render(),
            "lg-buddy 1.1.0\nversion: 1.1.0\nchannel: dev\ncommit: unknown\n"
        );
        assert!(!info.is_release_build());
    }

    #[test]
    fn stable_release_output_reports_version_channel_and_commit() {
        let info = VersionInfo {
            version: "1.1.0",
            channel: ReleaseChannel::Stable,
            commit: Some("7f1fb0c"),
        };

        assert_eq!(
            info.render(),
            "lg-buddy 1.1.0\nversion: 1.1.0\nchannel: stable\ncommit: 7f1fb0c\n"
        );
        assert!(info.is_release_build());
    }

    #[test]
    fn prerelease_channel_is_derived_from_release_version_suffix() {
        assert_eq!(
            ReleaseChannel::from_release_version(Some("1.1.0-beta.1")),
            ReleaseChannel::Prerelease
        );
    }

    #[test]
    fn commit_metadata_is_trimmed_and_blank_commit_metadata_is_treated_as_absent() {
        let info = VersionInfo {
            version: "1.1.0",
            channel: ReleaseChannel::Stable,
            commit: Some("  7f1fb0c  "),
        };

        assert_eq!(
            info.render(),
            "lg-buddy 1.1.0\nversion: 1.1.0\nchannel: stable\ncommit: 7f1fb0c\n"
        );

        let info = VersionInfo {
            version: "1.1.0",
            channel: ReleaseChannel::Stable,
            commit: Some(""),
        };

        assert_eq!(
            info.render(),
            "lg-buddy 1.1.0\nversion: 1.1.0\nchannel: stable\ncommit: unknown\n"
        );
    }

    #[test]
    fn current_version_output_mentions_package_version() {
        let output = version_text();

        assert!(output.starts_with(&format!("lg-buddy {}\n", env!("CARGO_PKG_VERSION"))));
        assert!(output.contains("version: "));
        assert!(output.contains("channel: "));
        assert!(output.contains("commit: "));
    }

    #[test]
    fn embedded_identity_matches_the_current_build_metadata() {
        const PREFIX: &[u8] = b"LG_BUDDY_RELEASE_IDENTITY_V1\0";
        const SUFFIX: &[u8] = b"\0LG_BUDDY_RELEASE_IDENTITY_END\0";
        let record = &LG_BUDDY_EMBEDDED_RELEASE_IDENTITY;
        assert!(record.starts_with(PREFIX));
        assert!(record.ends_with(SUFFIX));
        let payload = &record[PREFIX.len()..record.len() - SUFFIX.len()];
        let manifest: serde_json::Value =
            serde_json::from_slice(payload).expect("embedded identity JSON");
        let current = VersionInfo::current();
        assert_eq!(manifest["version"], current.version());
        assert_eq!(manifest["release_tag"], format!("v{}", current.version()));
        assert_eq!(manifest["channel"], current.channel().as_str());
        assert_eq!(manifest["commit"], current.commit().unwrap_or("unknown"));
        assert!(manifest["target"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
    }
}
