//! Decide once per process whether a globally loaded layer should run the HUD.
//! Vulkan is also used by desktop apps, so loading the layer is not game detection.
use std::path::Path;

pub fn enabled() -> bool {
    let executable = std::fs::read_link("/proc/self/exe").unwrap_or_default();
    let command = std::env::args_os().next().unwrap_or_default();
    should_enable(&executable, Path::new(&command), |key| {
        std::env::var(key).ok()
    })
}

fn should_enable(executable: &Path, command: &Path, env: impl Fn(&str) -> Option<String>) -> bool {
    if env("ARGUS_LASSO_HUD_DISABLE").as_deref() == Some("1") {
        return false;
    }
    // Do not let inherited launch variables put a HUD on a terminal or launcher.
    for path in [executable, command] {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if matches!(
            name.as_ref(),
            "konsole"
                | "gnome-terminal"
                | "gnome-terminal-server"
                | "kgx"
                | "ptyxis"
                | "ghostty"
                | "alacritty"
                | "kitty"
                | "wezterm"
                | "wezterm-gui"
                | "foot"
                | "footclient"
                | "xterm"
                | "uxterm"
                | "terminator"
                | "tilix"
                | "rio"
                | "cosmic-term"
                | "mate-terminal"
                | "xfce4-terminal"
                | "qterminal"
                | "cool-retro-term"
                | "steam"
                | "steamwebhelper"
                | "lutris"
                | "heroic"
                | "gamescope"
                | "kwin_wayland"
                | "kwin_x11"
                | "gnome-shell"
        ) {
            return false;
        }
    }
    // Explicit opt-in also supports standalone games and diagnostic applications.
    if env("ARGUS_LASSO_HUD").as_deref() == Some("1") {
        return true;
    }
    // Steam sets these for native and Proton games, including non-Steam shortcuts.
    if ["SteamAppId", "SteamGameId"].iter().any(|key| {
        env(key)
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|id| id > 0)
    }) {
        return true;
    }
    [executable, command].iter().any(|path| {
        path.to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase()
            .contains("/steamapps/common/")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_for(exe: &str, command: &str, vars: &[(&str, &str)]) -> bool {
        should_enable(Path::new(exe), Path::new(command), |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn ordinary_vulkan_applications_are_not_games() {
        for exe in ["/usr/bin/ptyxis", "/usr/bin/firefox", "/usr/bin/vkcube"] {
            assert!(!enabled_for(exe, exe, &[]));
        }
        assert!(!enabled_for("/usr/bin/app", "app", &[("SteamAppId", "0")]));
        assert!(!enabled_for(
            "/usr/bin/app",
            "app",
            &[("SteamGameId", "bad")]
        ));
    }

    #[test]
    fn detects_native_and_proton_games_and_allows_explicit_opt_in() {
        assert!(enabled_for(
            "/games/steamapps/common/Game/game",
            "game",
            &[]
        ));
        assert!(enabled_for(
            "/usr/bin/wine64-preloader",
            "game.exe",
            &[("SteamAppId", "123")]
        ));
        assert!(enabled_for(
            "/games/native",
            "native",
            &[("SteamGameId", "456")]
        ));
        assert!(enabled_for(
            "/usr/bin/wine64",
            "Z:\\games\\steamapps\\common\\Game\\game.exe",
            &[]
        ));
        assert!(enabled_for(
            "/games/standalone",
            "standalone",
            &[("ARGUS_LASSO_HUD", "1")]
        ));
    }

    #[test]
    fn inherited_game_variables_do_not_activate_terminals_or_launchers() {
        let vars = [("ARGUS_LASSO_HUD", "1"), ("SteamAppId", "123")];
        for exe in [
            "/usr/bin/ptyxis",
            "/usr/bin/konsole",
            "/usr/bin/ghostty",
            "/steam/steamwebhelper",
            "/usr/bin/gamescope",
        ] {
            assert!(!enabled_for(exe, exe, &vars));
        }
        assert!(!enabled_for(
            "/usr/bin/python3",
            "/usr/bin/terminator",
            &vars
        ));
    }

    #[test]
    fn disable_wins_over_game_detection_and_explicit_opt_in() {
        assert!(!enabled_for(
            "/games/steamapps/common/Game/game",
            "game",
            &[
                ("ARGUS_LASSO_HUD", "1"),
                ("SteamAppId", "123"),
                ("ARGUS_LASSO_HUD_DISABLE", "1")
            ]
        ));
    }
}
