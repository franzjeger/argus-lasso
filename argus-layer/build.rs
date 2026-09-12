fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    for shader in ["overlay.vert", "overlay.frag"] {
        println!("cargo:rerun-if-changed=shaders/{shader}");
        let status = std::process::Command::new("glslangValidator")
            .args([
                "-V",
                &format!("shaders/{shader}"),
                "-o",
                &format!("{out}/{shader}.spv"),
            ])
            .status()
            .expect("install glslangValidator to compile overlay shaders");
        assert!(status.success(), "shader compilation failed: {shader}");
    }
}
