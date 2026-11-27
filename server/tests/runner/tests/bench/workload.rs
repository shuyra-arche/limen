use super::{Workload, dependencies};
use std::{fs, path::PathBuf};

#[test]
fn dependency_information_preserves_escaped_source_paths() {
    let record = "image.d: src/main.rs owner/with\\ space.rs \\\n owner/child.rs\n\nsrc/main.rs:\n";
    let paths = dependencies(record).unwrap();
    assert_eq!(
        paths,
        [
            PathBuf::from("src/main.rs"),
            PathBuf::from("owner/with space.rs"),
            PathBuf::from("owner/child.rs"),
        ]
    );
    assert!(dependencies("image.d:\n").is_err());
    assert!(dependencies("image.d: source.rs\\").is_err());
}

#[test]
fn unrelated_workloads_do_not_change_local_identity() {
    let root = std::env::temp_dir().join(format!("kernel-workload-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let root = {
        fs::create_dir(&root).unwrap();
        root.canonicalize().unwrap()
    };
    let entry = "server/tests/target/riscv64/benches/clock.rs";
    let owner = "server/benches/metal/chrono/clock.rs";
    let unrelated = "server/benches/metal/sync/local.rs";
    for name in [entry, owner, unrelated] {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, name).unwrap();
    }
    let info = format!("image.d: {entry} {owner}\n");
    let first = Workload::collect(&root, &root, &info).unwrap();
    fs::write(root.join(unrelated), "new sync workload").unwrap();
    assert_eq!(first, Workload::collect(&root, &root, &info).unwrap());
    fs::write(root.join(owner), "new clock workload").unwrap();
    assert_ne!(first, Workload::collect(&root, &root, &info).unwrap());
    assert!(Workload::collect(&root, &root, &format!("image.d: {entry}\n")).is_err());
    fs::remove_dir_all(root).unwrap();
}
