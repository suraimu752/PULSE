//! Light, dark, and "follow Windows" theme choice.
//!
//! The window stores the same codes: 0 light, 1 dark, 2 system.
//! System reads `AppsUseLightTheme` (0 = dark, anything else = light).
//! When that value cannot be read, system mode stays light.

/// Which appearance the user asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemePreference {
    Light,
    Dark,
    System,
}

impl ThemePreference {
    pub fn from_code(code: i32) -> Self {
        match code {
            0 => Self::Light,
            1 => Self::Dark,
            _ => Self::System,
        }
    }

    pub fn code(self) -> i32 {
        match self {
            Self::Light => 0,
            Self::Dark => 1,
            Self::System => 2,
        }
    }
}

/// Interpret the `AppsUseLightTheme` DWORD. Zero is the dark app theme.
pub fn light_from_dword(value: u32) -> bool {
    value != 0
}

/// Resolve the preference against the last system reading.
///
/// `system_is_light == None` means the registry value was unavailable, so
/// system mode uses light rather than inventing a dark default.
pub fn effective_dark(preference: ThemePreference, system_is_light: Option<bool>) -> bool {
    match preference {
        ThemePreference::Light => false,
        ThemePreference::Dark => true,
        ThemePreference::System => !system_is_light.unwrap_or(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dword_zero_is_dark_and_other_values_are_light() {
        assert!(!light_from_dword(0));
        assert!(light_from_dword(1));
        assert!(light_from_dword(2));
    }

    #[test]
    fn system_mode_follows_the_reading_and_defaults_light() {
        assert!(!effective_dark(ThemePreference::Light, Some(false)));
        assert!(effective_dark(ThemePreference::Dark, Some(true)));
        assert!(effective_dark(ThemePreference::System, Some(false)));
        assert!(!effective_dark(ThemePreference::System, Some(true)));
        assert!(!effective_dark(ThemePreference::System, None));
        assert_eq!(ThemePreference::from_code(2).code(), 2);
        assert_eq!(ThemePreference::from_code(9), ThemePreference::System);
    }
}
