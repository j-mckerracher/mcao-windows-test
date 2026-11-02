//! Platform leakage guard for DPES-752 AC4 and review finding F2.
//!
//! The guard reads what the workspace declares instead of naming crates here. `cargo metadata
//! --locked` lists each workspace package with its normal dependencies and each dependency's
//! `target` cfg, and the locked version of every package. A workspace crate takes its platform
//! family from its declarations: it is platform-only when every normal declaration of it is gated
//! to one family, and a platform-only package is not a product graph. A workspace crate declared
//! under mixed gates fails the guard instead of being classified: dropping it would hide a live
//! dependency from the check.
//! A third-party crate takes its family from its real platform support, not from the workspace's
//! gate on it, because a gate says where the workspace asks for a crate and not what the crate
//! supports. Each third-party crate that a package gates to a family, or declares both
//! unconditionally and gated, must have a `SUPPORT_RECORDS` entry pinned to its locked version.
//! `cargo tree --locked` then gives each product package's normal dependency graph on each named
//! triple, evaluated by cargo's own cfg logic. No shell, Python, or other interpreter is involved.
//! Every graph must contain its own package, and every declaration the workspace makes must appear
//! in the graph of its own family, so the negative assertions cannot pass on an empty or broken
//! graph. A dependency gated by a cfg other than `cfg(unix)` or `cfg(windows)` fails the guard
//! loudly instead of being ignored. The guard covers only platform-gated crates that a workspace
//! package declares; a platform-specific crate reached only through a third-party dependency and
//! never declared by the workspace is outside its reach.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::process::Command;

use serde_json::Value;

const WINDOWS: &str = "x86_64-pc-windows-msvc";
const LINUX: &str = "x86_64-unknown-linux-gnu";
const MACOS: &str = "aarch64-apple-darwin";

/// The named triples, each with its platform family. The mapping is written here, not inferred.
const TRIPLES: [(&str, Family); 3] = [
    (WINDOWS, Family::Windows),
    (LINUX, Family::Unix),
    (MACOS, Family::Unix),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Family {
    Unix,
    Windows,
}

const FAMILIES: [Family; 2] = [Family::Unix, Family::Windows];

impl Family {
    fn other(self) -> Self {
        match self {
            Self::Unix => Self::Windows,
            Self::Windows => Self::Unix,
        }
    }
}

/// How a normal dependency is gated in its manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Gate {
    Always,
    Only(Family),
}

/// The real platform support of a third-party crate that a workspace package declares under a
/// platform gate, or under both an unconditional and a platform-gated declaration. The record is
/// pinned to the locked version: a version change fails the guard until its support is reviewed.
struct SupportRecord {
    name: &'static str,
    version: &'static str,
    families: &'static [Family],
}

/// Each entry states the crate's own platform support and the source it was read from.
const SUPPORT_RECORDS: [SupportRecord; 4] = [
    // libc 0.2.191: docs.rs `targets` list x86_64-pc-windows-msvc, and src/lib.rs has `#[cfg(windows)]` items.
    SupportRecord {
        name: "libc",
        version: "0.2.191",
        families: &[Family::Unix, Family::Windows],
    },
    // windows-sys 0.61.2: "Rust for Windows" bindings; docs.rs default-target x86_64-pc-windows-msvc.
    SupportRecord {
        name: "windows-sys",
        version: "0.61.2",
        families: &[Family::Windows],
    },
    // serde 1.0.229: src/core/de/impls.rs and src/core/ser/impls.rs have `cfg(unix)` and `cfg(windows)` arms.
    SupportRecord {
        name: "serde",
        version: "1.0.229",
        families: &[Family::Unix, Family::Windows],
    },
    // serde_json 1.0.154: pure Rust; src has no `cfg(unix)`, `cfg(windows)`, or `target_os` gate.
    SupportRecord {
        name: "serde_json",
        version: "1.0.154",
        families: &[Family::Unix, Family::Windows],
    },
];

/// One normal dependency that a workspace package declares.
struct Declaration {
    package: String,
    dependency: String,
    gate: Gate,
}

