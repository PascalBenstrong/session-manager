// Charts the performance checks of a CI run (or a local one): how much more each thing costs
// when the inactive sessions hold 10× the saved tabs, against each check's limit.
//
//   node tools/perf-report.mjs results/*.json [--baseline dir] [--svg chart.svg] >> "$GITHUB_STEP_SUMMARY"
//
// Input: the files tools/perf-test.mjs and tools/perf-real-test.mjs write with SM_PERF_OUT.
// Output (stdout): Markdown for the job summary — a Mermaid chart per suite and a table of
// every check (the table is the accessible view, and has every number). --svg writes one
// chart of all suites (light and dark, tooltips), uploaded as an artifact. --baseline: the
// results of the last run on main, shown next to this run's in the table.
import { readFileSync, writeFileSync, readdirSync, existsSync } from 'node:fs';
import { join } from 'node:path';

const args = process.argv.slice(2);
const opt = (name) => {
  const i = args.indexOf(name);
  return i >= 0 ? args.splice(i, 2)[1] : null;
};
const svgOut = opt('--svg');
const baselineDir = opt('--baseline');
const read = (f) => {
  try {
    return JSON.parse(readFileSync(f, 'utf8'));
  } catch {
    return null;
  }
};
const runs = args.map(read).filter(Boolean);
const baseline = new Map(
  baselineDir && existsSync(baselineDir)
    ? readdirSync(baselineDir).filter((f) => f.endsWith('.json')).map((f) => read(join(baselineDir, f))).filter(Boolean).map((r) => [r.suite, r])
    : [],
);

// Colors: the reference palette's first three categorical slots (validated all-pairs, light
// and dark), assigned by suite — never by rank — so a suite keeps its color whichever ran.
const SLOTS = [
  { light: '#2a78d6', dark: '#3987e5' }, // mock
  { light: '#eb6834', dark: '#d95926' }, // Chrome / Edge
  { light: '#1baf7a', dark: '#199e70' }, // Firefox
];
const slotOf = (suite) => (/mock/i.test(suite) ? 0 : /firefox/i.test(suite) ? 2 : /chrome|edge/i.test(suite) ? 1 : 0);
const fmt = (x) => (Number.isFinite(x) ? `${x.toFixed(2)}×` : '–');
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]);

// ------------------------------------------------------------------ Markdown

const all = runs.flatMap((r) => r.checks);
const failed = all.filter((c) => !c.ok);
const out = [];
out.push('## Performance');
out.push('');
out.push(
  !runs.length
    ? '**⚠️ No performance results:** the performance tests did not run (see the steps above).'
    : failed.length
    ? `**❌ ${failed.length} of ${all.length} checks failed** — ${failed.map((c) => c.label || c.name).join(', ')}.`
    : `**✅ All ${all.length} checks passed.**`,
);
out.push('');
out.push('Each run is done twice: the same sessions, with the inactive ones holding **10× the saved tabs** the second time. A ratio is the second run ÷ the first: 1× means no growth; a cost proportional to all tabs would be about 10×.');
out.push('');
for (const run of runs) {
  const scaling = run.checks.filter((c) => Number.isFinite(c.ratio));
  out.push(`### ${run.suite}`);
  out.push('');
  if (scaling.length) {
    const color = SLOTS[slotOf(run.suite)].light;
    const max = Math.max(2.5, ...scaling.map((c) => Math.max(c.ratio, c.limit ?? 0) * 1.1));
    out.push('```mermaid');
    out.push('---');
    out.push('config:');
    out.push('  themeVariables:');
    out.push('    xyChart:');
    out.push(`      plotColorPalette: "${color}, #898781"`);
    out.push('---');
    out.push('xychart-beta horizontal');
    out.push(`  title "${run.suite}: large ÷ small (bars) and limit (line)"`);
    out.push(`  x-axis [${scaling.map((c) => `"${(c.label || c.name).replace(/"/g, "'")}"`).join(', ')}]`);
    out.push(`  y-axis "large ÷ small" 0 --> ${max.toFixed(1)}`);
    out.push(`  bar [${scaling.map((c) => c.ratio.toFixed(2)).join(', ')}]`);
    out.push(`  line [${scaling.map((c) => (c.limit ?? 2).toFixed(2)).join(', ')}]`);
    out.push('```');
    out.push('');
  }
  const base = baseline.get(run.suite);
  const hasBase = !!base;
  out.push(`| Check | Small → large | Ratio | Limit |${hasBase ? ' Last on main |' : ''} Result |`);
  out.push(`|---|---|---:|---:|${hasBase ? '---:|' : ''}:---:|`);
  for (const c of run.checks) {
    const prev = base?.checks.find((b) => b.name === c.name);
    const prevCell = hasBase ? ` ${prev && Number.isFinite(prev.ratio) ? fmt(prev.ratio) : '–'} |` : '';
    out.push(`| ${esc(c.name)} | ${esc(c.detail)} | ${fmt(c.ratio)} | ${Number.isFinite(c.limit) ? `< ${fmt(c.limit)}` : '–'} |${prevCell} ${c.ok ? '✅ pass' : '❌ fail'} |`);
  }
  out.push('');
}
if (svgOut) out.push('The chart of all suites is in the run\'s **performance** artifact (`perf-chart.svg`).');
console.log(out.join('\n'));

