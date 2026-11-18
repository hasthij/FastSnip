// Stamps the exe with its version, name and publisher (File properties > Details).
fn main() {
    let v = std::env::var("FASTSNIP_VERSION").unwrap_or_else(|_| "0.1.0.0".into());
    println!("cargo:rerun-if-env-changed=FASTSNIP_VERSION");
    let mut parts: Vec<u64> = v.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    parts.resize(4, 0);
    let packed = (parts[0] << 48) | (parts[1] << 32) | (parts[2] << 16) | parts[3];
    let mut res = winresource::WindowsResource::new();
    res.set("FileDescription", "FastSnip core")
        .set("ProductName", "FastSnip")
        .set("CompanyName", "Hasthi J")
        .set("LegalCopyright", "GPL-3.0-or-later")
        .set("FileVersion", &v)
        .set("ProductVersion", &v)
        .set_version_info(winresource::VersionInfo::FILEVERSION, packed)
        .set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);
    let _ = res.compile();
}
