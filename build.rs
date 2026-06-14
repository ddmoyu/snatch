fn main() {
    #[cfg(target_os = "windows")]
    {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        // Use forward slashes — rc.exe handles them
        let icon_path = manifest_dir.replace("\\", "/") + "/assets/icon.ico";
        
        let rc_content = format!("APP_ICON ICON \"{}\"\n", icon_path);
        std::fs::write("resource.rc", &rc_content).unwrap();
        
        let rc_path = r"C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\rc.exe";
        
        match std::process::Command::new(rc_path)
            .args(["-fo", "resource.res", "resource.rc"])
            .output()
        {
            Ok(out) if out.status.success() => {
                println!("cargo:rustc-link-arg-bins=resource.res");
            }
            _ => {
                println!("cargo:warning=Icon not embedded");
            }
        }
    }
}
