//! Tests for reading the desktop's own list of applications.
//!
//! The fixtures are shortened copies of real files from the machine this was
//! written on, kept verbatim in the parts that matter: a browser app entry is
//! the case that motivated the whole thing, because a name like "YouTube
//! Music" exists nowhere else on the system.

use super::*;

const PWA: &str = "\
[Desktop Entry]
Version=1.0
Terminal=false
Type=Application
Name=YouTube Music
Exec=brave --profile-directory=Default --app-id=cinhimbnkkaeohfgghhklpknlkffjgod
Icon=brave-cinhimb-Default
StartupWMClass=crx_cinhimbnkkaeohfgghhklpknlkffjgod
";

const WITH_ACTIONS: &str = "\
[Desktop Entry]
Type=Application
Name=Editor
Exec=editor %U

[Desktop Action new-window]
Name=Open a New Window
Exec=editor --new-window
";

#[test]
fn a_browser_app_parses_into_a_runnable_line() {
    let app = parse(PWA).expect("a browser app entry is an application");
    assert_eq!(app.name, "YouTube Music");
    assert!(
        app.command
            .contains("--app-id=cinhimbnkkaeohfgghhklpknlkffjgod"),
        "the app id is the whole difference between this and a browser window"
    );
}

#[test]
fn field_codes_never_reach_the_program() {
    // `%U` left in arrives as a literal argument, and some programs take it
    // for a filename. There is no file and no URL to substitute here.
    let app = parse(WITH_ACTIONS).expect("an application");
    assert_eq!(app.command, "editor");
}

#[test]
fn extra_actions_are_not_applications() {
    // Reading straight through the file would pick up "Open a New Window" as
    // an application with a name of its own, and speaking "editor" could then
    // land on it.
    let app = parse(WITH_ACTIONS).expect("an application");
    assert_eq!(app.name, "Editor");
}

#[test]
fn entries_hidden_from_the_menu_are_hidden_here_too() {
    // A URL handler is a real `.desktop` file that no menu shows, because
    // starting it by hand does nothing useful.
    let hidden = "[Desktop Entry]\nType=Application\nName=Handler\nExec=x\nNoDisplay=true\n";
    assert!(parse(hidden).is_none());
    let link = "[Desktop Entry]\nType=Link\nName=Somewhere\nURL=https://example.com\n";
    assert!(parse(link).is_none(), "a link is not something to run");
}

fn apps() -> Vec<App> {
    ["YouTube Music", "WhatsApp Web", "Volume Control", "Steam"]
        .iter()
        .map(|n| App {
            name: n.to_string(),
            command: format!("run-{}", n.to_lowercase().replace(' ', "-")),
        })
        .collect()
}

#[test]
fn one_spoken_word_reaches_a_two_word_name() {
    // The case that decided the resolver has two stages: whole-string
    // Jaro-Winkler scores "youtube" against "YouTube Music" at 0.86, under
    // the 0.82 threshold only by luck and under a stricter one for certain.
    // Per token it is 1.00, because the word is literally in there.
    let apps = apps();
    let hit = resolve("youtube", &apps, 0.82).expect("youtube names something");
    assert_eq!(hit.name, "YouTube Music");
    let hit = resolve("whatsapp", &apps, 0.82).expect("whatsapp names something");
    assert_eq!(hit.name, "WhatsApp Web");
}

#[test]
fn the_full_name_still_wins_over_a_shared_word() {
    // "Volume Control" and "Volume Control" — one spoken word that appears in
    // two names has to be settled by the whole name when one is given.
    let mut apps = apps();
    apps.push(App {
        name: "Volume Mixer".to_string(),
        command: "mixer".to_string(),
    });
    let hit = resolve("volume mixer", &apps, 0.82).expect("names something");
    assert_eq!(hit.name, "Volume Mixer");
}

#[test]
fn nothing_is_better_than_the_wrong_thing() {
    // Launching is not reversible in the way focusing is: the wrong guess
    // starts a program the person did not ask for, and they have to notice
    // and close it. Below the threshold the answer is no answer.
    let apps = apps();
    assert!(resolve("outlook", &apps, 0.82).is_none());
    assert!(resolve("", &apps, 0.82).is_none());
    // Articles and noise on their own name nothing.
    assert!(resolve("o a de", &apps, 0.82).is_none());
}
