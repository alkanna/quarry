// The app's version lives in the quarry package's manifest, the one release-please bumps.
fn main() {
    println!("cargo:rerun-if-changed=../Cargo.toml");
    let toml = std::fs::read_to_string("../Cargo.toml").expect("the quarry manifest");
    let version = toml.lines().find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"')).expect("a version line in the quarry manifest");
    println!("cargo:rustc-env=QUARRY_VERSION={version}");
}
