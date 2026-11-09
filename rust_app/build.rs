fn main() {
    println!("cargo:rerun-if-changed=assets/autoflow.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/autoflow.ico");
        res.set("ProductName", "AutoFlow Assistant [Native Hybrid Edition]");
        res.set(
            "FileDescription",
            "AutoFlow Assistant — Гібридна система автоматизації Windows (UIA + SIMD Vision)",
        );
        res.set("LegalCopyright", "AutoFlow Hybrid Edition");
        if let Err(e) = res.compile() {
            println!("cargo:warning=Не вдалося вбудувати PE-іконку через winresource: {}", e);
        }
    }
}

