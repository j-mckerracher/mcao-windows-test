//! npm package discovery without the npm command. The prefix comes from the environment and
//! `.npmrc` files, and the package's `bin` entry comes from its `package.json`. The result is an
//! argv pair, `node <script>`, so no npm entry point or `.cmd` shim is ever started.

use std::path::PathBuf;

use serde_json::Value;

use super::discovery::{self, Found};
use super::host::{Host, Platform};

/// An argv pair that runs a package's script directly: `node <script> <args...>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeArgv {
    pub executable: PathBuf,
    pub script: PathBuf,
}

/// Finds the npm global prefix from files and environment only; `npm prefix` is never run.
///
/// Windows checks `%APPDATA%\npm`, then the `prefix=` line of `%USERPROFILE%\.npmrc`. Other
/// platforms check `NPM_CONFIG_PREFIX`, then the `prefix=` line of `$HOME/.npmrc`.
pub fn npm_prefix(host: &dyn Host) -> Result<PathBuf, String> {
    let direct = match host.platform() {
        Platform::Windows => host
            .env_var("APPDATA")
            .map(|appdata| PathBuf::from(appdata).join("npm")),
        Platform::Unix => host.env_var("NPM_CONFIG_PREFIX").map(PathBuf::from),
    };
    if let Some(dir) = direct.filter(|dir| host.is_dir(dir)) {
        return Ok(dir);
    }

    let (home_var, missing) = match host.platform() {
        Platform::Windows => (
            "USERPROFILE",
            "no npm prefix: %APPDATA%\\npm is absent and %USERPROFILE%\\.npmrc has no existing prefix=",
        ),
        Platform::Unix => (
            "HOME",
            "no npm prefix: NPM_CONFIG_PREFIX is unset and $HOME/.npmrc has no existing prefix=",
        ),
    };
    host.env_var(home_var)
        .map(PathBuf::from)
        .and_then(|home| host.read_to_string(&home.join(".npmrc")))
        .and_then(|text| parse_npmrc_prefix(&text))
        .filter(|dir| host.is_dir(dir))
        .ok_or_else(|| missing.to_owned())
}

/// The `prefix=` value of an `.npmrc` file, with surrounding whitespace and quotes removed.
pub fn parse_npmrc_prefix(text: &str) -> Option<PathBuf> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if key.trim() != "prefix" {
            return None;
        }
        let value = value.trim().trim_matches(['"', '\'']);
        (!value.is_empty()).then(|| PathBuf::from(value))
    })
}

/// Resolves a globally installed npm package to `node <script>`. Windows keeps global packages in
/// `<prefix>\node_modules`, and Unix keeps them in `<prefix>/lib/node_modules`.
pub fn resolve_npm_shim_argv(host: &dyn Host, package: &str) -> Result<NodeArgv, String> {
    let prefix = npm_prefix(host)?;
    let modules = match host.platform() {
        Platform::Windows => prefix.join("node_modules"),
        Platform::Unix => prefix.join("lib").join("node_modules"),
    };
    let package_dir = modules.join(package);
    let manifest_path = package_dir.join("package.json");
    let manifest = host
        .read_to_string(&manifest_path)
        .ok_or_else(|| format!("{} is not readable", manifest_path.display()))?;
    let script = package_dir.join(bin_path(&manifest, package)?);
    if !host.is_file(&script) {
        return Err(format!("bin script {} does not exist", script.display()));
    }
    Ok(NodeArgv {
        executable: node_executable(host)?,
        script,
    })
}

/// The script path of a package manifest. `bin` is either a string or an object keyed by command
/// name; an object with one entry is accepted whatever its key.
fn bin_path(manifest: &str, package: &str) -> Result<String, String> {
    let json: Value = serde_json::from_str(manifest)
        .map_err(|error| format!("package.json does not parse: {error}"))?;
    let Some(bin) = json.get("bin") else {
        return Err("package.json has no bin field".to_owned());
    };
    let path = match bin {
        Value::String(path) => Some(path.as_str()),
        Value::Object(map) => match map.get(package) {
            Some(entry) => entry.as_str(),
            None if map.len() == 1 => map.values().next().and_then(Value::as_str),
            None => None,
        },
        _ => None,
    };
    path.map(str::to_owned)
        .ok_or_else(|| "package.json bin entry is not a script path".to_owned())
}

