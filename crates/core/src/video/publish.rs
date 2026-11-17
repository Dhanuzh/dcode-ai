//! After rendering: an upload folder per platform (`/video publish`), the
//! person's sign-off (`/video approve`), and where a video stands
//! (`/video status`). Nothing is uploaded — the folders are for you.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::record::load;
use super::voice;

/// A platform: (id, name, cut it takes, longest seconds it accepts).
pub const PLATFORMS: &[(&str, &str, &str, f64)] = &[
    ("youtube", "YouTube", "long", 12.0 * 3600.0),
    ("youtube-shorts", "YouTube Shorts", "short", 180.0),
    ("instagram-reels", "Instagram Reels", "short", 180.0),
    ("whatsapp-status", "WhatsApp Status", "short", 60.0),
    ("tiktok", "TikTok", "short", 600.0),
    ("linkedin", "LinkedIn", "long", 600.0),
    ("x", "X", "long", 140.0),
];

fn sha256_file(p: &Path) -> Option<String> {
    let bytes = std::fs::read(p).ok()?;
    Some(format!("{:x}", Sha256::digest(&bytes)))
}

/// The files a sign-off covers: the videos, the script, the takes.
fn signed_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = ["video.mp4", "video-clean.mp4", "short.mp4", "edl.json"]
        .iter()
        .map(|f| dir.join(f))
        .filter(|p| p.is_file())
        .collect();
    let mut takes: Vec<PathBuf> = std::fs::read_dir(voice::voice_dir(dir))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".take.wav"))
        .collect();
    takes.sort();
    v.extend(takes);
    v
}

fn rel(dir: &Path, p: &Path) -> String {
    p.strip_prefix(dir)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Records the rendered files' sha256 in `approved.json`: "this is what I
/// release". A person's step — no agent tool does it.
pub fn approve(dir: &Path) -> Result<usize, String> {
    let files = signed_files(dir);
    if !files
        .iter()
        .any(|p| p.extension().is_some_and(|x| x == "mp4"))
    {
        return Err("nothing rendered to approve — /video render first".into());
    }
    let map: serde_json::Map<String, serde_json::Value> = files
        .iter()
        .filter_map(|p| Some((rel(dir, p), sha256_file(p)?.into())))
        .collect();
    let n = map.len();
    std::fs::write(
        dir.join("approved.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "approved_at": chrono::Local::now().to_rfc3339(),
            "files": map,
        }))
        .unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    Ok(n)
}

/// Files changed since approval (empty when it still holds), or None when
/// never approved.
pub fn changed_since_approval(dir: &Path) -> Option<Vec<String>> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("approved.json")).ok()?).ok()?;
    let files = v["files"].as_object()?;
    let mut changed = Vec::new();
    for (name, sum) in files {
        if sha256_file(&dir.join(name)).as_deref() != sum.as_str() {
            changed.push(name.clone());
        }
    }
    for p in signed_files(dir) {
        let r = rel(dir, &p);
        if !files.contains_key(&r) {
            changed.push(r);
        }
    }
    Some(changed)
}

