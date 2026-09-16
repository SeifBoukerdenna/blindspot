"""Offline, disposable OOXML fixtures for the sandboxed extraction helper."""
import io
import json
import pathlib
import subprocess
import sys
import tempfile
import zipfile


def package(entries):
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, data in entries.items():
            archive.writestr(name, data)
    return output.getvalue()


def extract(kind, data, budget=2097152):
    with tempfile.TemporaryFile() as source:
        source.write(data)
        source.seek(0)
        result = subprocess.run([sys.argv[1], "--index", kind, str(budget), "100", "0"],
                                stdin=source, capture_output=True, timeout=25, check=True)
        return json.loads(result.stdout)


slides = {
    "ppt/presentation.xml": '<p:presentation xmlns:p="urn:p" xmlns:r="urn:r"><p:sldIdLst><p:sldId r:id="second"/><p:sldId r:id="first"/></p:sldIdLst></p:presentation>',
    "ppt/_rels/presentation.xml.rels": '<Relationships><Relationship Id="first" Target="slides/slide1.xml"/><Relationship Id="second" Target="slides/slide2.xml"/></Relationships>',
    "ppt/slides/slide1.xml": '<a:p xmlns:a="urn:a"><a:r><a:t>Renewal deadline</a:t></a:r></a:p>',
    "ppt/slides/slide2.xml": '<a:p xmlns:a="urn:a"><a:r><a:t>Camera security</a:t></a:r></a:p>',
}
result = extract("pptx", package(slides))
assert result["text"] == "Camera security\fRenewal deadline", result
assert not result["partial"], result
result = extract("pptx", package(slides), 10)
assert result["partial"] and len(result["text"].encode()) <= 10, result

sheets = {
    "xl/workbook.xml": '<workbook xmlns:r="urn:r"><sheets><sheet name="Budget" r:id="one"/></sheets></workbook>',
    "xl/_rels/workbook.xml.rels": '<Relationships><Relationship Id="one" Target="worksheets/sheet1.xml"/></Relationships>',
    "xl/sharedStrings.xml": '<sst><si><r><t>Renewal</t></r><r><t> price</t></r></si></sst>',
    "xl/worksheets/sheet1.xml": '<worksheet><sheetData><row r="8"><c r="A8" t="s"><v>0</v></c><c r="B8"><f>WEBSERVICE("https://example.invalid")</f><v>42</v></c></row></sheetData></worksheet>',
}
result = extract("xlsx", package(sheets))
assert "Sheet Budget · Row 8: A8: Renewal price | B8: 42" in result["text"], result
assert "WEBSERVICE" not in result["text"], result

external = dict(slides)
external["ppt/_rels/presentation.xml.rels"] = '<Relationships><Relationship Id="first" TargetMode="External" Target="https://example.invalid/test.xml"/><Relationship Id="second" Target="../../private.xml"/></Relationships>'
result = extract("pptx", package(external))
assert result["status"] == "empty" and result["partial"], result
for malicious in [
    {**slides, "../escape.xml": "escape"},
    {**slides, "ppt/oversized.xml": "x" * 4194305},
    {**slides, "ppt/slides/slide2.xml": '<!DOCTYPE x [<!ENTITY secret SYSTEM "file:///etc/passwd">]><x>&secret;</x>'},
]:
    assert extract("pptx", package(malicious))["status"] == "unreadable"
assert extract("xlsx", b"not a zip")["status"] == "unreadable"
print("PASS: 8 Office extraction cases (ordering, limits, cached cells, external links, traversal, archive bomb, XML entity, corrupt archive)")
