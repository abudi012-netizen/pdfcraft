//! Preferences ▸ Reading colours: the app-wide preference (Acrobat's Replace Document Colors),
//! how it survives a restart, and how documents open with it.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_render::ReadingColors;
use pdfcraft_ui_egui::PdfCraftApp;

/// One valid page (proper xref, so no repair banner covers the dialog): black text on white.
fn pdf() -> Vec<u8> {
    pdf_with("The quick brown fox")
}

/// [`pdf`] with other text on its page.
fn pdf_with(text: &str) -> Vec<u8> {
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        {
            let body = format!("BT /F1 14 Tf 20 150 Td ({text}) Tj ET");
            format!("<< /Length {} >>\nstream\n{}\nendstream", body.len(), body).into_bytes()
        },
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

/// A small PNG for the Create ▸ Images path, which opens the document it builds.
fn png() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, 8, 4);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[120u8; 8 * 4 * 3]).unwrap();
    }
    out
}

/// The Reading colours combo of the preferences dialog: it shows the chosen mode as its value,
/// and no other combo in the dialog shows `Off` while the setting is off.
fn combo(h: &mut Harness<'static, PdfCraftApp>) {
    // The preferences dialog grew taller than the window, which leaves this combo below the
    // fold where a click lands outside the dialog and dismisses it; scroll it into view and
    // let the scroll settle before clicking.
    h.get_by(|n| n.role() == Role::ComboBox && n.value().is_some_and(|v| v == "Off")).scroll_to_me();
    h.run_steps(4);
    h.get_by(|n| n.role() == Role::ComboBox && n.value().is_some_and(|v| v == "Off")).click();
}

#[test]
fn reading_colours_preference_round_trips() {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("doc.pdf", None, pdf()).expect("opens");
        app.set_option("dialog", "preferences").unwrap();
        app
    });
    h.run_steps(4);
    assert_eq!(h.state().reading_colors, ReadingColors::default());
    // Switch the preset to white-on-black in the dialog's Reading colours combo. The open
    // document keeps repainting, so run a fixed number of steps and let the popup settle.
    combo(&mut h);
    h.run_steps(20);
    h.get_by_label("White on black").click();
    h.run_steps(2);
    assert_eq!(h.state().reading_colors, ReadingColors::WHITE_ON_BLACK);
    // The preference is app-wide: the already open document follows it too.
    let id = h.state().views[0].id;
    assert_eq!(h.state().session.reading_colors(id), Some(ReadingColors::WHITE_ON_BLACK));
    // Closing the dialog keeps everything.
    h.state_mut().set_option("dialog", "none").unwrap();
    h.run_steps(1);

    // The choice survives a restart through the same path the settings file uses.
    let mut restored = PdfCraftApp::new();
    restored.restore(&h.state().persist());
    assert_eq!(restored.reading_colors, ReadingColors::WHITE_ON_BLACK);
    // A settings file without the field keeps Off.
    let mut fresh = PdfCraftApp::new();
    fresh.restore(r#"{"theme": "dark"}"#);
    assert_eq!(fresh.reading_colors, ReadingColors::default());
    // A corrupt value fails to deserialize and falls back to Off instead of panicking.
    fresh.restore(r#"{"reading_colors": {"params": {"text": "zzz", "background": [0, 0, 0], "only_black": false, "line_art": true}}}"#);
    assert_eq!(fresh.reading_colors, ReadingColors::default());
    fresh.restore(r#"{"reading_colors": 7}"#);
    assert_eq!(fresh.reading_colors, ReadingColors::default());
}

#[test]
fn reading_colours_seed_on_open() {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.reading_colors = ReadingColors::WHITE_ON_BLACK;
        app.open_bytes("doc.pdf", None, pdf()).expect("opens");
        app
    });
    h.run_steps(4);
    // The document opened with the preference already applied.
    let id = h.state().views[0].id;
    assert_eq!(h.state().session.reading_colors(id), Some(ReadingColors::WHITE_ON_BLACK));
    // Changing the preference applies it to the open document as well.
    h.state_mut().set_reading_colors(ReadingColors::YELLOW_ON_BLACK);
    h.run_steps(1);
    assert_eq!(h.state().session.reading_colors(id), Some(ReadingColors::YELLOW_ON_BLACK));

    // Revisions and the signed version open through their own session calls, bypassing the
    // ordinary open: they seed the new document the same way. A second revision comes from an
    // incremental save.
    {
        let s = &mut h.state_mut().session;
        s.apply(id, pdfcraft_engine::Edit::SetInfo { key: "Title".into(), value: "Second".into() }).unwrap();
        let saved = s.save_bytes(id).unwrap();
        s.mark_saved(id, saved, None).unwrap();
    }
    h.state_mut().open_revision(1);
    let revision = h.state().views.last().unwrap().id;
    assert_ne!(revision, id);
    assert_eq!(h.state().session.reading_colors(revision), Some(ReadingColors::YELLOW_ON_BLACK));
    let len = h.state().session.get(id).unwrap().bytes.len();
    h.state_mut().view_signed_version(len);
    let signed = h.state().views.last().unwrap().id;
    assert_ne!(signed, id);
    assert_eq!(h.state().session.reading_colors(signed), Some(ReadingColors::YELLOW_ON_BLACK));

    // Created documents open through their own session calls too — a comment summary via
    // `open_created`, an image import via `open_created_bytes` — and seed the same way.
    h.state_mut().summarize_comments();
    let summary = h.state().views.last().unwrap().id;
    assert_ne!(summary, signed);
    assert_eq!(h.state().session.reading_colors(summary), Some(ReadingColors::YELLOW_ON_BLACK));
    h.state_mut().create_from_images(vec![("scan.png".into(), png())]);
    let created = h.state().views.last().unwrap().id;
    assert_ne!(created, summary);
    assert_eq!(h.state().session.reading_colors(created), Some(ReadingColors::YELLOW_ON_BLACK));

    // And back off, for every open document.
    h.state_mut().set_reading_colors(ReadingColors::default());
    h.run_steps(1);
    for view in &h.state().views {
        assert_eq!(h.state().session.reading_colors(view.id), Some(ReadingColors::default()));
    }
}

