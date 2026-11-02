#Requires -Version 7.0
<#
.SYNOPSIS
Redacts Rust workspace CI failure logs and writes a manifest of what was removed.

.DESCRIPTION
Reads each file directly under -InputDir whose name matches -Include (default rust-ci-*.log) and
writes a redacted copy of it under -OutDir, then writes _MANIFEST.txt there. Runs under pwsh on
every runner, so Windows legs need no bash. Redaction rules:
- A line matching a credential pattern is replaced by one marker line. The GitHub token prefixes
  ghp_, gho_, ghu_, ghs_, ghr_ and github_pat_, the Bearer and Basic Authorization schemes, URL
  userinfo with a password (scheme://user:password@host), and sk- keys are credential patterns.
  Token prefixes must not be preceded by a letter or digit, so task-ids and risk-words are kept.
- The value of every non-empty environment variable named FM_*, containing SECRET, TOKEN, KEY,
  CREDENTIAL, PASSWORD, PASSWD or PASSPHRASE, or ending in _PWD is replaced wherever it occurs.
  A value may span lines and is matched across either line ending, so a multi-line value is removed
  whole. The bare PWD variable is the shell working directory, not a secret, so it is not matched.
- A path under the home directory's .aws, .ssh or .git directory, a .cargo/credentials file, or a
  .netrc or _netrc file is replaced by a marker.
The manifest records counts per file and environment variable names, never values.
#>
param(
  [Parameter(Mandatory)][string]$InputDir,
  [Parameter(Mandatory)][string]$OutDir,
  [string[]]$Include = @('rust-ci-*.log')
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $InputDir -PathType Container)) {
  throw "input directory is missing: $InputDir"
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$linePatterns = [ordered]@{
  'ghp_'                  = '(?<![A-Za-z0-9])ghp_'
  'gho_'                  = '(?<![A-Za-z0-9])gho_'
  'ghu_'                  = '(?<![A-Za-z0-9])ghu_'
  'ghs_'                  = '(?<![A-Za-z0-9])ghs_'
  'ghr_'                  = '(?<![A-Za-z0-9])ghr_'
  'github_pat_'           = '(?<![A-Za-z0-9])github_pat_'
  'Bearer '               = '(?<![A-Za-z0-9])Bearer '
  'Authorization Basic'   = '(?<![A-Za-z0-9])Authorization["'']?\s*:\s*["'']?Basic\s'
  'URL userinfo'          = '[A-Za-z][A-Za-z0-9+.-]*://[^\s/@]+:[^\s/@]+@'
  'sk-'                   = '(?<![A-Za-z0-9])sk-'
}

$envValues = @{}
foreach ($entry in [Environment]::GetEnvironmentVariables().GetEnumerator()) {
  $name = [string]$entry.Key
  $value = [string]$entry.Value
  if ($value -eq '') { continue }
  if ($name -like 'FM_*' -or $name -match 'SECRET|TOKEN|KEY|CREDENTIAL|PASSWORD|PASSWD|PASSPHRASE|_PWD$') {
    $envValues[$name] = $value
  }
}
# Longest values first, so a value that contains a shorter one is removed whole. Each value is
# matched with any line ending between its lines, because Get-Content -Raw keeps the file's own.
$envPatterns = @($envValues.GetEnumerator() | Sort-Object { ([string]$_.Value).Length } -Descending | ForEach-Object {
  [pscustomobject]@{
    Name    = [string]$_.Key
    Pattern = (([string]$_.Value -split "`r?`n" | ForEach-Object { [regex]::Escape($_) }) -join '\r?\n')
  }
})

$homePathPattern = $null
if ($HOME) {
  # Each separator matches a run of slashes or backslashes, so the forward-slash and JSON-escaped
  # (doubled backslash) spellings of the home directory match as well as the native one.
  $homeRoot = (($HOME.TrimEnd('/', '\') -split '[\\/]+') | ForEach-Object { [regex]::Escape($_) }) -join '[\\/]+'
  $homePathPattern = $homeRoot +
    '[\\/]+(?:\.(?:aws|ssh|git)(?:[\\/]+[^\s"''<>|]*)?|\.cargo[\\/]+credentials[^\s"''<>|]*|\.netrc|_netrc)(?![A-Za-z0-9_.-])'
}

$inputs = @(Get-ChildItem -LiteralPath $InputDir -File | Where-Object {
  foreach ($pattern in $Include) { if ($_.Name -like $pattern) { return $true } }
  return $false
} | Sort-Object Name)
$manifest = [System.Collections.Generic.List[string]]::new()
$manifest.Add('Rust workspace CI failure-artifact redaction manifest')
$manifest.Add("files: $($inputs.Count)")
$envRedacted = [System.Collections.Generic.SortedSet[string]]::new()

foreach ($file in $inputs) {
  $text = Get-Content -LiteralPath $file.FullName -Raw
  if ($null -eq $text) { $text = '' }
  $envHits = 0
  foreach ($envPattern in $envPatterns) {
    $count = [regex]::Matches($text, $envPattern.Pattern).Count
    if ($count -gt 0) {
      $envHits += $count
      $envRedacted.Add($envPattern.Name) | Out-Null
      $text = [regex]::Replace($text, $envPattern.Pattern, '[REDACTED]')
    }
  }
  $lines = @($text -split "`r?`n")
  if ($lines.Count -gt 0 -and $lines[-1] -eq '') { $lines = @($lines | Select-Object -SkipLast 1) }

  $redacted = [System.Collections.Generic.List[string]]::new()
  $lineHits = [ordered]@{}
  $pathHits = 0
  foreach ($line in $lines) {
    $matchedPattern = $null
    foreach ($pattern in $linePatterns.GetEnumerator()) {
      if ($line -match $pattern.Value) { $matchedPattern = $pattern.Key; break }
    }
    if ($null -ne $matchedPattern) {
      $lineHits[$matchedPattern] = 1 + [int]$lineHits[$matchedPattern]
      $redacted.Add("[REDACTED line matched pattern: $matchedPattern]")
      continue
    }

    if ($null -ne $homePathPattern) {
      $count = [regex]::Matches($line, $homePathPattern, 'IgnoreCase').Count
      if ($count -gt 0) {
        $pathHits += $count
        $line = [regex]::Replace($line, $homePathPattern, '[REDACTED path]', 'IgnoreCase')
      }
    }
    $redacted.Add($line)
  }

  Set-Content -LiteralPath (Join-Path $OutDir $file.Name) -Value $redacted -Encoding utf8NoBOM
  $lineTotal = 0
  foreach ($hits in $lineHits.Values) { $lineTotal += $hits }
  $patternPart = @($lineHits.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" })
  $manifest.Add("$($file.Name): lines_matched=$lineTotal by_pattern=[$($patternPart -join ', ')] env_values=$envHits home_paths=$pathHits")
}

$manifest.Add('env_names_redacted: ' + $(if ($envRedacted.Count -gt 0) { ($envRedacted -join ', ') } else { 'none' }) + ' (values are never recorded)')
$manifest.Add('rules: credential-pattern lines ghp_, gho_, ghu_, ghs_, ghr_, github_pat_, Bearer , Authorization Basic, URL userinfo with a password, sk-; FM_*, *SECRET*, *TOKEN*, *KEY*, *CREDENTIAL*, *PASSWORD*, *PASSWD*, *PASSPHRASE*, *_PWD env values; home .aws, .ssh, .git, .cargo/credentials*, .netrc and _netrc paths')
Set-Content -LiteralPath (Join-Path $OutDir '_MANIFEST.txt') -Value $manifest -Encoding utf8NoBOM
Write-Host "Redacted $($inputs.Count) file(s) into $OutDir with _MANIFEST.txt."
