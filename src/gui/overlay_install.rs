//! Preserve the installed, versioned layer when changing automatic loading.
use std::{io, path::PathBuf};
fn manifest_path() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is unavailable"))?;
    Ok(PathBuf::from(home).join(".local/share/vulkan/implicit_layer.d/ArgusOverlay.json"))
}
fn read() -> io::Result<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(manifest_path()?)?).map_err(io::Error::other)
}
pub fn is_global() -> io::Result<bool> {
    Ok(read()?["layer"].get("enable_environment").is_none())
}
fn configure(manifest: &mut serde_json::Value, global: bool) -> io::Result<()> {
    let layer = manifest
        .get_mut("layer")
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| io::Error::other("Invalid Vulkan layer manifest; reinstall the overlay"))?;
    if layer.get("name").and_then(|v| v.as_str()) != Some("VK_LAYER_ARGUS_OVERLAY")
        || layer.get("library_path").and_then(|v| v.as_str()).is_none()
    {
        return Err(io::Error::other(
            "Unrecognized Vulkan layer manifest; reinstall the overlay",
        ));
    }
    if global {
        layer.remove("enable_environment");
    } else {
        layer.insert(
            "enable_environment".into(),
            serde_json::json!({"ARGUS_LASSO_HUD":"1"}),
        );
    }
    layer.insert(
        "disable_environment".into(),
        serde_json::json!({"ARGUS_LASSO_HUD_DISABLE":"1"}),
    );
    Ok(())
}
pub fn set_global(global: bool) -> io::Result<()> {
    let mut manifest = read()?;
    configure(&mut manifest, global)?;
    let path = manifest_path()?;
    let temporary = path.with_extension("json.new");
    std::fs::write(
        &temporary,
        serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?,
    )?;
    std::fs::rename(temporary, path)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loading_toggle_preserves_installed_library_and_protocol() {
        let original = serde_json::json!({"layer":{"name":"VK_LAYER_ARGUS_OVERLAY","library_path":"/versioned/build/libargus_layer.so","implementation_version":"5","library_arch":"64"}});
        let mut manifest = original.clone();
        for global in [false, true, false] {
            configure(&mut manifest, global).unwrap();
            for key in ["library_path", "implementation_version", "library_arch"] {
                assert_eq!(manifest["layer"][key], original["layer"][key]);
            }
            assert_eq!(
                manifest["layer"].get("enable_environment").is_none(),
                global
            );
        }
        assert!(configure(&mut serde_json::json!({}), true).is_err());
    }
}
