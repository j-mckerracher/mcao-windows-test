//! `resource-thresholds` parsing. Parsing is pure; [`load_thresholds`] is the only read.
//!
//! Grammar: one `<key> <value>` pair per line, `#` comments, blank lines ignored. A value is
//! `off` or a number. Any bad line (wrong field count, duplicate or unknown key, bad value)
//! discards the whole file and applies the built-in defaults, with a warning that names the
//! file and line.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::Path;

use super::ResourceKind;

pub const THRESHOLDS_FILE: &str = "resource-thresholds";

/// Limits in the file's units. `None` means the check is off.
#[derive(Clone, Debug, PartialEq)]
pub struct Thresholds {
    pub memory_used_pct: Option<f64>,
    pub swap_used_pct: Option<f64>,
    pub cpu_used_pct: Option<f64>,
    pub disk_free_pct: Option<f64>,
    pub running_agents: Option<u32>,
}

impl Thresholds {
    pub fn defaults() -> Self {
        Self {
            memory_used_pct: Some(90.0),
            swap_used_pct: Some(85.0),
            cpu_used_pct: Some(90.0),
            disk_free_pct: Some(10.0),
            running_agents: None,
        }
    }

    pub fn limit_for(&self, kind: ResourceKind) -> Option<f64> {
        match kind {
            ResourceKind::MemoryUsedPct => self.memory_used_pct,
            ResourceKind::SwapUsedPct => self.swap_used_pct,
            ResourceKind::CpuUsedPct => self.cpu_used_pct,
            ResourceKind::DiskFreePct => self.disk_free_pct,
            ResourceKind::RunningAgents => self.running_agents.map(f64::from),
        }
    }
}

/// Thresholds plus where they came from. `source` is `defaults` when no file was applied.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedThresholds {
    pub thresholds: Thresholds,
    pub source: String,
    pub warning: Option<String>,
}

impl LoadedThresholds {
    fn defaults_with(warning: Option<String>) -> Self {
        Self {
            thresholds: Thresholds::defaults(),
            source: "defaults".to_string(),
            warning,
        }
    }
}

/// Parses thresholds text. `label` names the source in the source field and in warnings.
pub fn parse_thresholds(text: &str, label: &str) -> LoadedThresholds {
    // Windows editors often write a UTF-8 byte-order mark, which is not part of the first key.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut thresholds = Thresholds::defaults();
    let mut seen = HashSet::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Err(why) = apply_line(line, &mut seen, &mut thresholds) {
            return LoadedThresholds::defaults_with(Some(format!(
                "{label}:{}: {why}; built-in defaults apply",
                index + 1
            )));
        }
    }
    LoadedThresholds {
        thresholds,
        source: label.to_string(),
        warning: None,
    }
}

/// Reads `<config_dir>/resource-thresholds`. An absent file is normal and silent. An
/// unreadable one falls back to defaults with a warning.
pub fn load_thresholds(config_dir: &Path) -> LoadedThresholds {
    let path = config_dir.join(THRESHOLDS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_thresholds(&text, &path.display().to_string()),
        Err(e) if e.kind() == ErrorKind::NotFound => LoadedThresholds::defaults_with(None),
        Err(e) => LoadedThresholds::defaults_with(Some(format!(
            "{} unreadable: {e}; built-in defaults apply",
            path.display()
        ))),
    }
}

fn apply_line<'a>(
    line: &'a str,
    seen: &mut HashSet<&'a str>,
    thresholds: &mut Thresholds,
) -> Result<(), String> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let (key, value) = match fields.as_slice() {
        [key, value] => (*key, *value),
        _ => return Err("expected `<key> <value>`".to_string()),
    };
    if !seen.insert(key) {
        return Err("duplicate key".to_string());
    }
    match key {
        "memory_used_pct" => thresholds.memory_used_pct = parse_percent(value)?,
        "swap_used_pct" => thresholds.swap_used_pct = parse_percent(value)?,
        "cpu_used_pct" => thresholds.cpu_used_pct = parse_percent(value)?,
        "disk_free_pct" => thresholds.disk_free_pct = parse_percent(value)?,
        "running_agents" => thresholds.running_agents = parse_count(value)?,
        _ => return Err("unknown key".to_string()),
    }
    Ok(())
}

fn parse_percent(value: &str) -> Result<Option<f64>, String> {
    if value == "off" {
        return Ok(None);
    }
    match value.parse::<f64>() {
        Ok(v) if v > 0.0 && v <= 100.0 => Ok(Some(v)),
        _ => Err("value must be `off` or a number above 0 and at most 100".to_string()),
    }
}

fn parse_count(value: &str) -> Result<Option<u32>, String> {
    if value == "off" {
        return Ok(None);
    }
    match value.parse::<u32>() {
        Ok(v) if (1..=999_999).contains(&v) => Ok(Some(v)),
        _ => Err("value must be `off` or an integer from 1 to 999999".to_string()),
    }
}
