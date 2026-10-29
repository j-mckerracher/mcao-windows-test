//! Platform leakage guard for DPES-752 AC4 and review finding F2.
//!
//! The guard reads what the workspace declares instead of naming crates here. `cargo metadata
//! --no-deps --locked` lists each workspace package with its normal dependencies and each
//! dependency's `target` cfg. A crate is platform-only when every normal declaration of it is gated
//! to one family, and a package that is itself platform-only is not a product graph. A crate
//! declared under mixed gates, such as an ungated declaration beside a platform-gated one, fails
//! the guard instead of being classified: dropping it would hide a live dependency from the
//! check. `cargo tree --locked` then gives each product package's normal dependency graph on each
//! named triple, evaluated by cargo's own cfg logic. No shell, Python, or other interpreter is
//! involved. Every graph must contain its own package, and every declaration the workspace makes
//! must appear in the graph of its own family, so the negative assertions cannot pass on an empty
//! or broken graph. A dependency gated by a cfg other than `cfg(unix)` or `cfg(windows)` fails
//! the guard loudly instead of being ignored. The guard covers only platform-gated crates that a
//! workspace package declares; a platform-specific crate reached only through a third-party
//! dependency is outside its reach.

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

/// One normal dependency that a workspace package declares.
struct Declaration {
    package: String,
    dependency: String,
    gate: Gate,
}

/// The workspace's own packages and their normal dependency declarations.
struct Workspace {
    packages: BTreeSet<String>,
    declarations: Vec<Declaration>,
}

impl Workspace {
    fn load() -> Self {
        let output = Command::new(cargo())
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["metadata", "--no-deps", "--locked", "--format-version", "1"])
            .output()
            .expect("spawn cargo metadata");
        assert!(
            output.status.success(),
            "cargo metadata --no-deps --locked failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata: Value =
            serde_json::from_slice(&output.stdout).expect("cargo metadata output is JSON");

        let mut packages = BTreeSet::new();
        let mut declarations = Vec::new();
        for package in metadata["packages"]
            .as_array()
            .expect("metadata lists packages")
        {
            let name = package["name"].as_str().expect("package name is a string");
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
        }
    }

    /// Each crate the workspace declares, mapped to the one family that gates every declaration of
    /// it, or to `None` when every declaration is unconditional. A crate declared under mixed gates
    /// panics rather than being classified: classifying it would drop a platform-gated declaration
    /// out of the exclusion check, which is the leak this guard exists to catch.
    fn crate_families(&self) -> BTreeMap<String, Option<Family>> {
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
                let mut distinct = gates.iter().copied();
                let family = match (distinct.next(), distinct.next()) {
                    (Some(Gate::Always), None) => None,
                    (Some(Gate::Only(family)), None) => Some(family),
                    _ => panic!(
                        "crate `{name}` has mixed platform gates {gates:?}: \
                         the guard places each crate in one family or in none"
                    ),
                };
                (name, family)
            })
            .collect()
    }

    /// Crates whose every normal declaration in the workspace is gated to `family`.
    fn family_only(&self, family: Family) -> BTreeSet<String> {
        self.crate_families()
            .into_iter()
            .filter(|(_, crate_family)| *crate_family == Some(family))
            .map(|(name, _)| name)
            .collect()
    }

    /// Packages that are not platform-only themselves. Only these graphs are checked.
    fn products(&self) -> BTreeSet<String> {
        let platform_only: BTreeSet<String> = self
            .crate_families()
            .into_iter()
            .filter_map(|(name, family)| family.map(|_| name))
            .collect();
        self.packages.difference(&platform_only).cloned().collect()
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

/// A platform-only workspace package is not a product, so its own graph is checked here on the
/// other family's triples. Its graph must hold no crate of its own family except itself.
#[test]
fn platform_only_package_graph_excludes_its_family_on_the_other_platform() {
    let workspace = Workspace::load();
    let mut checked = 0;
    for (package, family) in workspace.crate_families() {
        let Some(family) = family else {
            continue;
        };
        if !workspace.packages.contains(&package) {
            continue;
        }
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

/// The review's F2 reproduction: a Unix-gated declaration beside an unconditional one. The guard
/// must refuse the crate, not drop it from the exclusion check.
#[test]
#[should_panic(expected = "mixed platform gates")]
fn crate_gated_to_a_family_and_also_declared_unconditionally_is_refused() {
    let workspace = Workspace {
        packages: BTreeSet::from(["mcao-core".to_owned(), "mcao-probe".to_owned()]),
        declarations: vec![
            declaration("mcao-probe", "mcao-backend-unix", Gate::Only(Family::Unix)),
            declaration("mcao-core", "mcao-backend-unix", Gate::Always),
        ],
    };
    workspace.crate_families();
}

#[test]
#[should_panic(expected = "mixed platform gates")]
fn crate_gated_to_both_families_is_refused() {
    let workspace = Workspace {
        packages: BTreeSet::from(["mcao-core".to_owned(), "mcao-probe".to_owned()]),
        declarations: vec![
            declaration("mcao-probe", "mcao-backend-unix", Gate::Only(Family::Unix)),
            declaration(
                "mcao-core",
                "mcao-backend-unix",
                Gate::Only(Family::Windows),
            ),
        ],
    };
    workspace.crate_families();
}

#[test]
fn crate_gated_to_one_family_in_every_declaration_is_classified_by_it() {
    let workspace = Workspace {
        packages: BTreeSet::from(["mcao-core".to_owned(), "mcao-probe".to_owned()]),
        declarations: vec![
            declaration("mcao-probe", "mcao-backend-unix", Gate::Only(Family::Unix)),
            declaration("mcao-core", "mcao-backend-unix", Gate::Only(Family::Unix)),
            declaration("mcao-core", "serde_json", Gate::Always),
        ],
    };
    let families = workspace.crate_families();
    assert_eq!(families.get("mcao-backend-unix"), Some(&Some(Family::Unix)));
    assert_eq!(families.get("serde_json"), Some(&None));
}

#[test]
fn platform_only_package_gates_each_third_party_dependency_to_its_family() {
    let workspace = Workspace {
        packages: BTreeSet::from(["mcao-backend-windows".to_owned(), "mcao-core".to_owned()]),
        declarations: vec![
            declaration(
                "mcao-backend-windows",
                "windows-sys",
                Gate::Only(Family::Windows),
            ),
            declaration("mcao-backend-windows", "mcao-core", Gate::Always),
        ],
    };
    assert_third_party_gated_to(&workspace, "mcao-backend-windows", Family::Windows);
}

#[test]
#[should_panic(
    expected = "mcao-backend-windows declares third-party windows-sys without gating it to Windows"
)]
fn platform_only_package_with_ungated_third_party_dependency_is_refused() {
    let workspace = Workspace {
        packages: BTreeSet::from(["mcao-backend-windows".to_owned(), "mcao-core".to_owned()]),
        declarations: vec![
            declaration("mcao-backend-windows", "windows-sys", Gate::Always),
            declaration("mcao-backend-windows", "mcao-core", Gate::Always),
        ],
    };
    assert_third_party_gated_to(&workspace, "mcao-backend-windows", Family::Windows);
}
