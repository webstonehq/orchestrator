import { describe, expect, it } from 'vitest';
import { TemplateError, isValid, parse, refDisplay, serialize } from './template';

function parseErr(template: string): TemplateError {
	try {
		parse(template);
	} catch (e) {
		expect(e).toBeInstanceOf(TemplateError);
		return e as TemplateError;
	}
	throw new Error(`expected parse to throw for: ${template}`);
}

describe('parse / serialize round-trips', () => {
	it('canonicalizes whitespace', () => {
		expect(serialize(parse('{{inputs.x}}'))).toBe('{{ inputs.x }}');
		expect(serialize(parse('{{   inputs.x   }}'))).toBe('{{ inputs.x }}');
	});

	it('canonicalizes filters and is a fixed point', () => {
		const tokens = parse("{{now()|dateAdd(-7,'DAYS')}}");
		const canonical = serialize(tokens);
		expect(canonical).toBe("{{ now() | dateAdd(-7, 'DAYS') }}");
		// Canonical form is a fixed point.
		expect(parse(canonical)).toEqual(tokens);
		expect(serialize(parse(canonical))).toBe(canonical);
	});

	it('round-trips mixed text and refs', () => {
		const template = '{{ vars.server }}/api/x?since={{ inputs.since }}';
		const tokens = parse(template);
		expect(serialize(tokens)).toBe(template);
		expect(tokens.length).toBe(3);
	});

	it('parses text-only templates with stray braces as literal', () => {
		const tokens = parse('no templates here { } }} still text');
		expect(tokens).toEqual([{ kind: 'text', value: 'no templates here { } }} still text' }]);
	});

	it('canonicalizes indices: [007] -> [7]', () => {
		expect(serialize(parse('{{ inputs.list[007] }}'))).toBe('{{ inputs.list[7] }}');
	});

	it('parses chained indexing and canonicalizes', () => {
		const tokens = parse('{{grid[0][1]}}');
		expect(tokens).toEqual([{ kind: 'ref', path: 'grid[0][1]', filters: [] }]);
		expect(serialize(tokens)).toBe('{{ grid[0][1] }}');
	});

	it('parses paths with index into canonical spelling', () => {
		const tokens = parse('{{ outputs.discover.items[0].name }}');
		expect(tokens[0]).toEqual({
			kind: 'ref',
			path: 'outputs.discover.items[0].name',
			filters: []
		});
	});

	it('exposes filter structure', () => {
		expect(parse("{{ inputs.since | dateAdd(2, 'HOURS') }}")).toEqual([
			{ kind: 'ref', path: 'inputs.since', filters: [{ name: 'dateAdd', n: 2, unit: 'HOURS' }] }
		]);
	});

	it('parses dateAdd for all units and negative n, chained', () => {
		expect(parse("{{ inputs.t | dateAdd(-7, 'DAYS') }}")[0]).toEqual({
			kind: 'ref',
			path: 'inputs.t',
			filters: [{ name: 'dateAdd', n: -7, unit: 'DAYS' }]
		});
		expect(parse("{{ inputs.t | dateAdd(5, 'HOURS') }}")[0]).toEqual({
			kind: 'ref',
			path: 'inputs.t',
			filters: [{ name: 'dateAdd', n: 5, unit: 'HOURS' }]
		});
		expect(parse("{{ inputs.t | dateAdd(90, 'MINUTES') }}")[0]).toEqual({
			kind: 'ref',
			path: 'inputs.t',
			filters: [{ name: 'dateAdd', n: 90, unit: 'MINUTES' }]
		});
		expect(
			serialize(parse("{{ inputs.t | dateAdd(1, 'DAYS') | dateAdd(-30, 'MINUTES') }}"))
		).toBe("{{ inputs.t | dateAdd(1, 'DAYS') | dateAdd(-30, 'MINUTES') }}");
	});

	it('handles multibyte text around refs', () => {
		const template = 'héllo — {{ inputs.x }} 世界 🚀{{ vars.y }}✓';
		const tokens = parse(template);
		expect(tokens).toEqual([
			{ kind: 'text', value: 'héllo — ' },
			{ kind: 'ref', path: 'inputs.x', filters: [] },
			{ kind: 'text', value: ' 世界 🚀' },
			{ kind: 'ref', path: 'vars.y', filters: [] },
			{ kind: 'text', value: '✓' }
		]);
		expect(serialize(tokens)).toBe(template);
	});

	it('handles adjacent refs with no text between', () => {
		const tokens = parse('{{ inputs.a }}{{ inputs.b }}');
		expect(tokens).toEqual([
			{ kind: 'ref', path: 'inputs.a', filters: [] },
			{ kind: 'ref', path: 'inputs.b', filters: [] }
		]);
		expect(serialize(tokens)).toBe('{{ inputs.a }}{{ inputs.b }}');
	});

	it('serializes an empty token list to an empty string', () => {
		expect(parse('')).toEqual([]);
		expect(serialize([])).toBe('');
	});
});