/// Where a recording stands, as lines.
pub fn status(dir: &Path) -> Result<Vec<String>, String> {
    let saved = load(dir)?;
    let edl = saved.edl.normalized(saved.duration);
    let mut out = Vec::new();
    out.push(format!(
        "\"{}\" · {:.0}s recorded · cuts: {}",
        if edl.title.is_empty() {
            "(no title)"
        } else {
            &edl.title
        },
        saved.duration,
        edl.cuts.join(", ")
    ));
    if edl.lines.is_empty() {
        out.push("no narration script (captions only)".into());
    } else {
        let takes: Vec<&str> = edl
            .lines
            .iter()
            .filter(|l| voice::take_path(dir, &l.id).is_file())
            .map(|l| l.id.as_str())
            .collect();
        let drafts = edl
            .lines
            .iter()
            .filter(|l| voice::draft_path(dir, &l.id).is_file())
            .count();
        out.push(format!(
            "script: {} lines · your takes: {}/{} · draft voice: {}/{} · voice: {}",
            edl.lines.len(),
            takes.len(),
            edl.lines.len(),
            drafts,
            edl.lines.len(),
            edl.voice
        ));
        let missing: Vec<&str> = edl
            .lines
            .iter()
            .filter(|l| !takes.contains(&l.id.as_str()))
            .map(|l| l.id.as_str())
            .take(8)
            .collect();
        if !takes.is_empty() && !missing.is_empty() {
            out.push(format!("no take yet: {}", missing.join(", ")));
        }
    }
    let rendered: Vec<&str> = [
        "video.mp4",
        "video-clean.mp4",
        "short.mp4",
        "video.gif",
        "thumbnail.png",
        "chapters.txt",
    ]
    .into_iter()
    .filter(|f| dir.join(f).is_file())
    .collect();
    out.push(if rendered.is_empty() {
        "not rendered — /video render".into()
    } else {
        format!("rendered: {}", rendered.join(", "))
    });
    let edl_time = std::fs::metadata(dir.join("edl.json"))
        .and_then(|m| m.modified())
        .ok();
    let render_time = std::fs::metadata(dir.join("render.json"))
        .and_then(|m| m.modified())
        .ok();
    if let (Some(e), Some(r)) = (edl_time, render_time)
        && e > r
    {
        out.push("the script changed after the last render — /video render".into());
    }
    out.push(match changed_since_approval(dir) {
        None => "not approved — /video approve when it's right".into(),
        Some(c) if c.is_empty() => "approved, unchanged since".into(),
        Some(c) => format!("approved, but changed since: {}", c.join(", ")),
    });
    Ok(out)
}

fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(to);
    std::fs::hard_link(from, to).or_else(|_| std::fs::copy(from, to).map(|_| ()))
}

