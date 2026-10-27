'use strict';

const { createHash } = require('node:crypto');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');

const ROOT = join(__dirname, '..', '..', '..');
const SHARED_DISTRIBUTION_FILES = new Set([
  'LiberationMono-Regular.ttf',
  'LiberationSans-Bold.ttf',
  'LiberationSans-BoldItalic.ttf',
  'LiberationSans-Italic.ttf',
  'LiberationSans-Regular.ttf',
]);

const FAMILIES = {
  Carlito: { directory: 'carlito', version: 'Version 1.104' },
  Caladea: { directory: 'caladea', version: 'Version 1.001' },
  'Liberation Sans': { directory: 'liberation', version: 'Version 2.1.5' },
  'Liberation Serif': { directory: 'liberation', version: 'Version 2.1.5' },
  'Liberation Mono': { directory: 'liberation', version: 'Version 2.1.5' },
};

function targetFamily(requested) {
  const family = requested.trim().toLocaleLowerCase('en-US');
  if (family === 'calibri' || family === 'calibri light' || family === 'carlito') return 'Carlito';
  if (family === 'cambria' || family === 'caladea' || family === 'cambria math') return 'Caladea';
  if (['times new roman', 'times', 'georgia', 'serif', 'liberation serif'].includes(family)) {
    return 'Liberation Serif';
  }
  if (['courier new', 'courier', 'consolas', 'liberation mono'].includes(family)) {
    return 'Liberation Mono';
  }
  return family === 'carlito' ? 'Carlito' : 'Liberation Sans';
}

function styleFor(request) {
  const bold = request.weight >= 600;
  if (bold && request.italic) return 'BoldItalic';
  if (bold) return 'Bold';
  if (request.italic) return 'Italic';
  return 'Regular';
}

function fontFile(family, style) {
  const stem = family.replaceAll(' ', '');
  return `${stem}-${style}.ttf`;
}

function fullName(family, style) {
  if (style === 'Regular' && family.startsWith('Liberation ')) return family;
  return `${family} ${style.replace('BoldItalic', 'Bold Italic')}`;
}

function postscriptName(family, style) {
  const stem = family.replaceAll(' ', '');
  return style === 'Regular' && family.startsWith('Liberation ') ? stem : `${stem}-${style}`;
}

function artifactFor(request) {
  const family = targetFamily(request.family);
  const descriptor = FAMILIES[family];
  if (!descriptor) return undefined;
  const style = styleFor(request);
  const file = fontFile(family, style);
  const path = SHARED_DISTRIBUTION_FILES.has(file)
    ? join(ROOT, 'packages', 'doc-yantra-fonts', 'fonts', file)
    : join(ROOT, 'office-core', 'fonts', descriptor.directory, file);
  const bytes = readFileSync(path);
  return {
    family,
    fullName: fullName(family, style),
    postscriptName: postscriptName(family, style),
    weight: request.weight >= 600 ? 700 : 400,
    italic: request.italic,
    stretch: 5,
    version: descriptor.version,
    faceIndex: 0,
    format: 'ttf',
    sha256: createHash('sha256').update(bytes).digest('hex'),
    source: 'deterministic_test_fixture',
    license: { id: 'OFL-1.1', embeddable: true, subsettable: true },
    bytes,
  };
}

function createMockOfficeFontProvider() {
  return {
    id: 'doc-yantra-test-fonts-v1',
    cacheKey(request) {
      const artifact = artifactFor(request);
      return artifact?.sha256 ?? `missing:${request.family}`;
    },
    async resolve(requests, signal) {
      if (signal.aborted) throw signal.reason;
      return requests.map((request) => {
        const artifact = artifactFor(request);
        return artifact
          ? { status: 'found', request, artifact }
          : { status: 'not_found', request };
      });
    },
  };
}

module.exports = { createMockOfficeFontProvider };
