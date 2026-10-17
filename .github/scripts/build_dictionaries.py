#!/usr/bin/env python3
"""Build PdfCraft's offline dictionaries (used by Define and Translate on selected text).

Downloads three free sources and writes compact, sorted tab-separated files:

  define.tsv     key \t term \t source \t part of speech \t definition
  translate.tsv  key \t language code \t part of speech \t translations \t sense
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


def mesh(path, out):
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
            d = clean(note)
            seen = set()
            for t in [name] + terms:
                k = key(t)
                if k and k not in seen:
                    seen.add(k)
                    out.append((k, name, "MeSH", "medical", d))
                    n += 1
        el.clear()
    log("  MeSH definitions:", n)


def wiktionary(path, defs_out, trans_out, have):
    log("reading Wiktionary")
    n_def = n_tr = lines = 0
    marker = b'"lang_code": "en"'
    with gzip.open(path, "rb") as f:
        for raw in f:
            lines += 1
            if marker not in raw:
                continue
            try:
                e = json.loads(raw)
            except ValueError:
                continue
            if e.get("lang_code") != "en":
                continue
            word = e.get("word") or ""
            k = key(word)
            if not k:
                continue
            pos = POS.get(e.get("pos", ""), e.get("pos", ""))
            # English definitions only for terms the other two sources don't have (drug names, newer terms).
            if k not in have:
                kept = 0
                for s in e.get("senses", []):
                    g = s.get("glosses") or s.get("raw_glosses") or []
                    if g and "form-of" not in (s.get("tags") or []):
                        defs_out.append((k, word, "Wiktionary", pos, clean(g[-1])))
                        n_def += 1
                        kept += 1
                        if kept >= MAX_SENSES:
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
            if lines % 2_000_000 == 0:
                log(f"  {lines} lines, {n_def} definitions, {n_tr} translations")
    log("  Wiktionary definitions:", n_def, "translations:", n_tr)


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
    defs, trans = [], []
    wordnet(fetch(WORDNET_URLS, os.path.join(work, "wordnet.xml.gz")), defs)
    mesh(fetch(MESH_URLS, os.path.join(work, "mesh.xml")), defs)
    have = {r[0] for r in defs}
    wiktionary(fetch(WIKTIONARY_URLS, os.path.join(work, "wiktextract.jsonl.gz")), defs, trans, have)
    write(os.path.join(out_dir, "define.tsv"), defs)
    write(os.path.join(out_dir, "translate.tsv"), trans)
    with open(os.path.join(out_dir, "CREDITS.txt"), "w", encoding="utf-8") as f:
        f.write(CREDITS)


if __name__ == "__main__":
    main()
