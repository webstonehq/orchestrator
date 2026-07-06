// Pure Gantt scale math for the run timeline tab.

export interface Tick {
	sec: number;
	label: string;
}

/** Nice step sizes in seconds: 1..30s, 1..40m, 1h..24h. */
const NICE_STEPS = [
	1, 2, 5, 10, 15, 30, 60, 90, 120, 180, 240, 300, 600, 900, 1200, 1800, 2400, 3600, 5400, 7200,
	10800, 14400, 21600, 43200, 86400
];

/**
 * Short human tick label. Seconds are kept whenever the value is not a whole
 * number of minutes (mock style: "0s 30s 60s 90s 120s").
 */
export function tickLabel(sec: number, secondsOnly = false): string {
	if (secondsOnly || sec < 60 || sec % 60 !== 0) return `${sec}s`;
	if (sec < 3600) return `${sec / 60}m`;
	const h = Math.floor(sec / 3600);
	const m = Math.round((sec % 3600) / 60);
	return m === 0 ? `${h}h` : `${h}h ${m}m`;
}

/**
 * Compute a 5-tick axis covering `spanSec`. The step is the smallest nice
 * value with 4*step >= span, so the axis is [0 .. 4*step] and every tick sits
 * on an even interval. Tick labels use one unit across the axis: seconds
 * while the step is sub-minute, m/h beyond. Returns the ticks plus the total
 * axis span.
 */
export function timelineTicks(spanSec: number): { ticks: Tick[]; axisSec: number } {
	const span = Math.max(spanSec, 1);
	let step = NICE_STEPS.find((s) => s * 4 >= span);
	if (step === undefined) step = Math.ceil(span / 4 / 86400) * 86400;
	const secondsOnly = step < 60;
	const ticks: Tick[] = [];
	for (let i = 0; i <= 4; i++) {
		ticks.push({ sec: i * step, label: tickLabel(i * step, secondsOnly) });
	}
	return { ticks, axisSec: step * 4 };
}

export interface BarGeometry {
	leftPct: number;
	widthPct: number;
}

/**
 * Position a task bar on the axis. `start`/`end` are RFC3339 strings; a null
 * end means "still running" and extends to `nowMs`. Returns null when the
 * task has not started or the axis is degenerate. Width is clamped to a
 * minimum of 0.75% so short tasks stay visible.
 */
export function barGeometry(
	start: string | null,
	end: string | null,
	runStartMs: number,
	axisSec: number,
	nowMs: number
): BarGeometry | null {
	if (!start || axisSec <= 0) return null;
	const startMs = new Date(start).getTime();
	if (Number.isNaN(startMs)) return null;
	const endMs = end ? new Date(end).getTime() : nowMs;
	const axisMs = axisSec * 1000;
	const leftPct = Math.max(0, ((startMs - runStartMs) / axisMs) * 100);
	const rawWidth = ((Math.max(endMs, startMs) - startMs) / axisMs) * 100;
	const widthPct = Math.min(Math.max(rawWidth, 0.75), 100 - leftPct);
	return { leftPct: Math.min(leftPct, 99.25), widthPct: Math.max(widthPct, 0.75) };
}
