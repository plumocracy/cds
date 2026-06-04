use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn hidden_emit_searches_when_local_prefix_is_absent() {
    let fixture = SearchFixture::new();
    fixture.init();

    let cwd = fixture.temp.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir(cwd.join("alpha")).unwrap();

    let output = fixture
        .cds()
        .arg("--cds-emit")
        .arg("--")
        .arg("manifest")
        .current_dir(&cwd)
        .env("PWD", &cwd)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let expected = format!(
        "builtin cd '{}'\n",
        fixture.project_dir.to_string_lossy().replace('\'', "'\\''")
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
}

#[test]
fn hidden_emit_searches_when_only_local_prefix_exists() {
    let fixture = SearchFixture::new();
    fixture.init();

    let cwd = fixture.temp.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir(cwd.join("music")).unwrap();

    let output = fixture
        .cds()
        .arg("--cds-emit")
        .arg("--")
        .arg("manifest")
        .current_dir(&cwd)
        .env("PWD", &cwd)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = format!(
        "builtin cd '{}'\n",
        fixture.project_dir.to_string_lossy().replace('\'', "'\\''")
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
}

#[test]
fn hidden_emit_preserves_cd_when_exact_local_entry_exists() {
    let fixture = SearchFixture::new();
    fixture.init();

    let cwd = fixture.temp.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir(cwd.join("manifest")).unwrap();

    let output = fixture
        .cds()
        .arg("--cds-emit")
        .arg("--")
        .arg("manifest")
        .current_dir(&cwd)
        .env("PWD", &cwd)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "builtin cd 'manifest'\n"
    );
}

#[test]
fn dir_type_count_lists_detected_directory_types() {
    let fixture = SearchFixture::new();
    fixture.init();

    let output = fixture.cds().arg("--dir-type-count").output().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("1\tchrome extension"), "{stdout}");
    assert!(stdout.contains("1\trust project"), "{stdout}");
}

#[test]
fn reset_prompts_and_deletes_indexed_data_when_confirmed() {
    let fixture = SearchFixture::new();
    fixture.init();

    let mut child = fixture
        .cds()
        .arg("--reset")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"y\n").unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(
        "This will delete all data in the cds database and it is irreversable. Continue [y/n]?"
    ));
    assert!(stdout.contains("database reset"));

    let counts = fixture.cds().arg("--dir-type-count").output().unwrap();
    assert!(counts.status.success());
    assert_eq!(String::from_utf8(counts.stdout).unwrap(), "");
}

struct SearchFixture {
    temp: tempfile::TempDir,
    project_dir: std::path::PathBuf,
}

impl SearchFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("Projects");
        let project_dir = projects.join("chrome-extension");
        std::fs::create_dir_all(project_dir.join("src")).unwrap();
        std::fs::write(
            project_dir.join("README.md"),
            "Chrome extension manifest browser popup",
        )
        .unwrap();
        std::fs::write(
            project_dir.join("manifest.json"),
            r#"{"manifest_version":3,"action":{},"permissions":["storage"]}"#,
        )
        .unwrap();
        let rust_dir = projects.join("rust-tool");
        std::fs::create_dir_all(&rust_dir).unwrap();
        std::fs::write(
            rust_dir.join("Cargo.toml"),
            "[package]\nname = \"rust-tool\"",
        )
        .unwrap();

        Self { temp, project_dir }
    }

    fn cds(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cds"));
        command
            .env("HOME", self.temp.path())
            .env("CDS_EMBEDDER", "fake")
            .env("CDS_CONFIG_DIR", self.temp.path().join("config"))
            .env("CDS_DATA_DIR", self.temp.path().join("data"))
            .env("CDS_CACHE_DIR", self.temp.path().join("cache"));
        command
    }

    fn init(&self) {
        let init = self.cds().arg("--init").output().unwrap();
        assert!(
            init.status.success(),
            "init stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        );
    }
}
