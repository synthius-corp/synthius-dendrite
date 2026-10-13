fn main() {
    println!("cargo:rerun-if-env-changed=SYNTHIUS_TELEGRAM_API_ID");
    println!("cargo:rerun-if-env-changed=SYNTHIUS_TELEGRAM_API_HASH");
}
