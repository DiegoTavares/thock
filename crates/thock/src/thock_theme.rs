//! The Thock theme (V43 §5): the desk's palette, built from the phone's
//! tokens and shipped as `assets/themes/thock/thock.json`, which the theme
//! registry loads like any bundled family. This module holds the one colour
//! the panels need that a theme has no key for, and the tests that keep the
//! palette readable.

use gpui::{App, Hsla};
use ui::ActiveTheme as _;

pub const THOCK_DARK: &str = "Thock Dark";
pub const THOCK_LIGHT: &str = "Thock Light";

/// Text drawn on a surface filled with the accent colour — today's cell in
/// the strip, a checked box. Themes have no key for it; the window background
/// is the phone's `amberInk` in both Thock themes, and in any other theme it
/// is the colour the accent was chosen to stand out against.
pub fn on_accent_color(cx: &App) -> Hsla {
    cx.theme().colors().background
}

#[cfg(test)]
pub(crate) const THEME_JSON: &[u8] = include_bytes!("../../../assets/themes/thock/thock.json");

/// Loads the shipped theme family into the registry and makes `name` the
/// active theme, for tests that read the palette through `cx.theme()`.
#[cfg(test)]
pub(crate) fn activate_for_tests(name: &str, cx: &mut App) {
    use gpui::BorrowAppContext as _;
    use settings::{SettingsStore, ThemeName, ThemeSelection};
    theme_settings::load_user_theme(&theme::ThemeRegistry::global(cx), THEME_JSON)
        .expect("the shipped Thock theme parses");
    cx.update_global::<SettingsStore, _>(|store, cx| {
        store.update_user_settings(cx, |content| {
            content.theme.theme = Some(ThemeSelection::Static(ThemeName(name.into())));
        });
    });
    theme_settings::reload_theme(cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use theme::AppearanceContent;
    use theme_settings::{ThemeContent, ThemeFamilyContent};

    fn family() -> ThemeFamilyContent {
        theme_settings::deserialize_user_theme(THEME_JSON).expect("the theme file parses")
    }

    fn theme(name: &str) -> ThemeContent {
        family()
            .themes
            .into_iter()
            .find(|theme| theme.name == name)
            .unwrap_or_else(|| panic!("{name} is in the family"))
    }

    fn channel(hex: &str, index: usize) -> f64 {
        let value = u8::from_str_radix(&hex[index..index + 2], 16).expect("a hex colour");
        let value = f64::from(value) / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    /// WCAG 2 relative luminance of a `#rrggbb[aa]` colour.
    fn luminance(hex: &str) -> f64 {
        let hex = hex.trim_start_matches('#');
        0.2126 * channel(hex, 0) + 0.7152 * channel(hex, 2) + 0.0722 * channel(hex, 4)
    }

    fn contrast(foreground: &str, background: &str) -> f64 {
        let (a, b) = (luminance(foreground), luminance(background));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn colour<'a>(
        theme: &'a ThemeContent,
        pick: fn(&'a ThemeContent) -> &'a Option<String>,
    ) -> &'a str {
        pick(theme).as_deref().expect("the theme sets the colour")
    }

    #[test]
    fn the_family_holds_a_dark_and_a_light_theme() {
        let family = family();
        assert_eq!(family.name, "Thock");
        let names: Vec<_> = family
            .themes
            .iter()
            .map(|theme| (theme.name.as_str(), theme.appearance))
            .collect();
        assert_eq!(
            names,
            [
                (THOCK_DARK, AppearanceContent::Dark),
                (THOCK_LIGHT, AppearanceContent::Light),
            ]
        );
    }

    #[test]
    fn text_stays_readable_on_every_surface() {
        for name in [THOCK_DARK, THOCK_LIGHT] {
            let theme = theme(name);
            let ground = colour(&theme, |theme| &theme.style.colors.background);
            let surface = colour(&theme, |theme| &theme.style.colors.panel_background);
            let ink = colour(&theme, |theme| &theme.style.colors.text);
            let muted = colour(&theme, |theme| &theme.style.colors.text_muted);
            let dim = colour(&theme, |theme| &theme.style.colors.text_placeholder);
            let amber = colour(&theme, |theme| &theme.style.colors.text_accent);
            for background in [ground, surface] {
                assert!(
                    contrast(ink, background) >= 7.0,
                    "{name}: ink on {background}"
                );
                assert!(
                    contrast(muted, background) >= 4.5,
                    "{name}: muted on {background}"
                );
                assert!(
                    contrast(dim, background) >= 3.0,
                    "{name}: dim on {background}"
                );
            }
            // `on_accent_color` is the window background (amberInk).
            assert!(contrast(ground, amber) >= 4.5, "{name}: amberInk on amber");
        }
    }

    #[test]
    fn headings_are_theme_addressable() {
        for name in [THOCK_DARK, THOCK_LIGHT] {
            let theme = theme(name);
            let syntax = &theme.style.syntax;
            assert_eq!(
                syntax["title.h1"].font_weight.map(|weight| weight.0),
                Some(600.0)
            );
            assert_eq!(
                syntax["title.h2"].font_style,
                Some(settings::FontStyleContent::Italic)
            );
            assert_eq!(
                syntax["title.h2"].color.as_deref(),
                theme.style.colors.text_accent.as_deref(),
                "{name}: the amber italic h2"
            );
        }
    }
}
