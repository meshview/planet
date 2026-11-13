#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>
"""Builds assets/areas.json, the closed list of areas an instance can be in.

An area is a country (ISO 3166-1) and, optionally, one of its top level
subdivisions (ISO 3166-2). Both come from Debian's iso-codes, which is also
where the Spanish country names come from. Subdivisions keep their ISO name,
which is the local one ("Catalunya", "Hessen") in both languages: iso-codes'
Spanish catalogue for them is mostly fuzzy guesses, and a wrong name is worse
than an untranslated one.

Usage: update-areas.py <iso-codes source tree> [output]

The source tree is a checkout or a release tarball of
https://salsa.debian.org/iso-codes-team/iso-codes, unpacked. Only the standard
library is needed.
"""

import json
import re
import sys
from pathlib import Path

LANGUAGES = ["es"]


def read_po(path):
    """msgid -> msgstr from a .po file, leaving out fuzzy and untranslated
    entries."""
    entries = {}
    fuzzy = False
    current = None
    msgid = msgstr = None

    def flush():
        if msgid and msgstr and not fuzzy:
            entries[msgid] = msgstr

    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            flush()
            fuzzy, current, msgid, msgstr = False, None, None, None
        elif line.startswith("#,"):
            fuzzy = "fuzzy" in line
        elif line.startswith("#"):
            continue
        elif line.startswith("msgid "):
            current, msgid = "id", unquote(line[6:])
        elif line.startswith("msgstr "):
            current, msgstr = "str", unquote(line[7:])
        elif line.startswith('"'):
            if current == "id":
                msgid += unquote(line)
            elif current == "str":
                msgstr += unquote(line)
    flush()
    return entries


def unquote(text):
    return json.loads(text)


def subdivision_name(name):
    """The name without the alternatives iso-codes puts in brackets after it:
    "Catalunya [Cataluña]" is "Catalunya"."""
    return re.sub(r"\s*\[.*\]$", "", name).strip()


def main():
    if len(sys.argv) not in (2, 3):
        sys.exit(__doc__)
    source = Path(sys.argv[1])
    output = Path(sys.argv[2]) if len(sys.argv) == 3 else (
        Path(__file__).resolve().parent.parent / "assets" / "areas.json")

    version = source.joinpath("meson.build").read_text(encoding="utf-8")
    version = re.search(r"version\s*:\s*'([^']+)'", version).group(1)

    countries = json.loads(
        source.joinpath("data", "iso_3166-1.json").read_text(encoding="utf-8"))["3166-1"]
    subdivisions = json.loads(
        source.joinpath("data", "iso_3166-2.json").read_text(encoding="utf-8"))["3166-2"]
    catalogues = {
        language: read_po(source / "iso_3166-1" / f"{language}.po")
        for language in LANGUAGES
    }

    regions = {}
    for subdivision in subdivisions:
        if "parent" in subdivision:
            continue
        country = subdivision["code"].split("-")[0]
        regions.setdefault(country, []).append({
            "code": subdivision["code"],
            "name": subdivision_name(subdivision["name"]),
        })

    areas = []
    for country in countries:
        name = country.get("common_name", country["name"])
        names = {"en": name}
        for language, catalogue in catalogues.items():
            names[language] = catalogue.get(name, name)
        areas.append({
            "code": country["alpha_2"],
            "names": names,
            "regions": sorted(regions.get(country["alpha_2"], []),
                              key=lambda region: region["name"]),
        })
    areas.sort(key=lambda area: area["code"])

    document = {
        "source": f"iso-codes {version}",
        "license": "LGPL-2.1-or-later",
        "countries": areas,
    }
    output.write_text(json.dumps(document, ensure_ascii=False, indent=1) + "\n",
                      encoding="utf-8")
    print(f"{output}: {len(areas)} countries, "
          f"{sum(len(area['regions']) for area in areas)} regions, from iso-codes {version}")


if __name__ == "__main__":
    main()
