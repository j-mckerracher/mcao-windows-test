# Signed release integration and external signer handoff

Status: planning packet. Nothing in this document is implemented, signed, verified, or published.
Release publication stays blocked until the external signing prerequisite in section 3 is satisfied.

This page is the maintainer handoff for DPES-754 (install channel and signing), DPES-889, and DPES-892.
It records what the owner has decided, what the repository does today, where signing and verification must slot in, the integration contract the external signer owner must fill, the acceptance checks to run once a signer exists, and the packaging work that can proceed now.

Items marked **NOT RUN** have no evidence in this repository.
Items marked **OPEN** name the role that must answer them.
No value in this page is invented: a signer identity, certificate subject, endpoint, key reference, trust root, or tool version appears here only where a source in the repository or the owner decision states it.

## 1. Decisions and non-decisions

### 1.1 Decided by the owner

The owner decision is recorded in the DPES-754, DPES-889, and DPES-892 Jira comments. It is restated here so that this packet does not depend on Jira access.

- Supported end-user distribution is **signed GitHub Releases**.
- **Signature verification is mandatory** before the artifact is trusted or installed. Verification is not display-only, and a verification failure stops installation.
- **No unsigned supported-release exception** exists.
- **No run-anyway path** exists. This includes the SmartScreen "Run anyway" prompt, which is not a fallback.
- Developer `cargo install` is **optional** and is not a supported end-user channel.
- **winget is deferred.**
- The corporate signer service and its identity approval are an **external prerequisite**. The owner asked for the signing integration and owner handoff to be prepared now, independent packaging and verification work to continue, and release publication to stay blocked until the prerequisite is met.
- The owner decision comments on DPES-754, DPES-889, and DPES-892 name **DPES-916 container provenance signing** and state that it **does not satisfy** Windows corporate code signing. Owner comment 76277 on DPES-916 says "Require the specified SBOMs, attestations", and DPES-916 does not use the phrase "provenance". The repository shows only image attestation: `.github/workflows/cloud-images.yml` sets `generate_attestation: true` at lines 81, 99, and 117. That attestation covers the controller, worker, and signer images (section 2.1), not the `mcao` binary.
- No certificate is to be purchased, and no signing identity is to be invented.

### 1.2 Not decided

Each item below is an **OPEN** question. The owner named in the table must answer it before the signing step can be implemented.

| Decision | Owner role | Status |
| --- | --- | --- |
| Corporate signer service: endpoint, protocol, and access model | Corporate signer service owner (external) | OPEN |
| Signing identity: approved publisher name and certificate subject | Identity approver (external) | OPEN |
| Key custody: where the private key lives, who can use it, and how CI requests a signature without holding key material | Signer service owner with the security reviewer | OPEN |
| Timestamping authority: which RFC 3161 service, and whether timestamping is required | Signer service owner | OPEN |
| Signature format: Authenticode embedded signature, detached signature, or both | Signer service owner with the release owner | OPEN |
| Verification tool on the end-user machine: which tool, which version, and how it is delivered | Release owner with the security reviewer | OPEN |
| Trust root that verification checks against: root and intermediate certificates, and revocation source | Identity approver and signer service owner | OPEN |
| Certificate subject that verification must match exactly | Identity approver (external) | OPEN |
| Which Windows targets and file types are signed (for example `mcao.exe` only, or also an installer or archive) | Release owner | OPEN |
| Release workflow trigger, runner, and identity that may request a signature | Release owner with the security reviewer | OPEN |

Nothing in this repository currently names any of these values. The inline text in `.github/workflows/windows-toolchain.yml` and the existing workflows does not reference a signer, a certificate, or a timestamp server.

## 2. Current artifact list and release flow

### 2.1 What exists today

