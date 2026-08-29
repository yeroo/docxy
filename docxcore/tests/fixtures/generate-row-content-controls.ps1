$ErrorActionPreference = 'Stop'

$fixturePath = Join-Path $PSScriptRoot 'row-content-controls.docx'
$parts = [ordered]@{
    '[Content_Types].xml' = @'
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>
'@
    '_rels/.rels' = @'
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>
'@
    'word/_rels/document.xml.rels' = @'
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>
'@
    'word/document.xml' = @'
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" mc:Ignorable="w15"><w:body><w:sdt><w:sdtPr><w:alias w:val="BlockControl"/><ux:blockProperty xmlns:ux="urn:docxy:row-controls" ux:val="block-kept"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Block sentinel</w:t></w:r></w:p></w:sdtContent></w:sdt><w:p><w:r><w:t xml:space="preserve">Inline: </w:t></w:r><w:sdt><w:sdtPr><w:alias w:val="InlineControl"/><ux:inlineProperty xmlns:ux="urn:docxy:row-controls" ux:val="inline-kept"/></w:sdtPr><w:sdtContent><w:r><w:t>inline sentinel</w:t></w:r></w:sdtContent></w:sdt><w:r><w:t>!</w:t></w:r></w:p><w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/><w:tblW w:w="0" w:type="auto"/><w:tblLook w:val="04A0"/></w:tblPr><w:tblGrid><w:gridCol w:w="2200"/><w:gridCol w:w="2200"/><w:gridCol w:w="2200"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Plain row</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>P2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>P3</w:t></w:r></w:p></w:tc></w:tr><w:sdt xmlns:ux="urn:docxy:row-controls" data-row-control="outer"><w:sdtPr><w:id w:val="101"/><w:alias w:val="Orders"/><w:tag w:val="orders"/><w15:repeatingSection w15:sectionTitle="Order"/><ux:unknownProperty ux:val="outer-kept"/></w:sdtPr><w:sdtEndPr><w:rPr><w:b/></w:rPr></w:sdtEndPr><w:sdtContent><w:sdt data-row-control="item-a"><w:sdtPr><w:id w:val="102"/><w:tag w:val="order-a"/><w15:repeatingSectionItem/><ux:itemProperty ux:val="a-kept"/></w:sdtPr><w:sdtContent><w:tr w:rsidR="00112233"><w:trPr><w:cantSplit/><w:trHeight w:val="360" w:hRule="atLeast"/><w:tblHeader/><ux:rowFlag ux:val="row-kept"/></w:trPr><w:tc><w:tcPr><w:tcW w:w="4400" w:type="dxa"/><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>Order A merged</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>A3</w:t></w:r></w:p></w:tc></w:tr></w:sdtContent><ux:itemTail ux:val="a-tail"/></w:sdt><ux:between ux:val="between-kept"/><w:sdt data-row-control="item-b"><w:sdtPr><w:id w:val="103"/><w:tag w:val="order-b"/><w15:repeatingSectionItem/><ux:itemProperty ux:val="b-kept"/></w:sdtPr><w:sdtContent><w:tr><w:trPr><w:cantSplit/><w:trHeight w:val="300"/></w:trPr><w:tc><w:p><w:r><w:t>Order B</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>B merged</w:t></w:r></w:p></w:tc></w:tr></w:sdtContent><ux:itemTail ux:val="b-tail"/></w:sdt></w:sdtContent><ux:outerTail ux:val="outer-tail"/></w:sdt><w:sdt xmlns:ux="urn:docxy:row-controls" data-row-control="adjacent"><w:sdtPr><w:id w:val="104"/><w:alias w:val="Adjacent"/><w:tag w:val="adjacent"/><ux:unknownProperty ux:val="adjacent-kept"/></w:sdtPr><w:sdtContent><w:tr><w:trPr><w:tblHeader/></w:trPr><w:tc><w:p><w:r><w:t>Adjacent row</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Adjacent 2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Adjacent 3</w:t></w:r></w:p></w:tc></w:tr></w:sdtContent><ux:adjacentTail ux:val="adjacent-tail"/></w:sdt><w:sdt data-row-control="empty"><w:sdtPr><w:id w:val="105"/><w:alias w:val="Empty row control"/></w:sdtPr><w:sdtContent></w:sdtContent></w:sdt><w:tr><w:tc><w:p><w:r><w:t>Tail row</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>T2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>T3</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>
'@
}

Add-Type -AssemblyName System.IO.Compression
$file = [System.IO.File]::Open(
    $fixturePath,
    [System.IO.FileMode]::Create,
    [System.IO.FileAccess]::ReadWrite,
    [System.IO.FileShare]::None
)
try {
    $archive = [System.IO.Compression.ZipArchive]::new(
        $file,
        [System.IO.Compression.ZipArchiveMode]::Create,
        $true
    )
    try {
        foreach ($part in $parts.GetEnumerator()) {
            $entry = $archive.CreateEntry(
                $part.Key,
                [System.IO.Compression.CompressionLevel]::Optimal
            )
            $entry.LastWriteTime = [System.DateTimeOffset]::new(2026, 8, 29, 0, 0, 0, [System.TimeSpan]::Zero)
            $stream = $entry.Open()
            try {
                $writer = [System.IO.StreamWriter]::new(
                    $stream,
                    [System.Text.UTF8Encoding]::new($false),
                    1024,
                    $true
                )
                try {
                    $writer.Write([string]$part.Value)
                    $writer.Flush()
                }
                finally {
                    $writer.Dispose()
                }
            }
            finally {
                $stream.Dispose()
            }
        }
    }
    finally {
        $archive.Dispose()
    }
}
finally {
    $file.Dispose()
}

Write-Output "Generated $fixturePath"
