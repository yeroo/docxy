# Regenerates fixtures/review.docx: a small Word document with a hyperlink and
# tracked changes (an insertion and a deletion), used by the browser tests to
# check that typed text lands at the right editor offset around atoms the
# editor cannot enter. Run from webapp/: python fixtures/make-review-docx.py
import zipfile

W = 'http://schemas.openxmlformats.org/wordprocessingml/2006/main'
R = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships'

document = f'''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}"><w:body>
<w:p><w:r><w:t>Review fixture</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">Start </w:t></w:r><w:hyperlink r:id="rId5"><w:r><w:t>link</w:t></w:r></w:hyperlink><w:ins w:id="1" w:author="Ada" w:date="2026-09-01T10:00:00Z"><w:r><w:t>added</w:t></w:r></w:ins><w:del w:id="2" w:author="Linus" w:date="2026-09-01T10:00:00Z"><w:r><w:delText>removed</w:delText></w:r></w:del><w:r><w:t xml:space="preserve"> end.</w:t></w:r></w:p>
<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr>
</w:body></w:document>'''

rels = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/" TargetMode="External"/></Relationships>'''

content_types = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>'''

root_rels = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>'''

with zipfile.ZipFile('fixtures/review.docx', 'w', zipfile.ZIP_DEFLATED) as z:
    for name, text in [
        ('[Content_Types].xml', content_types),
        ('_rels/.rels', root_rels),
        ('word/document.xml', document),
        ('word/_rels/document.xml.rels', rels),
    ]:
        info = zipfile.ZipInfo(name, date_time=(2026, 9, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        z.writestr(info, text)
