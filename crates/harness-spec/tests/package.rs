// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a workflow runs another through `imports:` and `use:`. The call is
//! checked when the package compiles (inputs, outputs, capabilities), the
//! imported contents are pinned by the bundle digest, and the safety rules of
//! a package hold: relative paths that stay inside it, no cycles, no
//! duplicate names, no unreachable files, no symlink out of the directory.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in composing long-running agent
//! work from reviewed, reusable parts then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;

use sven_harness_spec::package::{compile_package, load_package, resolve_import};
use sven_harness_spec::{HarnessBundle, PlanPrimitive};

const MAIN: &str = r#"
api_version: "2"
name: caller
inputs: { repo: string }
imports: { review: ./review.yaml }
execution: { limits: { model_turns: 10 } }
workflow:
  - name: look
    use: review
    with: { target: "{{ inputs.repo }}" }
outputs:
  verdict: { type: string, value: "{{ tasks.look.output.verdict }}" }
"#;

const REVIEW: &str = r#"
api_version: "2"
name: reviewer
inputs: { target: string }
execution: { limits: { model_turns: 6 } }
workflow:
  - name: read
    task: "Review {{ inputs.target }}."
outputs:
  verdict: { type: string, value: "{{ tasks.read.output }}" }
"#;

fn files(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(p, t)| ((*p).to_string(), (*t).to_string()))
        .collect()
}

fn refused(main: &str, entries: &[(&str, &str)]) -> String {
    compile_package(main, &files(entries))
        .expect_err("expected the package to be refused")
        .to_string()
}

#[test]
fn a_use_task_delegates_to_the_imported_workflow_and_reads_what_it_returns() {
    let package = compile_package(MAIN, &files(&[("review.yaml", REVIEW)])).expect("compiles");
    let PlanPrimitive::Delegate {
        target,
        store_as,
        input,
        ..
    } = &package.plan.steps["look__work"].primitive
    else {
        panic!("a delegate step");
    };
    assert_eq!(target, "reviewer");
    assert!(store_as.is_some());
    assert_eq!(input["target"], "{{inputs.repo}}");
    assert_eq!(package.imports.len(), 1);
    assert_eq!(package.imports[0].1.name, "reviewer");
    // The caller holds what its child holds, plus the right to start it.
    let held: Vec<&str> = package
        .spec
        .capabilities
        .iter()
        .flatten()
        .map(|c| c.as_str())
        .collect();
    assert!(held.contains(&"SpawnChild"), "{held:?}");
}

#[test]
fn a_call_that_does_not_fit_the_imported_interface_is_refused() {
    let with = |from: &str, to: &str| MAIN.replace(from, to);
    let review = &[("review.yaml", REVIEW)];
    assert!(refused(
        &with("with: { target: \"{{ inputs.repo }}\" }", "with: {}"),
        review
    )
    .contains("takes input `target`"));
    assert!(refused(
        &with(
            "with: { target: \"{{ inputs.repo }}\" }",
            "with: { target: x, extra: 1 }"
        ),
        review
    )
    .contains("does not take"));
    assert!(refused(
        &with(
            "with: { target: \"{{ inputs.repo }}\" }",
            "with: { target: 5 }"
        ),
        review
    )
    .contains("`target`"));
    assert!(refused(
        &with("tasks.look.output.verdict", "tasks.look.output.score"),
        review
    )
    .contains("does not return"));
    assert!(refused(
        &with("tasks.look.output.verdict", "tasks.look.stop"),
        review
    )
    .contains("only an `output`"));
    assert!(refused(&with("use: review", "use: other"), review).contains("names no import"));
    assert!(refused(
        &with("    use: review\n", "    use: review\n    task: do it\n"),
        review
    )
    .contains("does not go with `use`"));
}

#[test]
fn a_caller_never_holds_less_than_its_child() {
    let narrow = MAIN.replace("execution:", "capabilities: [SpawnChild]\nexecution:");
    let child = REVIEW.replace(
        "name: reviewer",
        "name: reviewer\nagent: { profile: coding }",
    );
    assert!(refused(&narrow, &[("review.yaml", &child)]).contains("needs"));
}

