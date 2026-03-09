use std::fs;
use std::path::Path;
use std::process::Command;

fn main() {
    let agent_dir = Path::new("crates/phantom-java-agent");
    let src_file = agent_dir.join("src/com/example/phantom/Agent.java");
    let out_dir = agent_dir.join("out");
    let jar_file = agent_dir.join("phantom-java-agent.jar");
    let manifest_file = agent_dir.join("manifest.txt");

    // Tell Cargo to rerun this script if the Java source changes
    println!("cargo:rerun-if-changed={}", src_file.display());

    // 1. Check for javac and jar
    if Command::new("javac").arg("-version").output().is_err() {
        println!("cargo:warning=javac not found. Skipping Java Agent build.");
        return;
    }

    // 2. Prepare output directory
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir).unwrap();
    }
    fs::create_dir_all(&out_dir).unwrap();

    // 3. Compile Java source
    let status = Command::new("javac")
        .args(["-d", "out"])
        .arg("src/com/example/phantom/Agent.java")
        .current_dir(agent_dir)
        .status()
        .expect("failed to execute javac");

    if !status.success() {
        panic!("Java compilation failed");
    }

    // 4. Create manifest
    fs::write(&manifest_file, "Premain-Class: com.example.phantom.Agent\n").unwrap();

    // 5. Create JAR
    let status = Command::new("jar")
        .args([
            "cvfm",
            "phantom-java-agent.jar",
            "manifest.txt",
            "-C",
            "out",
            ".",
        ])
        .current_dir(agent_dir)
        .status()
        .expect("failed to execute jar");

    if !status.success() {
        panic!("Failed to create JAR file");
    }

    // 6. Cleanup
    let _ = fs::remove_dir_all(&out_dir);
    let _ = fs::remove_file(&manifest_file);

    println!("cargo:warning=Successfully built {}", jar_file.display());
}
