//! Hand-rolled template parser. No regex, so the grammar stays trivially
//! portable to the TypeScript chip editor in the UI.

use super::{DateUnit, ExprError, Filter, RefExpr, Segment};

/// Parse a template into text and `{{ expr }}` segments.
pub(crate) fn parse_template(template: &str) -> Result<Vec<Segment>, ExprError> {
    let bytes = template.as_bytes();
    let mut segments = Vec::new();
    let mut text = String::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'{' && bytes.get(i + 1) == Some(&b'{') {
            if !text.is_empty() {
                segments.push(Segment::Text(std::mem::take(&mut text)));
            }
            let mut p = Parser {
                src: template,
                pos: i + 2,
                open: i,
            };
            segments.push(Segment::Ref(p.parse_expr()?));
            i = p.pos;
        } else {
            // Byte-wise scan is UTF-8 safe: '{' never occurs inside a
            // multi-byte sequence. Copy the whole char to keep `text` valid.
            let ch = template[i..].chars().next().expect("in-bounds char");
            text.push(ch);
            i += ch.len_utf8();
        }
    }

    if !text.is_empty() {
        segments.push(Segment::Text(text));
    }
    Ok(segments)
}

struct Parser<'a> {
    src: &'a str,
    /// Current byte offset into `src`.
    pos: usize,
    /// Byte offset of the opening `{{` of the expression being parsed.
    open: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, byte: u8, what: &str) -> Result<(), ExprError> {
        if self.eat(byte) {
            Ok(())
        } else {
            Err(ExprError::at(format!("expected {what}"), self.pos))
        }
    }

    fn parse_expr(&mut self) -> Result<RefExpr, ExprError> {
        self.skip_ws();
        if self.at_close() || self.peek().is_none() {
            if self.at_close() {
                self.pos += 2;
                return Err(ExprError::at("empty expression in '{{ }}'", self.open));
            }
            return Err(ExprError::at("unclosed '{{'", self.open));
        }

        let path = self.parse_path()?;
        self.skip_ws();

        let mut filters = Vec::new();
        while self.eat(b'|') {
            self.skip_ws();
            filters.push(self.parse_filter()?);
            self.skip_ws();
        }

        if self.at_close() {
            self.pos += 2;
            Ok(RefExpr { path, filters })
        } else if self.peek().is_none() {
            Err(ExprError::at("unclosed '{{'", self.open))
        } else {
            Err(ExprError::at("expected '}}'", self.pos))
        }
    }

    fn at_close(&self) -> bool {
        self.src.as_bytes().get(self.pos..self.pos + 2) == Some(b"}}")
    }

    fn parse_ident(&mut self) -> Result<&'a str, ExprError> {
        let start = self.pos;
        match self.peek() {
            Some(b) if b.is_ascii_alphabetic() || b == b'_' => self.pos += 1,
            _ => {
                return Err(ExprError::at(
                    "expected identifier (letters, digits, '_'; must not start with a digit)",
                    self.pos,
                ));
            }
        }
        while matches!(self.peek(), Some(b) if b.is_ascii_alphanumeric() || b == b'_') {
            self.pos += 1;
        }
        Ok(&self.src[start..self.pos])
    }

    /// Parse a reference path and return its canonical spelling.
    fn parse_path(&mut self) -> Result<String, ExprError> {
        let head_start = self.pos;
        let head = self.parse_ident()?;

        // Function-call head: only `now()` exists in v1.
        if self.eat(b'(') {
            if head != "now" {
                return Err(ExprError::at(
                    format!("unknown function: {head} (only 'now()' is supported)"),
                    head_start,
                ));
            }
            self.skip_ws();
            self.expect(b')', "')' after 'now('")?;
            return Ok("now()".to_string());
        }

        let mut path = String::from(head);
        loop {
            if self.eat(b'.') {
                path.push('.');
                path.push_str(self.parse_ident()?);
            } else if self.eat(b'[') {
                let index = self.parse_uint()?;
                self.expect(b']', "']' after array index")?;
                path.push('[');
                path.push_str(&index.to_string());
                path.push(']');
            } else {
                break;
            }
        }
        Ok(path)
    }

    fn parse_uint(&mut self) -> Result<u64, ExprError> {
        let start = self.pos;
        while matches!(self.peek(), Some(b) if b.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(ExprError::at(
                "expected array index (unsigned integer)",
                start,
            ));
        }
        self.src[start..self.pos]
            .parse::<u64>()
            .map_err(|_| ExprError::at("array index out of range", start))
    }

    fn parse_int(&mut self) -> Result<i64, ExprError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let digits_start = self.pos;
        while matches!(self.peek(), Some(b) if b.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == digits_start {
            return Err(ExprError::at("expected integer", start));
        }
        self.src[start..self.pos]
            .parse::<i64>()
            .map_err(|_| ExprError::at("integer out of range", start))
    }

    fn parse_filter(&mut self) -> Result<Filter, ExprError> {
        let name_start = self.pos;
        let name = self.parse_ident()?;
        if name != "dateAdd" {
            return Err(ExprError::at(
                format!("unknown filter: {name} (only 'dateAdd' is supported)"),
                name_start,
            ));
        }
        self.skip_ws();
        self.expect(b'(', "'(' after filter name")?;
        self.skip_ws();
        let n = self.parse_int()?;
        self.skip_ws();
        self.expect(b',', "',' between dateAdd arguments")?;
        self.skip_ws();
        let unit = self.parse_date_unit()?;
        self.skip_ws();
        self.expect(b')', "')' after filter arguments")?;
        Ok(Filter::DateAdd { n, unit })
    }

    fn parse_date_unit(&mut self) -> Result<DateUnit, ExprError> {
        let quote_start = self.pos;
        self.expect(b'\'', "single-quoted unit ('DAYS', 'HOURS', or 'MINUTES')")?;
        let start = self.pos;
        while matches!(self.peek(), Some(b) if b != b'\'') {
            self.pos += 1;
        }
        if self.peek().is_none() {
            return Err(ExprError::at("unterminated unit string", quote_start));
        }
        let unit = &self.src[start..self.pos];
        self.pos += 1; // closing quote
        match unit {
            "DAYS" => Ok(DateUnit::Days),
            "HOURS" => Ok(DateUnit::Hours),
            "MINUTES" => Ok(DateUnit::Minutes),
            other => Err(ExprError::at(
                format!("unknown date unit: '{other}' (expected 'DAYS', 'HOURS', or 'MINUTES')"),
                quote_start,
            )),
        }
    }
}
