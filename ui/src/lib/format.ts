// Small formatting helpers shared across screens.

/**
 * Relative time like "22m ago", "3h ago", "5d ago". Sub-minute past times
 * render "just now"; future times render "in 22m" (sub-minute: "in 1m").
 * Invalid input renders an em dash.
 */
export function relativeTime(input: string | number | Date, now: Date = new Date()): string {
	const date = input instanceof Date ? input : new Date(input);
	const time = date.getTime();
	if (Number.isNaN(time)) return '—';

	let diff = Math.round((now.getTime() - time) / 1000);
	const future = diff < 0;
	diff = Math.abs(diff);

	if (!future && diff < 60) return 'just now';

	let text: string;
	if (diff < 60) text = '1m';
	else if (diff < 3600) text = `${Math.floor(diff / 60)}m`;
	else if (diff < 86400) text = `${Math.floor(diff / 3600)}h`;
	else text = `${Math.floor(diff / 86400)}d`;

	return future ? `in ${text}` : `${text} ago`;
}

/**
 * Duration from seconds: "0s", "45s", "3m 12s", "1h 4m".
 * Negative / NaN inputs render an em dash. Fractions are rounded.
 */
export function duration(seconds: number): string {
	if (!Number.isFinite(seconds) || seconds < 0) return '—';
	const total = Math.round(seconds);
	if (total < 60) return `${total}s`;
	if (total < 3600) return `${Math.floor(total / 60)}m ${total % 60}s`;
	const h = Math.floor(total / 3600);
	const m = Math.floor((total % 3600) / 60);
	return `${h}h ${m}m`;
}

/** Integer with thousands separators: 3190 -> "3,190". */
export function formatNumber(n: number): string {
	if (!Number.isFinite(n)) return '—';
	return new Intl.NumberFormat('en-US').format(n);
}

/** Fraction (0..1) as a percentage: 0.984 -> "98.4%", 1 -> "100%". */
export function formatPercent(fraction: number, digits = 1): string {
	if (!Number.isFinite(fraction)) return '—';
	const value = (fraction * 100).toFixed(digits).replace(/\.0+$/, '');
	return `${value}%`;
}
