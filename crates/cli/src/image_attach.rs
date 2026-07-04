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

/// Read an image from the system clipboard and store as PNG under the session.
///
/// Tries the native clipboard first. Under WSL the Linux clipboard is
/// disconnected from the Windows clipboard, so if that yields no image we fall
/// back to fetching the Windows clipboard image via `powershell.exe`.
pub fn paste_clipboard_image(
    workspace: &Path,
    session_id: &str,
) -> Result<ImageAttachment, String> {
    match clipboard_image_via_arboard(workspace, session_id) {
        Ok(attachment) => Ok(attachment),
        Err(native_err) => {
            if is_wsl() {
                paste_clipboard_image_wsl(workspace, session_id)
            } else {
                Err(native_err)
            }
        }
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

/// WSL fallback: save the *Windows* clipboard image to the session directory via
/// `powershell.exe` (GetImage needs an STA apartment).
fn paste_clipboard_image_wsl(
    workspace: &Path,
    session_id: &str,
) -> Result<ImageAttachment, String> {
    let filename = nanos_name("paste", "png");
    let dir = session_attachments_dir(workspace, session_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create attachments dir: {e}"))?;
    let dest = dir.join(&filename);
    let win_path = wsl_windows_path(&dest)?;

    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms,System.Drawing; \
         $img=[System.Windows.Forms.Clipboard]::GetImage(); \
         if($img -eq $null){{exit 2}}; \
         $img.Save('{}',[System.Drawing.Imaging.ImageFormat]::Png); exit 0",
        win_path.replace('\'', "''")
    );

    let out = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .output()
        .map_err(|e| format!("powershell.exe unavailable for WSL clipboard bridge: {e}"))?;

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
            "WSL clipboard bridge failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
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
