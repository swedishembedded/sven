// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A host that must not trust the places a tenant can write reads only the
//! configuration file it is told to. One test, alone in this binary: it
//! changes the process's home and working directory.

use sven_config::ConfigDocument;

#[test]
fn explicit_loading_ignores_home_and_working_directory_layers() {
    let tenant = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tenant.path().join(".config/sven")).unwrap();
    std::fs::write(
        tenant.path().join(".config/sven/config.yaml"),
        "from_home: true\n",
    )
    .unwrap();
    std::fs::write(tenant.path().join(".sven.yaml"), "from_cwd: true\n").unwrap();
    let explicit = tenant.path().join("explicit.yaml");
    std::fs::write(&explicit, "from_flag: true\n").unwrap();
    std::env::set_var("HOME", tenant.path());
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::set_current_dir(tenant.path()).unwrap();

    // The ordinary load does see what the tenant wrote ...
    let ordinary = ConfigDocument::load(Some(&explicit)).unwrap();
    assert!(ordinary.root().get("from_home").is_some());
    assert!(ordinary.root().get("from_cwd").is_some());

    // ... the explicit one sees the flag's file alone.
    let only = ConfigDocument::load_explicit(Some(&explicit)).unwrap();
    assert!(only.root().get("from_home").is_none());
    assert!(only.root().get("from_cwd").is_none());
    assert!(only.root().get("from_flag").is_some());
    assert_eq!(only.files(), std::slice::from_ref(&explicit));
    assert!(ConfigDocument::load_explicit(None)
        .unwrap()
        .files()
        .is_empty());
}
