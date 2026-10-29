/// Run git in the manifest dir; `None` when git or the repository is missing
/// (a source tarball build), which just leaves the build id unset.
fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// The commit and git dir this binary was built from, embedded so a running
/// Mast can tell which build is installed and list the merges between them.
fn emit_build_id() {
    let sha = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let git_dir = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .unwrap_or_default();
    println!("cargo:rustc-env=MAST_BUILD_SHA={sha}");
    println!("cargo:rustc-env=MAST_BUILD_GIT_DIR={git_dir}");
    // Rebuild the (tiny) build script output when the checked-out commit moves.
    for path in ["HEAD", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--path-format=absolute", "--git-path", path]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--path-format=absolute", "--git-path", &branch])
    {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn main() {
    emit_build_id();
    println!("cargo:rerun-if-changed=../../dist/windows/zeron.rc");
    println!("cargo:rerun-if-changed=../../dist/windows/zeron.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile_for(
            "../../dist/windows/zeron.rc",
            &["zeron"],
            embed_resource::NONE,
        )
        .manifest_required()
        .expect("Windows app icon resource compilation failed");
    }
}
