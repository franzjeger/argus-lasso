fn main() {
    let icon_path = "assets/icon.png";
    println!("cargo:rerun-if-changed={icon_path}");

    let file = std::fs::File::open(icon_path)
        .expect("assets/icon.png not found — regenerate it with `make icon`");
    let decoder = png::Decoder::new(file);
    let mut reader = decoder
        .read_info()
        .expect("failed to decode assets/icon.png");
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .expect("failed to read PNG frame");

    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => buf[..info.buffer_size()]
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 0xff])
            .collect(),
        other => panic!("unsupported PNG color type: {other:?}"),
    };

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out = std::path::Path::new(&out_dir);
    std::fs::write(out.join("icon_rgba.bin"), &rgba).expect("failed to write icon_rgba.bin");

    // Emit the dimensions as source rather than env vars so src/icon.rs can pick
    // them up with include!. Keeping them tied to the PNG means assets/icon.png
    // can be re-rendered at any resolution without touching Rust code — the old
    // hard-coded 64x64 silently mismatched the buffer the moment it changed.
    std::fs::write(
        out.join("icon_dims.rs"),
        format!(
            "pub const W: u32 = {};\npub const H: u32 = {};\n",
            info.width, info.height
        ),
    )
    .expect("failed to write icon_dims.rs");
}
