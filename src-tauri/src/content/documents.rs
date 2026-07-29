// Plain text out of the document formats a chat attachment can be, so a model
// can read a spreadsheet or a PDF the same way it reads a .py file.
//
// The target is not fidelity, it is a prompt. Layout, fonts, colours and cell
// formatting are all deliberately thrown away — what matters is that the words
// and numbers arrive in reading order, that a table still looks like a table,
// and that a 40 MB file cannot blow up the request. Anything this cannot read
// says so rather than returning empty text, because a silent empty attachment
// looks to the user exactly like a model that ignored their file.
//
// Every extractor takes a character budget and reports the full length, for the
// same reason ai/chat.rs's text path does: truncation the user cannot see is
// worse than truncation they can.
use std::io::Read;
use std::path::Path;

/// What came out of a file, and how much of it there was.
#[derive(Debug)]
pub struct Extracted {
    pub text: String,
    /// Characters before the budget was applied — larger than `text` when it
    /// was cut short.
    pub full_chars: usize,
}

impl Extracted {
    fn new(text: String, max_chars: usize) -> Self {
        let full_chars = text.chars().count();
        if full_chars <= max_chars {
            return Self { text, full_chars };
        }
        Self {
            text: text.chars().take(max_chars).collect(),
            full_chars,
        }
    }
}

