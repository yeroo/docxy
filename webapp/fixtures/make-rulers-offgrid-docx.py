# Regenerates fixtures/rulers-offgrid.docx: one paragraph with an off-grid left
# indent (567 twips, not a multiple of the ruler's 180-twip snap grid), used by
# the browser tests to check that a ruler click without movement is not an
# edit (it must not snap the indent). Run from webapp/:
# python fixtures/make-rulers-offgrid-docx.py
import zipfile

W = 'http://schemas.openxmlformats.org/wordprocessingml/2006/main'

document = f'''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}"><w:body>
<w:p><w:pPr><w:ind w:left="567"/></w:pPr><w:r><w:t>Off-grid indent</w:t></w:r></w:p>
<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr>
</w:body></w:document>'''

content_types = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>'''

root_rels = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>'''

with zipfile.ZipFile('fixtures/rulers-offgrid.docx', 'w', zipfile.ZIP_DEFLATED) as z:
    for name, text in [
        ('[Content_Types].xml', content_types),
        ('_rels/.rels', root_rels),
        ('word/document.xml', document),
    ]:
        info = zipfile.ZipInfo(name, date_time=(2026, 9, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        z.writestr(info, text)
