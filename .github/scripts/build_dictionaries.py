#!/usr/bin/env python3
"""Build PdfCraft's offline dictionaries (used by Define and Translate on selected text).

Downloads three free sources and writes compact, sorted tab-separated files:

  define.tsv     key \t term \t source \t part of speech \t definition
  translate.tsv  key \t language code \t part of speech \t translations \t sense
  see.tsv        key \t key it is another name for (MeSH entry terms), so a definition is stored once
  CREDITS.txt    where each source comes from and its licence

`key` is the term folded for lookup: lower case, hyphens and runs of spaces turned into one
space. Sources:

  - Open English WordNet 2025 (CC BY 4.0)            https://en-word.net
  - Wiktionary via Wiktextract / kaikki.org (CC BY-SA 4.0)  https://kaikki.org
  - MeSH, Medical Subject Headings, U.S. National Library of Medicine  https://www.nlm.nih.gov/mesh

Usage: build_dictionaries.py OUT_DIR [WORK_DIR]
"""

import gzip
import json
import os
import re
import sys
import urllib.request
import xml.etree.ElementTree as ET

LANGS = {"ar": "Arabic", "de": "German", "fr": "French", "es": "Spanish"}
MAX_DEF = 600  # characters per definition kept
MAX_SENSES = 6  # definitions kept per term and source
WIKI_SENSES = 2  # Wiktionary fills gaps only: its first senses are enough
# Glosses that only point at another entry: the other entry is looked up instead.
SKIP_GLOSS = re.compile(r"^(alternative|obsolete|archaic|dated|rare) (form|spelling)|^(misspelling|plural|eye dialect|nonstandard)", re.I)
WIKI_TERM = re.compile(r"^[A-Za-z][A-Za-z' .-]*$")

WORDNET_URLS = ["https://github.com/globalwordnet/english-wordnet/releases/download/2025-edition/english-wordnet-2025.xml.gz",
                "https://en-word.net/static/english-wordnet-2025.xml.gz"]
WIKTIONARY_URLS = ["https://kaikki.org/dictionary/raw-wiktextract-data.jsonl.gz"]
MESH_URLS = [f"https://nlmpubs.nlm.nih.gov/projects/mesh/MESH_FILES/xmlmesh/desc{y}.xml" for y in (2026, 2025)]


def log(*a):
    print(*a, flush=True)


def fetch(urls, dest):
    if os.path.exists(dest) and os.path.getsize(dest) > 0:
        return dest
    last = None
    for u in urls:
        try:
            log("downloading", u)
            req = urllib.request.Request(u, headers={"User-Agent": "pdfcraft-dictionary-build/1"})
            with urllib.request.urlopen(req, timeout=600) as r, open(dest + ".part", "wb") as f:
                while True:
                    b = r.read(1 << 20)
                    if not b:
                        break
                    f.write(b)
            os.replace(dest + ".part", dest)
            log("  ->", dest, os.path.getsize(dest), "bytes")
            return dest
        except Exception as e:  # try the next mirror
            last = e
            log("  failed:", e)
    raise SystemExit(f"could not download {dest}: {last}")


def key(term):
    t = term.strip().lower().replace("‐", "-").replace("‑", "-").replace("–", "-")
    t = re.sub(r"[-_\s]+", " ", t)
    return t.strip(" .,;:!?'\"()[]")


def clean(s):
    s = re.sub(r"\s+", " ", s or "").strip()
    return s[:MAX_DEF]


POS = {"n": "noun", "v": "verb", "a": "adjective", "s": "adjective", "r": "adverb", "noun": "noun", "verb": "verb",
       "adj": "adjective", "adv": "adverb", "name": "name", "prep": "preposition", "conj": "conjunction",
       "pron": "pronoun", "intj": "interjection", "num": "numeral", "phrase": "phrase", "prefix": "prefix",
       "suffix": "suffix", "abbrev": "abbreviation", "det": "determiner"}


