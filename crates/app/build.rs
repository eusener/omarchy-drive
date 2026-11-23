// O client OAuth padrão entra no binário via `option_env!`; recompila quando muda.
fn main() {
    for v in ["OMARCHY_DRIVE_GOOGLE_CLIENT_ID", "OMARCHY_DRIVE_GOOGLE_CLIENT_SECRET", "OMARCHY_DRIVE_MS_CLIENT_ID"] {
        println!("cargo:rerun-if-env-changed={v}");
    }
}