/// Extension -> fenced-code language token, for everything read as text.
///
/// Doubles as the list of what counts as text at all: an extension in here is
/// readable, one that isn't goes to the document parsers or comes back
/// unsupported. One table rather than two so a newly added extension can never
/// be readable but unhighlightable, or the reverse.
///
/// The token on the right is a highlighter language name, not the extension —
/// "py" is a file extension, "python" is a language, and the two differ often
/// enough (`rs`/`rust`, `cs`/`csharp`, `yml`/`yaml`) that mapping them is the
/// whole point of the table.
pub const TEXT_EXTS: &[(&str, &str)] = &[
    // plain
    ("txt", ""),
    ("log", ""),
    ("text", ""),
    ("md", "markdown"),
    ("markdown", "markdown"),
    ("rst", ""),
    ("tex", "latex"),
    // data / config
    ("json", "json"),
    ("jsonl", "json"),
    ("ndjson", "json"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("toml", "toml"),
    ("ini", "ini"),
    ("cfg", "ini"),
    ("conf", "ini"),
    ("properties", "ini"),
    ("env", "bash"),
    ("xml", "xml"),
    ("svg", "xml"),
    // No highlighter language: a CSV has no syntax to colour, and naming one
    // that the bundled highlighter does not know would just fail silently.
    ("csv", ""),
    ("tsv", ""),
    ("sql", "sql"),
    ("graphql", "graphql"),
    ("gql", "graphql"),
    ("proto", "protobuf"),
    ("diff", "diff"),
    ("patch", "diff"),
    // web
    ("html", "html"),
    ("htm", "html"),
    ("css", "css"),
    ("scss", "scss"),
    ("sass", "scss"),
    ("less", "less"),
    ("vue", "html"),
    ("svelte", "html"),
    // scripting
    ("js", "javascript"),
    ("mjs", "javascript"),
    ("cjs", "javascript"),
    ("jsx", "javascript"),
    ("ts", "typescript"),
    ("tsx", "typescript"),
    ("py", "python"),
    ("pyw", "python"),
    ("rb", "ruby"),
    ("php", "php"),
    ("pl", "perl"),
    ("lua", "lua"),
    ("r", "r"),
    ("jl", "julia"),
    ("dart", "dart"),
    // shells
    ("sh", "bash"),
    ("bash", "bash"),
    ("zsh", "bash"),
    ("fish", "bash"),
    ("ps1", "powershell"),
    ("psm1", "powershell"),
    ("bat", "dos"),
    ("cmd", "dos"),
    // compiled
    ("rs", "rust"),
    ("go", "go"),
    ("c", "c"),
    ("h", "c"),
    ("cpp", "cpp"),
    ("cc", "cpp"),
    ("cxx", "cpp"),
    ("hpp", "cpp"),
    ("hh", "cpp"),
    ("hxx", "cpp"),
    ("cs", "csharp"),
    ("fs", "fsharp"),
    ("vb", "vbnet"),
    ("java", "java"),
    ("kt", "kotlin"),
    ("kts", "kotlin"),
    ("scala", "scala"),
    ("swift", "swift"),
    ("m", "objectivec"),
    ("mm", "objectivec"),
    ("zig", "zig"),
    ("nim", "nim"),
    ("hs", "haskell"),
    ("ex", "elixir"),
    ("exs", "elixir"),
    ("erl", "erlang"),
    ("clj", "clojure"),
    ("groovy", "groovy"),
    ("gradle", "groovy"),
    ("asm", "x86asm"),
    ("s", "x86asm"),
    ("sol", "solidity"),
    ("tf", "hcl"),
    ("hcl", "hcl"),
];

/// Files that carry their type in the whole name instead of an extension.
/// Matched case-insensitively against the full filename.
const TEXT_FILENAMES: &[(&str, &str)] = &[
    ("dockerfile", "dockerfile"),
    ("makefile", "makefile"),
    ("gnumakefile", "makefile"),
    ("cmakelists.txt", "cmake"),
    ("gemfile", "ruby"),
    ("rakefile", "ruby"),
    ("procfile", ""),
    (".gitignore", ""),
    (".gitattributes", ""),
    (".editorconfig", "ini"),
    (".env", "bash"),
];

/// The language token for a file read as text, or None when it isn't one.
pub fn text_language(name: &str, ext: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if let Some((_, lang)) = TEXT_FILENAMES.iter().find(|(n, _)| *n == lower) {
        return Some(lang);
    }
    TEXT_EXTS.iter().find(|(e, _)| *e == ext).map(|(_, lang)| *lang)
}

/// The formats handled here. Kept as one list so the caller does not have to
/// know which extension goes to which extractor.
pub const DOCUMENT_EXTS: &[&str] = &["xlsx", "xlsm", "xlsb", "xls", "pdf", "docx", "pptx", "zip"];

pub fn is_document(ext: &str) -> bool {
    DOCUMENT_EXTS.contains(&ext)
}

/// Extracts text from a document, or explains why it could not.
///
/// The error is a message meant for the user: these formats fail for reasons
/// that are worth telling them about (a scanned PDF has no text in it at all,
/// an .xls saved by something ancient may not parse) and "unsupported" would be
/// a lie in those cases.
pub fn extract(path: &Path, ext: &str, max_chars: usize) -> Result<Extracted, String> {
    match ext {
        "xlsx" | "xlsm" | "xlsb" | "xls" => extract_spreadsheet(path, max_chars),
        "pdf" => extract_pdf(path, max_chars),
        "docx" => extract_docx(path, max_chars),
        "pptx" => extract_pptx(path, max_chars),
        "zip" => extract_zip(path, max_chars),
        _ => Err(format!("No reader for .{ext} files.")),
    }
}

// --- spreadsheets -------------------------------------------------------------

/// Every sheet, as tab-separated rows under a heading per sheet.
///
/// Tab-separated rather than CSV-quoted: the reader is a language model, and
/// tabs keep columns visually aligned in the prompt without the quoting rules
/// a CSV writer would have to apply to every cell containing a comma.
fn extract_spreadsheet(path: &Path, max_chars: usize) -> Result<Extracted, String> {
    use calamine::{open_workbook_auto, Reader};

    let mut workbook = open_workbook_auto(path).map_err(|e| format!("Couldn't open the workbook: {e}"))?;
    let sheet_names = workbook.sheet_names().to_vec();
    if sheet_names.is_empty() {
        return Err("The workbook has no sheets.".into());
    }

    let mut out = String::new();
    for name in &sheet_names {
        let Ok(range) = workbook.worksheet_range(name) else { continue };
        if range.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        // Named even when there is only one sheet: "Sheet1" tells the model
        // nothing, but a sheet actually called "2026 Budget" tells it a lot.
        out.push_str(&format!("# {name}\n"));
        for row in range.rows() {
            let cells: Vec<String> = row.iter().map(cell_to_string).collect();
            // Trailing empty cells are an artefact of the used-range being a
            // rectangle; a row of nothing but those is not a row.
            let last = cells.iter().rposition(|c| !c.is_empty());
            match last {
                Some(last) => {
                    out.push_str(&cells[..=last].join("\t"));
                    out.push('\n');
                }
                None => continue,
            }
        }
        // Stop reading once the budget is spent — a workbook can be enormous,
        // and there is no point formatting sheets that will be thrown away.
        if out.chars().count() > max_chars {
            break;
        }
    }

    if out.trim().is_empty() {
        return Err("Every sheet in the workbook is empty.".into());
    }
    Ok(Extracted::new(out, max_chars))
}

fn cell_to_string(cell: &calamine::Data) -> String {
    use calamine::Data;
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.trim().to_string(),
        Data::Float(f) => {
            // Whole numbers come back as floats, and "2026" reads better than
            // "2026.0" — especially as a year or an id, where the model may
            // otherwise treat the decimal as significant.
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{}", *f as i64)
            } else {
                f.to_string()
            }
        }
        Data::Int(i) => i.to_string(),
        // A formula cell yields its last cached result, which is what Excel
        // stores alongside the formula. A file written by a library that saves
        // formulas without cached values (openpyxl does) therefore reads as
        // empty here — correct, since there is genuinely no value in the file.
        Data::Bool(b) => b.to_string(),
        Data::DateTime(d) => d.to_string(),
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("#{e:?}"),
    }
}

