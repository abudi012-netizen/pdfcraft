//! Define and Translate on selected text: offline dictionaries, no AI and no network.
//!
//! The data is two sorted tab-separated files built by `.github/scripts/build_dictionaries.py`
//! from Open English WordNet, Wiktionary (kaikki.org) and MeSH (U.S. National Library of
//! Medicine), shipped beside the app in a `dictionaries` folder:
//!
//! - `define.tsv`: `key \t term \t source \t part of speech \t definition`
//! - `translate.tsv`: `key \t language code \t part of speech \t translations \t sense`
//!
//! `key` is the term folded by [`fold`]. Lookups binary-search the rows, so a lookup is
//! instant once the files are loaded (about a second, once per run).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The languages Translate offers (ISO 639-1 code, English name).
pub const LANGUAGES: &[(&str, &str)] = &[("ar", "Arabic"), ("de", "German"), ("fr", "French"), ("es", "Spanish")];

/// The sources, in the order their definitions are shown: medical meaning first.
const SOURCES: [&str; 3] = ["MeSH", "WordNet", "Wiktionary"];
/// Most definitions shown for one term.
const MAX_DEFINITIONS: usize = 8;
/// Longest selection looked up (characters); longer is cut.
const MAX_QUERY: usize = 80;
/// Most words looked up one by one when a phrase isn't in the dictionaries.
const MAX_WORDS: usize = 8;
/// Words skipped when a phrase is looked up word by word.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "of", "to", "in", "on", "at", "by", "for", "with", "and", "or", "as", "it", "its",
    "this", "that", "these", "those", "from", "into", "than", "then", "which", "who", "whom", "not", "no",
];
/// Biggest data file read (bytes): a damaged or foreign file must not exhaust memory.
const MAX_FILE: u64 = 1 << 30;

/// One definition.
#[derive(Clone, Debug, PartialEq)]
pub struct Definition {
    /// The term as the source spells it ("Biological Availability").
    pub term: String,
    /// `MeSH`, `WordNet` or `Wiktionary`.
    pub source: String,
    pub part_of_speech: String,
    pub text: String,
}

/// The words for a term in one language.
#[derive(Clone, Debug, PartialEq)]
pub struct Translation {
    /// ISO 639-1 code, from [`LANGUAGES`].
    pub language: String,
    pub part_of_speech: String,
    /// The translations, separated by "; ".
    pub words: String,
    /// Which meaning they translate ("act of absorbing"), when the source says.
    pub sense: String,
}

/// What was found for one term.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Entry {
    /// What was looked up, as selected (trimmed).
    pub query: String,
    /// The dictionary form found ("absorb" for "absorbed"), or `None` when nothing was found.
    pub found: Option<String>,
    pub definitions: Vec<Definition>,
    pub translations: Vec<Translation>,
}

impl Entry {
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty() && self.translations.is_empty()
    }
}

/// One loaded table: the file's text and where each row starts and ends.
#[derive(Default)]
struct Table {
    text: String,
    rows: Vec<(usize, usize)>,
}

impl Table {
    fn parse(text: String) -> Self {
        let mut rows = Vec::new();
        let mut start = 0;
        for line in text.split_inclusive('\n') {
            let end = start + line.len();
            let body = line.trim_end_matches(['\n', '\r']);
            if !body.is_empty() && body.contains('\t') {
                rows.push((start, start + body.len()));
            }
            start = end;
        }
        // The builder sorts by key; sort again (stably) so a hand-edited file still works.
        let mut table = Table { text, rows };
        let keys: Vec<String> = table.rows.iter().map(|&r| table.key_of(r).to_owned()).collect();
        let mut order: Vec<usize> = (0..table.rows.len()).collect();
        if keys.windows(2).any(|w| w[0] > w[1]) {
            order.sort_by(|&a, &b| keys[a].cmp(&keys[b]));
            table.rows = order.into_iter().filter_map(|i| table.rows.get(i).copied()).collect();
        }
        table
    }