fn node_executable(host: &dyn Host) -> Result<PathBuf, String> {
    match discovery::find_executable(host, "node") {
        Found::Exe(path) => Ok(path),
        Found::ShimOnly(path) => Err(format!(
            "node is only a script shim at {}; a runnable node executable is required",
            path.display()
        )),
        Found::Missing => Err("node is not on PATH".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolchain::host::fake::{FakeHost, assert_no_shell_or_npm};

    #[test]
    fn windows_prefix_prefers_appdata_npm() {
        let host = FakeHost::new(Platform::Windows)
            .env("APPDATA", "wappdata")
            .dir("wappdata/npm")
            .env("USERPROFILE", "wuser")
            .file("wuser/.npmrc", "prefix=wother\n")
            .dir("wother");
        assert_eq!(npm_prefix(&host), Ok(PathBuf::from("wappdata").join("npm")));
    }

    #[test]
    fn windows_prefix_falls_back_to_npmrc() {
        let host = FakeHost::new(Platform::Windows)
            .env("APPDATA", "wappdata")
            .env("USERPROFILE", "wuser")
            .file("wuser/.npmrc", "registry=x\nprefix = \"wprefix\"\n")
            .dir("wprefix");
        assert_eq!(npm_prefix(&host), Ok(PathBuf::from("wprefix")));
    }

    #[test]
    fn windows_prefix_error_names_both_sources() {
        let host = FakeHost::new(Platform::Windows).env("USERPROFILE", "wuser");
        let error = npm_prefix(&host).expect_err("no prefix source exists");
        assert!(error.contains("%APPDATA%\\npm"), "{error}");
        assert!(error.contains(".npmrc"), "{error}");
    }

    #[test]
    fn unix_prefix_uses_config_env_then_npmrc() {
        let from_env = FakeHost::new(Platform::Unix)
            .env("NPM_CONFIG_PREFIX", "uprefix")
            .dir("uprefix");
        assert_eq!(npm_prefix(&from_env), Ok(PathBuf::from("uprefix")));

        let from_npmrc = FakeHost::new(Platform::Unix)
            .env("HOME", "uhome")
            .file("uhome/.npmrc", "prefix=unrc\n")
            .dir("unrc");
        assert_eq!(npm_prefix(&from_npmrc), Ok(PathBuf::from("unrc")));
    }

    #[test]
    fn npmrc_prefix_ignores_other_keys_and_empty_values() {
        assert_eq!(parse_npmrc_prefix("prefixes=x\n"), None);
        assert_eq!(parse_npmrc_prefix("prefix=\n"), None);
        assert_eq!(parse_npmrc_prefix("; prefix=x\n"), None);
        assert_eq!(
            parse_npmrc_prefix("prefix='single'\n"),
            Some(PathBuf::from("single"))
        );
    }

    #[test]
    fn windows_resolves_package_to_node_argv_without_npm() {
        let script = "wappdata/npm/node_modules/tasks-axi/bin/cli.js";
        let host = FakeHost::new(Platform::Windows)
            .env("APPDATA", "wappdata")
            .dir("wappdata/npm")
            .path_var(&["wbin"])
            .file(
                "wappdata/npm/node_modules/tasks-axi/package.json",
                r#"{"bin":"bin/cli.js"}"#,
            )
            .file(script, "")
            .exe("wbin/node.exe");
        let resolved = resolve_npm_shim_argv(&host, "tasks-axi").expect("package resolves");
        assert_eq!(resolved.executable, PathBuf::from("wbin").join("node.exe"));
        assert_eq!(resolved.script, PathBuf::from(script));
        assert!(host.runs().is_empty(), "resolution starts no process");
        assert_no_shell_or_npm(&host.runs());
    }

    #[test]
    fn object_bin_selects_the_package_entry() {
        let host = FakeHost::new(Platform::Windows)
            .env("APPDATA", "wappdata")
            .dir("wappdata/npm")
            .path_var(&["wbin"])
            .file(
                "wappdata/npm/node_modules/quota-axi/package.json",
                r#"{"bin":{"quota-axi":"dist/main.js","other":"x.js"}}"#,
            )
            .file("wappdata/npm/node_modules/quota-axi/dist/main.js", "")
            .exe("wbin/node.exe");
        let resolved = resolve_npm_shim_argv(&host, "quota-axi").expect("package resolves");
        assert_eq!(
            resolved.script,
            PathBuf::from("wappdata/npm/node_modules/quota-axi/dist/main.js")
        );
    }

    #[test]
    fn unix_package_lives_under_lib_node_modules() {
        let host = FakeHost::new(Platform::Unix)
            .env("NPM_CONFIG_PREFIX", "uprefix")
            .dir("uprefix")
            .path_var(&["ubin"])
            .file(
                "uprefix/lib/node_modules/no-mistakes/package.json",
                r#"{"bin":"bin/nm.js"}"#,
            )
            .file("uprefix/lib/node_modules/no-mistakes/bin/nm.js", "")
            .exe("ubin/node");
        let resolved = resolve_npm_shim_argv(&host, "no-mistakes").expect("package resolves");
        assert_eq!(
            resolved.script,
            PathBuf::from("uprefix/lib/node_modules/no-mistakes/bin/nm.js")
        );
    }

    #[test]
    fn missing_package_and_shim_only_node_are_errors() {
        let no_package = FakeHost::new(Platform::Windows)
            .env("APPDATA", "wappdata")
            .dir("wappdata/npm")
            .path_var(&["wbin"])
            .exe("wbin/node.exe");
        assert!(resolve_npm_shim_argv(&no_package, "absent").is_err());

        let shim_node = FakeHost::new(Platform::Windows)
            .env("APPDATA", "wappdata")
            .dir("wappdata/npm")
            .path_var(&["wbin"])
            .file(
                "wappdata/npm/node_modules/tool/package.json",
                r#"{"bin":"t.js"}"#,
            )
            .file("wappdata/npm/node_modules/tool/t.js", "")
            .file("wbin/node.cmd", "");
        let error = resolve_npm_shim_argv(&shim_node, "tool").expect_err("node shim is rejected");
        assert!(error.contains("shim"), "{error}");
    }

    #[test]
    fn manifest_without_bin_is_rejected() {
        assert!(bin_path(r#"{"name":"x"}"#, "x").is_err());
        assert!(bin_path("not json", "x").is_err());
    }
}
