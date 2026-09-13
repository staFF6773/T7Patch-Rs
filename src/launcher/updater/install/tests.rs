use super::super::tests::{stage, Directory};
use super::*;

fn original(dir: &Directory) {
    fs::write(dir.0.join(FILES[0]), b"old exe").unwrap();
    fs::write(dir.0.join(FILES[1]), b"old dll").unwrap();
    fs::write(dir.0.join("t7patch.conf"), b"playername=Unchanged").unwrap();
}
fn assert_original(dir: &Directory) {
    assert_eq!(fs::read(dir.0.join(FILES[0])).unwrap(), b"old exe");
    assert_eq!(fs::read(dir.0.join(FILES[1])).unwrap(), b"old dll");
    assert_eq!(
        fs::read(dir.0.join("t7patch.conf")).unwrap(),
        b"playername=Unchanged"
    );
}
#[test]
fn installs_a_matching_pair_and_keeps_settings() {
    let dir = Directory::new();
    original(&dir);
    let stage = stage(&dir);
    transaction(&dir.0, &stage, replace).unwrap();
    load_manifest(&stage)
        .unwrap()
        .verify_directory(&dir.0)
        .unwrap();
    assert!(!dir.0.join(JOURNAL).exists());
    assert_eq!(
        fs::read(dir.0.join("t7patch.conf")).unwrap(),
        b"playername=Unchanged"
    );
}
#[test]
fn locked_dll_rolls_back_the_already_replaced_exe() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = Directory::new();
    original(&dir);
    let stage = stage(&dir);
    let _locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(dir.0.join(FILES[1]))
        .unwrap();
    let error = transaction(&dir.0, &stage, replace).unwrap_err();
    assert!(error.contains("previous version restored"), "{error}");
    assert_original(&dir);
    assert!(!dir.0.join(JOURNAL).exists());
}
#[test]
fn interruption_between_files_recovers_on_the_next_attempt() {
    let dir = Directory::new();
    original(&dir);
    let stage = stage(&dir);
    let result = std::panic::catch_unwind(|| {
        let _ = transaction(&dir.0, &stage, |from, to| {
            replace(from, to)?;
            panic!("simulated process interruption after first rename");
        });
    });
    assert!(result.is_err());
    assert!(dir.0.join(JOURNAL).exists());
    recover(&dir.0).unwrap();
    assert_original(&dir);
    assert!(!dir.0.join(JOURNAL).exists());
}
#[test]
fn bad_backup_does_not_partially_restore_or_discard_the_journal() {
    let dir = Directory::new();
    original(&dir);
    let stage = stage(&dir);
    let _ = std::panic::catch_unwind(|| {
        let _ = transaction(&dir.0, &stage, |from, to| {
            replace(from, to)?;
            panic!("interrupted");
        });
    });
    let installed_exe = fs::read(dir.0.join(FILES[0])).unwrap();
    fs::write(stage.join("backup").join(FILES[1]), b"corrupt").unwrap();
    assert!(recover(&dir.0).is_err());
    assert_eq!(fs::read(dir.0.join(FILES[0])).unwrap(), installed_exe);
    assert!(dir.0.join(JOURNAL).exists());
}
#[test]
fn rejects_staging_paths_outside_the_install_directory() {
    let dir = Directory::new();
    for name in [
        "../outside",
        ".t7patch-update-../outside",
        ".t7patch-update-",
        "unrelated",
    ] {
        assert!(stage_in(&dir.0, name).is_err());
    }
}

#[test]
fn existing_recovery_journal_is_not_overwritten() {
    let dir = Directory::new();
    original(&dir);
    let stage = stage(&dir);
    fs::write(dir.0.join(JOURNAL), b"existing recovery information").unwrap();
    assert!(transaction(&dir.0, &stage, replace).is_err());
    assert_original(&dir);
    assert_eq!(
        fs::read(dir.0.join(JOURNAL)).unwrap(),
        b"existing recovery information"
    );
}
