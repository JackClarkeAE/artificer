//! Where Artificer keeps what a person makes and chooses: the Part Library,
//! the theme, preferences, and the workspace it reopens.
//!
//! Every system sets aside a per-user folder for exactly this, and hides it
//! from everyday browsing, so it is neither in the way nor easy to tidy away
//! by accident:
//!
//! - **Windows:** `%APPDATA%\Artificer`, in the hidden Roaming `AppData`
//!   folder. It is deliberately not `%LOCALAPPDATA%\Artificer`: that is the
//!   folder the installer puts the application in (its pack id is
//!   `Artificer`, ADR 0029), and uninstalling removes that folder whole.
//!   Keeping a person's parts there would delete them with the app.
//! - **macOS:** `~/Library/Application Support/Artificer`; `~/Library` is
//!   hidden in the Finder.
//! - **Linux and other Unix:** `$XDG_DATA_HOME/artificer`, else
//!   `~/.local/share/artificer`, inside a dot-folder.
//!
//! When the system names no such folder there is no answer rather than a
//! guess. The temporary folder in particular is emptied by the system, so a
//! library kept there would not survive a restart.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The folder, inside the data folder, that holds the Part Library.
pub const LIBRARY_FOLDER: &str = "catalog";
/// The note left in the library folder for anyone who finds it.
pub const LIBRARY_README: &str = "README.txt";

const LIBRARY_README_TEXT: &str = "\
This folder is your Artificer Part Library.

It holds every part saved into the library, each of its versions, and the
pictures the library shows for them. Artificer reads it every time it starts.

Deleting, moving or renaming this folder removes those parts from the
library. To keep a copy of your parts, back up the whole folder; to move
them to another computer, copy the whole folder to the same place there.
";

/// The systems whose conventions differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    OtherUnix,
}

impl Platform {
    /// The system this build runs on.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::OtherUnix
        }
    }
}

/// An environment variable that names an absolute folder on `platform`. A
/// relative value would land wherever the app happened to start, so it
/// counts as unset. The test is the target system's rather than this
/// build's, so each system's rules can be checked anywhere.
fn variable(
    platform: Platform,
    environment: &dyn Fn(&str) -> Option<OsString>,
    name: &str,
) -> Option<PathBuf> {
    let value = environment(name).filter(|value| !value.is_empty())?;
    let text = value.to_string_lossy();
    let absolute = match platform {
        Platform::Windows => {
            let bytes = text.as_bytes();
            text.starts_with(r"\\")
                || (bytes.len() >= 3
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && matches!(bytes[2], b'\\' | b'/'))
        }
        Platform::MacOs | Platform::OtherUnix => text.starts_with('/'),
    };
    absolute.then(|| PathBuf::from(value))
}

/// The per-user data folder on `platform`, reading the environment through
/// `environment`. `None` when the system names none.
#[must_use]
pub fn data_directory_for(
    platform: Platform,
    environment: &dyn Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    match platform {
        Platform::Windows => variable(platform, environment, "APPDATA")
            .or_else(|| {
                variable(platform, environment, "USERPROFILE")
                    .map(|profile| profile.join("AppData").join("Roaming"))
            })
            .map(|roaming| roaming.join("Artificer")),
        Platform::MacOs => variable(platform, environment, "HOME").map(|home| {
            home.join("Library")
                .join("Application Support")
                .join("Artificer")
        }),
        Platform::OtherUnix => variable(platform, environment, "XDG_DATA_HOME")
            .or_else(|| {
                variable(platform, environment, "HOME")
                    .map(|home| home.join(".local").join("share"))
            })
            .map(|data| data.join("artificer")),
    }
}

/// Where earlier builds kept the same things, when that differs from
/// [`data_directory_for`]: only on Windows, where they sat inside the
/// installer's folder.
#[must_use]
pub fn legacy_data_directory_for(
    platform: Platform,
    environment: &dyn Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    match platform {
        Platform::Windows => {
            variable(platform, environment, "LOCALAPPDATA").map(|local| local.join("Artificer"))
        }
        Platform::MacOs | Platform::OtherUnix => None,
    }
}

