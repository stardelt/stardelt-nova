//! `sqlx::migrate!` embeds `migrations/*.sql` at compile time but does not tell Cargo to
//! watch the directory: without this line a NEW migration file is silently ignored until
//! some Rust file changes too.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