def wordnet(path, out):
    log("reading WordNet")
    defs = {}
    entries = []
    with gzip.open(path, "rb") as f:
        for _, el in ET.iterparse(f, events=("end",)):
            tag = el.tag.rsplit("}", 1)[-1]
            if tag == "Synset":
                d = el.find("Definition")
                if d is not None and d.text:
                    defs[el.get("id")] = clean(d.text)
                el.clear()
            elif tag == "LexicalEntry":
                lemma = el.find("Lemma")
                if lemma is not None:
                    form = lemma.get("writtenForm", "")
                    pos = POS.get(lemma.get("partOfSpeech", ""), "")
                    synsets = [s.get("synset") for s in el.findall("Sense")]
                    entries.append((form, pos, synsets))
                el.clear()
    n = 0
    for form, pos, synsets in entries:
        for s in synsets[:MAX_SENSES]:
            d = defs.get(s)
            if d:
                out.append((key(form), form, "WordNet", pos, d))
                n += 1
    log("  WordNet definitions:", n)


def mesh(path, out, see):
    log("reading MeSH")
    n = 0
    for _, el in ET.iterparse(path, events=("end",)):
        if el.tag != "DescriptorRecord":
            continue
        name = el.findtext("DescriptorName/String") or ""
        note = None
        for c in el.findall("ConceptList/Concept"):
            if c.get("PreferredConceptYN") == "Y":
                note = c.findtext("ScopeNote")
                terms = [t.findtext("String") or "" for t in c.findall("TermList/Term")]
                break
        else:
            terms = []
        if name and note:
            main = key(name)
            out.append((main, name, "MeSH", "medical", clean(note)))
            n += 1
            for t in terms:
                k = key(t)
                if k and k != main:
                    see.append((k, main))
        el.clear()
    log("  MeSH definitions:", n, "other names:", len(see))


REVERSE_MAX_WORDS = 3  # an English gloss this short reads as a translation ("evaporation")
GLOSS_SPLIT = re.compile(r"\s*[;,]\s*")
GLOSS_DROP = re.compile(r"\([^)]*\)")


def reverse_glosses(e, out):
    """An Arabic (German, French, Spanish) entry glossed in English, turned round: its short
    glosses become English keys that translate to the entry's word."""
    code = e.get("lang_code")
    word = (e.get("word") or "").strip()
    if not word:
        return
    pos = POS.get(e.get("pos", ""), e.get("pos", ""))
    for s in (e.get("senses") or [])[:3]:
        tags = s.get("tags") or []
        if "form-of" in tags or "alt-of" in tags:
            continue
        for g in (s.get("glosses") or [])[-1:]:
            g = GLOSS_DROP.sub("", g)
            for part in GLOSS_SPLIT.split(g):
                part = re.sub(r"^(to|a|an|the) ", "", part.strip().rstrip("."), flags=re.I)
                if part and WIKI_TERM.match(part) and len(part.split()) <= REVERSE_MAX_WORDS:
                    out.setdefault((key(part), code), []).append((pos, word))


