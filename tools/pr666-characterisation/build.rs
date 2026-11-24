// The PR's calendar/mod.rs also declares `commands` and `oauth`, which need the
// whole app. Its type definitions are taken verbatim; only the four `pub mod`
// lines are left out.
fn main() {
    let source = std::fs::read_to_string("pr666/mod.rs").unwrap();
    let types: String = source.lines().filter(|line| !line.starts_with("pub mod ")).collect::<Vec<_>>().join("\n");
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("types.rs");
    std::fs::write(out, types).unwrap();
    println!("cargo:rerun-if-changed=../pr666/mod.rs");
}
