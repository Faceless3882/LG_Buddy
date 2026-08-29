use std::io;

use super::{
    screen, EffectiveSetting, SettingAlias, SettingOperation, SettingType, SettingsApplyOutcome,
    SettingsChange, SettingsError, SettingsMutationAction,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct SettingsFormatter;

impl SettingsFormatter {
    pub fn write_get<W: io::Write>(
        &self,
        writer: &mut W,
        setting: EffectiveSetting,
    ) -> Result<(), SettingsError> {
        writeln!(writer, "{}", setting.required_value()?).map_err(output_error)
    }

    pub fn write_list<W: io::Write>(
        &self,
        writer: &mut W,
        settings: &[EffectiveSetting],
    ) -> Result<(), SettingsError> {
        for setting in settings {
            writeln!(
                writer,
                "{}={} ({}, {}, ops: {})",
                setting.key_name(),
                format_effective_value(setting),
                setting.source().as_str(),
                setting.definition().mutability().as_str(),
                format_operations(setting.definition().supported_operations(), ",")
            )
            .map_err(output_error)?;
        }

        Ok(())
    }

    pub fn write_describe<W: io::Write>(
        &self,
        writer: &mut W,
        settings: &[EffectiveSetting],
    ) -> Result<(), SettingsError> {
        self.write_describe_with_backend(writer, settings, screen::BackendPresentation::Raw)
    }

    pub(super) fn write_describe_with_backend<W: io::Write>(
        &self,
        writer: &mut W,
        settings: &[EffectiveSetting],
        screen_backend: screen::BackendPresentation,
    ) -> Result<(), SettingsError> {
        for (index, setting) in settings.iter().enumerate() {
            if index > 0 {
                writeln!(writer).map_err(output_error)?;
            }

            self.write_single_description(writer, setting, screen_backend)?;
        }

        Ok(())
    }

    pub fn write_change<W: io::Write>(
        &self,
        writer: &mut W,
        change: &SettingsChange,
        apply: &SettingsApplyOutcome,
    ) -> Result<(), SettingsError> {
        let mutation = change.mutation();

        match (mutation.action(), change.file_changed()) {
            (SettingsMutationAction::Set, true) => {
                writeln!(
                    writer,
                    "{}={} (saved to {})",
                    mutation.key_name(),
                    mutation.new_value()?,
                    change.path().display()
                )
                .map_err(output_error)?;
            }
            (SettingsMutationAction::Set, false) => {
                writeln!(
                    writer,
                    "{} already set to {} ({})",
                    mutation.key_name(),
                    mutation.new_value()?,
                    change.path().display()
                )
                .map_err(output_error)?;
            }
            (SettingsMutationAction::Unset, true) => {
                writeln!(
                    writer,
                    "{} unset (saved to {})",
                    mutation.key_name(),
                    change.path().display()
                )
                .map_err(output_error)?;
            }
            (SettingsMutationAction::Unset, false) => {
                writeln!(
                    writer,
                    "{} already unset ({})",
                    mutation.key_name(),
                    change.path().display()
                )
                .map_err(output_error)?;
            }
        }

        if !change.file_changed() {
            writeln!(writer, "config: unchanged").map_err(output_error)?;
        }

        writeln!(writer, "apply: {apply}").map_err(output_error)?;
        Ok(())
    }

    fn write_single_description<W: io::Write>(
        &self,
        writer: &mut W,
        setting: &EffectiveSetting,
        screen_backend: screen::BackendPresentation,
    ) -> Result<(), SettingsError> {
        let definition = setting.definition();

        writeln!(writer, "{}", setting.key_name()).map_err(output_error)?;
        writeln!(writer, "  storage key: {}", setting.storage_key()).map_err(output_error)?;
        writeln!(writer, "  type: {}", definition.value_type().as_str()).map_err(output_error)?;
        writeln!(
            writer,
            "  current: {}",
            format_described_value(setting, screen_backend)
        )
        .map_err(output_error)?;
        writeln!(writer, "  source: {}", setting.source().as_str()).map_err(output_error)?;
        writeln!(writer, "  default: {}", definition.default_value_label())
            .map_err(output_error)?;
        writeln!(writer, "  mutability: {}", definition.mutability().as_str())
            .map_err(output_error)?;
        writeln!(
            writer,
            "  supported operations: {}",
            format_operations(definition.supported_operations(), ", ")
        )
        .map_err(output_error)?;

        match definition.value_type() {
            SettingType::Enum(enum_type) => {
                writeln!(
                    writer,
                    "  allowed values: {}",
                    format_described_enum_values(setting, enum_type.values(), screen_backend)
                )
                .map_err(output_error)?;
                if !enum_type.aliases().is_empty() {
                    writeln!(writer, "  aliases: {}", format_aliases(enum_type.aliases()))
                        .map_err(output_error)?;
                }
            }
            SettingType::Integer(integer_type) => {
                writeln!(
                    writer,
                    "  range: {}..={}",
                    integer_type.min(),
                    integer_type.max()
                )
                .map_err(output_error)?;
            }
            SettingType::Ipv4 | SettingType::MacAddress => {}
        }

        writeln!(writer, "  apply: {}", definition.apply_strategy().as_str())
            .map_err(output_error)?;
        writeln!(writer, "  description: {}", definition.description()).map_err(output_error)?;
        Ok(())
    }
}

fn format_operations(operations: &[SettingOperation], separator: &str) -> String {
    operations
        .iter()
        .map(|operation| operation.as_str())
        .collect::<Vec<_>>()
        .join(separator)
}

pub(super) fn format_effective_value(setting: &EffectiveSetting) -> String {
    if let Some(value) = setting.invalid_value() {
        return format!("<invalid: {value}>");
    }

    setting
        .value()
        .map(|value| value.to_string())
        .unwrap_or_else(|| "<missing>".to_string())
}

fn format_described_value(
    setting: &EffectiveSetting,
    screen_backend: screen::BackendPresentation,
) -> String {
    let value = format_effective_value(setting);
    if setting.key_name() == "screen.backend" {
        screen::format_backend_choice(&value, screen_backend)
    } else {
        value
    }
}

fn format_described_enum_values(
    setting: &EffectiveSetting,
    values: &[&str],
    screen_backend: screen::BackendPresentation,
) -> String {
    if setting.key_name() != "screen.backend" {
        return values.join(", ");
    }

    values
        .iter()
        .map(|value| screen::format_backend_choice(value, screen_backend))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_aliases(aliases: &[SettingAlias]) -> String {
    aliases
        .iter()
        .map(|alias| format!("{} -> {}", alias.from(), alias.to()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn output_error(err: io::Error) -> SettingsError {
    SettingsError::WriteOutput(err.to_string())
}
