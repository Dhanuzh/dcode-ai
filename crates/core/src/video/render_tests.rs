use super::*;
use crate::video::{Caption, Line, Segment};

#[test]
fn output_and_source_times_agree() {
    let edl = Edl {
        title: "T".into(),
        segments: vec![
            Segment {
                from: 0.0,
                to: 4.0,
                speed: 1.0,
            },
            Segment {
                from: 4.0,
                to: 16.0,
                speed: 6.0,
            },
        ],
        ..Default::default()
    };
    // Title 2.5 s, then 4 s, then 12 s at 6× = 2 s.
    assert!((source_time(&edl, 2.5) - 0.0).abs() < 1e-9);
    assert!((source_time(&edl, 6.5) - 4.0).abs() < 1e-9);
    assert!((source_time(&edl, 7.5) - 10.0).abs() < 1e-9);
    assert!((edl.output_time(10.0).unwrap() - 7.5).abs() < 1e-9);
}

#[test]
fn captions_follow_the_cut() {
    let edl = Edl {
        segments: vec![Segment {
            from: 2.0,
            to: 6.0,
            speed: 1.0,
        }],
        captions: vec![
            Caption {
                from: 3.0,
                to: 4.0,
                text: "Kept".into(),
            },
            Caption {
                from: 7.0,
                to: 8.0,
                text: "Cut".into(),
            },
        ],
        ..Default::default()
    };
    let s = srt(&edl);
    assert!(s.contains("00:00:01,000 --> 00:00:02,000\nKept"), "{s}");
    assert!(!s.contains("Cut"));
}

/// A tiny recording rendered end to end (GIF always; MP4 if ffmpeg).
#[test]
fn a_small_recording_renders() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("raw");
    std::fs::create_dir_all(&raw).unwrap();
    let mut frames = Vec::new();
    for i in 0..4u8 {
        let img = RgbImage::from_pixel(320, 200, image::Rgb([40 * i, 80, 120]));
        let file = format!("f{i}.jpg");
        img.save(raw.join(&file)).unwrap();
        frames.push(FrameInfo {
            t: i as f64,
            file,
            css_w: 320.0,
        });
    }
    let events = vec![Event {
        t: 1.5,
        kind: "click".into(),
        x: Some(160.0),
        y: Some(100.0),
        w: Some(60.0),
        h: Some(24.0),
        label: "OK".into(),
    }];
    let mut edl = super::super::draft(&events, 4.0, "Test video");
    edl.voice = "none".into();
    edl.width = 320;
    edl.cuts = vec!["long".into(), "short".into()];
    edl.lines[0].section = "Start".into();
    std::fs::write(
        dir.path().join("frames.json"),
        serde_json::to_string(&frames).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("events.json"),
        serde_json::to_string(&events).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("edl.json"),
        serde_json::to_string(&edl).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.path().join("meta.json"), r#"{"duration":4.0}"#).unwrap();
    let r = render(dir.path(), |_| {}).unwrap();
    let gif = std::fs::read(&r.gif).unwrap();
    assert_eq!(&gif[..3], b"GIF");
    assert!(r.seconds > 4.0, "cards included");
    if let Some(mp4) = r.mp4 {
        assert!(std::fs::metadata(mp4).unwrap().len() > 1000);
        assert!(dir.path().join("short.mp4").is_file());
        assert!(dir.path().join("video-clean.mp4").is_file());
    }
    for f in [
        "thumbnail.png",
        "short-thumbnail.png",
        "short-review.png",
        "captions.vtt",
        "short.srt",
        "chapters.txt",
        "description.txt",
    ] {
        assert!(dir.path().join(f).is_file(), "{f}");
    }
    let srt = std::fs::read_to_string(&r.srt).unwrap();
    assert!(
        srt.contains("Click OK."),
        "captions follow the script: {srt}"
    );
}

