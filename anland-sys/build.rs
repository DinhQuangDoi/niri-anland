fn main() {
    cc::Build::new()
        .file("src/vendor/display_producer.c")
        .file("src/vendor/socket_utils.c")
        .include("src/vendor")
        .compile("display_producer");

    println!("cargo:rustc-link-lib=pthread");
}