use agentcenter_core::cockpit::HandoffIndex;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    time::SystemTime,
};

#[test]
fn agentcenter_handoff_exact_session_metadata_only() {
    let root = std::env::temp_dir().join(format!("ac-handoff-meta-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let a = root.join("s61-random-operation.md");
    let b = root.join("s6162-random-operation.md");
    fs::write(&a, "CONTENT_CANARY_NEVER_READ").unwrap();
    fs::write(&b, "OTHER_SESSION_CANARY").unwrap();
    fs::set_permissions(&a, fs::Permissions::from_mode(0o0)).unwrap();
    symlink(&b, root.join("s61-symlink.md")).unwrap();
    fs::create_dir_all(root.join("s61-directory.md")).unwrap();
    let mut index = HandoffIndex::new();
    let now = SystemTime::now();
    let found = index.find_agentcenter(&root, "a", now).unwrap();
    assert_eq!(found.path, a);
    assert!(found.age_min >= 0.0);
    assert!(!format!("{found:?}").contains("CONTENT_CANARY"));
    assert_eq!(index.find_agentcenter(&root, "ab", now).unwrap().path, b);
    assert!(index.find_agentcenter(&root, "missing", now).is_none());
    assert!(index.find_agentcenter(&root, "", now).is_none());
    let src = include_str!("../src/cockpit/handoff.rs");
    for call in ["read_to_string", "read_to_end", "File::open", "fs::read("] {
        assert!(!src.contains(call), "handoff scanner opens content: {call}");
    }
    fs::remove_dir_all(root).unwrap();
}
