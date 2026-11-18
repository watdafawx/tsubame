// Version resource + manifest (asInvoker, longPathAware) for tsubame-handler.exe, so the shim
// looks like what it is to Explorer, Task Manager and antivirus (research R1 section 3.5).
fn main() {
    println!("cargo:rerun-if-changed=res/handler.rc");
    println!("cargo:rerun-if-changed=res/handler.manifest");
    #[cfg(windows)]
    embed_resource::compile("res/handler.rc", embed_resource::NONE).manifest_required().unwrap();
}