- **No release artifact is produced.** No workflow publishes a GitHub Release, uploads a binary to a release, or triggers on a tag. The triggers in `.github/workflows/ci.yml:3-10` are `push`, `pull_request`, and `workflow_dispatch`. Cloud image workflows trigger on `push` to `main` (`.github/workflows/cloud-images.yml`). The only `tags:` keys in `.github/workflows/` belong to container image tags (`cloud-images.yml:34`, `:46`, `:58`).
- **Versioning is a placeholder.** `Cargo.toml:12` sets `version = "0.0.0"`, and `crates/mcao-cli/Cargo.toml:6` sets `publish = false`. The crate description at `crates/mcao-cli/Cargo.toml:3` is marked PROPOSED.
- **The release procedure is a tag, not an artifact.** `docs/RELEASE.md` records the `UPSTREAM` file, creates an annotated `vX.Y.Z` tag on a release commit (`docs/RELEASE.md:66-70`), and pushes the commit before the tag (`docs/RELEASE.md:72-79`). It does not build, sign, checksum, or upload any file. Agents and crewmates never create or push release tags (`docs/RELEASE.md:6`).
- **No release-profile build exists.** `.github/workflows/ci.yml:149-152` builds `mcao` with `cargo build -p mcao-cli --locked --bin mcao` for timing artifacts, and the same command appears at `ci.yml:206`, `:308`, `:429`, and `:494`. The uploaded artifacts include timing and lint telemetry (`ci.yml:162-164`, `:86`) and other CI records. `.github/workflows/windows-toolchain.yml:36` builds `mcao.exe` on `windows-2022` without `--release`, and uploads only the qualification record (`windows-toolchain.yml:3-4`). Its permissions are `contents: read` (`windows-toolchain.yml:24`).
- **Install is a clone, not an artifact.** The README install path is `git clone`, then `./install.sh`, then `mcao setup` (`README.md:88-91`). `install.sh:5-6` links `~/.local/bin/mcao` to the clone's entry script. `install.sh:70` resolves that target to `$CLONE_DIR/bin/mcao`, and `install.sh:180-181` requires it to exist and be executable. `install.sh` does not build, download, or verify a Rust binary, and it does not invoke `cargo`.
- **The entry point is a Bash script.** `bin/mcao:1-2` routes `mcao setup` to `bin/mcao-setup.sh`. The Rust binary at `crates/mcao-cli/src/main.rs` is not installed by `install.sh`.
- **Existing SBOM and attestation exist only for container images.** `.github/workflows/cloud-images.yml:80`, `:98`, and `:116` set `generate_sbom: true`, and `:81`, `:99`, and `:117` set `generate_attestation: true`. These cover the controller, worker, and signer images, not the `mcao` binary.
- **Release tests check documentation only.** `tests/release.test.sh:34-54` asserts that `docs/RELEASE.md` contains the tagging, push-order, and rollback steps. It does not test any artifact.

### 2.2 Artifact list today

| Artifact | Produced by | Released to end users | Signed | Checksummed |
| --- | --- | --- | --- | --- |
| Git tag and `UPSTREAM` commit | `docs/RELEASE.md` procedure | Via the clone | No | No |
| `mcao` debug binary, Linux and Windows (no macOS build in CI) | CI build steps: `ci.yml:152`, `:206`, `:308`, `:429`, `:494` (ubuntu jobs); `windows-toolchain.yml:36` (`windows-2022`) | No | No | No |
| Timing and lint telemetry artifacts | `ci.yml:162-164`, `ci.yml:86` | No (CI only) | No | No |
| Controller, worker, and signer container images | `cloud-images.yml` | No, internal Artifact Registry | Attested, not code-signed | Digest-pinned |

### 2.3 Where the signing step and the verification step go

Neither step exists. The placements below are proposals for the release-channel work, to be confirmed by the release owner.

**Signing step (new).** Placement: a release job that runs after a release build of `mcao` for each supported Windows target and before any archive or GitHub Release asset is created. It must sign the file that users run, namely `mcao.exe`, before that file is archived or hashed. The signing request is made by the workflow and fulfilled by the external signer. CI must not hold private key material. The step is gated on the OPEN items in section 1.2 and stays absent until the signer exists.

