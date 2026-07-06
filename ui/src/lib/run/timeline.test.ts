import { describe, expect, it } from 'vitest';
import { barGeometry, tickLabel, timelineTicks } from './timeline';

describe('timelineTicks', () => {
	it('produces 5 evenly spaced ticks covering the span', () => {
		const { ticks, axisSec } = timelineTicks(118);
		expect(ticks).toHaveLength(5);
		expect(axisSec).toBe(120);
		expect(ticks.map((t) => t.sec)).toEqual([0, 30, 60, 90, 120]);
		expect(ticks.map((t) => t.label)).toEqual(['0s', '30s', '60s', '90s', '120s']);
	});

	it('picks the smallest nice step that covers the span', () => {
		expect(timelineTicks(3).axisSec).toBe(4); // step 1
		expect(timelineTicks(4).axisSec).toBe(4);
		expect(timelineTicks(5).axisSec).toBe(8); // step 2
		expect(timelineTicks(21).axisSec).toBe(40); // step 10
		expect(timelineTicks(121).axisSec).toBe(240); // step 60
	});

	it('handles degenerate and huge spans', () => {
		expect(timelineTicks(0).axisSec).toBe(4);
		expect(timelineTicks(-5).axisSec).toBe(4);
		const huge = timelineTicks(10 * 86400); // 10 days -> step rounded to whole days
		expect(huge.axisSec % 86400).toBe(0);
		expect(huge.axisSec).toBeGreaterThanOrEqual(10 * 86400);
	});

	it('labels minute and hour ticks in short form', () => {
		expect(timelineTicks(700).ticks.map((t) => t.label)).toEqual([
			'0s',
			'3m',
			'6m',
			'9m',
			'12m'
		]);
		expect(tickLabel(5400)).toBe('1h 30m');
		expect(tickLabel(7200)).toBe('2h');
		expect(tickLabel(90)).toBe('90s');
	});
});

describe('barGeometry', () => {
	const runStart = Date.parse('2026-07-05T12:00:00Z');
	const now = Date.parse('2026-07-05T12:01:00Z');

	it('positions a finished bar by run-relative offsets', () => {
		const g = barGeometry(
			'2026-07-05T12:00:30Z',
			'2026-07-05T12:01:00Z',
			runStart,
			120,
			now
		);
		expect(g).not.toBeNull();
		expect(g!.leftPct).toBeCloseTo(25);
		expect(g!.widthPct).toBeCloseTo(25);
	});

	it('extends a running bar to now', () => {
		const g = barGeometry('2026-07-05T12:00:00Z', null, runStart, 120, now);
		expect(g!.leftPct).toBe(0);
		expect(g!.widthPct).toBeCloseTo(50);
	});

	it('clamps tiny bars to a visible minimum and stays inside the axis', () => {
		const g = barGeometry('2026-07-05T12:00:00Z', '2026-07-05T12:00:00Z', runStart, 3600, now);
		expect(g!.widthPct).toBeGreaterThanOrEqual(0.75);
		const late = barGeometry('2026-07-05T13:00:00Z', null, runStart, 3600, now + 3_600_000);
		expect(late!.leftPct + late!.widthPct).toBeLessThanOrEqual(100);
	});

	it('returns null for unstarted tasks or invalid input', () => {
		expect(barGeometry(null, null, runStart, 120, now)).toBeNull();
		expect(barGeometry('garbage', null, runStart, 120, now)).toBeNull();
		expect(barGeometry('2026-07-05T12:00:00Z', null, runStart, 0, now)).toBeNull();
	});
});
