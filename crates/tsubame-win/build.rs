// The app manifest (res/tsubame.manifest) says longPathAware. Test executables get the same
// setting so long-path behaviour of the shell (Recycle Bin, IFileOperation) is tested the way
// the app runs. Also applied to this crate's examples; nothing downstream of the crate is affected.
fn main() {
    println!("cargo:rerun-if-changed=longpath.manifest");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{dir}/longpath.manifest");
    }
}