// --- pdf ----------------------------------------------------------------------

/// Note on what does not come through: a PDF has no notion of a table, only of
/// glyphs at coordinates, so a table arrives as its cells in reading order and
/// nothing here can recover the columns. Text drawn as an image does not arrive
/// at all — that is the empty-result case below.
fn extract_pdf(path: &Path, max_chars: usize) -> Result<Extracted, String> {
    let text = pdf_extract::extract_text(path).map_err(|e| format!("Couldn't read the PDF: {e}"))?;
    // Trimmed at the ends as well as internally: a real PDF's text layer
    // measured here opened with a blank line, which then led the attachment.
    let cleaned = tidy_whitespace(&text).trim().to_string();
    if cleaned.is_empty() {
        // The single most common PDF failure, and one the user can act on:
        // there is nothing to extract because the pages are images.
        return Err("This PDF has no text layer — it is probably scanned images, which would need OCR.".into());
    }
    Ok(Extracted::new(cleaned, max_chars))
}

// --- Office XML containers ----------------------------------------------------
//
// docx and pptx are both a zip of XML. The text lives in one element type in a
// known part, so there is nothing here a dependency would do better:
//
//   docx: word/document.xml         text in <w:t>, paragraphs end at </w:p>
//   pptx: ppt/slides/slideN.xml     text in <a:t>, paragraphs end at </a:p>
//
// What this deliberately does not read: headers, footers, footnotes, comments,
// tracked changes, speaker notes, and text inside embedded objects or SmartArt.
// Those live in other parts of the container and are not the document's body.

fn extract_docx(path: &Path, max_chars: usize) -> Result<Extracted, String> {
    let mut archive = open_zip(path)?;
    let xml = read_part(&mut archive, "word/document.xml")
        .ok_or("This does not look like a .docx — word/document.xml is missing.")?;
    let text = text_from_office_xml(&xml, b"t", b"p");
    if text.trim().is_empty() {
        return Err("The document has no text in it.".into());
    }
    Ok(Extracted::new(text, max_chars))
}