describe('now()', () => {
	it('is allowed as the entire path', () => {
		expect(parse('{{ now() }}')).toEqual([{ kind: 'ref', path: 'now()', filters: [] }]);
		expect(serialize(parse('{{now(  )}}'))).toBe('{{ now() }}');
	});

	it('rejects member access after now()', () => {
		// After `now()` the path is complete; anything but `|` or `}}` errors.
		const err = parseErr('{{ now().x }}');
		expect(err.message).toContain("expected '}}'");
		expect(err.offset).toBe(8);
	});

	it('rejects unknown functions with offset of the name', () => {
		const err = parseErr('{{ upper() }}');
		expect(err.message).toContain('unknown function');
		expect(err.message).toContain('upper');
		expect(err.offset).toBe(3);
	});
});

describe('parse errors', () => {
	it('unclosed {{ carries the offset of the opener', () => {
		const err = parseErr('abc {{ inputs.x');
		expect(err.message).toContain('unclosed');
		expect(err.offset).toBe(4);

		const err2 = parseErr('abc {{');
		expect(err2.message).toContain('unclosed');
		expect(err2.offset).toBe(4);
	});

	it('empty expression', () => {
		const err = parseErr('{{ }}');
		expect(err.message).toContain('empty');
		expect(err.offset).toBe(0);

		const err2 = parseErr('{{}}');
		expect(err2.message).toContain('empty');
		expect(err2.offset).toBe(0);
	});

	it('unknown filter names the filter and carries an offset', () => {
		const err = parseErr('{{ inputs.x | upper }}');
		expect(err.message).toContain('unknown filter');
		expect(err.message).toContain('upper');
		expect(err.offset).toBe(14);
	});

	it('trailing dot in path', () => {
		const err = parseErr('{{ inputs. }}');
		expect(err.message).toContain('expected identifier');
		expect(err.offset).toBe(10);
	});

	it('leading dot in path', () => {
		const err = parseErr('{{ .inputs }}');
		expect(err.message).toContain('expected identifier');
		expect(err.offset).toBe(3);
	});

	it('non-numeric array index', () => {
		const err = parseErr('{{ inputs.x[abc] }}');
		expect(err.message).toContain('expected array index');
		expect(err.offset).toBe(12);
	});

	it('unterminated array index', () => {
		const err = parseErr('{{ inputs.x[1 }}');
		expect(err.message).toContain("']' after array index");
		expect(err.offset).toBe(13);
	});

	it('identifier starting with a digit', () => {
		const err = parseErr('{{ 9lives }}');
		expect(err.message).toContain('expected identifier');
		expect(err.offset).toBe(3);
	});

	it('unknown date unit', () => {
		const err = parseErr("{{ inputs.x | dateAdd(1, 'WEEKS') }}");
		expect(err.message).toContain('unknown date unit');
		expect(err.message).toContain('WEEKS');
		expect(err.offset).toBe(25);
	});

	it('non-integer filter argument', () => {
		const err = parseErr("{{ inputs.x | dateAdd(x, 'DAYS') }}");
		expect(err.message).toContain('expected integer');
		expect(err.offset).toBe(22);
	});

	it('unterminated unit string', () => {
		const err = parseErr("{{ inputs.x | dateAdd(1, 'DAYS");
		expect(err.message).toContain('unterminated unit string');
		expect(err.offset).toBe(25);
	});

	it('missing quote around unit', () => {
		const err = parseErr('{{ inputs.x | dateAdd(1, DAYS) }}');
		expect(err.message).toContain('single-quoted unit');
		expect(err.offset).toBe(25);
	});
});

describe('refDisplay', () => {
	it('renders canonical chip text', () => {
		expect(refDisplay({ path: 'inputs.x', filters: [] })).toBe('{{ inputs.x }}');
		expect(
			refDisplay({ path: 'inputs.x', filters: [{ name: 'dateAdd', n: -7, unit: 'DAYS' }] })
		).toBe("{{ inputs.x | dateAdd(-7, 'DAYS') }}");
	});
});

describe('isValid', () => {
	it('accepts valid templates', () => {
		expect(isValid('plain text')).toBe(true);
		expect(isValid('{{ inputs.x }}')).toBe(true);
		expect(isValid("a {{ now() | dateAdd(1, 'DAYS') }} b")).toBe(true);
		expect(isValid('stray } and }} are fine')).toBe(true);
	});

	it('rejects invalid templates', () => {
		expect(isValid('{{')).toBe(false);
		expect(isValid('{{ }}')).toBe(false);
		expect(isValid('{{ inputs. }}')).toBe(false);
		expect(isValid('{{ inputs.x | upper }}')).toBe(false);
		expect(isValid('{{ now().x }}')).toBe(false);
	});
});
