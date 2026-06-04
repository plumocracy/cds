use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn docker_cd_equivalence_random_tree() {
    if !docker_is_available() {
        eprintln!("skipping docker cd equivalence test because Docker is not available");
        return;
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let image = std::env::var("CDS_DOCKER_IMAGE").unwrap_or_else(|_| "rust:1-slim".to_string());
    let cache_root = std::env::var_os("CDS_DOCKER_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("target/docker-cache"));
    let cargo_cache = cache_root.join("cargo");
    let target_cache = cache_root.join("target");

    fs::create_dir_all(&cargo_cache).expect("docker cargo cache directory is created");
    fs::create_dir_all(&target_cache).expect("docker target cache directory is created");

    let output = Command::new("docker")
        .arg("run")
        .arg("--rm")
        .arg("--volume")
        .arg(format!("{}:/workspace", manifest_dir.display()))
        .arg("--volume")
        .arg(format!("{}:/tmp/cargo", cargo_cache.display()))
        .arg("--volume")
        .arg(format!("{}:/tmp/cds-target", target_cache.display()))
        .arg("--workdir")
        .arg("/workspace")
        .arg("--env")
        .arg("CARGO_HOME=/tmp/cargo")
        .arg("--env")
        .arg("CARGO_TARGET_DIR=/tmp/cds-target")
        .arg(image)
        .arg("bash")
        .arg("-lc")
        .arg("cargo build --quiet && bash tests/docker_cd_equivalence.sh")
        .output()
        .expect("docker cd equivalence test starts");

    assert!(
        output.status.success(),
        "docker cd equivalence test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn docker_is_available() -> bool {
    Command::new("docker")
        .arg("info")
        .arg("--format")
        .arg("{{.ServerVersion}}")
        .output()
        .is_ok_and(|output| output.status.success())
}
