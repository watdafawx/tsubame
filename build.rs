// Embeds the app icon and manifest (res/tsubame.rc). The icon is what Explorer,
// the taskbar and Alt-Tab show, and a manifest gets us PerMonitorV2 DPI instead
// of bitmap-stretched windows.
fn main() {
    println!("cargo:rerun-if-changed=res/tsubame.rc");
    println!("cargo:rerun-if-changed=res/tsubame.manifest");
    println!("cargo:rerun-if-changed=res/tsubame.ico");
    embed_resource::compile("res/tsubame.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
