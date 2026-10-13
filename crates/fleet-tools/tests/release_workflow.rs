//! The release workflow, read as data (ADR-0028 clauses 17 to 19): which rows build and publish the
//! operator tool, how its archives are named, packed and checked, and that what selects the
//! Client's archives never picks up one of the operator tool's. Read as text, step by step — the
//! workflow is the contract, and parsing its YAML would add a dependency to state what a line says.

use std::collections::BTreeMap;
use std::path::Path;

fn workflow() -> String {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/release.yml");
    std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()))
}

/// The rows of the build matrix: each `- { key: value, … }` line, as a map.
fn matrix_rows(workflow: &str) -> Vec<BTreeMap<String, String>> {
    workflow
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("- { ")?.strip_suffix(" }"))
        .map(|row| {
            row.split(", ")
                .map(|pair| {
                    let (key, value) = pair.split_once(": ").expect("key: value");
                    (key.to_string(), value.trim_matches('"').to_string())
                })
                .collect()
        })
        .collect()
}

/// The build job's step called `name`: its lines from `- name:` up to the next step.
fn step<'w>(workflow: &'w str, name: &str) -> &'w str {
    let start = workflow
        .find(&format!("- name: {name}\n"))
        .unwrap_or_else(|| panic!("no step {name:?}"));
    let rest = &workflow[start..];
    let end = rest[1..]
        .find("\n      - ")
        .map_or(rest.len(), |end| end + 1);
    &rest[..end]
}

/// `*` against any run of characters — enough for the shell globs the notes write.
fn glob(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, tail)) => {
            name.starts_with(head)
                && (head.len()..=name.len())
                    .any(|i| name.is_char_boundary(i) && glob(tail, &name[i..]))
        }
    }
}

// Verifies: ADR-0028
#[test]
fn the_operator_tool_is_built_for_four_targets_and_its_version_asserted_where_it_runs() {
    let workflow = workflow();
    let rows = matrix_rows(&workflow);
    assert_eq!(rows.len(), 5, "the Client's five rows");
    let mut tool: Vec<&str> = rows
        .iter()
        .filter(|row| row["operator_tool"] == "true")
        .map(|row| row["target"].as_str())
        .collect();
    tool.sort_unstable();
    assert_eq!(
        tool,
        [
            "aarch64-apple-darwin",
            "aarch64-unknown-linux-gnu",
            "x86_64-apple-darwin",
            "x86_64-unknown-linux-gnu"
        ]
    );
    for row in &rows {
        if row["os"] == "windows" {
            assert_eq!(row["operator_tool"], "false", "no Windows operator tool");
        }
    }

    // Every row builds the packer for its runner, the Windows row included.
    let packer = step(&workflow, "Build the packer");
    assert!(!packer.contains("if:"), "{packer}");
    assert!(packer.contains("--bin opamp-fleetctl"), "{packer}");
    assert!(!packer.contains("--target"), "{packer}");

    let build = step(&workflow, "Build the operator tool");
    assert!(build.contains("if: matrix.operator_tool"), "{build}");
    assert!(
        build.contains("-p fleet-tools --bin opamp-fleetctl --target ${{ matrix.target }}"),
        "{build}"
    );

    let agrees = step(
        &workflow,
        "The operator tool agrees with the name it will be given",
    );
    assert!(
        agrees.contains("if: matrix.native && matrix.operator_tool"),
        "{agrees}"
    );
    assert!(
        agrees.contains("release/opamp-fleetctl --version"),
        "{agrees}"
    );
    assert!(
        agrees.contains("${{ needs.version.outputs.full }}"),
        "{agrees}"
    );
}

