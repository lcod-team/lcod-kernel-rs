use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-env-changed=LCOD_EMBED_RUNTIME");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let module_path = out_dir.join("embedded_runtime.rs");
    let bundle_dest = out_dir.join("lcod_runtime_bundle.tar.gz");

    let code = match env::var("LCOD_EMBED_RUNTIME") {
        Ok(bundle_path) if !bundle_path.trim().is_empty() => {
            let source = Path::new(&bundle_path);
            if !source.is_file() {
                panic!(
                    "LCOD_EMBED_RUNTIME must point to an existing file, got {}",
                    source.display()
                );
            }
            println!("cargo:rerun-if-changed={}", source.display());
            fs::copy(source, &bundle_dest)?;
            "pub fn bundle_bytes() -> Option<&'static [u8]> {\n    Some(include_bytes!(\"lcod_runtime_bundle.tar.gz\"))\n}\n".to_string()
        }
        _ => {
            if bundle_dest.exists() {
                let _ = fs::remove_file(&bundle_dest);
            }
            "pub fn bundle_bytes() -> Option<&'static [u8]> { None }\n".to_string()
        }
    };

    let mut file = fs::File::create(&module_path)?;
    file.write_all(code.as_bytes())?;
    Ok(())
}