/// View ▸ Night reading turns night reading on for every open document with the last theme
/// chosen, and off again; the theme survives being switched off and a restart.
#[test]
fn night_reading_toggles_and_remembers_its_theme() {
    use pdfcraft_render::NightTheme;
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("doc.pdf", None, pdf()).expect("opens");
        app
    });
    h.run_steps(2);
    let id = h.state().views[0].id;
    assert!(h.state_mut().execute("view.night_reading"));
    h.run_steps(1);
    let night = |theme| ReadingColors { params: None, night: Some(theme) };
    assert_eq!(h.state().session.reading_colors(id), Some(night(NightTheme::DarkGrey)));
    // Choosing sepia in Preferences is remembered when night reading is switched off and on.
    h.state_mut().set_reading_colors(night(NightTheme::Sepia));
    assert!(h.state_mut().execute("view.night_reading"));
    assert_eq!(h.state().session.reading_colors(id), Some(ReadingColors::default()));
    assert!(h.state_mut().execute("view.night_reading"));
    assert_eq!(h.state().session.reading_colors(id), Some(night(NightTheme::Sepia)));
    h.run_steps(2);
    // Another kind of reading colours gives way to night reading, and night reading to it.
    h.state_mut().set_reading_colors(ReadingColors::WHITE_ON_BLACK);
    assert!(h.state_mut().execute("view.night_reading"));
    assert_eq!(h.state().reading_colors, night(NightTheme::Sepia));
    // Restart: the setting and the remembered theme come back; junk keeps dark grey.
    let mut restored = PdfCraftApp::new();
    restored.restore(&h.state().persist());
    assert_eq!((restored.reading_colors, restored.night_theme), (night(NightTheme::Sepia), NightTheme::Sepia));
    let mut fresh = PdfCraftApp::new();
    fresh.restore(r#"{"night_theme": "neon"}"#);
    assert_eq!(fresh.night_theme, NightTheme::DarkGrey);
}

/// Edit ▸ Search in multiple PDFs: files that aren't open are read in the background, every
/// match is listed by file, and opening one goes to that exact match in the file's own find.
#[test]
fn search_in_multiple_pdfs_finds_and_opens_the_match() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-search-files-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.pdf"), pdf_with("Paracetamol relieves pain")).unwrap();
    std::fs::write(dir.join("b.pdf"), pdf_with("Ibuprofen or paracetamol, then paracetamol")).unwrap();
    std::fs::write(dir.join("c.pdf"), pdf_with("Aspirin only")).unwrap();
    std::fs::write(dir.join("broken.pdf"), b"not a pdf").unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app
    });
    h.run_steps(1);
    assert!(h.state_mut().execute("edit.search_files"));
    h.state_mut().search_files_add(vec![dir.join("a.pdf"), dir.join("b.pdf"), dir.join("c.pdf"), dir.join("broken.pdf"), dir.join("a.pdf")]);
    for _ in 0..400 {
        h.run_steps(1);
        if !h.state_mut().search_files_busy() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!h.state_mut().search_files_busy(), "the files were read");
    h.state_mut().search_files_query("PARACETAMOL", false, false);
    h.run_steps(2);
    let found = h.state_mut().search_files_results();
    let names: Vec<(&str, usize)> = found.iter().map(|(n, p, _)| (n.as_str(), *p)).collect();
    assert_eq!(names, [("a.pdf", 1), ("b.pdf", 1), ("b.pdf", 1)]);
    assert!(h.query_by_label_contains("3 instances in 2 files").is_some(), "the count shows");
    assert!(h.query_by_label_contains("broken.pdf: couldn't read it").is_some(), "the unreadable file says so");

    // Opening the second match in b.pdf opens it and selects that very match.
    h.state_mut().search_files_open_result(2);
    for _ in 0..400 {
        h.run_steps(1);
        let state = h.state();
        let done = state.views.len() == 1 && state.views[0].find.as_ref().is_some_and(|f| f.current.is_some() && f.matches.len() == 2);
        if done && !h.state_mut().search_files_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!h.state().search_files_showing(), "the dialog closed");
    let view = &h.state().views[0];
    let name = h.state().session.get(view.id).map(|d| d.name.clone());
    assert_eq!(name.as_deref(), Some("b.pdf"));
    let find = view.find.as_ref().expect("the search carried into the document's find");
    assert_eq!(find.query, "PARACETAMOL");
    assert_eq!(find.current, Some(1), "the second match on the page is the current one");
    let _ = std::fs::remove_dir_all(&dir);
}
