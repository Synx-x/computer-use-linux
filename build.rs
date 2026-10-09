// Release builds embed the PWA from web/ when it exists. The cfg flag keeps
// include_dir from failing on a checkout that has no web/ yet.
fn main() {
    println!("cargo:rustc-check-cfg=cfg(cul_web)");
    println!("cargo:rerun-if-changed=web");
    if std::path::Path::new("web/index.html").is_file() {
        println!("cargo:rustc-cfg=cul_web");
    }
}
