//! The folder listing the chooser is drawn from.
//!
//! What matters: the order (folders first, names ignoring case), saying how
//! much a filter hides, and not falling over on a real disk (broken links,
//! unreadable folders, a name gone between listing and stat).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use beatbox_shell::files::{self, CAP};

struct Room(PathBuf);

impl Room {
    fn new() -> Self {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("beatbox-shell-files-{}-{n}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        let room = Self(files::resolved(&path));
        fs::create_dir(room.0.join("Zebra")).unwrap();
        fs::create_dir(room.0.join("alpha")).unwrap();
        fs::write(room.0.join("win10.qcow2"), vec![b'x'; 10]).unwrap();
        fs::write(room.0.join("Ubuntu.iso"), vec![b'y'; 20]).unwrap();
        fs::write(room.0.join("notes.txt"), "hello").unwrap();
        fs::write(room.0.join(".hidden.iso"), "").unwrap();
        room
    }

    fn text(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for Room {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn names(room: &Room, suffixes: &str) -> Vec<String> {
    files::listing(&room.text(), suffixes)
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

#[test]
fn test_folders_come_first_then_names_ignoring_case() {
    let room = Room::new();
    let names = names(&room, "");
    assert_eq!(names[..2], ["alpha", "Zebra"]);
    assert_eq!(
        names[2..],
        [".hidden.iso", "notes.txt", "Ubuntu.iso", "win10.qcow2"]
    );
}

/// Filtering by what you want to open must not hide the way to it.
#[test]
fn test_a_filter_keeps_every_folder() {
    let room = Room::new();
    let view = files::listing(&room.text(), ".qcow2").unwrap();
    let folders: Vec<_> = view
        .entries
        .iter()
        .filter(|entry| entry.folder)
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(folders, ["alpha", "Zebra"]);
}

#[test]
fn test_the_filter_says_how_much_it_is_hiding() {
    let room = Room::new();
    let view = files::listing(&room.text(), ".qcow2").unwrap();
    let kept: Vec<_> = view
        .entries
        .iter()
        .filter(|entry| !entry.folder)
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(kept, ["win10.qcow2"]);
    assert_eq!(view.filtered, 3, "notes.txt, Ubuntu.iso and the hidden one");
}

#[test]
fn test_the_filter_does_not_care_about_case() {
    let room = Room::new();
    assert!(names(&room, ".ISO").contains(&"Ubuntu.iso".to_owned()));
}

/// The page has a "show hidden" toggle; dropping them here would make it a lie.
#[test]
fn test_a_dotfile_is_marked_rather_than_dropped() {
    let room = Room::new();
    let hidden: Vec<_> = files::listing(&room.text(), "")
        .unwrap()
        .entries
        .into_iter()
        .filter(|entry| entry.hidden)
        .map(|entry| entry.name)
        .collect();
    assert_eq!(hidden, [".hidden.iso"]);
}

#[test]
fn test_sizes_are_read_and_folders_have_none() {
    let room = Room::new();
    let rows = files::listing(&room.text(), "").unwrap().entries;
    let row = |name: &str| rows.iter().find(|entry| entry.name == name).unwrap();
    assert_eq!(row("win10.qcow2").size, 10);
    assert_eq!(row("alpha").size, 0);
    assert!(row("win10.qcow2").modified > 0.0);
}

/// The field holds a file; "browse from here" means its folder.
#[test]
fn test_a_file_opens_the_folder_holding_it() {
    let room = Room::new();
    let file = room.0.join("win10.qcow2");
    let view = files::listing(&file.to_string_lossy(), "").unwrap();
    assert_eq!(view.path, room.text());
}

#[test]
fn test_the_parent_is_where_up_goes() {
    let room = Room::new();
    assert_eq!(
        files::listing(&room.text(), "").unwrap().parent,
        room.0.parent().unwrap().to_string_lossy()
    );
}

#[test]
fn test_the_root_of_the_disk_has_nowhere_above_it() {
    let room = Room::new();
    let top = room.0.ancestors().last().unwrap().to_path_buf();
    assert_eq!(
        files::listing(&top.to_string_lossy(), "").unwrap().parent,
        ""
    );
}

#[test]
fn test_the_crumbs_run_from_the_root_to_here() {
    let room = Room::new();
    let crumbs = files::listing(&room.text(), "").unwrap().crumbs;
    // The top of this disk: "/" on Linux, "C:\" on Windows.
    let top = room.0.ancestors().last().unwrap();
    assert_eq!(crumbs[0].path, top.to_string_lossy());
    assert_eq!(crumbs.last().unwrap().path, room.text());
    assert_eq!(
        crumbs.last().unwrap().name,
        room.0.file_name().unwrap().to_string_lossy()
    );
}

#[test]
fn test_an_empty_path_is_home_and_not_a_crash() {
    let home = files::resolved(&std::env::home_dir().unwrap());
    assert_eq!(files::listing("", "").unwrap().path, home.to_string_lossy());
}

#[test]
fn test_a_folder_that_is_not_there_is_a_fault_and_not_an_exception() {
    let room = Room::new();
    let missing = room.0.join("no").join("such").join("folder");
    let fault = files::listing(&missing.to_string_lossy(), "").unwrap_err();
    assert_eq!(fault.code, "no-such-folder");
}

/// It has a name, so it exists. Zeroes say what could not be read.
#[cfg(unix)]
#[test]
fn test_a_broken_symlink_is_still_a_row() {
    let room = Room::new();
    std::os::unix::fs::symlink(room.0.join("not-here.iso"), room.0.join("gone.iso")).unwrap();
    let rows = files::listing(&room.text(), "").unwrap().entries;
    let row = rows.iter().find(|entry| entry.name == "gone.iso").unwrap();
    assert_eq!((row.size, row.modified), (0, 0.0));
}

#[test]
fn test_an_enormous_folder_is_capped_and_says_so() {
    let room = Room::new();
    let many = room.0.join("many");
    fs::create_dir(&many).unwrap();
    for index in 0..CAP + 5 {
        fs::write(many.join(format!("f{index:05}.iso")), "").unwrap();
    }
    let view = files::listing(&many.to_string_lossy(), "").unwrap();
    assert_eq!(view.entries.len(), CAP);
    assert_eq!(view.beyond, 5);
}

// --- the places ---------------------------------------------------------------------

#[test]
fn test_every_place_exists_and_appears_once() {
    let paths: Vec<String> = files::places()
        .into_iter()
        .map(|place| place.path)
        .collect();
    let mut unique = paths.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        paths.len(),
        "the same folder twice under two names"
    );
    for path in &paths {
        assert!(
            Path::new(path).is_dir(),
            "{path} is offered and is not there"
        );
    }
}

#[test]
fn test_home_is_one_of_them() {
    let home = files::resolved(&std::env::home_dir().unwrap());
    assert!(
        files::places()
            .iter()
            .any(|place| place.path == home.to_string_lossy())
    );
}

/// It was, as `cwd`, until it read as "This project" to somebody picking a
/// disk image.
#[test]
fn test_this_projects_own_checkout_is_not_one_of_them() {
    assert!(files::places().iter().all(|place| place.key != "cwd"));
}

/// Where an installer ISO or a disk somebody was handed tends to sit.
///
/// Unless it is home itself: `xdg-user-dir` answers `$HOME` for a folder
/// the desktop never set up (a CI runner, a bare server), and home is
/// already on the rail, once.
#[test]
fn test_downloads_is_offered_when_it_exists() {
    let downloads = files::resolved(&files::user_dir("DOWNLOAD", "Downloads"));
    let home = files::resolved(&std::env::home_dir().unwrap());
    if downloads.is_dir() && downloads != home {
        assert!(files::places().iter().any(|place| place.key == "downloads"));
    }
}

/// `places` never lets one broken lookup take the rest down with it.
#[test]
fn test_xdg_user_dir_missing_falls_back_without_failing() {
    let downloads = files::user_dir_asking("no-such-xdg-user-dir-here", "DOWNLOAD", "Downloads");
    assert_eq!(downloads, std::env::home_dir().unwrap().join("Downloads"));
    files::places(); // must not fail
}

/// `key` is a locale key, not a folder name: `beatbox.files.place.*`.
#[test]
fn test_each_place_names_a_string_the_page_can_translate() {
    let text = include_str!("../../locales/en.json");
    let catalogue: serde_json::Value = serde_json::from_str(text).unwrap();
    for place in files::places() {
        let key = format!("beatbox.files.place.{}", place.key);
        assert!(catalogue.get(&key).is_some(), "{key} is not in en.json");
    }
}
