fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=windows/shrew.rc");
        println!("cargo:rerun-if-changed=../../assets/icons/Shrew.ico");
        // The manifest comes from GPUI, and a second one fails the link
        embed_resource::compile("windows/shrew.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
