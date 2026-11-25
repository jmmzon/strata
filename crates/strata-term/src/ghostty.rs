// SPDX-License-Identifier: MIT

//! Ghostty's installed resources and the look its user config sets, so the
//! pane matches the user's Ghostty: palette, font family and size.

use std::path::{Path, PathBuf};

use libghostty_vt::color::{Palette, Rgb};

/// The palette and font a Ghostty config and its theme set.
#[derive(Clone, Debug, Default)]
pub struct GhosttyLook {
    pub palette: Option<Palette>,
    pub font_family: Option<String>,
    pub font_size: Option<f64>,
}

/// Ghostty's `share/ghostty` directory: `$GHOSTTY_RESOURCES_DIR`, the one
/// next to the `ghostty` on `PATH`, or the usual system directories.
#[must_use]
pub fn resources_dir() -> Option<PathBuf> {
    let from_env = std::env::var_os("GHOSTTY_RESOURCES_DIR").map(PathBuf::from);
    let beside_binary = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("ghostty"))
            .find(|binary| binary.is_file())
            .and_then(|binary| binary.canonicalize().ok())
            .and_then(|binary| Some(binary.parent()?.parent()?.join("share/ghostty")))
    });
    from_env
        .into_iter()
        .chain(beside_binary)
        .chain([
            "/usr/share/ghostty".into(),
            "/usr/local/share/ghostty".into(),
        ])
        .find(|dir| dir.join("shell-integration").is_dir())
}

/// The terminfo directory Ghostty installs beside its resources.
#[must_use]
pub fn terminfo_dir(resources: &Path) -> Option<PathBuf> {
    let dir = resources.parent()?.join("terminfo");
    dir.is_dir().then_some(dir)
}

/// `$XDG_CONFIG_HOME/ghostty`, or `~/.config/ghostty`.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|dir| dir.join("ghostty"))
}

/// Reads the user config in `config_dir`; `dark` picks between the halves of
/// a `theme = light:…,dark:…` pair.
#[must_use]
pub fn read_look(config_dir: &Path, resources: Option<&Path>, dark: bool) -> GhosttyLook {
    let config = ["config", "config.ghostty"]
        .iter()
        .filter_map(|name| std::fs::read_to_string(config_dir.join(name)).ok())
        .collect::<Vec<_>>()
        .join("\n");
    let mut settings = Settings::default();
    if let Some(theme) = settings_value(&config, "theme").and_then(|value| pick_theme(value, dark))
        && let Some(text) = theme_file(&theme, config_dir, resources)
            .and_then(|file| std::fs::read_to_string(file).ok())
    {
        settings.apply(&text);
    }
    settings.apply(&config);
    settings.into_look()
}

#[derive(Default)]
struct Settings {
    entries: Vec<(u8, Rgb)>,
    font_family: Option<String>,
    font_size: Option<f64>,
}

impl Settings {
    fn apply(&mut self, text: &str) {
        for (key, value) in lines(text) {
            match key {
                "palette" => {
                    if let Ok((index, rgb)) = Palette::parse_palette_entry(value) {
                        self.entries.push((index.0, rgb));
                    }
                }
                // Repeats add fallbacks after the first; an empty value resets.
                "font-family" if value.is_empty() => self.font_family = None,
                "font-family" => {
                    self.font_family.get_or_insert_with(|| value.to_owned());
                }
                "font-size" => self.font_size = value.parse().ok().filter(|size: &f64| *size > 0.0),
                _ => {}
            }
        }
    }

    fn into_look(self) -> GhosttyLook {
        let palette = (!self.entries.is_empty()).then(|| {
            let mut palette = Palette::default();
            for (index, rgb) in self.entries {
                palette.0[usize::from(index)] = rgb;
            }
            palette
        });
        GhosttyLook {
            palette,
            font_family: self.font_family,
            font_size: self.font_size,
        }
    }
}

fn lines(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim(), unquote(value.trim())))
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}

fn settings_value<'a>(text: &'a str, wanted: &str) -> Option<&'a str> {
    lines(text)
        .filter(|(key, _)| *key == wanted)
        .map(|(_, value)| value)
        .last()
}

fn pick_theme(value: &str, dark: bool) -> Option<String> {
    if !value.contains("light:") && !value.contains("dark:") {
        return Some(value.to_owned()).filter(|value| !value.is_empty());
    }
    let wanted = if dark { "dark:" } else { "light:" };
    value
        .split(',')
        .find_map(|part| part.trim().strip_prefix(wanted))
        .map(|name| name.trim().to_owned())
}

fn theme_file(theme: &str, config_dir: &Path, resources: Option<&Path>) -> Option<PathBuf> {
    if Path::new(theme).is_absolute() {
        return Some(PathBuf::from(theme)).filter(|file| file.is_file());
    }
    std::iter::once(config_dir.join("themes"))
        .chain(resources.map(|dir| dir.join("themes")))
        .map(|dir| dir.join(theme))
        .find(|file| file.is_file())
}