#[test]
fn imports_that_lead_nowhere_safe_are_refused() {
    for (rel, why) in [
        ("../review.yaml", "climbs out"),
        ("/etc/review.yaml", "relative path"),
        ("https://example.com/review.yaml", "remote"),
        ("review.txt", ".yaml"),
    ] {
        let main = MAIN.replace("./review.yaml", rel);
        assert!(
            refused(&main, &[("review.yaml", REVIEW)]).contains(why),
            "{rel}: expected `{why}`"
        );
    }
    assert!(
        resolve_import("sub", "../shared/x.yaml").is_ok(),
        "a sibling stays inside"
    );
    assert_eq!(
        resolve_import("sub", "../shared/x.yaml").unwrap(),
        "shared/x.yaml"
    );
}

#[test]
fn a_cycle_a_duplicate_name_a_stray_file_and_a_version_one_import_are_refused() {
    let looped = r#"
api_version: "2"
name: reviewer
inputs: { target: string }
imports: { back: ./main.yaml }
execution: { limits: { model_turns: 6 } }
workflow:
  - name: again
    use: back
    with: { repo: x }
outputs:
  verdict: { type: string, value: "{{ tasks.again.output.verdict }}" }
"#;
    let main = MAIN.replace("./review.yaml", "./main.yaml");
    let cyc = refused(&main, &[("main.yaml", looped)]);
    assert!(cyc.contains("circular import"), "{cyc}");

    let same = REVIEW.replace("name: reviewer", "name: caller");
    assert!(refused(MAIN, &[("review.yaml", &same)]).contains("named `caller`"));

    assert!(
        refused(MAIN, &[("review.yaml", REVIEW), ("extra.yaml", REVIEW)])
            .contains("not imported by any")
    );

    let v1 = "api_version: \"1\"\nname: old\nentry: r\nsteps:\n  r: { return: { value: {} } }\n";
    assert!(refused(MAIN, &[("review.yaml", v1)]).contains("no longer accepted"));
}

#[test]
fn the_bundle_digest_pins_imported_behaviour_and_ignores_formatting() {
    let bundle = |review: &str| HarnessBundle {
        document: MAIN.to_string(),
        imports: files(&[("review.yaml", review)]),
        ..Default::default()
    };
    let base = bundle(REVIEW).digest();
    assert!(!base.is_empty());
    let reformatted = REVIEW.replace("name: reviewer", "# a comment\nname: reviewer");
    assert_eq!(bundle(&reformatted).digest(), base);
    let changed = REVIEW.replace("Review {{ inputs.target }}.", "Audit {{ inputs.target }}.");
    assert_ne!(
        bundle(&changed).digest(),
        base,
        "different behaviour, different pin"
    );
    let compiled = bundle(REVIEW).compile().expect("compiles");
    assert_eq!(compiled.imports.len(), 1);
}

#[test]
fn a_package_directory_loads_with_its_imports_and_refuses_a_symlink_out() {
    let root = tempfile::tempdir().unwrap();
    let pkg = root.path().join("pkg");
    std::fs::create_dir_all(pkg.join("lib")).unwrap();
    std::fs::write(
        pkg.join("harness.yaml"),
        MAIN.replace("./review.yaml", "./lib/review.yaml"),
    )
    .unwrap();
    std::fs::write(pkg.join("lib/review.yaml"), REVIEW).unwrap();
    let bundle = load_package(&pkg).expect("loads");
    assert_eq!(
        bundle.imports.keys().collect::<Vec<_>>(),
        ["lib/review.yaml"]
    );
    bundle.compile().expect("compiles");

    // A symlink inside the package that leads outside it is refused.
    #[cfg(unix)]
    {
        let outside = root.path().join("secret.yaml");
        std::fs::write(&outside, REVIEW).unwrap();
        std::fs::remove_file(pkg.join("lib/review.yaml")).unwrap();
        std::os::unix::fs::symlink(&outside, pkg.join("lib/review.yaml")).unwrap();
        let error = load_package(&pkg).expect_err("refused").to_string();
        assert!(error.contains("outside the package"), "{error}");
    }
}

