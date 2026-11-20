// Runs tools/idb-conformance.js against the in-memory fake (Node) and prints the
// observations as JSON (tools/idb-real-test.mjs compares them with real browsers).
//
//   node tools/idb-node-check.mjs
import { createFakeIndexedDB } from './fake-indexeddb.js';
import { idbConformance } from './idb-conformance.js';
import { createDb } from '../web/platform-common.js';

export async function fakeObservations() {
  return idbConformance(createFakeIndexedDB(), createDb);
}

if (import.meta.url === `file:///${process.argv[1].replace(/\\/g, '/')}` || process.argv[1]?.endsWith('idb-node-check.mjs')) {
  const timer = setTimeout(() => {
    console.error('timed out (a transaction never finished)');
    process.exit(1);
  }, 10000);
  const log = await fakeObservations();
  clearTimeout(timer);
  for (const [name, value] of log) console.log(`${name}: ${JSON.stringify(value)}`);
}
