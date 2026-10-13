// Reproducible icon assets from the existing Synthius vector artwork.
// Keep the two solid logo contours; omit the raster-trace edge fragments.
import fs from 'node:fs/promises';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(path.join(root, 'package.json'));
const sharp = require('sharp');
const out = path.join(root, 'src-tauri/icons');
const original = await fs.readFile(path.join(root, 'assets/synthius-logo.svg'), 'utf8');
const contours = [...original.matchAll(/<path\b[^>]*>/g)].slice(0, 2).map(m => m[0].replace(/fill="[^"]*"/, 'fill="currentColor"'));
if (contours.length !== 2) throw Error('Expected the original two solid Synthius logo contours');
const glyph = contours.join('\n');
const svg = (color, background = false) => `<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 2048 2048" color="${color}">${background ? '<circle cx="1024" cy="1024" r="1000" fill="#181818"/>' : ''}<g transform="translate(-12 -14) translate(1024 1024) scale(1.02) translate(-1024 -1024)">${glyph}</g></svg>\n`;
const brand = svg('#ffffff');
const app = svg('#ffffff', true);
await fs.mkdir(path.join(out, 'tray'), { recursive: true });
await fs.writeFile(path.join(out, 'brand.svg'), brand);
await fs.writeFile(path.join(out, 'app.svg'), app);

async function render(source, size) {
  // Supersampling gives smooth fractional coverage even for a 16px glyph.
  return sharp(Buffer.from(source), { density: 72 * (size * 8 / 512) })
    .resize(size, size, { kernel: 'lanczos3' }).ensureAlpha().png().toBuffer();
}
function ico(images) {
  const header = Buffer.alloc(6 + images.length * 16);
  header.writeUInt16LE(1, 2); header.writeUInt16LE(images.length, 4);
  let offset = header.length;
  images.forEach(({size, data}, i) => {
    const at = 6 + i * 16;
    header[at] = header[at + 1] = size === 256 ? 0 : size;
    header.writeUInt16LE(1, at + 4); header.writeUInt16LE(32, at + 6);
    header.writeUInt32LE(data.length, at + 8); header.writeUInt32LE(offset, at + 12);
    offset += data.length;
  });
  return Buffer.concat([header, ...images.map(image => image.data)]);
}
const smallSizes = [16, 20, 24, 32, 40, 48, 64];
for (const color of ['white', 'black']) {
  const source = svg(color === 'white' ? '#ffffff' : '#181818');
  const images = [];
  for (const size of smallSizes) {
    const data = await render(source, size);
    await fs.writeFile(path.join(out, 'tray', `${color}-${size}x${size}.png`), data);
    images.push({size, data});
    // Existing portable fallback uses the monochrome white glyph.
    if (color === 'white') await fs.writeFile(path.join(out, 'tray', `${size}x${size}.png`), data);
  }
  await fs.writeFile(path.join(out, 'tray', `${color}.ico`), ico(images));
}
const appImages = [];
for (const size of [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]) appImages.push({size, data:await render(app, size)});
await fs.writeFile(path.join(out, 'icon.ico'), ico(appImages));
for (const size of [32, 64, 128, 256, 512]) {
  const name = size === 512 ? 'icon.png' : size === 256 ? '128x128@2x.png' : `${size}x${size}.png`;
  await fs.writeFile(path.join(out, name), await render(app, size));
}
console.log('Synthius app and monochrome tray assets regenerated at native Windows sizes.');
