"""Generate the public, artificial source-format acceptance fixtures."""

from __future__ import annotations

from io import BytesIO
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZIP_STORED, ZipFile


ROOT = Path(__file__).resolve().parents[1] / "tests" / "fixtures"
SENTENCE = (
    "Synthetic person records a calm morning routine, a collaborative project, "
    "a family call, and a plan to learn a new language next month."
)


def write(name: str, content: str | bytes) -> None:
    ROOT.mkdir(parents=True, exist_ok=True)
    data = content.encode("utf-8") if isinstance(content, str) else content
    (ROOT / name).write_bytes(data)


def zip_bytes(entries: dict[str, str | bytes], *, mimetype: str | None = None) -> bytes:
    output = BytesIO()
    with ZipFile(output, "w") as archive:
        if mimetype is not None:
            archive.writestr("mimetype", mimetype, compress_type=ZIP_STORED)
        for name, content in entries.items():
            archive.writestr(name, content, compress_type=ZIP_DEFLATED)
    return output.getvalue()


def pdf_bytes(text: str) -> bytes:
    escaped = text.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
    stream = f"BT /F1 11 Tf 72 720 Td ({escaped}) Tj ET".encode("ascii")
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        f"<< /Length {len(stream)} >>\nstream\n".encode("ascii") + stream + b"\nendstream",
    ]
    output = bytearray(b"%PDF-1.4\n% synthetic\n")
    offsets = [0]
    for index, body in enumerate(objects, start=1):
        offsets.append(len(output))
        output.extend(f"{index} 0 obj\n".encode("ascii"))
        output.extend(body)
        output.extend(b"\nendobj\n")
    xref = len(output)
    output.extend(f"xref\n0 {len(objects) + 1}\n".encode("ascii"))
    output.extend(b"0000000000 65535 f \n")
    for offset in offsets[1:]:
        output.extend(f"{offset:010d} 00000 n \n".encode("ascii"))
    output.extend(
        f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode("ascii")
    )
    return bytes(output)


def main() -> None:
    write(
        "telegram.json",
        '{"name":"Synthetic chat","messages":[{"id":1,"type":"message",'
        '"date":"2026-01-01T10:00:00Z","from":"Synthetic Alex",'
        '"from_id":"synthetic-user","text":"Synthetic telegram memory about a language lesson."}]}',
    )
    write(
        "whatsapp-standard.txt",
        "01.02.2026 10:30 - Synthetic Alex - Synthetic whatsapp standard memory.\n"
        "01.02.2026 10:31 - Synthetic Sam - Another artificial message.",
    )
    write(
        "whatsapp-alternate.txt",
        "[01/02/2026, 10:30:05] Synthetic Alex: Synthetic whatsapp alternate memory.\n"
        "[01/02/2026, 10:31:05] Synthetic Sam: Another artificial message.",
    )
    write("plain.txt", SENTENCE)
    write("plain.text", SENTENCE)
    write("notes.md", f"# Synthetic journal\n\n{SENTENCE}")
    write("notes.markdown", f"# Synthetic journal alias\n\n{SENTENCE}")
    write(
        "obsidian.md",
        f"---\ntags:\n  - synthetic\n---\n# Daily note\n\n{SENTENCE} See [[Synthetic Project]] for context.",
    )
    write("notes.html", f"<!doctype html><html><body><p>{SENTENCE}</p></body></html>")
    write("notes.htm", f"<!doctype html><html><body><p>{SENTENCE}</p></body></html>")
    write(
        "apple-notes.html",
        f'<!doctype html><html><head><meta name="generator" content="Apple Notes"></head>'
        f'<body><article class="note">{SENTENCE}</article></body></html>',
    )
    write("notes.json", '{"title":"Synthetic journal","body":"' + SENTENCE + '"}')
    write("profile.pdf", pdf_bytes(SENTENCE))
    write(
        "profile.docx",
        zip_bytes(
            {
                "[Content_Types].xml": '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>',
                "word/document.xml": f'<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{SENTENCE}</w:t></w:r></w:p></w:body></w:document>',
            }
        ),
    )
    write("notes.rtf", r"{\rtf1\ansi " + SENTENCE + r"\par}")
    write(
        "notes.odt",
        zip_bytes(
            {
                "META-INF/manifest.xml": '<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"/>',
                "content.xml": f'<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:text><text:p>{SENTENCE}</text:p></office:text></office:body></office:document-content>',
            },
            mimetype="application/vnd.oasis.opendocument.text",
        ),
    )
    write(
        "memoir.epub",
        zip_bytes(
            {
                "META-INF/container.xml": '<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.xhtml"/></rootfiles></container>',
                "OEBPS/content.xhtml": f"<html><body><p>{SENTENCE}</p></body></html>",
            },
            mimetype="application/epub+zip",
        ),
    )
    write(
        "memoir.fb2",
        f'<?xml version="1.0"?><FictionBook><body><section><p>{SENTENCE}</p></section></body></FictionBook>',
    )
    write("archive.zip", zip_bytes({"journal.md": f"# Synthetic archive\n\n{SENTENCE}"}))
    write(
        "notion-export.zip",
        zip_bytes(
            {
                "Home aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.md": f"# Synthetic home\n\n{SENTENCE}",
                "Project bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.md": f"# Synthetic project\n\n{SENTENCE}",
            }
        ),
    )
    write(
        "linkedin-export.zip",
        zip_bytes(
            {
                "Profile.csv": "First Name,Last Name,Headline\nSynthetic,Alex,Artificial profile",
                "Positions.csv": "Company Name,Title,Description\nExample Lab,Researcher," + SENTENCE,
            }
        ),
    )
    write(
        "instagram-export.zip",
        zip_bytes(
            {
                "personal_information/personal_information.json": '{"profile_user":[{"string_map_data":{"Name":{"value":"Synthetic Alex"}}}]}',
                "content/posts_1.json": '[{"title":"Synthetic post","text":"' + SENTENCE + '"}]',
            }
        ),
    )
    write(
        "facebook-export.zip",
        zip_bytes(
            {
                "messages/inbox/synthetic_friend/message_1.json": '{"participants":[{"name":"Synthetic Alex"}],"messages":[{"sender_name":"Synthetic Alex","timestamp_ms":1767225600000,"content":"' + SENTENCE + '"}]}',
            }
        ),
    )


if __name__ == "__main__":
    main()