    fn line(&self, (s, e): (usize, usize)) -> &str {
        self.text.get(s..e).unwrap_or("")
    }

    fn key_of(&self, row: (usize, usize)) -> &str {
        self.line(row).split('\t').next().unwrap_or("")
    }

    /// Every row whose key is `key`, as its fields after the key.
    fn find(&self, key: &str) -> Vec<Vec<&str>> {
        let first = self.rows.partition_point(|&r| self.key_of(r) < key);
        self.rows
            .get(first..)
            .unwrap_or(&[])
            .iter()
            .take_while(|&&r| self.key_of(r) == key)
            .map(|&r| self.line(r).split('\t').skip(1).collect())
            .collect()
    }

    fn has(&self, key: &str) -> bool {
        let first = self.rows.partition_point(|&r| self.key_of(r) < key);
        self.rows.get(first).is_some_and(|&r| self.key_of(r) == key)
    }
}

/// The loaded dictionaries.
#[derive(Default)]
pub struct Dictionary {
    define: Table,
    translate: Table,
    /// Other names: `key \t key it stands for` (a MeSH entry term → its heading).
    see: Table,
}

/// A term folded for lookup, as the data builder folds keys: lower case, hyphens, underscores
/// and runs of spaces as one space, surrounding punctuation dropped.
pub fn fold(term: &str) -> String {
    let lower = term.trim().to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut space = false;
    for c in lower.chars() {
        let c = match c {
            '\u{2010}' | '\u{2011}' | '\u{2013}' | '-' | '_' => ' ',
            '\u{2019}' => '\'',
            c if c.is_whitespace() => ' ',
            c => c,
        };
        if c == ' ' {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out.trim_matches(|c: char| " .,;:!?'\"()[]{}«»“”‘’•·".contains(c)).to_owned()
}

/// The dictionary forms an English word may come from, most likely first ("studies" → "study").
fn base_forms(word: &str) -> Vec<String> {
    let mut out = vec![word.to_owned()];
    let w = word.strip_suffix("'s").unwrap_or(word);
    let rules: [(&str, &str); 14] = [
        ("ies", "y"),
        ("ches", "ch"),
        ("shes", "sh"),
        ("sses", "ss"),
        ("xes", "x"),
        ("zes", "z"),
        ("ses", "s"),
        ("s", ""),
        ("ied", "y"),
        ("ed", ""),
        ("ed", "e"),
        ("ing", ""),
        ("ing", "e"),
        ("ly", ""),
    ];
    if w != word {
        out.push(w.to_owned());
    }
    for (suffix, with) in rules {
        if let Some(stem) = w.strip_suffix(suffix)
            && stem.chars().count() >= 2
        {
            out.push(format!("{stem}{with}"));
            // "stopped" → "stop", "running" → "run".
            if (suffix == "ed" || suffix == "ing") && with.is_empty() {
                let chars: Vec<char> = stem.chars().collect();
                if let [.., a, b] = chars.as_slice()
                    && a == b
                {
                    out.push(chars[..chars.len() - 1].iter().collect());
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|f| seen.insert(f.clone()));
    out
}

impl Dictionary {
    /// Dictionaries from the two files' text (tests, or files read elsewhere).
    pub fn parse(define: String, translate: String) -> Self {
        Self::parse_with_names(define, translate, String::new())
    }

    /// [`Self::parse`], with the other-names table (`see.tsv`).
    pub fn parse_with_names(define: String, translate: String, see: String) -> Self {
        Dictionary { define: Table::parse(define), translate: Table::parse(translate), see: Table::parse(see) }
    }

    /// The dictionaries in `dir`.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let read = |name: &str| -> Result<String, String> {
            let path = dir.join(name);
            let len = std::fs::metadata(&path).map_err(|e| format!("{name}: {e}"))?.len();
            if len > MAX_FILE {
                return Err(format!("{name} is too large"));
            }
            std::fs::read_to_string(&path).map_err(|e| format!("{name}: {e}"))
        };
        Ok(Self::parse_with_names(read("define.tsv")?, read("translate.tsv").unwrap_or_default(), read("see.tsv").unwrap_or_default()))
    }

    /// Whether `key` has a definition or a translation.
    fn known(&self, key: &str) -> bool {
        self.define.has(key) || self.translate.has(key) || self.see.has(key)
    }

    /// The headings `key` is another name for (at most a few).
    fn names_for(&self, key: &str) -> Vec<String> {
        self.see.find(key).into_iter().filter_map(|f| f.first().map(|s| (*s).to_owned())).filter(|s| s != key).take(4).collect()
    }

    fn entry(&self, query: &str, key: &str) -> Entry {
        let names = self.names_for(key);
        let mut definitions: Vec<Definition> = std::iter::once(key)
            .chain(names.iter().map(String::as_str))
            .flat_map(|k| self.define.find(k))
            .map(|f| Definition {
                term: f.first().copied().unwrap_or("").to_owned(),
                source: f.get(1).copied().unwrap_or("").to_owned(),
                part_of_speech: f.get(2).copied().unwrap_or("").to_owned(),
                text: f.get(3).copied().unwrap_or("").to_owned(),
            })
            .filter(|d| !d.text.is_empty())
            .collect();
        // Medical meaning first, then WordNet, then Wiktionary; each source in its own order.
        definitions.sort_by_key(|d| SOURCES.iter().position(|s| *s == d.source).unwrap_or(SOURCES.len()));
        definitions.dedup_by(|a, b| a.text == b.text);
        definitions.truncate(MAX_DEFINITIONS);
        let translations = self
            .translate
            .find(key)
            .into_iter()
            .map(|f| Translation {
                language: f.first().copied().unwrap_or("").to_owned(),
                part_of_speech: f.get(1).copied().unwrap_or("").to_owned(),
                words: f.get(2).copied().unwrap_or("").to_owned(),
                sense: f.get(3).copied().unwrap_or("").to_owned(),
            })
            .filter(|t| !t.words.is_empty())
            .collect();
        Entry { query: query.to_owned(), found: Some(key.to_owned()), definitions, translations }
    }

    /// Look up one term: its exact form, then English base forms ("absorbed" → "absorb").
    fn lookup_one(&self, query: &str) -> Entry {
        let key = fold(query);
        let forms = if key.contains(' ') { vec![key.clone()] } else { base_forms(&key) };
        let Some(found) = forms.iter().find(|f| self.known(f)) else {
            return Entry { query: query.trim().to_owned(), ..Default::default() };
        };
        let mut entry = self.entry(query.trim(), found);
        // "absorbed" is defined as an adjective, but translated under "absorb": a language the
        // form found has no words in takes them from the other forms.
        for other in forms.iter().filter(|f| *f != found && self.translate.has(f)) {
            let more = self.entry(query.trim(), other).translations;
            let missing: Vec<_> = more.into_iter().filter(|t| !entry.translations.iter().any(|have| have.language == t.language)).collect();
            entry.translations.extend(missing);
        }
        entry
    }

    /// Look up a selection. A word or a known phrase gives one entry; a phrase the
    /// dictionaries don't have gives one entry per word (at most [`MAX_WORDS`]), so a sentence
    /// is at least translated word by word.
    pub fn lookup(&self, selection: &str) -> Vec<Entry> {
        let mut text: String = selection.chars().take(MAX_QUERY).collect();
        text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if fold(&text).is_empty() {
            return Vec::new();
        }
        let whole = self.lookup_one(&text);
        let words: Vec<&str> = text.split(' ').filter(|w| !fold(w).is_empty()).collect();
        if !whole.is_empty() || words.len() < 2 {
            return vec![whole];
        }
        // Word by word: the little words carry no meaning worth a card.
        let content: Vec<&str> = words.iter().copied().filter(|w| !STOP_WORDS.contains(&fold(w).as_str())).collect();
        let words = if content.is_empty() { words } else { content };
        words.into_iter().take(MAX_WORDS).map(|w| self.lookup_one(w)).collect()
    }
}

/// Where the dictionaries are looked for, in order: `PDFCRAFT_DICTIONARIES`, then beside the
/// app as the packages put them (the folder of the executable, the macOS bundle's Resources,
/// an FHS `share/pdfcraft`), then the source tree (development).
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = std::env::var_os("PDFCRAFT_DICTIONARIES") {
        dirs.push(PathBuf::from(d));
    }
    if let Some(exe) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        dirs.extend([exe.join("dictionaries"), exe.join("../Resources/dictionaries"), exe.join("../share/pdfcraft/dictionaries")]);
    }
    dirs.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/dictionaries"));
    dirs
}

/// Whether the dictionaries are installed.
pub fn available() -> bool {
    search_dirs().iter().any(|d| d.join("define.tsv").is_file())
}

/// The dictionaries, loaded once and shared. Slow the first time (about a second).
pub fn shared() -> Result<Arc<Dictionary>, String> {
    static LOADED: Mutex<Option<Arc<Dictionary>>> = Mutex::new(None);
    let mut slot = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(d) = slot.as_ref() {
        return Ok(d.clone());
    }
    let dir = search_dirs()
        .into_iter()
        .find(|d| d.join("define.tsv").is_file())
        .ok_or_else(|| "the dictionaries are not installed (download the app again)".to_owned())?;
    let d = Arc::new(Dictionary::load(&dir)?);
    *slot = Some(d.clone());
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Dictionary {
        let define = "absorb\tabsorb\tWordNet\tverb\tbecome imbued\n\
absorption\tabsorption\tWordNet\tnoun\ta process in which one substance permeates another\n\
absorption\tAbsorption\tMeSH\tmedical\tUptake of substances through membranes\n\
bioavailability\tBiological Availability\tMeSH\tmedical\tThe extent to which the active ingredient becomes available\n\
half life\thalf-life\tWordNet\tnoun\tthe time required for something to fall to half its initial value\n\
study\tstudy\tWordNet\tnoun\ta detailed critical inspection\n"
            .to_owned();
        let translate =
            "absorption\tar\tnoun\tامتصاص; استيعاب\tact of absorbing\nabsorption\tde\tnoun\tAbsorption\t\ndrug\tar\tnoun\tدواء\t\n".to_owned();
        Dictionary::parse(define, translate)
    }

    #[test]
    fn folding_matches_the_builder() {
        assert_eq!(fold("  Half-Life, "), "half life");
        assert_eq!(fold("“Loading  dose”"), "loading dose");
        assert_eq!(fold("Na–K pump"), "na k pump");
        assert_eq!(fold("..."), "");
    }

    #[test]
    fn medical_meaning_comes_first() {
        let e = &sample().lookup("Absorption")[0];
        assert_eq!(e.found.as_deref(), Some("absorption"));
        assert_eq!(e.definitions[0].source, "MeSH");
        assert_eq!(e.definitions[1].source, "WordNet");
        assert_eq!(e.translations[0].words, "امتصاص; استيعاب");
        assert_eq!(e.translations[0].sense, "act of absorbing");
        assert_eq!(e.translations[1].language, "de");
    }

    #[test]
    fn inflected_words_find_their_base_form() {
        let d = sample();
        assert_eq!(d.lookup("absorbed")[0].found.as_deref(), Some("absorb"));
        assert_eq!(d.lookup("studies")[0].found.as_deref(), Some("study"));
        assert_eq!(d.lookup("drugs")[0].translations[0].words, "دواء");
        // Defined as written, translated under its base form.
        let d2 = Dictionary::parse("absorbed\tabsorbed\tWordNet\tadjective\tengrossed\n".into(), "absorb\tar\tverb\tامتص\t\n".into());
        let e = &d2.lookup("absorbed")[0];
        assert_eq!(e.definitions[0].text, "engrossed");
        assert_eq!(e.translations[0].words, "امتص");
    }

    #[test]
    fn phrases_and_their_spellings() {
        let d = sample();
        assert_eq!(d.lookup("half-life")[0].found.as_deref(), Some("half life"));
        assert_eq!(d.lookup("Half life")[0].definitions.len(), 1);
    }

    #[test]
    fn an_unknown_phrase_is_looked_up_word_by_word() {
        let entries = sample().lookup("the drug absorption rate");
        assert_eq!(entries.len(), 3, "\"the\" is skipped");
        assert_eq!(entries[0].translations[0].words, "دواء");
        assert!(entries[2].is_empty());
        assert_eq!(sample().lookup("of the").len(), 2, "only little words: all of them");
    }

    #[test]
    fn nothing_selected_finds_nothing() {
        assert!(sample().lookup("  ").is_empty());
        assert!(sample().lookup("xyzzy")[0].is_empty());
        assert!(Dictionary::default().lookup("absorption")[0].is_empty());
    }

    #[test]
    fn broken_and_unsorted_rows_are_handled() {
        let d = Dictionary::parse("zeta\tzeta\tWordNet\tnoun\tlast\nno tabs here\n\nalpha\talpha\tWordNet\tnoun\tfirst\n".into(), String::new());
        assert_eq!(d.lookup("alpha")[0].definitions[0].text, "first");
        assert_eq!(d.lookup("zeta")[0].definitions[0].text, "last");
        let short = Dictionary::parse("beta\tbeta\n".into(), String::new());
        assert!(short.lookup("beta")[0].definitions.is_empty());
    }

    #[test]
    fn other_names_lead_to_the_heading() {
        let d = Dictionary::parse_with_names(
            "biological availability\tBiological Availability\tMeSH\tmedical\tThe extent of uptake\nbioavailability\tbioavailability\tWordNet\tnoun\ta general sense\n".into(),
            String::new(),
            "bioavailability\tbiological availability\n".into(),
        );
        let e = &d.lookup("Bioavailability")[0];
        assert_eq!(e.definitions.len(), 2);
        assert_eq!(e.definitions[0].source, "MeSH");
        assert_eq!(e.definitions[0].term, "Biological Availability");
    }

    #[test]
    fn long_selections_are_capped() {
        let long = "drug ".repeat(200);
        assert!(sample().lookup(&long).len() <= MAX_WORDS);
    }
}

/// Checks a real build of the data: `PDFCRAFT_DICTIONARIES=dir cargo test -p pdfcraft-engine
/// real_dictionaries -- --ignored --nocapture`.
#[cfg(test)]
#[test]
#[ignore]
fn real_dictionaries() {
    let Some(dir) = std::env::var_os("PDFCRAFT_DICTIONARIES") else { return };
    let t = std::time::Instant::now();
    let d = Dictionary::load(Path::new(&dir)).expect("loads");
    eprintln!("loaded in {:?}", t.elapsed());
    for q in ["Absorption", "bioavailability", "warfarin", "half-life", "evaporated", "condensers", "loading dose", "the drug is absorbed"] {
        for e in d.lookup(q) {
            let ar: Vec<&str> = e.translations.iter().filter(|t| t.language == "ar").map(|t| t.words.as_str()).collect();
            eprintln!(
                "{q:?} -> {:?}: {} defs, first {:?}; ar {:?}",
                e.found,
                e.definitions.len(),
                e.definitions.first().map(|d| (&d.source, d.text.chars().take(60).collect::<String>())),
                ar
            );
        }
    }
}