/// The workspace's own packages, their normal dependency declarations, the locked versions of
/// every package, and the support records that classify third-party crates.
struct Workspace {
    packages: BTreeSet<String>,
    declarations: Vec<Declaration>,
    locked_versions: BTreeMap<String, BTreeSet<String>>,
    records: &'static [SupportRecord],
}

impl Workspace {
    fn load() -> Self {
        let output = Command::new(cargo())
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["metadata", "--locked", "--format-version", "1"])
            .output()
            .expect("spawn cargo metadata");
        assert!(
            output.status.success(),
            "cargo metadata --locked failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata: Value =
            serde_json::from_slice(&output.stdout).expect("cargo metadata output is JSON");

        let members: BTreeSet<String> = metadata["workspace_members"]
            .as_array()
            .expect("metadata lists workspace members")
            .iter()
            .map(|id| {
                id.as_str()
                    .expect("workspace member id is a string")
                    .to_owned()
            })
            .collect();
        let mut packages = BTreeSet::new();
        let mut declarations = Vec::new();
        let mut locked_versions: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for package in metadata["packages"]
            .as_array()
            .expect("metadata lists packages")
        {
            let name = package["name"].as_str().expect("package name is a string");
            locked_versions.entry(name.to_owned()).or_default().insert(
                package["version"]
                    .as_str()
                    .expect("package version is a string")
                    .to_owned(),
            );
            let id = package["id"].as_str().expect("package id is a string");
            if !members.contains(id) {
                continue;
            }
            packages.insert(name.to_owned());
            for dependency in package["dependencies"]
                .as_array()
                .expect("package lists dependencies")
            {
                // Only normal dependencies ship; dev and build dependencies are not checked.
                if dependency["kind"].is_null() {
                    declarations.push(Declaration {
                        package: name.to_owned(),
                        dependency: dependency["name"]
                            .as_str()
                            .expect("dependency name is a string")
                            .to_owned(),
                        gate: gate_of(&dependency["target"]),
                    });
                }
            }
        }
        Self {
            packages,
            declarations,
            locked_versions,
            records: &SUPPORT_RECORDS,
        }
    }

    /// Each crate the workspace declares, mapped to the platform families it is supported on. A
    /// workspace crate takes its families from its declarations. A third-party crate takes its
    /// families from its support record; one without a record fails unless every declaration of it
    /// is unconditional.
    fn supported_families(&self) -> BTreeMap<String, BTreeSet<Family>> {
        let mut declared: BTreeMap<String, BTreeSet<Gate>> = BTreeMap::new();
        for declaration in &self.declarations {
            declared
                .entry(declaration.dependency.clone())
                .or_default()
                .insert(declaration.gate);
        }
        declared
            .into_iter()
            .map(|(name, gates)| {
                let families = if self.packages.contains(&name) {
                    workspace_families(&name, &gates)
                } else {
                    self.third_party_families(&name, &gates)
                };
                (name, families)
            })
            .collect()
    }

    fn third_party_families(&self, name: &str, gates: &BTreeSet<Gate>) -> BTreeSet<Family> {
        match self.records.iter().find(|record| record.name == name) {
            Some(record) => {
                self.assert_locked_version(record);
                record.families.iter().copied().collect()
            }
            None if gates.len() == 1 && gates.contains(&Gate::Always) => {
                FAMILIES.into_iter().collect()
            }
            None => panic!(
                "crate `{name}` has platform gates {gates:?} and no support record: \
                 record its real platform support in SUPPORT_RECORDS"
            ),
        }
    }

    fn assert_locked_version(&self, record: &SupportRecord) {
        let locked = self
            .locked_versions
            .get(record.name)
            .cloned()
            .unwrap_or_default();
        assert!(
            locked.len() == 1 && locked.contains(record.version),
            "support record for `{}` pins {} but Cargo.lock has {locked:?}: \
             review its platform support, then update the record",
            record.name,
            record.version
        );
    }

