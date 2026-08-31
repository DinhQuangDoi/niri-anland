fn main() {
    println!("cargo:rerun-if-changed=src/vendor/display_producer.c");
    println!("cargo:rerun-if-changed=src/vendor/display_producer.h");
    println!("cargo:rerun-if-changed=src/vendor/socket_utils.c");
    println!("cargo:rerun-if-changed=src/vendor/socket_utils.h");
    println!("cargo:rerun-if-changed=src/vendor/protocol.h");

    cc::Build::new()
        .file("src/vendor/display_producer.c")
        .file("src/vendor/socket_utils.c")
        .include("src/vendor")
        .compile("display_producer");

    println!("cargo:rustc-link-lib=pthread");
}