fn process_environment(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// The per-user data folder on this system.
#[must_use]
pub fn data_directory() -> Option<PathBuf> {
    data_directory_for(Platform::current(), &process_environment)
}

/// The Part Library's folder on this system: `ARTIFICER_CATALOG_DIR` when it
/// is set, otherwise the library folder in [`data_directory`].
#[must_use]
pub fn library_directory() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("ARTIFICER_CATALOG_DIR").filter(|value| !value.is_empty())
    {
        return Some(PathBuf::from(root));
    }
    data_directory().map(|data| data.join(LIBRARY_FOLDER))
}

/// Brings what an earlier build kept in the installer's folder over to the
/// safe one, once. Nothing already in the safe folder is overwritten, and
/// the old copies are left where they are, so a failure part-way loses
/// nothing. Returns what was copied.
pub fn migrate_legacy_data() -> io::Result<Vec<PathBuf>> {
    let platform = Platform::current();
    match (
        legacy_data_directory_for(platform, &process_environment),
        data_directory_for(platform, &process_environment),
    ) {
        (Some(legacy), Some(current)) => migrate(&legacy, &current),
        _ => Ok(Vec::new()),
    }
}

/// The things an earlier build kept in its data folder.
const MIGRATED_ENTRIES: [&str; 4] = [
    LIBRARY_FOLDER,
    "theme.json",
    "preferences.json",
    "current.artificer",
];

/// Copies each of the data folder's known entries from `legacy` into
/// `current` where `current` does not have it yet.
pub fn migrate(legacy: &Path, current: &Path) -> io::Result<Vec<PathBuf>> {
    if legacy == current || !legacy.is_dir() {
        return Ok(Vec::new());
    }
    let mut copied = Vec::new();
    for entry in MIGRATED_ENTRIES {
        let from = legacy.join(entry);
        let to = current.join(entry);
        if to.exists() {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&from) else {
            continue;
        };
        fs::create_dir_all(current)?;
        if metadata.is_dir() {
            copy_directory(&from, &to)?;
        } else if metadata.is_file() {
            copy_file_atomically(&from, &to)?;
        } else {
            continue;
        }
        copied.push(to);
    }
    Ok(copied)
}