#[test]
fn private_boxes_are_pixelated_around_page_changes() {
    let edl = Edl {
        privacy: true,
        blur: vec![crate::video::BlurBox {
            from: 0.0,
            to: 2.0,
            x: 1.0,
            y: 1.0,
            w: 2.0,
            h: 2.0,
        }],
        ..Default::default()
    };
    let privacy = vec![
        Privacy {
            t: 1.0,
            boxes: vec![[10.0, 10.0, 5.0, 5.0]],
        },
        Privacy {
            t: 4.0,
            boxes: vec![[50.0, 50.0, 5.0, 5.0]],
        },
    ];
    let sc = Scene {
        edl: &edl,
        clicks: vec![],
        targets: vec![],
        ends: vec![],
        privacy: &privacy,
        stills: false,
        duration: 10.0,
        backdrops: Default::default(),
        camera: crate::video::motion::camera(&edl, |_| [1.0, 0.0, 0.0], 1.0),
        cursor: None,
        canvases: Default::default(),
        dir: Path::new("."),
        webcam: None,
    };
    // Between two looks: both (the page may be changing), plus the manual box.
    assert_eq!(sc.blur_at(1.5).len(), 3);
    // After the last look: only it.
    assert_eq!(sc.blur_at(9.0), vec![[50.0, 50.0, 5.0, 5.0]]);
    // Before the first look: the first.
    assert!(sc.blur_at(0.2).contains(&[10.0, 10.0, 5.0, 5.0]));

    let mut img = RgbImage::from_fn(64, 64, |x, y| image::Rgb([(x * 4) as u8, (y * 4) as u8, 0]));
    let before = img.clone();
    pixelate(&mut img, 10.0, 10.0, 20.0, 20.0);
    assert_ne!(img, before);
    assert_eq!(
        img.get_pixel(60, 60),
        before.get_pixel(60, 60),
        "outside untouched"
    );
    // A block is one colour.
    assert_eq!(img.get_pixel(8, 8), img.get_pixel(9, 9));
}

#[test]
fn code_and_terminal_cards_draw() {
    let f = font();
    let ins = Insert {
        at: 0.0,
        seconds: 3.0,
        kind: "terminal".into(),
        title: "bash".into(),
        items: vec!["$ cargo test".into(), "test result: ok".into()],
    };
    let img = insert_card(640, 360, &f, &ins, 1.0);
    assert_eq!(img.dimensions(), (640, 360));
    let plain = background(640, 360);
    assert_ne!(img, plain, "something drawn");
}

fn line(id: &str, at: f64, text: &str, section: &str) -> Line {
    Line {
        id: id.into(),
        at,
        text: text.into(),
        section: section.into(),
        only: Vec::new(),
        anchor: String::new(),
    }
}

#[test]
fn the_video_holds_until_a_line_is_said() {
    let edl = Edl {
        end_text: "E".into(),
        segments: vec![Segment {
            from: 0.0,
            to: 10.0,
            speed: 1.0,
        }],
        ..Default::default()
    };
    let (a, b) = (line("a", 1.0, "x", "Intro"), line("b", 3.0, "y", "Main"));
    // `a` takes 5 s to say but `b` starts 2 s later: hold 3.35 s at 3 s.
    let (holds, starts) = fit(&edl, &[&a, &b], &[5.0, 1.0]);
    assert_eq!(holds.len(), 1);
    assert_eq!(holds[0].at, 3.0);
    assert!((holds[0].seconds - (5.0 + LINE_GAP - 2.0)).abs() < 1e-9);
    assert!(
        (starts[1] - (1.0 + 5.0 + LINE_GAP)).abs() < 1e-9,
        "{starts:?}"
    );
    // A last line that runs past the recording holds before the end card.
    let (holds, _) = fit(&edl, &[&a, &line("c", 9.0, "z", "")], &[1.0, 4.0]);
    assert_eq!(holds.last().unwrap().at, 10.0);
    let ch = chapters(&[&a, &b], &[1.0, 6.35]);
    assert_eq!(chapters_text(&ch), "0:00 Intro\n0:06 Main\n");
    // Too few and too short for YouTube: listed, not timestamped.
    let edl = Edl {
        title: "Make a project".into(),
        ..Default::default()
    };
    let d = description(&edl, &ch);
    assert!(d.starts_with("Make a project\n\n"), "{d}");
    assert!(d.contains("- Intro") && !d.contains("0:00"), "{d}");
    let ok = vec![
        (0.0, "Intro".to_string()),
        (12.0, "Main".to_string()),
        (30.0, "Wrap".to_string()),
    ];
    assert!(description(&edl, &ok).contains("Chapters:\n0:00 Intro\n0:12 Main\n0:30 Wrap\n"));
}

#[test]
fn long_lines_become_several_captions() {
    let cues = line_cues(
        "First we open the settings page, then we turn on dark mode and save it for everyone.",
        10.0,
        16.0,
    );
    assert!(cues.len() >= 2, "{cues:?}");
    assert_eq!(cues[0].from, 10.0);
    assert!((cues.last().unwrap().to - 16.0).abs() < 1e-9);
    assert!(cues.iter().all(|c| c.text.split_whitespace().count() <= 12));
    let v = vtt_of(&cues);
    assert!(v.starts_with("WEBVTT") && v.contains("00:00:10.000 --> "));
}