    /// Crates supported on `family` alone.
    fn family_only(&self, family: Family) -> BTreeSet<String> {
        self.supported_families()
            .into_iter()
            .filter(|(_, families)| *families == BTreeSet::from([family]))
            .map(|(name, _)| name)
            .collect()
    }

    /// Packages that are not platform-only themselves. Only these graphs are checked.
    fn products(&self) -> BTreeSet<String> {
        let platform_only: BTreeSet<String> = self
            .supported_families()
            .into_iter()
            .filter(|(_, families)| families.len() == 1)
            .map(|(name, _)| name)
            .collect();
        self.packages.difference(&platform_only).cloned().collect()
    }
}

/// A workspace crate's families, from the one gate that every declaration of it shares.
fn workspace_families(name: &str, gates: &BTreeSet<Gate>) -> BTreeSet<Family> {
    let mut distinct = gates.iter().copied();
    match (distinct.next(), distinct.next()) {
        (Some(Gate::Always), None) => FAMILIES.into_iter().collect(),
        (Some(Gate::Only(family)), None) => BTreeSet::from([family]),
        _ => panic!(
            "crate `{name}` has mixed platform gates {gates:?}: \
             the guard places each workspace crate in one family or in none"
        ),
    }
}

fn gate_of(target: &Value) -> Gate {
    match target {
        Value::Null => Gate::Always,
        Value::String(cfg) => match cfg.as_str() {
            "cfg(unix)" => Gate::Only(Family::Unix),
            "cfg(windows)" => Gate::Only(Family::Windows),
            other => panic!(
                "unclassified platform gate `{other}`: the guard knows only cfg(unix) and cfg(windows)"
            ),
        },
        other => panic!("unexpected dependency target field: {other}"),
    }
}