fn extract_pptx(path: &Path, max_chars: usize) -> Result<Extracted, String> {
    let mut archive = open_zip(path)?;

    // Slides are numbered, and zip entry order is not slide order — slide10
    // sorts before slide2 as a string. Read the numbers and sort on them, or a
    // deck comes out shuffled.
    let mut slides: Vec<(u32, String)> = archive
        .file_names()
        .filter_map(|name| {
            let rest = name.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?;
            Some((rest.parse::<u32>().ok()?, name.to_string()))
        })
        .collect();
    if slides.is_empty() {
        return Err("This does not look like a .pptx — it has no slides in it.".into());
    }
    slides.sort_by_key(|(number, _)| *number);

    let mut out = String::new();
    for (number, name) in slides {
        let Some(xml) = read_part(&mut archive, &name) else { continue };
        let text = text_from_office_xml(&xml, b"t", b"p");
        if text.trim().is_empty() {
            continue;
        }
        // Numbered because "what does slide 4 say" is a question people ask,
        // and without the marker the deck arrives as one undifferentiated wall.
        out.push_str(&format!("# Slide {number}\n{}\n\n", text.trim()));
        if out.chars().count() > max_chars {
            break;
        }
    }

    if out.trim().is_empty() {
        return Err("Every slide in the deck is empty, or its text sits in images.".into());
    }
    Ok(Extracted::new(out, max_chars))
}

// --- zip ----------------------------------------------------------------------

/// Reads the text files out of a zip, each under a heading naming its path.
///
/// "Extract" here means into the prompt, not onto disk. Nothing is written
/// anywhere: unpacking an archive from a chat box would mean deciding where the
/// files go and what to do about paths inside it that point outside it (the zip
/// traversal problem), and neither is something a chat attachment should be
/// deciding. What a model can use is the contents, and that is what it gets.
///
/// Binary entries are named and skipped rather than mangled — knowing a zip
/// contains a 40 MB .dll is useful, seeing its bytes as replacement characters
/// is not.
fn extract_zip(path: &Path, max_chars: usize) -> Result<Extracted, String> {
    /// Guards against a zip of ten thousand tiny files: past this many text
    /// entries the budget is long gone anyway, and the loop stops being cheap.
    const MAX_ENTRIES: usize = 200;

    let mut archive = open_zip(path)?;
    // Sorted by path so the same archive always reads the same way — zip entry
    // order is whatever the writer chose.
    let mut names: Vec<String> = archive.file_names().map(str::to_string).collect();
    names.sort();
    if names.is_empty() {
        return Err("The archive is empty.".into());
    }

    let mut out = String::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut read = 0usize;
    for raw_name in &names {
        // The zip spec says entry paths use forward slashes, but PowerShell's
        // Compress-Archive writes backslashes — measured on a real archive, where
        // "src\main.py" arrived intact. Both separators are handled rather than
        // trusting the spec, and the displayed path is normalised so the same
        // project reads the same however it was zipped.
        let name = raw_name.replace('\\', "/");
        // Directory entries carry no content.
        if name.ends_with('/') {
            continue;
        }
        // Names inside a zip are paths, so the type comes from the last segment.
        let leaf = name.rsplit('/').next().unwrap_or(&name);
        let ext = leaf
            .rsplit_once('.')
            .map(|(_, e)| e.to_lowercase())
            .unwrap_or_default();
        let Some(lang) = text_language(leaf, &ext) else {
            skipped.push(name);
            continue;
        };
        if read >= MAX_ENTRIES || out.chars().count() > max_chars {
            skipped.push(name);
            continue;
        }
        // Read by the ORIGINAL name: the normalisation above is for display, and
        // the archive only knows the bytes it was written with.
        let Some(body) = read_part(&mut archive, raw_name) else {
            skipped.push(name);
            continue;
        };
        read += 1;
        if body.trim().is_empty() {
            continue;
        }
        // Fenced with the language, and headed with the path inside the archive
        // — the path is what makes a file in a project meaningful, and a fence
        // keeps one file's contents from running into the next one's.
        out.push_str(&format!("## {name}\n```{lang}\n{}\n```\n\n", body.trim_end()));
    }

    if out.trim().is_empty() {
        return Err(format!(
            "Nothing readable in the archive — its {} entr{} are all binary or empty.",
            names.len(),
            if names.len() == 1 { "y" } else { "ies" }
        ));
    }
    if !skipped.is_empty() {
        // Listed, not silently dropped: "why did it not mention the .dll" has an
        // answer the user can see.
        out.push_str("## Not read (binary, empty, or past the limit)\n");
        for name in skipped.iter().take(40) {
            out.push_str(&format!("- {name}\n"));
        }
        if skipped.len() > 40 {
            out.push_str(&format!("- …and {} more\n", skipped.len() - 40));
        }
    }
    Ok(Extracted::new(out, max_chars))
}

