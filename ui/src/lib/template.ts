/**
 * Template expression parser — TypeScript port of the Rust grammar in
 * `src/expr/{mod,parse}.rs`. The Rust implementation is ground truth; this
 * file mirrors its structure (peek/eat/expect/skipWs over an index) so
 * future grammar changes diff cleanly between the two.
 *
 * Grammar (v1):
 * ```text
 * expr    = path { "|" filter }
 * path    = "now()" | ident { "." ident | "[" uint "]" }
 * ident   = (ALPHA | "_") { ALPHA | DIGIT | "_" }
 * filter  = "dateAdd" "(" int "," "'" unit "'" ")"
 * unit    = "DAYS" | "HOURS" | "MINUTES"
 * ```
 *
 * A template is a sequence of literal text and `{{ <expr> }}` segments.
 * `{`, `}`, and `}}` alone in text are literal; `{{` always starts an
 * expression. Whitespace inside `{{ }}` is insignificant; `serialize`
 * canonicalizes to the single-space form `{{ path | filter(args) }}` and
 * numeric indices lose leading zeros (`[007]` -> `[7]`).
 *
 * Offsets: the Rust parser reports byte offsets; here we scan JS strings by
 * UTF-16 code unit, so offsets are code-unit offsets. All grammar-significant
 * characters (`{`, `}`, idents, digits, quotes) are ASCII and never occur
 * inside a surrogate pair or multi-code-unit sequence, so the scan logic is
 * identical; only the numeric value of offsets after non-ASCII text differs.
 */

export type Token =
	| { kind: 'text'; value: string }
	| { kind: 'ref'; path: string; filters: Filter[] };

export type Filter = { name: 'dateAdd'; n: number; unit: 'DAYS' | 'HOURS' | 'MINUTES' };

/** Error from parsing a template. `offset` is the code-unit offset. */
export class TemplateError extends Error {
	offset?: number;

	constructor(message: string, offset?: number) {
		super(message);
		this.name = 'TemplateError';
		this.offset = offset;
	}
}

function errAt(message: string, offset: number): TemplateError {
	return new TemplateError(message, offset);
}

/** Parse a template into text and `{{ expr }}` tokens. Throws TemplateError. */
export function parse(template: string): Token[] {
	const tokens: Token[] = [];
	let text = '';
	let i = 0;

	while (i < template.length) {
		if (template[i] === '{' && template[i + 1] === '{') {
			if (text.length > 0) {
				tokens.push({ kind: 'text', value: text });
				text = '';
			}
			const p = new Parser(template, i + 2, i);
			tokens.push(p.parseExpr());
			i = p.pos;
		} else {
			text += template[i];
			i += 1;
		}
	}

	if (text.length > 0) {
		tokens.push({ kind: 'text', value: text });
	}
	return tokens;
}

/**
 * Serialize tokens back to a template string in canonical form.
 * `serialize(parse(t))` yields the canonical spelling of `t` and is a
 * fixed point.
 */
export function serialize(tokens: Token[]): string {
	let out = '';
	for (const token of tokens) {
		if (token.kind === 'text') {
			out += token.value;
		} else {
			out += refDisplay(token);
		}
	}
	return out;
}

/** Canonical display form of a ref token: `{{ path | dateAdd(-7, 'DAYS') }}`. */
export function refDisplay(token: { path: string; filters: Filter[] }): string {
	let out = '{{ ' + token.path;
	for (const filter of token.filters) {
		out += ` | dateAdd(${filter.n}, '${filter.unit}')`;
	}
	return out + ' }}';
}

/** True when `template` parses without error. */
export function isValid(template: string): boolean {
	try {
		parse(template);
		return true;
	} catch (e) {
		if (e instanceof TemplateError) return false;
		throw e;
	}
}

const IDENT_START = /[A-Za-z_]/;
const IDENT_CONT = /[A-Za-z0-9_]/;
const DIGIT = /[0-9]/;
const WS = /[ \t\r\n]/;

class Parser {
	src: string;
	/** Current code-unit offset into `src`. */
	pos: number;
	/** Offset of the opening `{{` of the expression being parsed. */
	open: number;

	constructor(src: string, pos: number, open: number) {
		this.src = src;
		this.pos = pos;
		this.open = open;
	}

	peek(): string | undefined {
		return this.pos < this.src.length ? this.src[this.pos] : undefined;
	}

	skipWs(): void {
		while (this.pos < this.src.length && WS.test(this.src[this.pos])) {
			this.pos += 1;
		}
	}

	eat(ch: string): boolean {
		if (this.peek() === ch) {
			this.pos += 1;
			return true;
		}
		return false;
	}

	expect(ch: string, what: string): void {
		if (!this.eat(ch)) {
			throw errAt(`expected ${what}`, this.pos);
		}
	}