fn cargo() -> OsString {
    env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

fn normal_dependencies(package: &str, target: &str) -> BTreeSet<String> {
    let output = Command::new(cargo())
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "tree", "--locked", "-p", package, "--target", target, "--edges", "normal", "--prefix",
            "none", "--format", "{p}",
        ])
        .output()
        .expect("spawn cargo tree");
    assert!(
        output.status.success(),
        "cargo tree -p {package} --target {target} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("cargo tree output is UTF-8")
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_product_graph_excludes_the_other_platform_family() {
    let workspace = Workspace::load();
    let products = workspace.products();
    assert!(
        !products.is_empty(),
        "no product package found in the workspace"
    );
    for family in FAMILIES {
        assert!(
            !workspace.family_only(family).is_empty(),
            "no {family:?}-only crate declared in the workspace; the exclusion check would be vacuous"
        );
    }

    for package in &products {
        for (target, family) in TRIPLES {
            let deps = normal_dependencies(package, target);
            assert!(
                deps.contains(package),
                "positive control: {package} missing from its own {target} graph: {deps:?}"
            );
            for krate in workspace.family_only(family.other()) {
                assert!(
                    !deps.contains(&krate),
                    "{krate} leaked into the {package} graph on {target}: {deps:?}"
                );
            }
        }
    }
}

#[test]
fn every_platform_gated_declaration_is_in_its_own_family_graph() {
    let workspace = Workspace::load();
    let products = workspace.products();
    let mut checked = 0;
    for declaration in workspace
        .declarations
        .iter()
        .filter(|declaration| products.contains(&declaration.package))
    {
        let Gate::Only(family) = declaration.gate else {
            continue;
        };
        for (target, _) in TRIPLES
            .into_iter()
            .filter(|(_, triple_family)| *triple_family == family)
        {
            let deps = normal_dependencies(&declaration.package, target);
            assert!(
                deps.contains(&declaration.dependency),
                "{} declares {} for {family:?}, but it is missing from the {target} graph: {deps:?}",
                declaration.package,
                declaration.dependency
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "no platform-gated declaration in a product package; the positive control is empty"
    );
}

#[test]
fn every_unconditional_declaration_is_in_every_product_graph() {
    let workspace = Workspace::load();
    let products = workspace.products();
    let mut checked = 0;
    for declaration in workspace
        .declarations
        .iter()
        .filter(|declaration| declaration.gate == Gate::Always)
        .filter(|declaration| products.contains(&declaration.package))
    {
        for (target, _) in TRIPLES {
            let deps = normal_dependencies(&declaration.package, target);
            assert!(
                deps.contains(&declaration.dependency),
                "{} declares {} unconditionally, but it is missing from the {target} graph: {deps:?}",
                declaration.package,
                declaration.dependency
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "no unconditional declaration in a product package; the positive control is empty"
    );
}

/// A platform-only package is not a product, so its own graph is checked here on the other
/// family's triples. Its graph must hold no crate of its own family except itself.
#[test]
fn platform_only_package_graph_excludes_its_family_on_the_other_platform() {
    let workspace = Workspace::load();
    let mut checked = 0;
    for (package, families) in workspace.supported_families() {
        if families.len() != 1 || !workspace.packages.contains(&package) {
            continue;
        }
        let family = families.iter().copied().next().expect("one family");
        assert_third_party_gated_to(&workspace, &package, family);
        for (target, _) in TRIPLES
            .into_iter()
            .filter(|(_, triple_family)| *triple_family != family)
        {
            let deps = normal_dependencies(&package, target);
            assert!(
                deps.contains(&package),
                "positive control: {package} missing from its own {target} graph: {deps:?}"
            );
            for krate in workspace.family_only(family) {
                if krate == package {
                    continue;
                }
                assert!(
                    !deps.contains(&krate),
                    "{krate} leaked into the {package} graph on {target}: {deps:?}"
                );
            }
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "no platform-only workspace package; the check is vacuous"
    );
}

/// A platform-only package must gate each third-party dependency it declares to its own family, so
/// the other platform's graph cannot receive it. Workspace crates are exempt because the guard
/// classifies them from the declarations that name them.
fn assert_third_party_gated_to(workspace: &Workspace, package: &str, family: Family) {
    let declarations = workspace
        .declarations
        .iter()
        .filter(|declaration| declaration.package == package)
        .filter(|declaration| !workspace.packages.contains(&declaration.dependency));
    for declaration in declarations {
        assert!(
            declaration.gate == Gate::Only(family),
            "{package} declares third-party {} without gating it to {family:?}",
            declaration.dependency
        );
    }
}

fn declaration(package: &str, dependency: &str, gate: Gate) -> Declaration {
    Declaration {
        package: package.to_owned(),
        dependency: dependency.to_owned(),
        gate,
    }
}

fn synthetic_workspace(
    declarations: Vec<Declaration>,
    locked_versions: &[(&str, &str)],
    records: &'static [SupportRecord],
) -> Workspace {
    let mut locked: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (name, version) in locked_versions {
        locked
            .entry((*name).to_owned())
            .or_default()
            .insert((*version).to_owned());
    }
    Workspace {
        packages: BTreeSet::from(["mcao-core".to_owned(), "mcao-probe".to_owned()]),
        declarations,
        locked_versions: locked,
        records,
    }
}

const TEST_LIBC_RECORD: [SupportRecord; 1] = [SupportRecord {
    name: "libc",
    version: "0.2.191",
    families: &[Family::Unix, Family::Windows],
}];

const TEST_UNIX_ONLY_RECORD: [SupportRecord; 1] = [SupportRecord {
    name: "unix-only-crate",
    version: "1.0.0",
    families: &[Family::Unix],
}];

/// The review's F2 reproduction: a third-party crate gated to a family beside an unconditional
/// declaration. Without a support record the guard must refuse the crate, not drop it from the
/// exclusion check.
#[test]
#[should_panic(expected = "has platform gates")]
fn crate_gated_to_a_family_and_also_declared_unconditionally_is_refused() {
    let workspace = synthetic_workspace(
        vec![
            declaration("mcao-probe", "unix-only-crate", Gate::Only(Family::Unix)),
            declaration("mcao-core", "unix-only-crate", Gate::Always),
        ],
        &[],
        &[],
    );
    workspace.supported_families();
}

#[test]
#[should_panic(expected = "has platform gates")]
fn crate_gated_to_both_families_without_a_record_is_refused() {
    let workspace = synthetic_workspace(
        vec![
            declaration("mcao-probe", "unix-only-crate", Gate::Only(Family::Unix)),
            declaration("mcao-core", "unix-only-crate", Gate::Only(Family::Windows)),
        ],
        &[],
        &[],
    );
    workspace.supported_families();
}

#[test]
#[should_panic(expected = "has platform gates")]
fn crate_gated_to_one_family_without_a_record_is_refused() {
    let workspace = synthetic_workspace(
        vec![
            declaration("mcao-probe", "unix-only-crate", Gate::Only(Family::Unix)),
            declaration("mcao-core", "unix-only-crate", Gate::Only(Family::Unix)),
        ],
        &[],
        &[],
    );
    workspace.supported_families();
}

/// A mixed-gate crate that has a record takes the recorded families, so libc's Unix-gated
/// declarations do not make it Unix-only.
#[test]
fn crate_with_a_support_record_takes_its_recorded_families() {
    let workspace = synthetic_workspace(
        vec![
            declaration("mcao-probe", "libc", Gate::Only(Family::Unix)),
            declaration("mcao-core", "libc", Gate::Always),
            declaration("mcao-core", "serde_json", Gate::Always),
        ],
        &[("libc", "0.2.191"), ("serde_json", "1.0.154")],
        &TEST_LIBC_RECORD,
    );
    let families = workspace.supported_families();
    assert_eq!(
        families.get("libc"),
        Some(&BTreeSet::from(FAMILIES)),
        "recorded libc must keep both families"
    );
    assert!(!workspace.family_only(Family::Unix).contains("libc"));
    assert!(!workspace.family_only(Family::Windows).contains("libc"));
    assert_eq!(
        families.get("serde_json"),
        Some(&BTreeSet::from(FAMILIES)),
        "an unconditional third-party crate without a record is supported on both families"
    );
}

/// The classification half of the negative control: a recorded Unix-only crate is excluded from
/// every Windows graph check, so a Windows product that reaches it fails the leak assertion.
#[test]
fn recorded_unix_only_crate_is_classified_as_unix_only() {
    let workspace = synthetic_workspace(
        vec![declaration(
            "mcao-core",
            "unix-only-crate",
            Gate::Only(Family::Unix),
        )],
        &[("unix-only-crate", "1.0.0")],
        &TEST_UNIX_ONLY_RECORD,
    );
    assert!(
        workspace
            .family_only(Family::Unix)
            .contains("unix-only-crate")
    );
}

#[test]
#[should_panic(expected = "pins 0.2.191 but Cargo.lock has")]
fn support_record_for_a_different_locked_version_is_refused() {
    let workspace = synthetic_workspace(
        vec![declaration("mcao-core", "libc", Gate::Only(Family::Unix))],
        &[("libc", "0.2.190")],
        &TEST_LIBC_RECORD,
    );
    workspace.supported_families();
}

#[test]
fn platform_only_package_gates_each_third_party_dependency_to_its_family() {
    let workspace = synthetic_workspace(
        vec![
            declaration("mcao-probe", "windows-sys", Gate::Only(Family::Windows)),
            declaration("mcao-probe", "mcao-core", Gate::Always),
        ],
        &[],
        &[],
    );
    assert_third_party_gated_to(&workspace, "mcao-probe", Family::Windows);
}

#[test]
#[should_panic(
    expected = "mcao-probe declares third-party windows-sys without gating it to Windows"
)]
fn platform_only_package_with_ungated_third_party_dependency_is_refused() {
    let workspace = synthetic_workspace(
        vec![
            declaration("mcao-probe", "windows-sys", Gate::Always),
            declaration("mcao-probe", "mcao-core", Gate::Always),
        ],
        &[],
        &[],
    );
    assert_third_party_gated_to(&workspace, "mcao-probe", Family::Windows);
}
