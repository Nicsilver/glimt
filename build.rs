fn main() {
    // PerMonitorV2 so overlay viewports are sized in real physical pixels per monitor.
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <asmv3:application xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
    <asmv3:windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </asmv3:windowsSettings>
  </asmv3:application>
</assembly>"#;

    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/glimt.ico");
    res.set_manifest(manifest);
    res.compile().expect("failed to compile Windows resources");
}