def wiktionary(path, defs_out, trans_out, have):
    log("reading Wiktionary")
    n_def = n_tr = lines = 0
    markers = [b'"lang_code": "en"'] + [f'"lang_code": "{c}"'.encode() for c in LANGS]
    reverse = {}
    direct = set()
    with gzip.open(path, "rb") as f:
        for raw in f:
            lines += 1
            if not any(m in raw for m in markers):
                continue
            try:
                e = json.loads(raw)
            except ValueError:
                continue
            if e.get("lang_code") in LANGS:
                reverse_glosses(e, reverse)
                continue
            if e.get("lang_code") != "en":
                continue
            word = e.get("word") or ""
            k = key(word)
            if not k:
                continue
            pos = POS.get(e.get("pos", ""), e.get("pos", ""))
            # English definitions only for terms the other two sources don't have (drug names, newer terms).
            if k not in have and WIKI_TERM.match(word) and k.count(" ") <= 2 and e.get("pos") not in ("name", "character", "symbol"):
                kept = 0
                for s in e.get("senses", []):
                    g = s.get("glosses") or s.get("raw_glosses") or []
                    tags = s.get("tags") or []
                    if g and "form-of" not in tags and "alt-of" not in tags and not SKIP_GLOSS.match(g[-1]):
                        defs_out.append((k, word, "Wiktionary", pos, clean(g[-1])))
                        n_def += 1
                        kept += 1
                        if kept >= WIKI_SENSES:
                            break
            trs = list(e.get("translations") or [])
            for s in e.get("senses", []):
                trs.extend(s.get("translations") or [])
            by = {}
            for t in trs:
                code = t.get("lang_code") or t.get("code")
                w = (t.get("word") or "").strip()
                if code in LANGS and w:
                    sense = clean(t.get("sense") or "")[:120]
                    by.setdefault((code, sense), [])
                    if w not in by[(code, sense)]:
                        by[(code, sense)].append(w)
            for (code, sense), words in by.items():
                trans_out.append((k, code, pos, "; ".join(words[:8]), sense))
                n_tr += 1
                direct.add((k, code))
            if lines % 2_000_000 == 0:
                log(f"  {lines} lines, {n_def} definitions, {n_tr} translations")
    # Words a translation section doesn't list, from the other language's own entries.
    n_rev = 0
    for (k, code), pairs in reverse.items():
        if (k, code) in direct or not k:
            continue
        by_pos = {}
        for pos, w in pairs:
            by_pos.setdefault(pos, [])
            if w not in by_pos[pos]:
                by_pos[pos].append(w)
        for pos, words in by_pos.items():
            trans_out.append((k, code, pos, "; ".join(words[:6]), ""))
            n_rev += 1
    log("  Wiktionary definitions:", n_def, "translations:", n_tr, "from other-language entries:", n_rev)


def write(path, rows):
    # Sorted by key only (stable), so each term keeps its senses in the source's order.
    rows = sorted(dict.fromkeys(rows), key=lambda r: r[0])
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        for r in rows:
            f.write("\t".join(c.replace("\t", " ").replace("\n", " ") for c in r) + "\n")
    log("wrote", path, len(rows), "rows", os.path.getsize(path), "bytes")


CREDITS = """PdfCraft's offline dictionaries (Define and Translate on selected text) are built from:

Open English WordNet 2025 edition
  https://en-word.net  (https://github.com/globalwordnet/english-wordnet)
  Licence: Creative Commons Attribution 4.0 (CC BY 4.0)

Wiktionary, extracted with Wiktextract (https://github.com/tatuylonen/wiktextract) and published by kaikki.org
  https://kaikki.org  /  https://en.wiktionary.org
  Licence: Creative Commons Attribution-ShareAlike 4.0 (CC BY-SA 4.0) and GFDL.
  The definitions and translations taken from Wiktionary are shared here under CC BY-SA 4.0.

MeSH (Medical Subject Headings), U.S. National Library of Medicine
  https://www.nlm.nih.gov/mesh
  Courtesy of the U.S. National Library of Medicine. Used under the NLM Terms and Conditions
  (https://www.nlm.nih.gov/databases/download/terms_and_conditions.html); NLM does not endorse PdfCraft.

The definitions were shortened and reformatted for lookup; their meaning is unchanged.
"""


def main():
    out_dir = sys.argv[1]
    work = sys.argv[2] if len(sys.argv) > 2 else os.path.join(out_dir, "..", "dict-work")
    os.makedirs(out_dir, exist_ok=True)
    os.makedirs(work, exist_ok=True)
    defs, trans, see = [], [], []
    wordnet(fetch(WORDNET_URLS, os.path.join(work, "wordnet.xml.gz")), defs)
    mesh(fetch(MESH_URLS, os.path.join(work, "mesh.xml")), defs, see)
    have = {r[0] for r in defs} | {r[0] for r in see}
    wiktionary(fetch(WIKTIONARY_URLS, os.path.join(work, "wiktextract.jsonl.gz")), defs, trans, have)
    write(os.path.join(out_dir, "define.tsv"), defs)
    write(os.path.join(out_dir, "translate.tsv"), trans)
    write(os.path.join(out_dir, "see.tsv"), see)
    with open(os.path.join(out_dir, "CREDITS.txt"), "w", encoding="utf-8") as f:
        f.write(CREDITS)


if __name__ == "__main__":
    main()
