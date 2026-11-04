fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon_with_id("branding/icons/buku.ico", "1");
        res.set_manifest_file("buku.manifest");
        res.set("ProductName", "BUKU");
        res.set("FileDescription", "BUKU");
        res.set("CompanyName", "BUKU");
        if let Err(err) = res.compile() {
            panic!("failed to embed Windows resources: {err}");
        }
    }
    println!("cargo:rerun-if-changed=branding/icons/buku.ico");
    println!("cargo:rerun-if-changed=buku.manifest");
}
