// Fails if any scripts/*.ps1 lacks a UTF-8 BOM. Windows PowerShell 5.1 reads
// BOM-less files in the system code page (CP949 on Korean Windows), which
// garbles the Korean strings in these scripts.
import { readdirSync, readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const dir = dirname(fileURLToPath(import.meta.url));
const bad = readdirSync(dir)
  .filter((f) => f.toLowerCase().endsWith('.ps1'))
  .filter((f) => {
    const b = readFileSync(join(dir, f));
    return !(b[0] === 0xef && b[1] === 0xbb && b[2] === 0xbf);
  });
if (bad.length) {
  console.error('PowerShell scripts without UTF-8 BOM:\n  ' + bad.join('\n  '));
  process.exit(1);
}
console.log('check-ps-bom: all scripts/*.ps1 have a UTF-8 BOM');
