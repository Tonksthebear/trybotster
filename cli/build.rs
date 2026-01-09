//! Build script for botster-hub.
//!
//! Downloads and embeds the Tailscale binary for the target platform.
//! The binary is downloaded at build time and embedded into the final
//! executable using `include_bytes!()`.

use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Tailscale version to embed.
/// Update this when new Tailscale versions are released.
const TAILSCALE_VERSION: &str = "1.76.6";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=TAILSCALE_BINARY_PATH");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let target = env::var("TARGET").expect("TARGET not set");

    // Allow CI to pre-download the binary
    if let Ok(prebuilt_path) = env::var("TAILSCALE_BINARY_PATH") {
        println!("cargo:warning=Using pre-built Tailscale binary from {prebuilt_path}");
        let dest = out_dir.join("tailscale");
        fs::copy(&prebuilt_path, &dest).expect("Failed to copy pre-built Tailscale binary");
        return;
    }

    // For local development, download the binary
    let tailscale_path = out_dir.join("tailscale");

    // Skip download if already present (for incremental builds)
    if tailscale_path.exists() {
        println!("cargo:warning=Tailscale binary already exists, skipping download");
        return;
    }

    println!("cargo:warning=Downloading Tailscale {TAILSCALE_VERSION} for {target}...");

    match download_tailscale(&target, &out_dir) {
        Ok(()) => println!("cargo:warning=Tailscale binary downloaded successfully"),
        Err(e) => {
            // Don't fail the build - create a placeholder so compilation can continue
            // The binary will fail at runtime if Tailscale features are used
            println!("cargo:warning=Failed to download Tailscale: {e}");
            println!("cargo:warning=Creating placeholder binary - Tailscale features will not work");
            create_placeholder(&tailscale_path);
        }
    }
}

fn download_tailscale(target: &str, out_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let (url, archive_type, binary_path_in_archive) = get_download_info(target)?;

    println!("cargo:warning=Downloading from {url}");

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let response = client.get(&url).send()?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}: {}", response.status(), url).into());
    }

    let bytes = response.bytes()?;
    let dest = out_dir.join("tailscale");

    match archive_type {
        ArchiveType::TarGz => extract_tar_gz(&bytes, &binary_path_in_archive, &dest)?,
        ArchiveType::Zip => extract_zip(&bytes, &binary_path_in_archive, &dest)?,
    }

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&dest)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dest, perms)?;
    }

    Ok(())
}

#[derive(Debug)]
enum ArchiveType {
    TarGz,
    Zip,
}

fn get_download_info(target: &str) -> Result<(String, ArchiveType, String), Box<dyn std::error::Error>> {
    // Map Rust target triples to Tailscale download URLs
    // Note: macOS uses a universal binary (works on both arm64 and x86_64)
    let (archive_type, arch) = match target {
        // macOS - universal binary
        "aarch64-apple-darwin" | "x86_64-apple-darwin" => (ArchiveType::Zip, None),
        // Linux
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => {
            (ArchiveType::TarGz, Some("amd64"))
        }
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => {
            (ArchiveType::TarGz, Some("arm64"))
        }
        _ => return Err(format!("Unsupported target: {target}").into()),
    };

    let url = match archive_type {
        // macOS: Tailscale-1.76.6-macos.zip (universal binary)
        ArchiveType::Zip => {
            format!("https://pkgs.tailscale.com/stable/Tailscale-{TAILSCALE_VERSION}-macos.zip")
        }
        // Linux: tailscale_1.76.6_amd64.tgz
        ArchiveType::TarGz => {
            let arch = arch.expect("Linux requires arch");
            format!("https://pkgs.tailscale.com/stable/tailscale_{TAILSCALE_VERSION}_{arch}.tgz")
        }
    };

    // Path to the tailscale binary within the archive
    let binary_path = match archive_type {
        // macOS zip contains the GUI app, but the binary works as CLI too
        // Path: Tailscale.app/Contents/MacOS/Tailscale
        ArchiveType::Zip => "Tailscale.app/Contents/MacOS/Tailscale".to_string(),
        ArchiveType::TarGz => {
            let arch = arch.expect("Linux requires arch");
            format!("tailscale_{TAILSCALE_VERSION}_{arch}/tailscale")
        }
    };

    Ok((url, archive_type, binary_path))
}

fn extract_tar_gz(
    data: &[u8],
    binary_path: &str,
    dest: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let decoder = GzDecoder::new(data);
    let mut archive = Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;

        if path.to_string_lossy() == binary_path {
            let mut file = File::create(dest)?;
            io::copy(&mut entry, &mut file)?;
            return Ok(());
        }
    }

    Err(format!("Binary not found in archive: {binary_path}").into())
}

fn extract_zip(
    data: &[u8],
    binary_path: &str,
    dest: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Cursor;
    use zip::ZipArchive;

    let cursor = Cursor::new(data);
    let mut archive = ZipArchive::new(cursor)?;

    // Try exact path first
    if let Ok(mut file) = archive.by_name(binary_path) {
        let mut out = File::create(dest)?;
        io::copy(&mut file, &mut out)?;
        return Ok(());
    }

    // Search for tailscale binary in archive (macOS zips have different structure)
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();

        if name.ends_with("/tailscale") || name == "tailscale" {
            let mut out = File::create(dest)?;
            io::copy(&mut file, &mut out)?;
            return Ok(());
        }
    }

    Err(format!("Binary not found in zip archive: {binary_path}").into())
}

fn create_placeholder(path: &Path) {
    // Create a small shell script that errors out
    let placeholder = if cfg!(windows) {
        "@echo off\necho Tailscale binary not available - download failed during build\nexit /b 1"
    } else {
        "#!/bin/sh\necho 'Tailscale binary not available - download failed during build'\nexit 1"
    };

    if let Ok(mut file) = File::create(path) {
        let _ = file.write_all(placeholder.as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = fs::metadata(path) {
                let mut perms = metadata.permissions();
                perms.set_mode(0o755);
                let _ = fs::set_permissions(path, perms);
            }
        }
    }
}
