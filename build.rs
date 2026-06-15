fn main() {
    #[cfg(target_os = "windows")]
    {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        // Use forward slashes — rc.exe handles them
        let icon_path = manifest_dir.replace("\\", "/") + "/assets/icon.ico";

        let rc_content = format!("APP_ICON ICON \"{}\"\n", icon_path);
        std::fs::write("resource.rc", &rc_content).unwrap();

        // Locate rc.exe instead of hardcoding a single SDK version: scan the Windows Kits
        // bin directories for the newest installed rc.exe, then fall back to PATH.
        let rc = find_rc().unwrap_or_else(|| "rc.exe".into());

        match std::process::Command::new(&rc)
            .args(["-fo", "resource.res", "resource.rc"])
            .output()
        {
            Ok(out) if out.status.success() => {
                println!("cargo:rustc-link-arg-bins=resource.res");
            }
            _ => {
                println!("cargo:warning=Icon not embedded (rc.exe not found or failed)");
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn find_rc() -> Option<std::path::PathBuf> {
    let roots = [
        r"C:\Program Files (x86)\Windows Kits\10\bin",
        r"C:\Program Files\Windows Kits\10\bin",
    ];
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    for root in roots {
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                let p = e.path().join("x64").join("rc.exe");
                if p.exists() {
                    candidates.push(p);
                }
            }
        }
    }
    // Directory names are SDK versions (e.g. 10.0.26100.0); lexical sort puts the newest last.
    candidates.sort();
    candidates.pop()
}
