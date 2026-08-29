use std::fmt;

const SETTINGS_SUBCOMMANDS: &[&str] = &["list", "describe", "get", "set", "unset"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsCommand {
    List,
    Describe(Option<String>),
    Get(String),
    Set { key: String, value: String },
    Unset(String),
}

impl SettingsCommand {
    pub fn parse<I, S>(args: I) -> Result<Self, SettingsParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut args = args.into_iter();
        let Some(subcommand) = args.next() else {
            return Err(SettingsParseError::MissingSubcommand);
        };

        match subcommand.as_ref() {
            "list" => {
                let extra_args = collect_args(args);
                if extra_args.is_empty() {
                    Ok(Self::List)
                } else {
                    Err(SettingsParseError::UnexpectedArguments {
                        subcommand: "list",
                        arguments: extra_args,
                    })
                }
            }
            "describe" => {
                let key = args.next().map(|arg| arg.as_ref().to_string());
                let extra_args = collect_args(args);
                if extra_args.is_empty() {
                    Ok(Self::Describe(key))
                } else {
                    Err(SettingsParseError::UnexpectedArguments {
                        subcommand: "describe",
                        arguments: extra_args,
                    })
                }
            }
            "get" => {
                let key = args
                    .next()
                    .ok_or(SettingsParseError::MissingKey { subcommand: "get" })?;
                let extra_args = collect_args(args);
                if extra_args.is_empty() {
                    Ok(Self::Get(key.as_ref().to_string()))
                } else {
                    Err(SettingsParseError::UnexpectedArguments {
                        subcommand: "get",
                        arguments: extra_args,
                    })
                }
            }
            "set" => {
                let key = args
                    .next()
                    .ok_or(SettingsParseError::MissingKey { subcommand: "set" })?;
                let value = args
                    .next()
                    .ok_or(SettingsParseError::MissingValue { subcommand: "set" })?;
                let extra_args = collect_args(args);
                if extra_args.is_empty() {
                    Ok(Self::Set {
                        key: key.as_ref().to_string(),
                        value: value.as_ref().to_string(),
                    })
                } else {
                    Err(SettingsParseError::UnexpectedArguments {
                        subcommand: "set",
                        arguments: extra_args,
                    })
                }
            }
            "unset" => {
                let key = args.next().ok_or(SettingsParseError::MissingKey {
                    subcommand: "unset",
                })?;
                let extra_args = collect_args(args);
                if extra_args.is_empty() {
                    Ok(Self::Unset(key.as_ref().to_string()))
                } else {
                    Err(SettingsParseError::UnexpectedArguments {
                        subcommand: "unset",
                        arguments: extra_args,
                    })
                }
            }
            other => Err(SettingsParseError::UnknownSubcommand(other.to_string())),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Describe(_) => "describe",
            Self::Get(_) => "get",
            Self::Set { .. } => "set",
            Self::Unset(_) => "unset",
        }
    }

    pub fn is_mutation(&self) -> bool {
        matches!(self, Self::Set { .. } | Self::Unset(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsParseError {
    MissingSubcommand,
    UnknownSubcommand(String),
    MissingKey {
        subcommand: &'static str,
    },
    MissingValue {
        subcommand: &'static str,
    },
    UnexpectedArguments {
        subcommand: &'static str,
        arguments: Vec<String>,
    },
}

impl fmt::Display for SettingsParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSubcommand => {
                write!(
                    f,
                    "missing settings command; expected one of {}",
                    SETTINGS_SUBCOMMANDS.join(", ")
                )
            }
            Self::UnknownSubcommand(subcommand) => {
                write!(f, "unknown settings command `{subcommand}`")
            }
            Self::MissingKey { subcommand } => {
                write!(f, "missing setting key for `settings {subcommand}`")
            }
            Self::MissingValue { subcommand } => {
                write!(f, "missing setting value for `settings {subcommand}`")
            }
            Self::UnexpectedArguments {
                subcommand,
                arguments,
            } => {
                write!(
                    f,
                    "unexpected arguments for `settings {subcommand}`: {}",
                    arguments.join(" ")
                )
            }
        }
    }
}

impl std::error::Error for SettingsParseError {}

fn collect_args<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .map(|arg| arg.as_ref().to_string())
        .collect()
}
