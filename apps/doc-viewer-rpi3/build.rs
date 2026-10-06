//! Link with our script. Done here rather than in `.cargo/config.toml`
//! rustflags because a `RUSTFLAGS` environment variable (CI sets
//! `-D warnings`) replaces config rustflags entirely.
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{dir}/link.ld");
    println!("cargo:rerun-if-changed=link.ld");
}