// Verifies: ADR-0028
#[test]
fn each_archive_is_named_like_the_clients_packed_by_the_tool_and_checksummed() {
    let workflow = workflow();
    assert!(workflow.contains("\n  OPERATOR_TOOL: opamp-fleetctl\n"));
    let pack = step(&workflow, "Pack the operator tool");
    assert!(pack.contains("if: matrix.operator_tool"), "{pack}");
    assert!(
        pack.contains(
            "name=\"${OPERATOR_TOOL}_${{ needs.version.outputs.version }}_${{ matrix.os }}_${{ matrix.arch }}.tar.gz\""
        ),
        "{pack}"
    );
    assert!(
        pack.contains("./target/release/opamp-fleetctl package pack \\"),
        "{pack}"
    );
    assert!(pack.contains("--format tar.gz"), "{pack}");
    // The member is named after the file packed: `opamp-fleetctl`.
    assert!(
        pack.contains("\"target/${{ matrix.target }}/release/opamp-fleetctl\")"),
        "{pack}"
    );
    assert!(!pack.contains("--program-name"), "{pack}");
    assert!(pack.contains("> \"$name.sha256\""), "{pack}");

    // Its checksum travels with it, and the publish job folds every `.sha256` into SHA256SUMS
    // and publishes every `.tar.gz`.
    let upload = workflow
        .split("- uses: actions/upload-artifact@")
        .find(|block| block.contains("steps.pack_tool.outputs.name"))
        .expect("an upload of the operator tool");
    assert!(upload.contains("if: matrix.operator_tool"), "{upload}");
    assert!(
        upload.contains("${{ steps.pack_tool.outputs.name }}.sha256"),
        "{upload}"
    );
    assert!(workflow.contains("cat *.sha256 | sort -k2 > SHA256SUMS"));
    assert!(workflow.contains("*.tar.gz *.deb *.rpm *.msi SHA256SUMS"));

    // No installer carries it.
    for installer in [
        "Build the Debian and RPM packages",
        "Build the MSI",
        "Checksum the installers",
    ] {
        assert!(
            !step(&workflow, installer).contains("opamp-fleetctl")
                && !step(&workflow, installer).contains("OPERATOR_TOOL"),
            "{installer} names the operator tool"
        );
    }
    for packaging in [
        "../fleet-agent/Cargo.toml",
        "../../packaging/windows/supervisor.wxs",
    ] {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join(packaging);
        let text = std::fs::read_to_string(&file).expect("read");
        assert!(!text.contains("opamp-fleetctl"), "{packaging}");
    }
}

// Verifies: ADR-0028
#[test]
fn the_notes_upload_loop_selects_the_clients_archives_alone() {
    let workflow = workflow();
    let loops: Vec<&str> = workflow
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("for f in ") && line.contains(".tar.gz"))
        .collect();
    assert_eq!(loops.len(), 1, "{loops:?}");
    let pattern = loops[0]
        .strip_prefix("for f in ")
        .and_then(|rest| rest.strip_suffix("; do"))
        .expect("for f in <glob>; do")
        .replace("${{ env.ARTIFACT }}", "supervisor")
        .replace("\\${v}", "1.2.3");
    for client in [
        "supervisor_1.2.3_linux_amd64.tar.gz",
        "supervisor_1.2.3_windows_amd64.tar.gz",
    ] {
        assert!(glob(&pattern, client), "{pattern} misses {client}");
    }
    for tool in [
        "opamp-fleetctl_1.2.3_linux_amd64.tar.gz",
        "opamp-fleetctl_1.2.3_darwin_arm64.tar.gz",
    ] {
        assert!(!glob(&pattern, tool), "{pattern} matches {tool}");
    }
}

// Verifies: ADR-0028
#[test]
fn the_crate_declares_one_binary_and_no_library() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest: toml::Table = std::fs::read_to_string(dir.join("Cargo.toml"))
        .expect("read")
        .parse()
        .expect("parse");
    assert!(!manifest.contains_key("lib"), "a [lib] section");
    let bins = manifest["bin"].as_array().expect("[[bin]]");
    assert_eq!(bins.len(), 1);
    assert_eq!(bins[0]["name"].as_str(), Some("opamp-fleetctl"));
    assert_eq!(bins[0]["path"].as_str(), Some("src/main.rs"));
    // Nothing Cargo would discover on its own: no `src/lib.rs`, no `src/bin/`.
    assert!(!dir.join("src/lib.rs").exists());
    assert!(!dir.join("src/bin").exists());
}