**Mandatory verification step (new, end-user side).** Placement: the first step of any future installer or download script, before the binary is copied into place and before any `PATH` update. Under the DPES-892 layout, the target location is `%LOCALAPPDATA%\Programs\mcao\bin\mcao.exe` with a user-scope `PATH` entry. Verification runs against the downloaded artifact. Any failure stops the install and leaves the existing installation unchanged.

**Release-side gate (new).** Placement: the release job must verify its own signed output with the same verification command that end users run, before it publishes the GitHub Release. A signed artifact that fails the gate is not published.

`install.sh` is not a verification point today and must not become one by accident. Its current behavior links a clone's script and never reads a release artifact. If `install.sh` later gains a download path, the verification step applies there too.

## 3. Integration contract with the external signer

The owner fills each field below. A field left as OPEN blocks the signing step; it must not be defaulted.

### 3.1 Request (CI to signer)

| Field | Content | Owner | Status |
| --- | --- | --- | --- |
| Transport and endpoint | OPEN | Signer service owner | OPEN |
| Requester authentication: the workflow identity the signer accepts | OPEN. No credential is requested or stored by this packet. | Signer service owner with the security reviewer | OPEN |
| Artifact input: the SHA-256 digest of the file to sign, or the file itself | OPEN | Signer service owner | OPEN |
| Artifact name and version | Derived from the release tag, for example `mcao` plus `vX.Y.Z`, from `docs/RELEASE.md:69` | Release owner | Proposed |
| Source commit SHA | Taken from the tag's commit | Release owner | Proposed |
| Target triple | OPEN, one per Windows target that is signed | Release owner | OPEN |
| Signing profile or policy name | OPEN | Signer service owner | OPEN |
| Timestamp request: whether to request one, and from which authority | OPEN | Signer service owner | OPEN |
| Request format and encoding | OPEN | Signer service owner | OPEN |

### 3.2 Response (signer to CI)

| Field | Content | Owner | Status |
| --- | --- | --- | --- |
| Signature form: embedded, detached, or both | OPEN | Signer service owner with the release owner | OPEN |
| Signature file name and location convention | OPEN | Release owner | OPEN |
| Signing certificate chain | OPEN | Signer service owner | OPEN |
| Certificate subject | OPEN, must match the approved identity exactly | Identity approver (external) | OPEN |
| Timestamp token, if any | OPEN | Signer service owner | OPEN |
| Error response shape and retry guidance | OPEN | Signer service owner | OPEN |

### 3.3 Verification (end-user machine and release gate)

| Field | Content | Owner | Status |
| --- | --- | --- | --- |
| Verification command | OPEN | Release owner with the security reviewer | OPEN |
| Verification tool and version, and how it reaches the end-user machine | OPEN | Release owner | OPEN |
| Trust root: which root or intermediate certificates are trusted | OPEN | Identity approver (external) | OPEN |
| Revocation behavior when offline or unreachable | OPEN | Security reviewer | OPEN |
| Expected subject match rule | OPEN, exact match to the approved subject in 3.2 | Identity approver (external) | OPEN |
| Timestamp validity rule for expired certificates | OPEN | Security reviewer | OPEN |
| Exit code that means "refused" | OPEN | Release owner | OPEN |

### 3.4 Key custody

No key material enters this repository, CI logs, or release assets. Where the private key lives, who may request a signature, and how a request is audited are OPEN with the signer service owner and the security reviewer.

## 4. Acceptance checks once a signer exists (test plan)

Every check below is **NOT RUN**. None of them can run until the OPEN items in section 3 are answered and a signed artifact exists.

