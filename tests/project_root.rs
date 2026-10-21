//! The project root from the environment: absolute, so a server that
//! changes into it keeps its records there and not a level deeper. In its
//! own binary, since the tests set the process's environment and folder;
//! they hold one lock, so they do not run at once.

use legume_mcp::project::{root_from_env, OFF_ENV, ROOT_ENV};
use std::sync::Mutex;

static ENV: Mutex<()> = Mutex::new(());

#[test]
fn the_root_is_absolute_and_off_keeps_none() {
    let _env = ENV.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    std::fs::create_dir(base.join("proj")).unwrap();
    std::env::set_current_dir(&base).unwrap();
    std::env::set_var(ROOT_ENV, "proj");
    std::env::remove_var(OFF_ENV);
    assert_eq!(root_from_env(), Some(base.join("proj")));

    // Resolved before changing into it and set again, it names the same
    // folder from inside.
    let root = root_from_env().unwrap();
    std::env::set_current_dir(&root).unwrap();
    std::env::set_var(ROOT_ENV, &root);
    assert_eq!(root_from_env(), Some(base.join("proj")));

    std::env::remove_var(ROOT_ENV);
    assert_eq!(root_from_env(), Some(base.join("proj")));

    std::env::set_var(OFF_ENV, "off");
    assert_eq!(root_from_env(), None);
}

#[test]
fn entering_a_project_makes_its_folder() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("new/proj");
    std::env::set_var(ROOT_ENV, &root);
    std::env::remove_var(OFF_ENV);
    let records = legume_mcp::project::enter().unwrap();
    assert_eq!(records, Some(root.clone()));
    assert_eq!(
        std::env::current_dir().unwrap().canonicalize().unwrap(),
        root
    );
}

#[test]
fn entering_a_relative_project_reads_it_from_where_the_server_starts() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    std::env::set_current_dir(&base).unwrap();
    std::env::set_var(ROOT_ENV, "proj");
    std::env::remove_var(OFF_ENV);
    let records = legume_mcp::project::enter().unwrap();
    assert_eq!(records, Some(base.join("proj")));
    assert_eq!(std::env::current_dir().unwrap(), base.join("proj"));
}
