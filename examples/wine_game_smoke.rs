use std::{
    error::Error,
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use packlift::{installer::Installer, system::LogHandler};

const FLATPAK_WINE: &str = "org.winehq.Wine//wow64-25.08";

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: wine_game_smoke <fixture-directory>")?;
    let game = root.join("Game files/bin/Game.exe");
    let data = root.join("state");
    let applications = root.join("shortcuts");
    let logger: LogHandler = Arc::new(|line| println!("{line}"));
    let installer = Installer::new(logger, Some(data), Some(applications))?;

    let (package, _) = installer.probe(&game)?;
    let app = installer.add_game(&package, FLATPAK_WINE)?;
    assert_eq!(app["runtime"], FLATPAK_WINE);

    let shortcut = PathBuf::from(
        app["exports"][0]["desktop"]
            .as_str()
            .ok_or("missing desktop shortcut")?,
    );
    let result = game.with_file_name("opened");
    assert!(
        !result.exists(),
        "registering a shortcut must not start the game"
    );

    installer.launch(&app)?;
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline && !result.exists() {
        thread::sleep(Duration::from_millis(250));
    }
    assert!(
        result.exists(),
        "the registered game shortcut did not start in its original folder"
    );

    installer.uninstall_game(&app)?;
    assert!(
        !shortcut.exists(),
        "removing a game must delete its menu shortcut"
    );
    assert!(
        game.is_file(),
        "removing a menu shortcut must preserve the game executable"
    );
    assert!(
        game.with_file_name("game-data").is_file(),
        "removing a menu shortcut must preserve game files"
    );
    Ok(())
}