// ------------------------------------------------------------------ SVG

if (svgOut) writeFileSync(svgOut, svg(runs));

function svg(runs) {
  // Rows: every scaling check, in the order first seen; a bar per suite that has it.
  const labels = [];
  for (const r of runs) for (const c of r.checks) if (Number.isFinite(c.ratio) && !labels.includes(c.label || c.name)) labels.push(c.label || c.name);
  const suites = runs.map((r) => ({ name: r.suite, slot: slotOf(r.suite), checks: new Map(r.checks.filter((c) => Number.isFinite(c.ratio)).map((c) => [c.label || c.name, c])) }));
  const maxRatio = Math.max(2.5, ...suites.flatMap((s) => [...s.checks.values()].map((c) => Math.max(c.ratio, c.limit ?? 0))));
  const max = Math.ceil(maxRatio * 1.1 * 2) / 2;

  const W = 820;
  const left = 190; // label column
  const right = 120; // room for value labels
  const x0 = left;
  const x1 = W - right;
  const x = (v) => x0 + (Math.min(v, max) / max) * (x1 - x0);
  const BAR = 14;
  const GAP = 2;
  const ROW_PAD = 14;
  const top = 92;
  const rowH = (n) => n * BAR + (n - 1) * GAP + ROW_PAD;
  const parts = [];
  let y = top;

  // Gridlines every 0.5×, the 1× reference stronger.
  const ticks = [];
  for (let v = 0; v <= max + 1e-9; v += 0.5) ticks.push(v);
  const rows = labels.map((label) => {
    const present = suites.filter((s) => s.checks.has(label));
    const h = rowH(present.length);
    const row = { label, present, y, h };
    y += h;
    return row;
  });
  const bottom = y;

  for (const v of ticks) {
    const cls = v === 1 ? 'ref' : 'grid';
    parts.push(`<line class="${cls}" x1="${x(v)}" x2="${x(v)}" y1="${top - 6}" y2="${bottom}"/>`);
    parts.push(`<text class="tick" x="${x(v)}" y="${bottom + 16}" text-anchor="middle">${v}×</text>`);
  }
  parts.push(`<line class="axis" x1="${x0}" x2="${x0}" y1="${top - 6}" y2="${bottom}"/>`);

  for (const row of rows) {
    const midY = row.y + (row.h - ROW_PAD) / 2;
    parts.push(`<text class="label" x="${x0 - 10}" y="${midY + 4}" text-anchor="end">${esc(row.label)}</text>`);
    let by = row.y;
    for (const s of row.present) {
      const c = s.checks.get(row.label);
      const end = x(c.ratio);
      const w = Math.max(end - x0, 2);
      const r = Math.min(4, w);
      // Square at the baseline, 4px round at the data end.
      const d = `M${x0},${by} H${x0 + w - r} Q${x0 + w},${by} ${x0 + w},${by + r} V${by + BAR - r} Q${x0 + w},${by + BAR} ${x0 + w - r},${by + BAR} H${x0} Z`;
      const tip = `${s.name} — ${row.label}: ${fmt(c.ratio)} (limit ${fmt(c.limit)}) · ${c.detail}${c.ok ? '' : ' · FAILED'}`;
      parts.push(`<g class="bar"><title>${esc(tip)}</title><path class="s${s.slot}" d="${d}"/><rect class="hit" x="${x0}" y="${by - 1}" width="${x1 - x0}" height="${BAR + 2}"/></g>`);
      const over = c.ok ? '' : '<tspan class="fail"> ✕ over limit</tspan>';
      parts.push(`<text class="value" x="${x0 + w + 6}" y="${by + BAR - 3}">${fmt(c.ratio)}${over}</text>`);
      by += BAR + GAP;
    }
    // The check's limit across its bars.
    const lim = row.present.map((s) => s.checks.get(row.label).limit).find(Number.isFinite);
    if (Number.isFinite(lim)) {
      parts.push(`<line class="limit" x1="${x(lim)}" x2="${x(lim)}" y1="${row.y - 3}" y2="${row.y + row.h - ROW_PAD + 3}"><title>Limit ${fmt(lim)}</title></line>`);
    }
  }

  // Legend: every suite, swatch + name (identity never by color alone: the table names them too).
  const legend = [];
  let lx = left;
  for (const s of suites) {
    legend.push(`<rect class="s${s.slot}" x="${lx}" y="56" width="10" height="10" rx="2"/><text class="legend" x="${lx + 16}" y="65">${esc(s.name)}</text>`);
    lx += 26 + s.name.length * 7;
  }
  legend.push(`<line class="limit" x1="${lx}" x2="${lx}" y1="54" y2="68"/><text class="legend" x="${lx + 8}" y="65">limit</text>`);

  const failedCount = suites.reduce((n, s) => n + [...s.checks.values()].filter((c) => !c.ok).length, 0);
  const H = bottom + 34;
  const title = 'Cost at 10× the saved tabs, relative to the small run';
  const sub = `Bars: large run ÷ small run (1× = no growth). Dashed: each check's limit. ${failedCount ? `${failedCount} over the limit.` : 'All within their limits.'}`;
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" role="img" aria-label="${esc(title)}">
<desc>${esc(sub)} ${esc(rows.map((r) => `${r.label}: ${r.present.map((s) => `${s.name} ${fmt(s.checks.get(r.label).ratio)}`).join(', ')}`).join('; '))}</desc>
<style>
  svg { font: 13px system-ui, -apple-system, "Segoe UI", sans-serif; font-variant-numeric: tabular-nums; }
  .bg { fill: #fcfcfb; }
  .title { fill: #0b0b0b; font-size: 16px; font-weight: 600; }
  .sub, .legend { fill: #52514e; font-size: 12px; }
  .label { fill: #0b0b0b; }
  .value { fill: #52514e; font-size: 12px; }
  .fail { fill: #0b0b0b; font-weight: 600; }
  .tick { fill: #898781; font-size: 11px; }
  .grid { stroke: #e1e0d9; stroke-width: 1; }
  .ref { stroke: #c3c2b7; stroke-width: 1; }
  .axis { stroke: #c3c2b7; stroke-width: 1; }
  .limit { stroke: #0b0b0b; stroke-width: 2; stroke-dasharray: 3 2; }
  .hit { fill: transparent; }
  .bar:hover path { opacity: 0.85; }
  .s0 { fill: ${SLOTS[0].light}; } .s1 { fill: ${SLOTS[1].light}; } .s2 { fill: ${SLOTS[2].light}; }
  @media (prefers-color-scheme: dark) {
    .bg { fill: #1a1a19; }
    .title, .label, .fail { fill: #f0efec; }
    .sub, .legend, .value { fill: #c3c2b7; }
    .grid { stroke: #2c2c2a; } .ref, .axis { stroke: #383835; }
    .limit { stroke: #f0efec; }
    .s0 { fill: ${SLOTS[0].dark}; } .s1 { fill: ${SLOTS[1].dark}; } .s2 { fill: ${SLOTS[2].dark}; }
  }
</style>
<rect class="bg" width="${W}" height="${H}"/>
<text class="title" x="${left}" y="26">${esc(title)}</text>
<text class="sub" x="${left}" y="44">${esc(sub)}</text>
${legend.join('\n')}
${parts.join('\n')}
</svg>
`;
}
