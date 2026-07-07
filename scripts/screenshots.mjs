// Regenerate the README UI screenshots in docs/screenshots/.
//
// Prerequisites (see docs/screenshots README note / project memory):
//   1. A server running at ORCH_URL (default http://127.0.0.1:4400).
//   2. Demo data loaded — run `bash scripts/demo.sh` once against a FRESH
//      database so the run-view shot is run #1 (9 success / 3 dropped 404s).
//      demo.sh also creates the `demo` admin used below.
//
// The API and UI are gated by a login, so this script authenticates first: on a
// fresh server it creates the admin via first-run setup, otherwise it signs in.
// Credentials default to the demo account created by demo.sh; override with
// ORCH_USER / ORCH_PASSWORD.
//
// Playwright is intentionally NOT a project dependency. This resolves it from an
// installed package if present, else from the npx cache
// (~/.npm/_npx/*/node_modules/playwright). If neither exists, run it once via
// `npx playwright@1.61.1 install chromium` and re-run.
//
// Usage:  node scripts/screenshots.mjs           (writes docs/screenshots/*.png)
//         OUT_DIR=/tmp/shots node scripts/screenshots.mjs

import { existsSync, readdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const BASE = (process.env.ORCH_URL || 'http://127.0.0.1:4400').replace(/\/$/, '');
const USER = process.env.ORCH_USER || 'demo';
const PASS = process.env.ORCH_PASSWORD || 'demo-password';
const OUT_DIR = process.env.OUT_DIR || 'docs/screenshots';
const FLOW_ID = process.env.FLOW_ID || 'demo_civic_minutes';
const RUN_ID = process.env.RUN_ID || '1';

/** Load Playwright from a dependency or the npx cache. */
async function loadPlaywright() {
	try {
		return await import('playwright');
	} catch {
		/* not installed as a dep — try the npx cache */
	}
	const npx = join(homedir(), '.npm', '_npx');
	if (existsSync(npx)) {
		for (const dir of readdirSync(npx)) {
			const p = join(npx, dir, 'node_modules', 'playwright', 'index.js');
			if (existsSync(p)) return await import(pathToFileURL(p).href);
		}
	}
	throw new Error(
		'Playwright not found. Install chromium once with ' +
			'`npx playwright@1.61.1 install chromium` (and run any `npx playwright` ' +
			'command so it lands in the npx cache), then re-run this script.'
	);
}

const pw = await loadPlaywright();
// Playwright's package is CommonJS; via dynamic import its exports may land on
// the module namespace or under `.default` depending on how it was resolved.
const chromium = pw.chromium ?? pw.default?.chromium;
if (!chromium) throw new Error('could not find the chromium export in playwright');

const browser = await chromium.launch();
// Dark theme only; retina-scale for crisp docs images.
const context = await browser.newContext({
	colorScheme: 'dark',
	deviceScaleFactor: 2,
	viewport: { width: 1440, height: 900 }
});

// Authenticate. context.request shares cookie storage with pages in this
// context, so the session cookie set here authenticates the captures below.
const setupRes = await context.request.get(`${BASE}/api/auth/setup`);
if (!setupRes.ok()) {
	throw new Error(`GET /api/auth/setup failed: ${setupRes.status()} ${await setupRes.text()}`);
}
const needsSetup = (await setupRes.json()).needed;
const authPath = needsSetup ? '/api/auth/setup' : '/api/auth/login';
const authRes = await context.request.post(`${BASE}${authPath}`, {
	data: { username: USER, password: PASS }
});
if (!authRes.ok()) {
	throw new Error(
		`authentication failed (${authPath}): ${authRes.status()} ${await authRes.text()}\n` +
			'For an already-configured server, set ORCH_USER / ORCH_PASSWORD.'
	);
}
console.log(`authenticated as ${USER} (${needsSetup ? 'created via setup' : 'signed in'})`);

// [file, path, viewport height]. Overview pages use a short height to crop dead
// space; the builder and run view get the full 900.
const shots = [
	['dashboard.png', '/', 480],
	['flow-builder.png', `/flows/${FLOW_ID}`, 900],
	['run-view.png', `/runs/${RUN_ID}`, 900]
];

for (const [file, path, height] of shots) {
	const page = await context.newPage();
	await page.setViewportSize({ width: 1440, height });
	await page.goto(`${BASE}${path}`, { waitUntil: 'networkidle' });
	// Ensure we captured the authenticated app, not the login/onboarding gate.
	await page.waitForSelector('.sidebar', { timeout: 10000 });
	const out = join(OUT_DIR, file);
	await page.screenshot({ path: out });
	console.log(`wrote ${out}`);
	await page.close();
}

await browser.close();
console.log('done');
