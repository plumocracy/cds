use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn install_script_writes_shell_integration_idempotently() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    let cargo_home = temp.path().join("cargo-home");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(cargo_home.join("bin")).unwrap();

    let fake_cargo = bin.join("cargo");
    fs::write(
        &fake_cargo,
        format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> '{}'\nmkdir -p '{}'\ncat > '{}/cds' <<'CDS'\n#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"${{1-}}\" = \"--daemon\" ]; then sleep 60; fi\nCDS\nchmod +x '{}/cds'\n",
            temp.path().join("cargo-args.log").display(),
            cargo_home.join("bin").display(),
            cargo_home.join("bin").display(),
            temp.path().join("cds-args.log").display(),
            cargo_home.join("bin").display(),
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_cargo).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_cargo, permissions).unwrap();

    for _ in 0..2 {
        let output = Command::new("bash")
            .arg("install.sh")
            .env("HOME", temp.path())
            .env("CARGO_HOME", &cargo_home)
            .env("SHELL", "/bin/zsh")
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let zshrc = fs::read_to_string(temp.path().join(".zshrc")).unwrap();
    assert_eq!(zshrc.matches("# >>> cds init >>>").count(), 1);
    assert!(zshrc.contains("command \""));
    assert!(zshrc.contains("--shell-init zsh"));

    let cargo_args = fs::read_to_string(temp.path().join("cargo-args.log")).unwrap();
    assert_eq!(cargo_args.matches("--force").count(), 0);

    let cds_args = fs::read_to_string(temp.path().join("cds-args.log")).unwrap();
    assert_eq!(cds_args.matches("--restart-daemon").count(), 2);
}

#[test]
fn install_script_force_passes_force_to_cargo_install() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    let cargo_home = temp.path().join("cargo-home");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(cargo_home.join("bin")).unwrap();

    let fake_cargo = bin.join("cargo");
    fs::write(
        &fake_cargo,
        format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" > '{}'\nmkdir -p '{}'\ncat > '{}/cds' <<'CDS'\n#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"${{1-}}\" = \"--daemon\" ]; then sleep 60; fi\nCDS\nchmod +x '{}/cds'\n",
            temp.path().join("cargo-args.log").display(),
            cargo_home.join("bin").display(),
            cargo_home.join("bin").display(),
            temp.path().join("cds-args.log").display(),
            cargo_home.join("bin").display(),
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_cargo).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_cargo, permissions).unwrap();

    let output = Command::new("bash")
        .arg("install.sh")
        .arg("--force")
        .env("HOME", temp.path())
        .env("CARGO_HOME", &cargo_home)
        .env("SHELL", "/bin/zsh")
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let cargo_args = fs::read_to_string(temp.path().join("cargo-args.log")).unwrap();
    assert!(cargo_args.contains("install --path"));
    assert!(cargo_args.contains("--force"));
    let cds_args = fs::read_to_string(temp.path().join("cds-args.log")).unwrap();
    assert!(cds_args.contains("--restart-daemon"));
}