/// A package that carries a skill and a gate script.
fn package_with_resources(script: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let pkg = root.path().join("pkg");
    std::fs::create_dir_all(pkg.join("skills/cad")).unwrap();
    std::fs::create_dir_all(pkg.join("bin")).unwrap();
    std::fs::write(pkg.join("harness.yaml"), MAIN).unwrap();
    std::fs::write(pkg.join("review.yaml"), REVIEW).unwrap();
    std::fs::write(
        pkg.join("skills/cad/SKILL.md"),
        "---\nname: cad\n---\nMeasure twice.\n",
    )
    .unwrap();
    std::fs::write(pkg.join("bin/check.sh"), script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            pkg.join("bin/check.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    (root, pkg)
}

#[test]
fn a_package_carries_its_skills_and_scripts_and_its_digest_pins_them() {
    let (_root, pkg) = package_with_resources("#!/bin/sh\nexit 0\n");
    let bundle = load_package(&pkg).expect("loads");
    assert_eq!(
        bundle.resources.keys().collect::<Vec<_>>(),
        ["bin/check.sh", "skills/cad/SKILL.md"]
    );
    assert!(bundle.resources["bin/check.sh"].executable);
    assert!(!bundle.resources["skills/cad/SKILL.md"].executable);
    bundle.compile().expect("a package with resources compiles");

    // A bundle without resources keeps the digest it always had.
    let workflows_only = HarnessBundle {
        resources: BTreeMap::new(),
        ..bundle.clone()
    };
    assert_ne!(
        bundle.digest(),
        workflows_only.digest(),
        "resources are pinned"
    );

    // A changed script is a changed program; so is a changed mode.
    let (_other, changed) = package_with_resources("#!/bin/sh\nexit 1\n");
    assert_ne!(load_package(&changed).unwrap().digest(), bundle.digest());
    let mut not_executable = bundle.clone();
    not_executable
        .resources
        .get_mut("bin/check.sh")
        .unwrap()
        .executable = false;
    assert_ne!(not_executable.digest(), bundle.digest());

    // The serialized bundle carries them, and reads back the same.
    let text = serde_yaml::to_string(&bundle).unwrap();
    let back: HarnessBundle = serde_yaml::from_str(&text).unwrap();
    assert_eq!(back, bundle);
    assert_eq!(back.digest(), bundle.digest());
}

#[test]
fn package_resources_materialize_with_their_modes() {
    let (_root, pkg) = package_with_resources("#!/bin/sh\nexit 0\n");
    let bundle = load_package(&pkg).unwrap();
    let out = tempfile::tempdir().unwrap();
    sven_harness_spec::resources::materialize(&bundle.resources, out.path()).unwrap();
    assert_eq!(
        std::fs::read_to_string(out.path().join("skills/cad/SKILL.md")).unwrap(),
        "---\nname: cad\n---\nMeasure twice.\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(out.path().join("bin/check.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}

#[test]
fn package_resources_refuse_symlinks_and_paths_outside_their_directories() {
    let (_root, pkg) = package_with_resources("#!/bin/sh\nexit 0\n");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/passwd", pkg.join("bin/passwd")).unwrap();
        let refused = load_package(&pkg).unwrap_err().to_string();
        assert!(refused.contains("symlink"), "{refused}");
        std::fs::remove_file(pkg.join("bin/passwd")).unwrap();
    }
    for bad in [
        "etc/passwd",
        "bin/../x",
        "bin/",
        "skills//x",
        "/bin/x",
        "bin/a b",
    ] {
        assert!(
            sven_harness_spec::resources::check_path(bad).is_err(),
            "{bad} must be refused"
        );
    }
    let mut bundle = load_package(&pkg).unwrap();
    bundle.resources.insert(
        "../escape".to_string(),
        sven_harness_spec::resources::Resource {
            content: b"x".to_vec(),
            executable: false,
        },
    );
    assert!(
        bundle.compile().is_err(),
        "a bundle cannot smuggle a path out"
    );
}