	parseExpr(): Token {
		this.skipWs();
		if (this.atClose() || this.peek() === undefined) {
			if (this.atClose()) {
				this.pos += 2;
				throw errAt("empty expression in '{{ }}'", this.open);
			}
			throw errAt("unclosed '{{'", this.open);
		}

		const path = this.parsePath();
		this.skipWs();

		const filters: Filter[] = [];
		while (this.eat('|')) {
			this.skipWs();
			filters.push(this.parseFilter());
			this.skipWs();
		}

		if (this.atClose()) {
			this.pos += 2;
			return { kind: 'ref', path, filters };
		} else if (this.peek() === undefined) {
			throw errAt("unclosed '{{'", this.open);
		} else {
			throw errAt("expected '}}'", this.pos);
		}
	}

	atClose(): boolean {
		return this.src.slice(this.pos, this.pos + 2) === '}}';
	}

	parseIdent(): string {
		const start = this.pos;
		const first = this.peek();
		if (first !== undefined && IDENT_START.test(first)) {
			this.pos += 1;
		} else {
			throw errAt(
				"expected identifier (letters, digits, '_'; must not start with a digit)",
				this.pos
			);
		}
		let ch: string | undefined;
		while ((ch = this.peek()) !== undefined && IDENT_CONT.test(ch)) {
			this.pos += 1;
		}
		return this.src.slice(start, this.pos);
	}

	/** Parse a reference path and return its canonical spelling. */
	parsePath(): string {
		const headStart = this.pos;
		const head = this.parseIdent();

		// Function-call head: only `now()` exists in v1.
		if (this.eat('(')) {
			if (head !== 'now') {
				throw errAt(`unknown function: ${head} (only 'now()' is supported)`, headStart);
			}
			this.skipWs();
			this.expect(')', "')' after 'now('");
			return 'now()';
		}

		let path = head;
		for (;;) {
			if (this.eat('.')) {
				path += '.' + this.parseIdent();
			} else if (this.eat('[')) {
				const index = this.parseUint();
				this.expect(']', "']' after array index");
				path += '[' + index.toString() + ']';
			} else {
				break;
			}
		}
		return path;
	}

	parseUint(): number {
		const start = this.pos;
		let ch: string | undefined;
		while ((ch = this.peek()) !== undefined && DIGIT.test(ch)) {
			this.pos += 1;
		}
		if (this.pos === start) {
			throw errAt('expected array index (unsigned integer)', start);
		}
		const n = Number(this.src.slice(start, this.pos));
		if (!Number.isSafeInteger(n)) {
			throw errAt('array index out of range', start);
		}
		return n;
	}

	parseInt(): number {
		const start = this.pos;
		if (this.peek() === '-') {
			this.pos += 1;
		}
		const digitsStart = this.pos;
		let ch: string | undefined;
		while ((ch = this.peek()) !== undefined && DIGIT.test(ch)) {
			this.pos += 1;
		}
		if (this.pos === digitsStart) {
			throw errAt('expected integer', start);
		}
		const n = Number(this.src.slice(start, this.pos));
		if (!Number.isSafeInteger(n)) {
			throw errAt('integer out of range', start);
		}
		return n;
	}

	parseFilter(): Filter {
		const nameStart = this.pos;
		const name = this.parseIdent();
		if (name !== 'dateAdd') {
			throw errAt(`unknown filter: ${name} (only 'dateAdd' is supported)`, nameStart);
		}
		this.skipWs();
		this.expect('(', "'(' after filter name");
		this.skipWs();
		const n = this.parseInt();
		this.skipWs();
		this.expect(',', "',' between dateAdd arguments");
		this.skipWs();
		const unit = this.parseDateUnit();
		this.skipWs();
		this.expect(')', "')' after filter arguments");
		return { name: 'dateAdd', n, unit };
	}

	parseDateUnit(): 'DAYS' | 'HOURS' | 'MINUTES' {
		const quoteStart = this.pos;
		this.expect("'", "single-quoted unit ('DAYS', 'HOURS', or 'MINUTES')");
		const start = this.pos;
		let ch: string | undefined;
		while ((ch = this.peek()) !== undefined && ch !== "'") {
			this.pos += 1;
		}
		if (this.peek() === undefined) {
			throw errAt('unterminated unit string', quoteStart);
		}
		const unit = this.src.slice(start, this.pos);
		this.pos += 1; // closing quote
		switch (unit) {
			case 'DAYS':
			case 'HOURS':
			case 'MINUTES':
				return unit;
			default:
				throw errAt(
					`unknown date unit: '${unit}' (expected 'DAYS', 'HOURS', or 'MINUTES')`,
					quoteStart
				);
		}
	}
}