/// Copies a folder into a new one beside `to` and renames it into place
/// last, so an interrupted copy leaves no half-library behind.
fn copy_directory(from: &Path, to: &Path) -> io::Result<()> {
    let staging = to.with_file_name(format!(
        ".{}.migrating-{}",
        to.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("data"),
        std::process::id()
    ));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    copy_tree(from, &staging)?;
    fs::rename(&staging, to)
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn copy_file_atomically(from: &Path, to: &Path) -> io::Result<()> {
    let staging = to.with_file_name(format!(
        ".{}.migrating-{}",
        to.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("data"),
        std::process::id()
    ));
    fs::copy(from, &staging)?;
    fs::rename(&staging, to)
}

/// Leaves a short note in the library folder saying what it is, so someone
/// who comes across it knows not to delete it. An existing note is kept.
pub fn write_library_readme(library: &Path) -> io::Result<()> {
    let path = library.join(LIBRARY_README);
    if path.exists() {
        return Ok(());
    }
    fs::create_dir_all(library)?;
    fs::write(path, LIBRARY_README_TEXT)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn environment(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let values = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
            .collect::<BTreeMap<_, _>>();
        move |name| values.get(name).cloned()
    }

    #[test]
    fn windows_keeps_data_out_of_the_installers_folder() {
        let windows = environment(&[
            ("APPDATA", r"C:\Users\ada\AppData\Roaming"),
            ("LOCALAPPDATA", r"C:\Users\ada\AppData\Local"),
            ("USERPROFILE", r"C:\Users\ada"),
        ]);
        let data = data_directory_for(Platform::Windows, &windows);
        let legacy = legacy_data_directory_for(Platform::Windows, &windows);
        assert_eq!(
            data,
            Some(PathBuf::from(r"C:\Users\ada\AppData\Roaming").join("Artificer"))
        );
        assert_eq!(
            legacy,
            Some(PathBuf::from(r"C:\Users\ada\AppData\Local").join("Artificer"))
        );
        assert_ne!(data, legacy, "uninstalling must not take the library");

        let without_appdata = environment(&[("USERPROFILE", r"C:\Users\ada")]);
        assert_eq!(
            data_directory_for(Platform::Windows, &without_appdata),
            Some(
                PathBuf::from(r"C:\Users\ada")
                    .join("AppData")
                    .join("Roaming")
                    .join("Artificer")
            ),
            "the profile's Roaming folder stands in for APPDATA"
        );
        let relative = environment(&[("APPDATA", r"Roaming")]);
        assert!(data_directory_for(Platform::Windows, &relative).is_none());
        assert!(data_directory_for(Platform::Windows, &environment(&[])).is_none());
    }

    #[test]
    fn macos_and_linux_use_their_hidden_per_user_folders() {
        let mac = environment(&[("HOME", "/Users/ada")]);
        assert_eq!(
            data_directory_for(Platform::MacOs, &mac),
            Some(PathBuf::from(
                "/Users/ada/Library/Application Support/Artificer"
            ))
        );
        let linux = environment(&[("HOME", "/home/ada")]);
        assert_eq!(
            data_directory_for(Platform::OtherUnix, &linux),
            Some(PathBuf::from("/home/ada/.local/share/artificer"))
        );
        let xdg = environment(&[("HOME", "/home/ada"), ("XDG_DATA_HOME", "/data/ada")]);
        assert_eq!(
            data_directory_for(Platform::OtherUnix, &xdg),
            Some(PathBuf::from("/data/ada/artificer"))
        );
        assert!(legacy_data_directory_for(Platform::MacOs, &mac).is_none());
        assert!(legacy_data_directory_for(Platform::OtherUnix, &linux).is_none());
    }

    #[test]
    fn with_no_home_there_is_no_folder_rather_than_a_temporary_one() {
        let empty = environment(&[]);
        for platform in [Platform::Windows, Platform::MacOs, Platform::OtherUnix] {
            assert_eq!(data_directory_for(platform, &empty), None, "{platform:?}");
        }
        // A relative value is not a home: it would land wherever the app
        // happened to start.
        let relative = environment(&[("HOME", "ada"), ("XDG_DATA_HOME", "data")]);
        assert_eq!(data_directory_for(Platform::OtherUnix, &relative), None);
    }

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "artificer-user-data-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn migration_copies_what_is_missing_and_overwrites_nothing() {
        let root = scratch("migrate");
        let legacy = root.join("Local").join("Artificer");
        let current = root.join("Roaming").join("Artificer");
        fs::create_dir_all(legacy.join("catalog").join("refs").join("part")).unwrap();
        fs::write(
            legacy
                .join("catalog")
                .join("refs")
                .join("part")
                .join("1.0.0.ref"),
            "digest\n",
        )
        .unwrap();
        fs::write(legacy.join("theme.json"), "old theme").unwrap();
        fs::write(legacy.join("preferences.json"), "old preferences").unwrap();
        fs::create_dir_all(legacy.join("current")).unwrap();
        fs::write(legacy.join("current").join("Artificer.exe"), "app").unwrap();
        fs::create_dir_all(&current).unwrap();
        fs::write(current.join("preferences.json"), "new preferences").unwrap();

        let copied = migrate(&legacy, &current).unwrap();
        assert_eq!(copied.len(), 2, "{copied:?}");
        assert_eq!(
            fs::read_to_string(
                current
                    .join("catalog")
                    .join("refs")
                    .join("part")
                    .join("1.0.0.ref")
            )
            .unwrap(),
            "digest\n"
        );
        assert_eq!(
            fs::read_to_string(current.join("theme.json")).unwrap(),
            "old theme"
        );
        assert_eq!(
            fs::read_to_string(current.join("preferences.json")).unwrap(),
            "new preferences",
            "what is already in the safe folder wins"
        );
        assert!(
            !current.join("current").exists(),
            "the installed app is not user data"
        );
        assert!(legacy.join("theme.json").exists(), "the old copy stays");
        assert!(migrate(&legacy, &current).unwrap().is_empty(), "once only");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_library_says_what_it_is() {
        let root = scratch("readme");
        write_library_readme(&root).unwrap();
        let note = fs::read_to_string(root.join(LIBRARY_README)).unwrap();
        assert!(note.contains("Part Library") && note.contains("back up"));
        fs::write(root.join(LIBRARY_README), "kept").unwrap();
        write_library_readme(&root).unwrap();
        assert_eq!(
            fs::read_to_string(root.join(LIBRARY_README)).unwrap(),
            "kept"
        );
        fs::remove_dir_all(&root).ok();
    }
}
