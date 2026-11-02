//! `server migrate` as an operator runs it (ADR-0029 clause 26): three named directories, what it
//! did on standard output, and an exit code a script can act on.

use std::process::Command;

fn migrate(config_dir: &std::path::Path, data_dir: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_server"))
        .arg("migrate")
        .arg("--config-dir")
        .arg(config_dir)
        .arg("--packages-dir")
        .arg(config_dir.with_file_name("fleet-packages"))
        .arg("--data-dir")
        .arg(data_dir)
        .output()
        .expect("run the server binary")
}

/// An old layout migrates into a new data directory, which the command names for `server.toml`;
/// the same data directory a second time is refused, non-zero, and a call missing a directory is
/// a usage error.
/// Verifies: ADR-0029
#[test]
fn migrate_writes_a_new_data_directory_once_and_says_so() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_dir = dir.path().join("fleet-configs");
    std::fs::create_dir_all(config_dir.join("revocation")).expect("dir");
    std::fs::write(config_dir.join("revocation/revocations.json"), "[]").expect("list");
    let data_dir = dir.path().join("fleet-data");

    let out = migrate(&config_dir, &data_dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout.contains("carried: the revocation list"), "{stdout}");
    assert!(stdout.contains("set data_dir to it"), "{stdout}");
    assert!(data_dir.join("revocations.json").is_file());

    let again = migrate(&config_dir, &data_dir);
    assert_eq!(again.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&again.stderr).contains("not empty"),
        "{again:?}"
    );

    let usage = Command::new(env!("CARGO_BIN_EXE_server"))
        .args(["migrate", "--config-dir", "x"])
        .output()
        .expect("run the server binary");
    assert_eq!(usage.status.code(), Some(2));
}

/// `server migrate-to-postgres` takes no `--data-dir` when the configuration names S3, as a Server
/// on PostgreSQL and S3 keeps nothing on its disk, and refuses one before it reaches the database
/// or the bucket, naming the flag; with the filesystem blob store a call without it is a usage
/// error (ADR-draft-the-servers-state-and-its-stores clause 27).
/// Verifies: ADR-draft-the-servers-state-and-its-stores
#[cfg(unix)]
#[test]
fn migrate_to_postgres_takes_a_data_dir_with_the_filesystem_blob_store_alone() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let password = dir.path().join("database-password");
    std::fs::write(&password, "secret").expect("password");
    std::fs::set_permissions(&password, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let database = format!(
        "[tls]\ncert_file = \"cert.pem\"\nkey_file = \"key.pem\"\nclient_ca_file = \"ca.pem\"\n\
         [database]\nstore = \"postgres\"\n[database.postgres]\nhost = \"127.0.0.1\"\nport = 9\n\
         dbname = \"fleet\"\nuser = \"fleet\"\npassword_file = {:?}\ntls = false\n",
        password.display().to_string()
    );
    let bucket = "[blobs]\nstore = \"s3\"\n[blobs.s3]\nendpoint = \"http://127.0.0.1:9\"\n\
                  bucket = \"fleet\"\nregion = \"us-east-1\"\n";
    let from = dir.path().join("fleet-data");
    std::fs::create_dir(&from).expect("from");
    let run = |config: &str, data_dir: Option<&str>| {
        let path = dir.path().join("server.toml");
        std::fs::write(&path, config).expect("server.toml");
        let mut command = Command::new(env!("CARGO_BIN_EXE_server"));
        command
            .arg("migrate-to-postgres")
            .arg("--from")
            .arg(&from)
            .arg("--config")
            .arg(&path);
        if let Some(data_dir) = data_dir {
            command.arg("--data-dir").arg(dir.path().join(data_dir));
        }
        command.output().expect("run the server binary")
    };

    let refused = run(&format!("{database}{bucket}"), Some("new"));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(stderr.starts_with("--data-dir: "), "{stderr}");
    assert!(!dir.path().join("new").exists(), "nothing is created");

    let usage = run(&database, None);
    assert_eq!(usage.status.code(), Some(2), "{usage:?}");
}
