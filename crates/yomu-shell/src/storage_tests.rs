use super::{publish_chapter, recover_chapter_publications};

#[test]
fn failed_replacement_and_interrupted_publish_preserve_complete_pages() {
    let root = std::env::temp_dir().join(format!("yomu-shell-storage-{}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    let target = root.join("00000000-0000-0000-0000-000000000001");
    let partial = root.join(".partial-test");
    let old = target.with_extension("old");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("0000.png"), b"good pages").unwrap();
    assert!(publish_chapter(&partial, &target).is_err());
    assert_eq!(
        std::fs::read(target.join("0000.png")).unwrap(),
        b"good pages"
    );
    // A process stops between moving the old copy aside and publishing the new.
    std::fs::rename(&target, &old).unwrap();
    recover_chapter_publications(&root).unwrap();
    recover_chapter_publications(&root).unwrap();
    assert_eq!(
        std::fs::read(target.join("0000.png")).unwrap(),
        b"good pages"
    );
    std::fs::create_dir(&partial).unwrap();
    std::fs::write(partial.join("0000.png"), b"new pages").unwrap();
    publish_chapter(&partial, &target).unwrap();
    assert_eq!(
        std::fs::read(target.join("0000.png")).unwrap(),
        b"new pages"
    );
    assert!(!old.exists());
    std::fs::remove_dir_all(root).unwrap();
}
