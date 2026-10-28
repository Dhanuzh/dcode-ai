//! Stage image attachments under `.dcode-ai/sessions/<id>/attachments/`.

use arboard::Clipboard;
use dcode_ai_common::message::ImageAttachment;
use image::{DynamicImage, ImageBuffer, Rgba};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn nanos_name(prefix: &str, ext: &str) -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{n}.{ext}")
}

pub fn session_attachments_dir(workspace: &Path, session_id: &str) -> PathBuf {
    workspace
        .join(".dcode-ai")
        .join("sessions")
        .join(session_id)
        .join("attachments")
}

fn relative_attachment_path(session_id: &str, filename: &str) -> String {
    format!(".dcode-ai/sessions/{session_id}/attachments/{filename}")
}

fn media_type_for_extension(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

/// Copy a user file into the session attachment directory and return a workspace-relative ref.
pub fn import_image_file(
    workspace: &Path,
    session_id: &str,
    src: &Path,
) -> Result<ImageAttachment, String> {
    let src = if src.is_absolute() {
        src.to_path_buf()
    } else {
        workspace.join(src)
    };
    if !src.is_file() {
        return Err(format!("not a file: {}", src.display()));
    }
    let ext = src
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("bin")
        .to_string();
    let media_type = media_type_for_extension(&ext).to_string();
    let filename = nanos_name("import", &ext);
    let dir = session_attachments_dir(workspace, session_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create attachments dir: {e}"))?;
    let dest = dir.join(&filename);
    std::fs::copy(&src, &dest).map_err(|e| format!("copy image: {e}"))?;
    Ok(ImageAttachment {
        media_type,
        path: relative_attachment_path(session_id, &filename),
    })
}

/// Store raw image bytes (e.g. a base64 image block from an editor) in the
/// session attachment directory.
pub fn save_image_bytes(
    workspace: &Path,
    session_id: &str,
    media_type: &str,
    bytes: &[u8],
) -> Result<ImageAttachment, String> {
    let ext = match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        other => return Err(format!("unsupported image type `{other}`")),
    };
    let filename = nanos_name("acp", ext);
    let dir = session_attachments_dir(workspace, session_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create attachments dir: {e}"))?;
    std::fs::write(dir.join(&filename), bytes).map_err(|e| format!("save image: {e}"))?;
    Ok(ImageAttachment {
        media_type: media_type.to_string(),
        path: relative_attachment_path(session_id, &filename),
    })
}

/// Read an image from the system clipboard and store as PNG under the session.
///
/// Tries the native clipboard first. Under WSL the Linux clipboard is
/// disconnected from the Windows clipboard, so if that yields no image we fall
/// back to fetching the Windows clipboard image via `powershell.exe`.
pub fn paste_clipboard_image(
    workspace: &Path,
    session_id: &str,
) -> Result<ImageAttachment, String> {
    // Under WSL the Windows clipboard is the one the user copied to; the
    // Linux one (WSLg) may hold an older image.
    if is_wsl() {
        return paste_clipboard_image_powershell(workspace, session_id, true).or_else(|ps_err| {
            clipboard_image_via_arboard(workspace, session_id).map_err(|_| ps_err)
        });
    }
    match clipboard_image_via_arboard(workspace, session_id) {
        Ok(attachment) => Ok(attachment),
        // Some clipboard formats (DIB screenshots) trip arboard on Windows,
        // and a file copied in Explorer is no bitmap at all;
        // System.Windows.Forms handles both.
        Err(native_err) if cfg!(windows) => {
            paste_clipboard_image_powershell(workspace, session_id, false)
                .map_err(|ps_err| format!("{native_err}; {ps_err}"))
        }
        Err(native_err) => Err(native_err),
    }
}

/// Save the Windows clipboard image via PowerShell (GetImage needs an STA
/// apartment), from native Windows or WSL (`powershell.exe`, Windows paths).
/// When the clipboard holds image *files* (copied in Explorer), the first
/// one is imported instead.
fn paste_clipboard_image_powershell(
    workspace: &Path,
    session_id: &str,
    wsl: bool,
) -> Result<ImageAttachment, String> {
    let filename = nanos_name("paste", "png");
    let dir = session_attachments_dir(workspace, session_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create attachments dir: {e}"))?;
    let dest = dir.join(&filename);
    let ps_dest = if wsl {
        wsl_windows_path(&dest)?
    } else {
        dest.display().to_string()
    };

    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms,System.Drawing; \
         $img=[System.Windows.Forms.Clipboard]::GetImage(); \
         if($img -ne $null){{$img.Save('{}',[System.Drawing.Imaging.ImageFormat]::Png); exit 0}}; \
         foreach($f in [System.Windows.Forms.Clipboard]::GetFileDropList()){{ \
           if($f -match '\\.(png|jpe?g|gif|webp|bmp)$'){{[Console]::Out.Write($f); exit 3}} }}; \
         exit 2",
        ps_dest.replace('\'', "''")
    );
    let exe = if wsl { "powershell.exe" } else { "powershell" };
    let out = std::process::Command::new(exe)
        .args(["-NoProfile", "-STA", "-Command", &script])
        .output()
        .map_err(|e| format!("{exe} unavailable for the clipboard: {e}"))?;

    if out.status.code() == Some(3) {
        let _ = std::fs::remove_file(&dest);
        let win_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let src = if wsl {
            wsl_linux_path(&win_path)?
        } else {
            PathBuf::from(win_path)
        };
        return import_image_file(workspace, session_id, &src);
    }
    if dest.is_file()
        && std::fs::metadata(&dest)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
    {
        return Ok(ImageAttachment {
            media_type: "image/png".into(),
            path: relative_attachment_path(session_id, &filename),
        });
    }
    let _ = std::fs::remove_file(&dest);
    if out.status.code() == Some(2) {
        Err("clipboard has no image — copy an image first, or use /image <path>".into())
    } else {
        Err(format!(
            "clipboard read failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// True when running inside the Windows Subsystem for Linux.
fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::fs::read_to_string("/proc/version")
            .map(|v| v.to_ascii_lowercase().contains("microsoft"))
            .unwrap_or(false)
}

/// Translate a Linux path to its Windows form via `wslpath -w`.
fn wsl_windows_path(linux_path: &Path) -> Result<String, String> {
    let out = std::process::Command::new("wslpath")
        .arg("-w")
        .arg(linux_path)
        .output()
        .map_err(|e| format!("wslpath unavailable: {e}"))?;
    if !out.status.success() {
        return Err("wslpath could not convert the attachment path".into());
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        return Err("wslpath returned an empty path".into());
    }
    Ok(path)
}

/// Translate a Windows path to its WSL form via `wslpath -u`.
fn wsl_linux_path(windows_path: &str) -> Result<PathBuf, String> {
    let out = std::process::Command::new("wslpath")
        .arg("-u")
        .arg(windows_path)
        .output()
        .map_err(|e| format!("wslpath unavailable: {e}"))?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || path.is_empty() {
        return Err("wslpath could not convert the clipboard file path".into());
    }
    Ok(PathBuf::from(path))
}

fn clipboard_image_via_arboard(
    workspace: &Path,
    session_id: &str,
) -> Result<ImageAttachment, String> {
    let mut clipboard = Clipboard::new().map_err(|e| format!("clipboard: {e}"))?;
    let img = clipboard
        .get_image()
        .map_err(|e| format!("clipboard has no image (try /image <path>): {e}"))?;

    let w = img.width as u32;
    let h = img.height as u32;
    let expected = (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "clipboard image dimensions overflow".to_string())?;
    if img.bytes.len() < expected {
        return Err("clipboard image buffer too small".into());
    }
    let rgba: Vec<u8> = img.bytes[..expected].to_vec();
    let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_raw(w, h, rgba)
        .ok_or_else(|| "invalid clipboard image buffer".to_string())?;
    let dyn_img = DynamicImage::ImageRgba8(buffer);

    let filename = nanos_name("paste", "png");
    let dir = session_attachments_dir(workspace, session_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create attachments dir: {e}"))?;
    let dest = dir.join(&filename);
    dyn_img.save(&dest).map_err(|e| format!("save png: {e}"))?;

    Ok(ImageAttachment {
        media_type: "image/png".into(),
        path: relative_attachment_path(session_id, &filename),
    })
}
