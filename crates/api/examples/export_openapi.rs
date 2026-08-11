use std::{env, fs, io, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rendered = serde_json::to_string_pretty(&asterism_api::openapi_document())?;
    if let Some(path) = env::args_os().nth(1) {
        write_document(PathBuf::from(path), rendered.as_bytes())?;
    } else {
        println!("{rendered}");
    }
    Ok(())
}

fn write_document(path: PathBuf, document: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, document)
}
