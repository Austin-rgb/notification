// `sqlx::migrate!` embeds ./migrations at compile time; without this, adding a
// migration file would not trigger a rebuild.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
