//! The pictures in the README (`assets/screenshots/*.svg`), drawn by the TUI's own renderer from a demo
//! snapshot. This module exists in test builds only, so the shipped binary carries neither the demo nor
//! the SVG writer.
//!
//! Two tests live here. `committed_screenshots_match_the_renderer` runs with every `cargo test` and
//! fails when a committed file differs from what the renderer draws now (the TUI changed, or someone
//! edited a file by hand). `regenerate_screenshots` is `#[ignore]`d and rewrites the files; the
//! maintainer runs it before a release, with the command in [`REGENERATE`].

mod demo;
mod svg;

use std::path::{Path, PathBuf};

/// The one command that rewrites the pictures; the failure message and the README quote it.
const REGENERATE: &str = "cargo test -p klobuk --bin klobuk regenerate_screenshots -- --ignored";

/// `assets/screenshots/` of the repository, found from this crate's manifest (`crates/klobuk`).
fn screenshots_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/screenshots")
}

/// Whether the tests run inside a checkout of the repository. The published crate has no `assets/`
/// (and no workspace root), so a packager running its tests has nothing to compare.
fn in_repository() -> bool {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Cargo.toml")
        .is_file()
}

/// A committed file as text, with CRLF read as LF: a checkout with `core.autocrlf` must not look stale.
fn committed(file: &str) -> Option<String> {
    let bytes = std::fs::read(screenshots_dir().join(file)).ok()?;
    Some(String::from_utf8_lossy(&bytes).replace("\r\n", "\n"))
}

#[test]
fn committed_screenshots_match_the_renderer() {
    if !in_repository() {
        return;
    }
    let pictures = demo::pictures();
    let mut stale: Vec<String> = pictures
        .iter()
        .filter(|(file, svg)| committed(file).as_deref() != Some(svg.as_str()))
        .map(|(file, _)| format!("{file} differs from the renderer or is missing"))
        .collect();
    // a picture nobody draws any more must not stay behind and be linked from the README
    if let Ok(entries) = std::fs::read_dir(screenshots_dir()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !pictures.iter().any(|(file, _)| *file == name) {
                stale.push(format!("{name} is not drawn by the renderer; delete it"));
            }
        }
    }
    assert!(
        stale.is_empty(),
        "the README screenshots in assets/screenshots are out of date:\n  {}\nregenerate them with:\n  {REGENERATE}\nand commit the result",
        stale.join("\n  ")
    );
}

#[test]
#[ignore = "writes assets/screenshots/*.svg; run it with the command in README.md"]
fn regenerate_screenshots() {
    let dir = screenshots_dir();
    std::fs::create_dir_all(&dir).expect("create assets/screenshots");
    for (file, svg) in demo::pictures() {
        std::fs::write(dir.join(file), svg).unwrap_or_else(|e| panic!("write {file}: {e}"));
    }
}

#[test]
fn the_pictures_are_the_same_on_every_run() {
    assert_eq!(demo::pictures(), demo::pictures());
}

#[test]
fn the_demo_shows_what_the_readme_says_it_shows() {
    let frames = demo::frames();
    let text = |file: &str| {
        frames
            .iter()
            .find(|(f, _)| *f == file)
            .map(|(_, t)| t.as_str())
            .unwrap_or_else(|| panic!("no picture {file}"))
    };
    let (board, now, queue) = (text("board.svg"), text("now.svg"), text("queue.svg"));
    // the Board: one column per status, the question in the banner
    for column in [
        "backlog 2",
        "ready 1",
        "in-progress 2",
        "blocked 1",
        "validating 1",
        "done 3",
    ] {
        assert!(board.contains(column), "{column:?} missing:\n{board}");
    }
    assert!(board.contains("nk-8 asks: Should note sync"), "{board}");
    // Now: the question, two workers with their cost, what is next and what is done
    for part in [
        "NEEDS YOU (1)",
        "RUNNING (2)",
        "$1.84 / $5",
        "$0.62 / $5",
        "UP NEXT (3)",
        "DONE TODAY (1)",
    ] {
        assert!(now.contains(part), "{part:?} missing:\n{now}");
    }
    assert!(now.contains("workers 2/3"), "{now}");
    // Queue: one item in the gates (with the gate in flight), one merged
    for part in [
        "IN THE QUEUE (1)",
        "gate test",
        "MERGED TODAY (1)",
        "gates on main green",
    ] {
        assert!(queue.contains(part), "{part:?} missing:\n{queue}");
    }
    // nothing of this repository's own store, whatever the TUI draws
    for (file, frame) in &frames {
        assert!(
            !frame.contains("asf-"),
            "{file} shows an id of this repository:\n{frame}"
        );
    }
}