| ID | Check | Expected result | Status |
| --- | --- | --- | --- |
| A1 | Verify-before-install: a validly signed artifact from the approved identity, installed through the real installer path | Verification passes, installation proceeds, `mcao --version` prints and exits 0 | NOT RUN |
| A2 | Tamper test: flip one byte in an otherwise valid signed artifact | Verification fails, installation is refused, the existing installation is unchanged | NOT RUN |
| A3 | Wrong-identity test: an artifact validly signed by an identity that is not the approved subject | Verification fails on subject mismatch, installation is refused | NOT RUN |
| A4 | Unsigned artifact refused: a release artifact with no signature | Installation is refused with a message that names the missing signature | NOT RUN |
| A5 | Checksum-only artifact refused: a matching checksum with no signature | Installation is refused, because a checksum is not a trust root | NOT RUN |
| A6 | No run-anyway path: search the installer and release scripts for any flag, environment variable, or config key that skips verification, and attempt each one | No such path exists, and each attempt is refused | NOT RUN |
| A7 | Expired or revoked certificate handling, per the timestamp and revocation rules in 3.3 | Behavior matches the OPEN rule once answered | NOT RUN |
| A8 | Release gate: the release job verifies its own signed output with the end-user command before publishing | An artifact that fails verification is not published | NOT RUN |

## 5. Packaging work that can proceed now

These items do not need the signer. They are separated here so that no one mistakes them for signing or verification.

- **Archive layout.** Define the archive contents and directory layout for each Windows target, for example `mcao.exe` plus the manifest that DPES-892 AC1 requires. The layout does not depend on the signer's output format, but the signature file name must be reserved in it. Owner: release owner. Status: may proceed.
- **Install location and manifest.** Use the DPES-892 AC1 location (`%LOCALAPPDATA%\Programs\mcao\bin\mcao.exe`) and a manifest of installed paths, so that the DPES-892 uninstall rule can remove only tracked files. Owner: release owner. Status: may proceed. Implementation is not part of this packet.
- **Checksums file as a convenience only.** A checksums file, with the proposed name `SHA256SUMS` (not an existing file in this repository or in the DPES-754, DPES-889, or DPES-892 records), helps a user confirm a download was not truncated. It is **not** a security control, is **not** a substitute for signature verification, and must not be described as one in release notes or in the installer. Owner: release owner. Status: may proceed, with that wording.
- **SBOM.** Generate a software bill of materials for the `mcao` binary from `Cargo.lock`. The container images already use `generate_sbom: true` (`cloud-images.yml:80`), but that tool and its format for a native binary are OPEN with the release owner. Status: may proceed once the format is chosen.
- **Release workflow skeleton without signing.** A draft workflow that builds release binaries and assembles the archive layout, with the signing and gate steps absent and marked as blocked. It must not publish a release. Status: may proceed, but publication must stay blocked.
- **Guard on the unsigned skeleton output.** The skeleton's unsigned build output is never uploaded (no workflow artifact step consumes it), never attached to a GitHub Release, and never published as a supported artifact. Any upload or release step added later consumes only the output of the signing step and the release-side gate in section 2.3.
- **Test plan for the verification command.** Write the tests for section 4 against a fixture signature only if the owner approves a non-production test certificate. Do not create a signing identity to do this. Status: blocked on the owner's decision.

## 6. Criteria mapping and what stays unmet

Criterion text below is taken from the DPES-754, DPES-889, and DPES-892 Jira descriptions. Re-check the text against Jira before acceptance.

### 6.1 DPES-754 (install channel and signing)