fn open_zip(path: &Path) -> Result<zip::ZipArchive<std::fs::File>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Couldn't open the file: {e}"))?;
    zip::ZipArchive::new(file).map_err(|e| format!("Couldn't read the file as an Office document: {e}"))
}

fn read_part(archive: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
    let mut entry = archive.by_name(name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    // Lossy: these parts are declared UTF-8 and virtually always are, and a
    // single bad byte should cost one character rather than the whole document.
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Collects the text of every `<*:text_tag>` element, breaking a line at the
/// end of every `<*:para_tag>`.
///
/// Namespace prefixes are ignored on purpose: the tags are `w:t`/`w:p` in Word
/// and `a:t`/`a:p` in PowerPoint, but a producer is free to bind those
/// namespaces to any prefix it likes, so matching the local name is what is
/// actually correct rather than what happens to work on files from Office.
fn text_from_office_xml(xml: &str, text_tag: &[u8], para_tag: &[u8]) -> String {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let local = |name: &[u8]| -> Vec<u8> {
        match name.iter().rposition(|b| *b == b':') {
            Some(colon) => name[colon + 1..].to_vec(),
            None => name.to_vec(),
        }
    };

    let mut reader = Reader::from_str(xml);
    let mut out = String::new();
    let mut line = String::new();
    let mut in_text = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if local(e.name().as_ref()) == text_tag {
                    in_text = true;
                }
            }
            Ok(Event::End(e)) => {
                let name = local(e.name().as_ref());
                if name == text_tag {
                    in_text = false;
                } else if name == para_tag {
                    // Paragraphs are the only structure worth keeping. Empty
                    // ones collapse rather than producing runs of blank lines.
                    if !line.trim().is_empty() {
                        out.push_str(line.trim_end());
                        out.push('\n');
                    }
                    line.clear();
                }
            }
            Ok(Event::Text(t)) if in_text => match t.decode() {
                Ok(decoded) => line.push_str(&decoded),
                Err(_) => line.push_str(&String::from_utf8_lossy(&t)),
            },
            // quick-xml reports an entity reference as its own event rather than
            // as part of the surrounding Text, so a catch-all arm here silently
            // deletes it: "Ali &amp; Veli" arrived as "Ali  Veli". Word writes
            // these constantly — every literal &, <, > in a document is one.
            Ok(Event::GeneralRef(r)) if in_text => {
                if let Ok(Some(ch)) = r.resolve_char_ref() {
                    // A numeric reference: &#231; or &#xE7;
                    line.push(ch);
                } else {
                    // A named one. XML predefines exactly these five; anything
                    // else would have to come from a DTD, which these files do
                    // not carry, so it is passed through as written rather than
                    // guessed at or dropped.
                    match &*r as &[u8] {
                        b"amp" => line.push('&'),
                        b"lt" => line.push('<'),
                        b"gt" => line.push('>'),
                        b"quot" => line.push('"'),
                        b"apos" => line.push('\''),
                        other => {
                            line.push('&');
                            line.push_str(&String::from_utf8_lossy(other));
                            line.push(';');
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            // A malformed part yields whatever was read before the break rather
            // than nothing: a partly-readable document is still useful.
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    if !line.trim().is_empty() {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Collapses the whitespace a PDF text layer arrives with.
///
/// Extractors emit a lot of incidental spacing — trailing spaces from glyph
/// positioning, and runs of blank lines where a page ended. Left in, they cost
/// tokens and tell the model nothing.
fn tidy_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            blank_run += 1;
            // One blank line is a paragraph break; more is noise.
            if blank_run > 1 {
                continue;
            }
            out.push('\n');
            continue;
        }
        blank_run = 0;
        out.push_str(trimmed);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_text_is_read_in_paragraphs() {
        let xml = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>First line</w:t></w:r></w:p>
    <w:p><w:r><w:t>Second </w:t></w:r><w:r><w:t>line</w:t></w:r></w:p>
    <w:p/>
    <w:p><w:r><w:t>Third</w:t></w:r></w:p>
  </w:body>
</w:document>"#;
        // Runs are joined inside a paragraph — Word splits a sentence across
        // runs whenever formatting changes mid-sentence, so joining is the
        // difference between "Second line" and "Second" / "line".
        assert_eq!(text_from_office_xml(xml, b"t", b"p"), "First line\nSecond line\nThird\n");
    }

    #[test]
    fn a_producers_namespace_prefix_does_not_matter() {
        // The prefix is arbitrary; only the local name is fixed by the format.
        let xml = r#"<doc xmlns:foo="x"><foo:p><foo:r><foo:t>Hello</foo:t></foo:r></foo:p></doc>"#;
        assert_eq!(text_from_office_xml(xml, b"t", b"p"), "Hello\n");
    }

    #[test]
    fn xml_entities_come_back_as_characters() {
        let xml = r#"<d><p><t>Ali &amp; Veli &lt;3</t></p></d>"#;
        assert_eq!(text_from_office_xml(xml, b"t", b"p"), "Ali & Veli <3\n");
    }

    #[test]
    fn text_outside_a_text_element_is_ignored() {
        // Word puts plenty of non-text content in document.xml (style ids,
        // revision markers). Scraping every Text event would drag it all in.
        let xml = r#"<d><p><rPr><sz>28</sz></rPr><t>Real text</t></p></d>"#;
        assert_eq!(text_from_office_xml(xml, b"t", b"p"), "Real text\n");
    }

    #[test]
    fn turkish_characters_survive() {
        let xml = r#"<d><p><t>Ücret tarifesi değişti — ığüşöç İĞÜŞÖÇ</t></p></d>"#;
        assert_eq!(
            text_from_office_xml(xml, b"t", b"p"),
            "Ücret tarifesi değişti — ığüşöç İĞÜŞÖÇ\n"
        );
    }

    #[test]
    fn a_truncated_part_yields_what_it_had() {
        // A malformed document should give up its readable prefix rather than
        // returning nothing at all.
        let xml = r#"<d><p><t>Kept</t></p><p><t>Also kept</t></p><p><t>unclosed"#;
        let out = text_from_office_xml(xml, b"t", b"p");
        assert!(out.starts_with("Kept\nAlso kept"), "got {out:?}");
    }

    #[test]
    fn the_budget_counts_characters_and_records_the_original_length() {
        let extracted = Extracted::new("ığüşöç".repeat(10), 7);
        assert_eq!(extracted.text.chars().count(), 7);
        assert_eq!(extracted.full_chars, 60);
    }

    #[test]
    fn whole_numbers_do_not_arrive_with_a_decimal_point() {
        use calamine::Data;
        // A year read as 2026.0 invites the model to treat the decimal as data.
        assert_eq!(cell_to_string(&Data::Float(2026.0)), "2026");
        assert_eq!(cell_to_string(&Data::Float(19.5)), "19.5");
        assert_eq!(cell_to_string(&Data::Empty), "");
        assert_eq!(cell_to_string(&Data::String("  padded  ".into())), "padded");
    }

    #[test]
    fn blank_line_runs_collapse_to_one() {
        // One blank line is a paragraph break and is kept; a run of them is the
        // page break a PDF extractor emits, and costs tokens for nothing.
        assert_eq!(tidy_whitespace("a\n\n\n\n\nb\n"), "a\n\nb\n");
        assert_eq!(tidy_whitespace("a\n\nb\n"), "a\n\nb\n");
        assert_eq!(tidy_whitespace("a\nb\n"), "a\nb\n");
        assert_eq!(tidy_whitespace("trailing   \nspace\n"), "trailing\nspace\n");
    }

    // --- real containers ------------------------------------------------------
    //
    // The tests above check the XML reader on strings. These build actual zip
    // files and go through the real entry point, which is what catches the
    // container-level mistakes: a wrong part name, and slide ordering.

    fn write_zip(path: &Path, parts: &[(&str, &str)]) {
        use std::io::Write;
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (name, body) in parts {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("widget-doc-test-{tag}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn para(tag: &str, text: &str) -> String {
        format!("<{tag}:p><{tag}:r><{tag}:t>{text}</{tag}:t></{tag}:r></{tag}:p>")
    }

    #[test]
    fn a_real_docx_is_read_through_the_zip() {
        let dir = temp_dir("docx");
        let path = dir.join("report.docx");
        let body = format!(
            "<w:document xmlns:w=\"x\"><w:body>{}{}</w:body></w:document>",
            para("w", "Ücret tarifesi"),
            para("w", "Ali &amp; Veli")
        );
        write_zip(&path, &[("[Content_Types].xml", "<Types/>"), ("word/document.xml", &body)]);

        let out = extract(&path, "docx", 10_000).unwrap();
        assert_eq!(out.text, "Ücret tarifesi\nAli & Veli\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_zip_that_is_not_a_docx_says_so() {
        let dir = temp_dir("notdocx");
        let path = dir.join("wrong.docx");
        write_zip(&path, &[("hello.txt", "not a document")]);

        let err = extract(&path, "docx", 10_000).unwrap_err();
        assert!(err.contains("document.xml"), "unhelpful error: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn slides_come_out_in_slide_order_not_alphabetical() {
        // The bug this exists for: zip entries are strings, and "slide10" sorts
        // before "slide2", so a ten-slide deck arrives with slide 10 second.
        let dir = temp_dir("pptx");
        let path = dir.join("deck.pptx");
        let slide = |text: &str| format!("<p:sld xmlns:a=\"x\"><p:cSld>{}</p:cSld></p:sld>", para("a", text));
        write_zip(
            &path,
            &[
                ("ppt/slides/slide1.xml", &slide("first")),
                ("ppt/slides/slide10.xml", &slide("tenth")),
                ("ppt/slides/slide2.xml", &slide("second")),
            ],
        );

        let out = extract(&path, "pptx", 10_000).unwrap();
        let order: Vec<&str> = out.text.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).collect();
        assert_eq!(order, vec!["first", "second", "tenth"], "got {:?}", out.text);
        // Slide numbers are kept, because "what does slide 2 say" is a real
        // question and the numbering has to be the deck's, not our index.
        assert!(out.text.contains("# Slide 10"), "{}", out.text);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_deck_whose_text_is_all_images_explains_itself() {
        let dir = temp_dir("emptypptx");
        let path = dir.join("pictures.pptx");
        write_zip(&path, &[("ppt/slides/slide1.xml", "<p:sld><p:cSld/></p:sld>")]);

        let err = extract(&path, "pptx", 10_000).unwrap_err();
        // Not "unsupported": the format was read fine, the deck just has no
        // text, and that is something the user can do something about.
        assert!(err.contains("images"), "unhelpful error: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_zip_yields_its_text_files_with_their_paths() {
        let dir = temp_dir("zip");
        let path = dir.join("project.zip");
        write_zip(
            &path,
            &[
                ("src/main.rs", "fn main() { println!(\"hi\"); }"),
                ("README.md", "# Project\nNotes here."),
                ("assets/logo.png", "\u{0}\u{1}binary\u{2}"),
                ("build/app.dll", "MZ\u{0}binary"),
                ("empty.txt", "   "),
            ],
        );

        let out = extract(&path, "zip", 10_000).unwrap();
        // Paths, because a file called main.rs means nothing without knowing it
        // is src/main.rs.
        assert!(out.text.contains("## README.md"), "{}", out.text);
        assert!(out.text.contains("## src/main.rs"), "{}", out.text);
        // Fenced with the language, so the block highlights when shown back.
        assert!(out.text.contains("```rust"), "{}", out.text);
        assert!(out.text.contains("```markdown"), "{}", out.text);
        // Binary entries are named but not inlined.
        assert!(!out.text.contains("MZ"), "a binary file's bytes got inlined");
        assert!(out.text.contains("- build/app.dll"), "{}", out.text);
        assert!(out.text.contains("- assets/logo.png"), "{}", out.text);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn zip_entries_are_read_in_a_stable_order() {
        // Entry order in a zip is whatever the writer chose, so the same archive
        // has to be sorted here or two reads of it disagree.
        let dir = temp_dir("ziporder");
        let path = dir.join("order.zip");
        write_zip(&path, &[("z.txt", "last"), ("a.txt", "first"), ("m.txt", "middle")]);

        let out = extract(&path, "zip", 10_000).unwrap();
        let order: Vec<&str> = out.text.lines().filter(|l| l.starts_with("## ")).collect();
        assert_eq!(order, vec!["## a.txt", "## m.txt", "## z.txt"], "{}", out.text);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_backslash_separated_archive_reads_the_same() {
        // PowerShell's Compress-Archive writes entry paths with backslashes,
        // against the zip spec. Measured on a real archive it produced: the
        // heading came out as "src\main.py". Handled rather than trusted,
        // because the entry still has to be READ by its original name.
        let dir = temp_dir("zipbackslash");
        let path = dir.join("windows.zip");
        write_zip(&path, &[("src\\main.py", "print(1)"), ("docs\\", "")]);

        let out = extract(&path, "zip", 10_000).unwrap();
        assert!(out.text.contains("## src/main.py"), "path not normalised: {}", out.text);
        assert!(out.text.contains("print(1)"), "content not read: {}", out.text);
        // A directory entry written with a backslash must not be treated as a
        // file with no extension.
        assert!(!out.text.contains("## docs"), "{}", out.text);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_archive_of_only_binaries_explains_itself() {
        let dir = temp_dir("zipbinary");
        let path = dir.join("drivers.zip");
        write_zip(&path, &[("a.dll", "MZ\u{0}"), ("b.sys", "\u{0}\u{0}")]);

        let err = extract(&path, "zip", 10_000).unwrap_err();
        assert!(err.contains("binary"), "unhelpful error: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_that_is_not_a_zip_at_all_fails_cleanly() {
        let dir = temp_dir("garbage");
        let path = dir.join("truncated.docx");
        std::fs::write(&path, b"PK\x03\x04 and then nothing useful").unwrap();

        assert!(extract(&path, "docx", 10_000).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn document_extensions_are_not_also_text_extensions() {
        // The two lists route a file to different readers; an extension in both
        // would be read by whichever check the caller happens to run first.
        for ext in DOCUMENT_EXTS {
            assert!(
                text_language("f", ext).is_none(),
                "{ext} is claimed by both the text and document paths"
            );
        }
    }
    #[test]
    fn every_extension_maps_to_one_language() {
        // Two rows for the same extension means whichever comes first silently
        // wins, and the table is long enough that a duplicate is easy to add.
        let mut seen = std::collections::HashSet::new();
        for (ext, _) in TEXT_EXTS {
            assert!(seen.insert(*ext), "{ext} is listed twice");
            assert!(!ext.starts_with('.'), "{ext} should be a bare extension");
            assert_eq!(*ext, ext.to_lowercase(), "{ext} must be lowercase to match");
        }
        for (name, _) in TEXT_FILENAMES {
            assert_eq!(*name, name.to_lowercase(), "{name} must be lowercase to match");
        }
    }

    #[test]
    fn the_language_is_the_language_not_the_extension() {
        // The whole reason the table exists: a highlighter wants "python", not
        // "py". Getting this wrong is invisible until code blocks render plain.
        assert_eq!(text_language("a.py", "py"), Some("python"));
        assert_eq!(text_language("a.rs", "rs"), Some("rust"));
        assert_eq!(text_language("a.cs", "cs"), Some("csharp"));
        assert_eq!(text_language("a.yml", "yml"), Some("yaml"));
        assert_eq!(text_language("a.cpp", "cpp"), Some("cpp"));
    }

    #[test]
    fn extensionless_files_are_recognised_by_name() {
        assert_eq!(text_language("Dockerfile", ""), Some("dockerfile"));
        assert_eq!(text_language("makefile", ""), Some("makefile"));
        // Case as it appears on disk must not matter.
        assert_eq!(text_language("MAKEFILE", ""), Some("makefile"));
    }

    #[test]
    fn a_binary_format_is_not_text() {
        assert_eq!(text_language("book.pdf", "pdf"), None);
        assert_eq!(text_language("sheet.xlsx", "xlsx"), None);
        assert_eq!(text_language("deck.pptx", "pptx"), None);
    }

}