/// Makes `out/<platform>/<cut>/` for each platform in the edit list whose
/// cut was rendered: the video, captions, title, description (with
/// chapters) and thumbnail. Returns a line per folder (with warnings).
pub fn publish(dir: &Path) -> Result<Vec<String>, String> {
    let saved = load(dir)?;
    let edl = saved.edl.normalized(saved.duration);
    let seconds: f64 = std::fs::read_to_string(dir.join("render.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["seconds"].as_f64())
        .unwrap_or(0.0);
    let chapters = std::fs::read_to_string(dir.join("chapters.txt")).unwrap_or_default();
    let platforms = if edl.platforms.is_empty() {
        super::default_platforms()
    } else {
        edl.platforms.clone()
    };
    let mut report = Vec::new();
    for id in &platforms {
        let Some(&(pid, name, cut, max)) = PLATFORMS.iter().find(|p| p.0 == id) else {
            report.push(format!(
                "{id}: unknown platform (known: {})",
                PLATFORMS.iter().map(|p| p.0).collect::<Vec<_>>().join(", ")
            ));
            continue;
        };
        let (video, caps, thumb) = if cut == "short" {
            (
                dir.join("short.mp4"),
                "short",
                dir.join("short-thumbnail.png"),
            )
        } else {
            let clean = dir.join("video-clean.mp4");
            // YouTube takes the clean master and the caption file; feeds
            // that autoplay muted get the captioned one.
            let v = if pid == "youtube" && clean.is_file() {
                clean
            } else {
                dir.join("video.mp4")
            };
            (v, "captions", dir.join("thumbnail.png"))
        };
        if !video.is_file() {
            report.push(format!(
                "{name}: skipped — no {cut} cut rendered (add \"{cut}\" to cuts, /video render)"
            ));
            continue;
        }
        let out = dir.join("out").join(pid).join(cut);
        std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        copy(&video, &out.join("video.mp4")).map_err(|e| e.to_string())?;
        for ext in ["srt", "vtt"] {
            let p = dir.join(format!("{caps}.{ext}"));
            if p.is_file() {
                copy(&p, &out.join(format!("captions.{ext}"))).map_err(|e| e.to_string())?;
            }
        }
        if thumb.is_file() {
            copy(&thumb, &out.join("thumbnail.png")).map_err(|e| e.to_string())?;
        }
        let mut desc = String::new();
        if !edl.subtitle.trim().is_empty() {
            desc.push_str(edl.subtitle.trim());
            desc.push_str("\n\n");
        }
        if cut == "long" && chapters.lines().count() >= 3 {
            desc.push_str("Chapters\n");
            desc.push_str(&chapters);
        }
        // The render's own description (chapters checked against YouTube's
        // rules) when there is one.
        if cut == "long"
            && let Ok(d) = std::fs::read_to_string(dir.join("description.txt"))
            && !d.trim().is_empty()
        {
            desc = d;
        }
        std::fs::write(out.join("title.txt"), format!("{}\n", edl.title.trim()))
            .map_err(|e| e.to_string())?;
        std::fs::write(out.join("description.txt"), desc).map_err(|e| e.to_string())?;
        let mut line = format!("{name}: {}", rel(dir, &out));
        // The short's length is the long cut's when both were rendered;
        // check the short's own from its captions' end when we can.
        let len = if cut == "short" {
            short_seconds(dir).unwrap_or(seconds)
        } else {
            seconds
        };
        if len > max {
            line.push_str(&format!(
                " — {len:.0}s is over its {max:.0}s limit; shorten the {cut} cut"
            ));
        }
        report.push(line);
    }
    Ok(report)
}

/// The short cut's length, from its last caption (close enough to warn).
fn short_seconds(dir: &Path) -> Option<f64> {
    let srt = std::fs::read_to_string(dir.join("short.srt")).ok()?;
    let last = srt.lines().rev().find(|l| l.contains(" --> "))?;
    let end = last.split(" --> ").nth(1)?.trim();
    let mut parts = end.split([':', ',']);
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s + 2.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        std::fs::write(p.join("frames.json"), r#"[{"t":0,"file":"f.jpg"}]"#).unwrap();
        std::fs::write(p.join("events.json"), "[]").unwrap();
        std::fs::write(p.join("meta.json"), r#"{"duration":5}"#).unwrap();
        std::fs::write(
            p.join("edl.json"),
            r#"{"title":"Demo","cuts":["long","short"],"lines":[{"id":"a","at":0,"text":"Hi","section":"One"}]}"#,
        )
        .unwrap();
        std::fs::write(p.join("video.mp4"), b"long").unwrap();
        std::fs::write(p.join("video-clean.mp4"), b"clean").unwrap();
        std::fs::write(p.join("short.mp4"), b"short").unwrap();
        std::fs::write(
            p.join("captions.srt"),
            "1\n00:00:00,000 --> 00:00:01,000\nHi\n",
        )
        .unwrap();
        std::fs::write(
            p.join("short.srt"),
            "1\n00:00:00,000 --> 00:01:30,000\nHi\n",
        )
        .unwrap();
        std::fs::write(p.join("render.json"), r#"{"seconds":20}"#).unwrap();
        d
    }

    #[test]
    fn publish_makes_a_folder_per_platform_and_checks_length() {
        let d = project();
        let r = publish(d.path()).unwrap();
        assert_eq!(r.len(), 4, "{r:?}");
        let yt = d.path().join("out/youtube/long");
        assert_eq!(std::fs::read(yt.join("video.mp4")).unwrap(), b"clean");
        assert!(yt.join("captions.srt").is_file());
        assert_eq!(
            std::fs::read_to_string(yt.join("title.txt")).unwrap(),
            "Demo\n"
        );
        let wa = d.path().join("out/whatsapp-status/short");
        assert_eq!(std::fs::read(wa.join("video.mp4")).unwrap(), b"short");
        assert!(
            r.iter()
                .any(|l| l.starts_with("WhatsApp Status") && l.contains("over its 60s")),
            "{r:?}"
        );
        assert!(
            !r.iter()
                .any(|l| l.starts_with("YouTube Shorts") && l.contains("over"))
        );
    }

    #[test]
    fn approval_notices_changes() {
        let d = project();
        assert!(changed_since_approval(d.path()).is_none());
        assert_eq!(approve(d.path()).unwrap(), 4);
        assert_eq!(
            changed_since_approval(d.path()).unwrap(),
            Vec::<String>::new()
        );
        std::fs::write(d.path().join("short.mp4"), b"re-rendered").unwrap();
        assert_eq!(
            changed_since_approval(d.path()).unwrap(),
            vec!["short.mp4".to_string()]
        );
        let s = status(d.path()).unwrap().join("\n");
        assert!(s.contains("changed since: short.mp4"), "{s}");
        assert!(s.contains("your takes: 0/1"), "{s}");
    }
}
