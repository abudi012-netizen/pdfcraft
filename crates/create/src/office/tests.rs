use std::sync::Arc;

use pdfcraft_cos::{SaveOptions, write_full};

use super::*;

/// Pages in `doc`, after writing it and reading it back (with a second reader, hayro).
fn count(doc: &Document) -> usize {
    let bytes = write_full(doc, &SaveOptions::default()).unwrap();
    let hayro = hayro_syntax::Pdf::new(bytes.clone()).expect("parses").pages().len();
    let doc = Document::open(Arc::new(bytes)).unwrap();
    let pages = doc.get(doc.root().unwrap()).as_dict().unwrap().reference(b"Pages").unwrap();
    let n = doc.get(pages).as_dict().unwrap().get(b"Kids").unwrap().as_array().unwrap().len();
    assert_eq!(n, hayro);
    n
}

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing""#;

fn png() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, 2, 2);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[255; 12]).unwrap();
    }
    out
}

fn docx() -> Vec<u8> {
    let document = format!(
        r#"<w:document {W}><w:body>
<w:p><w:pPr><w:pStyle w:val="Kop1"/></w:pPr><w:r><w:t>Pharmacokinetics</w:t></w:r></w:p>
<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:b/><w:sz w:val="28"/></w:rPr><w:t xml:space="preserve">Half-life </w:t></w:r><w:r><w:rPr><w:i/></w:rPr><w:t>(t½) of drug α</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Absorption</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Distribution</w:t></w:r></w:p>
<w:p><w:r><w:t>Before</w:t></w:r><w:r><w:br w:type="page"/><w:t>After</w:t></w:r></w:p>
<w:p><w:r><w:drawing><wp:inline><wp:extent cx="1270000" cy="635000"/><a:graphic><a:graphicData><a:blip r:embed="rId5"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
<w:tbl><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="6000"/></w:tblGrid>
<w:tr><w:tc><w:p><w:r><w:t>Drug</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Vd</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>Digoxin is wide</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr>
</w:body></w:document>"#
    );
    let styles = format!(r#"<w:styles {W}><w:style w:type="paragraph" w:styleId="Kop1"><w:name w:val="heading 1"/></w:style></w:styles>"#);
    let numbering = format!(
        r#"<w:numbering {W}><w:abstractNum w:abstractNumId="7"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/><w:lvlText w:val="%1)"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="7"/></w:num></w:numbering>"#
    );
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
    let image = png();
    zip::build(&[
        ("word/document.xml", document.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
        ("word/_rels/document.xml.rels", rels.as_bytes()),
        ("word/media/image1.png", &image),
    ])
}

fn paras(flow: &Flow) -> Vec<&Para> {
    flow.blocks.iter().filter_map(|b| if let Block::Para(p) = b { Some(p) } else { None }).collect()
}

#[test]
fn word_documents_keep_headings_lists_breaks_pictures_and_tables() {
    let flow = docx::read(&docx()).unwrap();
    assert_eq!(flow.page, (11906.0 / 20.0, 16838.0 / 20.0), "A4 from the section");
    let p = paras(&flow);
    assert_eq!(p[0].heading, Some(1), "a heading style found by its name, whatever its id");
    assert_eq!(p[1].align, Align::Center);
    assert_eq!(p[1].runs[0], Run { text: "Half-life ".into(), bold: true, italic: false, size: Some(14.0) });
    assert!(p[1].runs[1].italic && p[1].runs[1].text.contains('α'));
    assert_eq!((p[2].bullet.as_deref(), p[3].bullet.as_deref()), (Some("1)"), Some("2)")), "numbered from the numbering part");
    let kinds: Vec<&str> = flow
        .blocks
        .iter()
        .map(|b| match b {
            Block::Para(p) if p.text() == "Before" => "before",
            Block::Para(p) if p.text() == "After" => "after",
            Block::PageBreak => "break",
            Block::Picture(_) => "picture",
            Block::Table(_) => "table",
            Block::Para(_) => "para",
        })
        .collect();
    assert_eq!(kinds, ["para", "para", "para", "para", "before", "break", "after", "para", "picture", "table"]);
    let Some(Block::Picture(pic)) = flow.blocks.iter().find(|b| matches!(b, Block::Picture(_))) else { panic!() };
    assert_eq!(pic.size, Some((100.0, 50.0)), "the size on the page, from EMUs");
    let Some(Block::Table(t)) = flow.blocks.last() else { panic!() };
    assert_eq!((t.widths.clone(), t.rows.len(), t.rows[1][0].span), (vec![2000.0, 6000.0], 2, 2));

    let (doc, report) = from_office("Lecture 3.docx", &docx()).unwrap();
    assert_eq!(report, OfficeReport::default(), "nothing lost");
    assert_eq!(count(&doc), 2, "the page break starts page 2");
}

const P: &str = r#"xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

fn rels(items: &[(&str, &str, &str)]) -> String {
    let mut s = String::from(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#);
    for (id, kind, target) in items {
        s.push_str(&format!(
            r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}"/>"#
        ));
    }
    s + "</Relationships>"
}

fn pptx(hidden_second: bool) -> Vec<u8> {
    let presentation = format!(
        r#"<p:presentation {P}><p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/></p:sldIdLst><p:sldSz cx="9144000" cy="5143500"/></p:presentation>"#
    );
    let slide1 = format!(
        r#"<p:sld {P}><p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:p><a:r><a:t>Indications of TDM</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:cNvPr id="3" name="Body"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="457200" y="1270000"/><a:ext cx="8229600" cy="3000000"/></a:xfrm></p:spPr><p:txBody><a:p><a:r><a:rPr b="1"/><a:t>Monitoring compliance</a:t></a:r></a:p><a:p><a:pPr lvl="1"/><a:r><a:t>Narrow therapeutic index</a:t></a:r></a:p><a:p><a:pPr><a:buNone/></a:pPr><a:r><a:t>No bullet</a:t></a:r></a:p></p:txBody></p:sp>
<p:grpSp><p:grpSpPr><a:xfrm><a:off x="1270000" y="1270000"/><a:ext cx="1270000" cy="1270000"/><a:chOff x="0" y="0"/><a:chExt cx="2540000" cy="2540000"/></a:xfrm></p:grpSpPr>
<p:pic><p:nvPicPr><p:cNvPr id="4" name="Pic"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId9"/></p:blipFill><p:spPr><a:xfrm><a:off x="1270000" y="0"/><a:ext cx="1270000" cy="1270000"/></a:xfrm></p:spPr></p:pic></p:grpSp>
<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="5" name="Chart"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="0" y="0"/><a:ext cx="100" cy="100"/></p:xfrm><a:graphic><a:graphicData uri="chart"/></a:graphic></p:graphicFrame>
</p:spTree></p:cSld></p:sld>"#
    );
    let show = if hidden_second { r#" show="0""# } else { "" };
    let slide2 = format!(
        r#"<p:sld {P}{show}><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="2" name="T"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="0" y="0"/><a:ext cx="6350000" cy="1270000"/></p:xfrm><a:graphic><a:graphicData><a:tbl><a:tblGrid><a:gridCol w="3175000"/><a:gridCol w="3175000"/></a:tblGrid><a:tr h="370840"><a:tc><a:txBody><a:p><a:r><a:t>Drug</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p><a:r><a:t>Level</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#
    );
    let layout = format!(
        r#"<p:sldLayout {P}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="T"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="635000" y="254000"/><a:ext cx="7620000" cy="889000"/></a:xfrm></p:spPr></p:sp></p:spTree></p:cSld></p:sldLayout>"#
    );
    let pres_rels = rels(&[("rId2", "slide", "slides/slide1.xml"), ("rId3", "slide", "slides/slide2.xml")]);
    let s1_rels = rels(&[("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"), ("rId9", "image", "../media/image1.png")]);
    let s2_rels = rels(&[("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml")]);
    let image = png();
    zip::build(&[
        ("ppt/presentation.xml", presentation.as_bytes()),
        ("ppt/_rels/presentation.xml.rels", pres_rels.as_bytes()),
        ("ppt/slides/slide1.xml", slide1.as_bytes()),
        ("ppt/slides/slide2.xml", slide2.as_bytes()),
        ("ppt/slides/_rels/slide1.xml.rels", s1_rels.as_bytes()),
        ("ppt/slides/_rels/slide2.xml.rels", s2_rels.as_bytes()),
        ("ppt/slideLayouts/slideLayout1.xml", layout.as_bytes()),
        ("ppt/media/image1.png", &image),
    ])
}

#[test]
fn slides_become_pages_with_their_boxes_in_place() {
    let deck = pptx::read(&pptx(false)).unwrap();
    assert_eq!(deck.size, (720.0, 405.0), "16:9 slides");
    assert_eq!(deck.slides.len(), 2);
    let s = &deck.slides[0];
    let Item::Text { rect, paras, size } = &s[0] else { panic!("{s:?}") };
    assert_eq!((*rect, *size), ([50.0, 20.0, 600.0, 70.0], 32.0), "the title's box comes from the layout");
    assert_eq!(paras[0].text(), "Indications of TDM");
    let Item::Text { rect, paras, .. } = &s[1] else { panic!() };
    assert_eq!(rect[..2], [36.0, 100.0]);
    assert_eq!((paras[0].bullet.as_deref(), paras[0].runs[0].bold), (Some("•"), true), "a body placeholder has bullets");
    assert_eq!((paras[1].level, paras[1].bullet.as_deref()), (1, Some("–")));
    assert_eq!(paras[2].bullet, None, "buNone");
    let Item::Picture { rect, .. } = &s[2] else { panic!() };
    assert_eq!(*rect, [150.0, 100.0, 50.0, 50.0], "a picture in a group, scaled by the group");
    assert_eq!(deck.skipped, 1, "the chart is counted as left out");
    assert!(matches!(&deck.slides[1][0], Item::Table { table, .. } if table.rows[0].len() == 2));

    assert_eq!(pptx::read(&pptx(true)).unwrap().slides.len(), 1, "hidden slides are left out");
    let (doc, report) = from_office("deck.pptx", &pptx(false)).unwrap();
    assert_eq!((count(&doc), report.skipped), (2, 1));
}

#[test]
fn rich_text_keeps_styles_code_pages_unicode_tables_and_pictures() {
    let pic: String = png().iter().map(|b| format!("{b:02x}")).collect();
    let rtf = format!(
        r"{{\rtf1\ansi\ansicpg1252\paperw11906\paperh16838{{\fonttbl{{\f0 Arial;}}}}{{\colortbl;\red0\green0\blue0;}}{{\*\generator Word;}}
\pard\qc\b\fs32 Title\b0\par
\pard Caf\'e9 \u945?-receptor \i italic\i0\line next\par
\trowd\cellx2000\cellx4000\pard\intbl A\cell B\cell\row
\pard After table\page Page two\par
{{\*\shppict{{\pict\pngblip\picwgoal1440\pichgoal720 {pic}}}}}{{\nonshppict{{\pict\wmetafile8 00ff}}}}
}}"
    );
    let flow = rtf::read(rtf.as_bytes());
    assert_eq!(flow.page, (11906.0 / 20.0, 16838.0 / 20.0));
    let p = paras(&flow);
    assert_eq!((p[0].text().as_str(), p[0].align, p[0].runs[0].bold, p[0].runs[0].size), ("Title", Align::Center, true, Some(16.0)));
    assert_eq!(p[1].text(), "Café α-receptor italic\nnext", "\\'e9 in 1252, \\u945 with its fallback skipped");
    assert!(p[1].runs.iter().any(|r| r.italic && r.text == "italic"));
    assert!(
        flow.blocks.iter().any(|b| matches!(b, Block::Table(t) if t.rows.len() == 1 && t.rows[0].len() == 2 && t.rows[0][1].paras[0].text() == "B"))
    );
    assert!(flow.blocks.iter().any(|b| matches!(b, Block::PageBreak)));
    let pictures: Vec<_> = flow.blocks.iter().filter_map(|b| if let Block::Picture(p) = b { Some(p) } else { None }).collect();
    assert_eq!(pictures.len(), 1, "the PNG, not also the metafile copy for old readers");
    assert_eq!((pictures[0].data.clone(), pictures[0].size), (png(), Some((72.0, 36.0))));
    assert!(from_office("notes.rtf", rtf.as_bytes()).is_ok());
}

#[test]
fn web_pages_keep_structure_and_drop_what_is_not_shown() {
    let pic: String = {
        let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        png()
            .chunks(3)
            .flat_map(|c| {
                let n = c.iter().enumerate().fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
                (0..4).map(move |i| if i <= c.len() { char::from(table[(n >> (18 - 6 * i) & 63) as usize]) } else { '=' })
            })
            .collect()
    };
    let html = format!(
        r#"<!DOCTYPE html><html><head><title>Ignored</title><style>p {{ color: red }}</style><script>alert("x < y")</script></head>
<body><h1 style="text-align: center">Renal   clearance</h1><!-- a comment -->
<p>CL = <b>k</b> &times; <i>Vd</i> &amp; more&nbsp;text<br>second line</p>
<ul><li>One</li><li>Two<ol><li>Nested</li></ol></li></ul>
<table><tr><th>Drug</th><td colspan="2">Value</td></tr><tr><td>A</td><td>B</td><td>C</td></tr></table>
<img src="data:image/png;base64,{pic}" width="40" height="20"><img src="https://example.com/x.png" alt="Logo">
<p>Unclosed <b>bold"#
    );
    let flow = html::read(&html);
    let p = paras(&flow);
    assert_eq!((p[0].heading, p[0].align, p[0].text().as_str()), (Some(1), Align::Center, "Renal clearance"));
    assert_eq!(p[1].text(), "CL = k × Vd & more\u{a0}text\nsecond line");
    assert!(p[1].runs.iter().any(|r| r.bold && r.text == "k"));
    let bullets: Vec<_> = p.iter().filter_map(|p| p.bullet.clone().map(|b| (b, p.level, p.text()))).collect();
    assert_eq!(bullets, [("•".into(), 0, "One".into()), ("•".into(), 0, "Two".into()), ("1.".into(), 1, "Nested".into())]);
    let Some(Block::Table(t)) = flow.blocks.iter().find(|b| matches!(b, Block::Table(_))) else { panic!() };
    assert_eq!((t.rows.len(), t.rows[0][1].span, t.rows[0][0].paras[0].runs[0].bold), (2, 2, true));
    assert!(flow.blocks.iter().any(|b| matches!(b, Block::Picture(p) if p.data == png() && p.size == Some((30.0, 15.0)))));
    assert_eq!(flow.skipped, 1, "the linked picture is not fetched");
    assert!(p.iter().any(|p| p.text().trim() == "[Logo]"));
    assert!(p.last().is_some_and(|p| p.text().trim() == "Unclosed bold"));
    assert!(!p.iter().any(|p| p.text().contains("Ignored") || p.text().contains("alert") || p.text().contains("color")));
}

#[test]
fn the_text_is_drawn_in_an_embedded_font_and_missing_glyphs_are_reported() {
    let (doc, report) = from_office("page.html", "<p>Clearance α β Ω → ≤ µg</p><p>دواء</p>".as_bytes()).unwrap();
    assert_eq!(report.replaced_chars, 4, "the four Arabic letters");
    assert!(report.message().is_some_and(|m| m.contains("4 character")));
    assert_eq!(count(&doc), 1);
}

#[test]
fn long_documents_flow_onto_more_pages() {
    let html: String = (0..400).map(|i| format!("<p>Paragraph {i} about drug levels in plasma.</p>")).collect();
    let (doc, _) = from_office("long.htm", html.as_bytes()).unwrap();
    assert!(count(&doc) > 5, "{}", count(&doc));
    let word = "x".repeat(5_000);
    let (doc, _) = from_office("word.html", format!("<p>{word}</p>").as_bytes()).unwrap();
    assert!(count(&doc) >= 2, "a word wider than the line is broken, not lost");
}

#[test]
fn symbol_font_bullets_become_the_shapes_they_show() {
    assert_eq!(bullet_char('Ø', Some("Wingdings")), "►");
    assert_eq!(bullet_char('§', Some("Wingdings")), "▪");
    assert_eq!(bullet_char('\u{f0a7}', None), "▪", "a Wingdings private-use code");
    assert_eq!(bullet_char('\u{f0b7}', Some("Symbol")), "•");
    assert_eq!(bullet_char('-', Some("Calibri")), "-");
    assert_eq!(bullet_char('\u{e123}', None), "•");
}

#[test]
fn damaged_files_are_refused_not_crashed_on() {
    for (name, bytes) in [("a.docx", docx()), ("a.pptx", pptx(false))] {
        for cut in (0..bytes.len()).step_by(37) {
            let _ = from_office(name, bytes.get(..cut).unwrap_or_default());
        }
        let mut flipped = bytes.clone();
        for i in (0..flipped.len()).step_by(11) {
            flipped[i] ^= 0x55;
        }
        let _ = from_office(name, &flipped);
    }
    for junk in ["{\\rtf1 {{{{{{\\u-5\\'zz\\bin99999999 }", "{\\rtf1\\pict\\pngblip zz}"] {
        let _ = from_office("x.rtf", junk.as_bytes());
    }
    let _ = from_office("x.html", "<table><td><table><tr><td><<<>>>&#xFFFFFFFF;&#0;<img src='data:image/png;base64,!!'>".as_bytes());
    assert!(from_office("x.docx", b"PK\x03\x04 not really").is_err());
    assert!(!is_office("x.docx", b"plain text"), "a .docx must be a ZIP");
    assert!(is_office("x.HTM", b"anything"));
}
