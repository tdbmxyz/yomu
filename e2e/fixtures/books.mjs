// Original, generated test publications. No copyrighted user files in CI.
import { writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
  }
  return (crc ^ 0xffffffff) >>> 0;
}
function zip(entries) {
  const parts = [], directory = [];
  let offset = 0;
  for (const [name, text] of entries) {
    const filename = Buffer.from(name), bytes = Buffer.from(text);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50); local.writeUInt16LE(20, 4);
    local.writeUInt32LE(crc32(bytes), 14); local.writeUInt32LE(bytes.length, 18);
    local.writeUInt32LE(bytes.length, 22); local.writeUInt16LE(filename.length, 26);
    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50); central.writeUInt16LE(20, 4); central.writeUInt16LE(20, 6);
    central.writeUInt32LE(crc32(bytes), 16); central.writeUInt32LE(bytes.length, 20);
    central.writeUInt32LE(bytes.length, 24); central.writeUInt16LE(filename.length, 28);
    central.writeUInt32LE(offset, 42);
    parts.push(local, filename, bytes); directory.push(central, filename);
    offset += local.length + filename.length + bytes.length;
  }
  const directoryBytes = Buffer.concat(directory), end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50); end.writeUInt16LE(entries.length, 8); end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(directoryBytes.length, 12); end.writeUInt32LE(offset, 16);
  return Buffer.concat([...parts, directoryBytes, end]);
}

export function writeBooks(dir) {
  const paragraphs = Array.from({ length: 45 }, (_, n) => `<p id="paragraph-${n + 1}">Paragraph ${n + 1}: A publication has an ordered collection of resources. The reader follows the spine, not alphabetical filename order.</p>`).join('');
  const html = (title, body) => `<html xmlns="http://www.w3.org/1999/xhtml"><head><title>${title}</title><link rel="stylesheet" href="style.css"/></head><body><h1>${title}</h1>${body}<script>document.body.setAttribute('data-book-script', 'ran'); parent.document.body.setAttribute('data-book-script', 'ran');</script></body></html>`;
  const epubEntries = [
    ['mimetype', 'application/epub+zip'],
    ['META-INF/container.xml', '<container><rootfiles><rootfile full-path="OPS/book.opf"/></rootfiles></container>'],
    ['OPS/book.opf', '<package xmlns:dc="http://purl.org/dc/elements/1.1/"><metadata><dc:title>Fixture EPUB</dc:title><dc:description>A generated book</dc:description></metadata><manifest><item id="first" href="z-first.xhtml" media-type="application/xhtml+xml"/><item id="second" href="a-second.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="Nav/toc.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="css" href="style.css" media-type="text/css"/><item id="cover" href="cover.png" media-type="image/png" properties="cover-image"/></manifest><spine><itemref idref="first"/><itemref idref="second"/></spine></package>'],
    ['OPS/Nav/toc.xhtml', '<html xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><a href="../z-first.xhtml">First section</a><a href="../a-second.xhtml">Second section</a></nav></body></html>'],
    ['OPS/z-first.xhtml', html('First section', '<img src="cover.png" alt="Fixture illustration"/><a href="a-second.xhtml#section-two">Go to second section</a><a href="#paragraph-1">Jump to paragraph</a>' + paragraphs)],
    ['OPS/a-second.xhtml', html('Second section', '<p id="section-two">The next resource in spine order.</p><a href="z-first.xhtml">Return to first section</a>')],
    ['OPS/style.css', 'h1 { color: #000 !important; letter-spacing: 0.5px; } body { font-family: serif; background: #050505 !important; } p { color: #8e0012 !important; text-shadow: 1px 0 #8e0012; }'],
    ['OPS/cover.png', Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aL1cAAAAASUVORK5CYII=', 'base64')],
  ];
  writeFileSync(resolve(dir, 'fixture.epub'), zip(epubEntries));
  writeFileSync(resolve(dir, 'Fixture Editions.epub'), zip(epubEntries.map(([name, data]) =>
    [name, name === 'OPS/book.opf' ? data.replace('Fixture EPUB', 'Fixture Editions') : data])));
  writeFileSync(resolve(dir, 'Fixture Editions.mobi'), 'Original fixture, catalog-only MOBI placeholder');

  const stream = text => { const body = `BT /F1 24 Tf 50 700 Td (${text}) Tj ET`; return `<< /Length ${body.length} >>\nstream\n${body}\nendstream`; };
  const objects = [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 6 0 R >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 7 0 R >>',
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
    stream('Fixture PDF - page one'), stream('Fixture PDF - page two'),
  ];
  let pdf = '%PDF-1.4\n'; const offsets = [0];
  objects.forEach((object, i) => { offsets.push(Buffer.byteLength(pdf)); pdf += `${i + 1} 0 obj\n${object}\nendobj\n`; });
  const xref = Buffer.byteLength(pdf);
  pdf += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  pdf += offsets.slice(1).map(offset => `${String(offset).padStart(10, '0')} 00000 n \n`).join('');
  pdf += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  writeFileSync(resolve(dir, 'Fixture PDF.pdf'), pdf);
  writeFileSync(resolve(dir, 'Fixture Editions.pdf'), pdf);
}