| Criterion | What this packet does | Status |
| --- | --- | --- |
| AC1: clean Windows host install, `mcao --version` exits 0, no Bash or WSL child processes, and preflight exit codes | Does not implement the install path. Section 2 records that no installer exists. | Unmet |
| AC2: `mcao preflight` with Git removed from `PATH` exits 3 and stderr names `Git: MISSING` with a remedy URL | Covered by test source, not re-run in this packet: `crates/mcao-cli/tests/preflight.rs:104-116` asserts exit 3, empty stdout, `Git: MISSING`, and `https://git-scm.com`. The Windows rule (only `.exe` or `.com` counts, no `.cmd` shim) is `preflight_does_not_accept_a_script_shim_as_git` (`:294-308`), which was compiled on Windows but not run there (DPES-754 comment 75872). | Partly met: native Windows run unverified; AC1 unmet |
| AC3: a path with spaces, non-ASCII characters, and UNC form is printed exactly, with no shell expansion; preflight exits 0 (or 3 on the backend check only) | Spaces, non-ASCII characters, `$`, and `~` are covered by `preflight_prints_an_fm_home_with_spaces_and_non_ascii_exactly` (`crates/mcao-cli/tests/preflight.rs:160-182`, Unix form; the Windows form was compiled only). No test passes a UNC home (`\\server\share`) to preflight; the `\\?\` form appears only in OneDrive matching (`crates/mcao-core/tests/preflight.rs:418` and `:426`). The merged preflight has no backend check, so the backend exit-3 branch does not exist yet. | Partly met: UNC unverified; backend check unmet |
| AC4: `cargo build -p mcao-cli --target x86_64-pc-windows-msvc`, binary statically linked and runs on Windows 10 and 11, and cross-target `cargo check` | Not in scope of this packet. The release build for the Windows target is not present (`windows-toolchain.yml:36` is a debug build). | Unmet |
| AC5: `mcao --help` and `mcao --version` exit 0 offline, with no `FM_HOME`, config, or state directory, with no file reads, network calls, or credentials | Exit code and output are covered by test source: `crates/mcao-cli/tests/cli.rs:38-47` (`--version`) and `:49-64` (`--help` with `FM_HOME` set to a missing directory). `crates/mcao-cli/src/main.rs:34-39` parses arguments with clap before any config or home is read. No test asserts the absence of file reads or network calls, and no fresh Windows host run exists. | Partly met: exit and output covered by test source; no-I/O claim by code reading only; fresh Windows host unverified |
| Signed release channel (owner decision): signed GitHub Releases with mandatory verification | Decision recorded (section 1.1). Signing and verification slots identified (section 2.3). Integration contract opened (section 3). | Partly prepared, not met |
| Signing integration with the corporate signer, identity approval, and no unsigned exception | Blocked on the external prerequisite (section 1.2). | Unmet, external |

### 6.2 DPES-889 (state encoding)

DPES-889 acceptance criteria AC1 (verbatim path write), AC2 (opaque path read on the other OS), and AC3 (format version and backend token) cover the state-file encoding. Line endings are AC6. This packet does not touch any of them. Its only relation to DPES-889 is the owner decision recorded in its comments, which section 1.1 restates. No DPES-889 criterion is met or claimed here.

### 6.3 DPES-892 (managed-laptop package and install)

| Criterion | What this packet does | Status |
| --- | --- | --- |
| AC1: binary at `%LOCALAPPDATA%\Programs\mcao\bin\mcao.exe`, manifest written, user `PATH` updated | Location and manifest are proposed (section 5). Installer is not implemented. | Unmet |
| AC2 and AC3: manifest-tracked uninstall and `WM_SETTINGCHANGE` broadcast | Not in scope of this packet. | Unmet |
| AC4 and AC5: OneDrive detection (warn when `FM_HOME` is synced; no error when it is safe) | Not verified in this packet. Merged preflight code has an environment-root check and a profile-folder warning (DPES-892 comments 74298 and 76297). | Not verified here |
| AC6 and AC7: retry helper (backoff on `ERROR_SHARING_VIOLATION`; success before the maximum attempts) | Slice 1 merged as PR 298 (DPES-892 comment 74298), gated on `PermissionDenied`, not on `ERROR_SHARING_VIOLATION` as the AC6 text says. | AC6 unmet as written; AC7 not verified here |
| AC8: non-admin user install to `%LOCALAPPDATA%` | Layout is compatible with it (section 5). Not implemented. | Unmet |
| AC9: "Workflow has signed binary step (implementation gated)" | The slot is identified (section 2.3), and the request and response contract is open (section 3). The step itself is not implemented and must stay gated on section 1.2. | Partly prepared, not met |

## 7. Verification status of this packet

- The structural audience check `bin/fm-doc-audience-check.sh` was run before and after the classification of this page; the before and after unclassified counts are in the pull request body.
- This packet makes no claim about signing, verification, installation, or publication, because none of them has been run.
- Every file path and line number in this page was read from the repository at the commit this page was written against. Those line numbers may drift.
