//! Installed games from Steam and Lutris, for the launcher's pickers.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The quoted strings on one line of a Valve KeyValues (VDF/ACF) file,
/// unescaped: `"name"  "Portal 2"` gives `["name", "Portal 2"]`. Inside a
/// string, `\"` is a quote and `\\` a backslash.
fn vdf_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut token = String::new();
        loop {
            match chars.next() {
                Some('\\') => match chars.next() {
                    Some('n') => token.push('\n'),
                    Some('t') => token.push('\t'),
                    Some(other) => token.push(other),
                    None => break,
                },
                Some('"') | None => break,
                Some(other) => token.push(other),
            }
        }
        tokens.push(token);
    }
    tokens
}

/// The value of the first `"key" "value"` line for `key`.
fn vdf_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find_map(|line| match vdf_tokens(line).as_slice() {
            [k, v] if k == key => Some(v.clone()),
            _ => None,
        })
}

/// Every `"path"` value in a libraryfolders.vdf.
fn library_paths(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| match vdf_tokens(line).as_slice() {
            [k, v] if k == "path" => Some(PathBuf::from(v)),
            _ => None,
        })
        .collect()
}

/// (appid, name) from one appmanifest_*.acf.
fn manifest_game(text: &str) -> Option<(String, String)> {
    Some((vdf_value(text, "appid")?, vdf_value(text, "name")?))
}

/// Installed Steam apps as (appid, name), sorted by name. Reads the default
/// library and every library listed in its libraryfolders.vdf.
pub fn steam_games() -> Vec<(String, String)> {
    let Some(home) = crate::config::home_dir() else {
        return Vec::new();
    };
    steam_games_under(&[home.join(".steam/steam"), home.join(".local/share/Steam")])
}

fn steam_games_under(roots: &[PathBuf]) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut lib_dirs: Vec<PathBuf> = Vec::new();
    let mut add = |dir: &Path, lib_dirs: &mut Vec<PathBuf>| {
        if let Ok(resolved) = dir.canonicalize() {
            if seen.insert(resolved.clone()) {
                lib_dirs.push(resolved);
            }
        }
    };
    for root in roots {
        add(&root.join("steamapps"), &mut lib_dirs);
    }
    for lib in lib_dirs.clone() {
        if let Ok(text) = std::fs::read_to_string(lib.join("libraryfolders.vdf")) {
            for path in library_paths(&text) {
                add(&path.join("steamapps"), &mut lib_dirs);
            }
        }
    }
    let mut games: HashMap<String, String> = HashMap::new();
    for apps_dir in &lib_dirs {
        let Ok(entries) = std::fs::read_dir(apps_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let fname = fname.to_string_lossy();
            if fname.starts_with("appmanifest_") && fname.ends_with(".acf") {
                if let Some((id, name)) = std::fs::read_to_string(entry.path())
                    .ok()
                    .as_deref()
                    .and_then(manifest_game)
                {
                    games.insert(id, name);
                }
            }
        }
    }
    let mut sorted: Vec<_> = games.into_iter().collect();
    sorted.sort_by_key(|(_, name)| name.to_lowercase());
    sorted
}

/// Separates the columns sqlite3 prints; not a character game names use,
/// unlike the default `|`.
const SQLITE_SEPARATOR: &str = "\u{1f}";

/// Installed Lutris games as (slug, "name  [runner]"), and a status line.
pub fn lutris_games() -> (Vec<(String, String)>, String) {
    let Some(home) = crate::config::home_dir() else {
        return (vec![], "Home directory unknown.".into());
    };
    let db = home.join(".local/share/lutris/pga.db");
    if !db.exists() {
        return (vec![], "Lutris database not found.".into());
    }
    // We don't want to pull in rusqlite; read the database with the sqlite3
    // CLI, read-only so a running Lutris is never disturbed.
    let output = std::process::Command::new("sqlite3")
        .arg("-readonly")
        .args(["-separator", SQLITE_SEPARATOR])
        .arg(&db)
        .arg("SELECT name,slug,runner FROM games WHERE installed=1 ORDER BY name COLLATE NOCASE")
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let games = parse_lutris_rows(&String::from_utf8_lossy(&o.stdout));
            let count = games.len();
            (games, format!("{count} installed games found"))
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            (vec![], format!("sqlite3 error: {err}"))
        }
        Err(e) => (vec![], format!("sqlite3 not found: {e}")),
    }
}

fn parse_lutris_rows(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, SQLITE_SEPARATOR).map(str::trim);
            let (name, slug, runner) = (fields.next()?, fields.next()?, fields.next()?);
            Some((slug.to_string(), format!("{name}  [{runner}]")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The old reader kept the closing quote: "620\"" became the app id,
    /// so the launcher got `steam -applaunch 620"` and refused it.
    #[test]
    fn manifest_values_come_back_without_quotes() {
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\t\"620\"\n\t\"Universe\"\t\t\"1\"\n\
                   \t\"name\"\t\t\"Portal 2\"\n\t\"installdir\"\t\t\"Portal 2\"\n}\n";
        assert_eq!(manifest_game(acf), Some(("620".into(), "Portal 2".into())));
    }

    #[test]
    fn escaped_quotes_and_backslashes_are_unescaped() {
        assert_eq!(
            vdf_tokens(r#"	"name"		"The \"Quoted\" Game \\ II""#),
            ["name", r#"The "Quoted" Game \ II"#]
        );
        // A section header or brace is not a key-value pair.
        assert_eq!(vdf_value("\"AppState\"\n{\n", "AppState"), None);
    }

    #[test]
    fn every_library_path_is_found() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"/home/u/.local/share/Steam\"\n\
                   \t\t\"label\"\t\t\"\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"/mnt/Games Drive/SteamLibrary\"\n\t}\n}\n";
        assert_eq!(
            library_paths(vdf),
            [
                PathBuf::from("/home/u/.local/share/Steam"),
                PathBuf::from("/mnt/Games Drive/SteamLibrary")
            ]
        );
    }

    #[test]
    fn a_library_is_scanned_through_every_listed_folder() {
        let root = std::env::temp_dir().join(format!("argus-steam-{}", std::process::id()));
        let extra = root.join("second library");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("steam/steamapps")).unwrap();
        std::fs::create_dir_all(extra.join("steamapps")).unwrap();
        std::fs::write(
            root.join("steam/steamapps/libraryfolders.vdf"),
            format!("\t\t\"path\"\t\t\"{}\"\n", extra.display()),
        )
        .unwrap();
        let manifest =
            |id: &str, name: &str| format!("\t\"appid\"\t\t\"{id}\"\n\t\"name\"\t\t\"{name}\"\n");
        std::fs::write(
            root.join("steam/steamapps/appmanifest_20.acf"),
            manifest("20", "beta"),
        )
        .unwrap();
        std::fs::write(
            extra.join("steamapps/appmanifest_10.acf"),
            manifest("10", "Alpha"),
        )
        .unwrap();
        assert_eq!(
            steam_games_under(&[root.join("steam")]),
            [("10".into(), "Alpha".into()), ("20".into(), "beta".into())]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn lutris_names_may_contain_the_old_separator() {
        let rows = format!("Foo | Bar{SQLITE_SEPARATOR}foo-bar{SQLITE_SEPARATOR}wine\nbroken\n");
        assert_eq!(
            parse_lutris_rows(&rows),
            [("foo-bar".into(), "Foo | Bar  [wine]".into())]
        );
    }
}
