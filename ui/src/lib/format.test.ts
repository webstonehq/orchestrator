import { describe, expect, it } from 'vitest';
import { duration, formatNumber, formatPercent, relativeTime } from './format';

const NOW = new Date('2026-07-05T12:00:00Z');

describe('relativeTime', () => {
	it('renders sub-minute past as "just now"', () => {
		expect(relativeTime('2026-07-05T11:59:38Z', NOW)).toBe('just now');
		expect(relativeTime(NOW, NOW)).toBe('just now');
	});

	it('renders minutes ago', () => {
		expect(relativeTime('2026-07-05T11:38:00Z', NOW)).toBe('22m ago');
	});

	it('renders hours ago', () => {
		expect(relativeTime('2026-07-05T08:59:00Z', NOW)).toBe('3h ago');
	});

	it('renders days ago', () => {
		expect(relativeTime('2026-06-30T12:00:00Z', NOW)).toBe('5d ago');
	});

	it('renders future times with "in"', () => {
		expect(relativeTime('2026-07-05T12:22:00Z', NOW)).toBe('in 22m');
		expect(relativeTime('2026-07-05T12:00:30Z', NOW)).toBe('in 1m');
		expect(relativeTime('2026-07-07T12:00:00Z', NOW)).toBe('in 2d');
	});

	it('accepts Date and epoch inputs', () => {
		expect(relativeTime(new Date('2026-07-05T11:00:00Z'), NOW)).toBe('1h ago');
		expect(relativeTime(NOW.getTime() - 120_000, NOW)).toBe('2m ago');
	});

	it('renders an em dash for invalid input', () => {
		expect(relativeTime('not-a-date', NOW)).toBe('—');
	});
});

describe('duration', () => {
	it('renders zero and sub-minute values in seconds', () => {
		expect(duration(0)).toBe('0s');
		expect(duration(45)).toBe('45s');
		expect(duration(59)).toBe('59s');
	});

	it('renders minutes and seconds', () => {
		expect(duration(60)).toBe('1m 0s');
		expect(duration(192)).toBe('3m 12s');
		expect(duration(3599)).toBe('59m 59s');
	});

	it('renders hours and minutes', () => {
		expect(duration(3600)).toBe('1h 0m');
		expect(duration(3845)).toBe('1h 4m');
		expect(duration(7 * 3600 + 30 * 60)).toBe('7h 30m');
	});

	it('rounds fractional seconds, carrying across unit boundaries', () => {
		expect(duration(59.6)).toBe('1m 0s');
		expect(duration(0.4)).toBe('0s');
	});

	it('renders an em dash for negative or non-finite values', () => {
		expect(duration(-1)).toBe('—');
		expect(duration(Number.NaN)).toBe('—');
		expect(duration(Number.POSITIVE_INFINITY)).toBe('—');
	});
});

describe('formatNumber', () => {
	it('adds thousands separators', () => {
		expect(formatNumber(3190)).toBe('3,190');
		expect(formatNumber(1_234_567)).toBe('1,234,567');
		expect(formatNumber(0)).toBe('0');
	});

	it('renders an em dash for non-finite values', () => {
		expect(formatNumber(Number.NaN)).toBe('—');
	});
});

describe('formatPercent', () => {
	it('formats fractions with one decimal by default', () => {
		expect(formatPercent(0.984)).toBe('98.4%');
	});

	it('drops trailing zero decimals', () => {
		expect(formatPercent(1)).toBe('100%');
		expect(formatPercent(0)).toBe('0%');
	});

	it('renders an em dash for non-finite values', () => {
		expect(formatPercent(Number.NaN)).toBe('—');
	});
});
