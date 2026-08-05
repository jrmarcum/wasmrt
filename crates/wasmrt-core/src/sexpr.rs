//! `sexpr` — the S-expression lexer/parser front-end for `.wat`/`.wast` text.
//!
//! Shared by the WAT assembler ([`crate::wat`], text → wasm binary) and the WAST script
//! runner ([`crate::wast`], assertions). Source is tokenized into a tree of [`Sexpr`] nodes:
//! atoms (keywords, `$identifiers`, numbers, `key=value` — kept as raw source text for the
//! assembler to interpret), strings (decoded to their byte values, so
//! `(module binary "\00asm…")` yields real bytes), and lists. Line comments (`;; …`) and
//! nestable block comments (`(; … ;)`) are trivia.
//!
//! Ported from wazmrt `src/sexpr.zig` (T6). Depth-capped against paren bombs; fails loud on
//! malformed input and never hangs — wazmrt's lone-`;` non-advance hang (12 bytes of input,
//! 10 GB RSS) is the cautionary tale, so a zero-length atom is an error here too.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// Cap on `(`-nesting. Real `.wat`/`.wast` nests a few dozen deep at most; this is a
/// stack-overflow guard against adversarial input, not a real limit.
const MAX_DEPTH: usize = 1024;

/// One parsed S-expression node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sexpr {
    /// A keyword (`module`, `i32.add`), identifier (`$x`), number, or `key=value` token,
    /// kept as raw source text.
    Atom(String),
    /// A string literal, decoded to its byte values (escapes resolved).
    Str(Vec<u8>),
    List(Vec<Sexpr>),
}

impl Sexpr {
    /// For a list, its leading atom (the "keyword"), else `None`.
    #[must_use]
    pub fn keyword(&self) -> Option<&str> {
        match self {
            Sexpr::List(items) => match items.first() {
                Some(Sexpr::Atom(a)) => Some(a),
                _ => None,
            },
            _ => None,
        }
    }

    #[must_use]
    pub fn as_atom(&self) -> Option<&str> {
        match self {
            Sexpr::Atom(a) => Some(a),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&[u8]> {
        match self {
            Sexpr::Str(s) => Some(s),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_list(&self) -> Option<&[Sexpr]> {
        match self {
            Sexpr::List(l) => Some(l),
            _ => None,
        }
    }
}

/// A parse failure, with the byte offset where it was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseErrorKind {
    UnexpectedEof,
    UnexpectedParen,
    UnterminatedString,
    UnterminatedList,
    BadEscape,
    /// List nesting exceeded [`MAX_DEPTH`] — refuses a `((((…` bomb that would otherwise
    /// overflow the host stack through the parser's recursion.
    NestingTooDeep,
    /// A character that starts no value and that trivia-skipping does not consume — today
    /// only a lone `;` (`;;` and `(;` are trivia; a bare `;` is not valid `.wat`).
    UnexpectedChar,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} at byte {}", self.kind, self.offset)
    }
}

impl core::error::Error for ParseError {}

type Result<T> = core::result::Result<T, ParseError>;

/// Parse an entire source into its sequence of top-level forms.
///
/// # Errors
/// Returns a [`ParseError`] on malformed input (unterminated list/string, bad escape,
/// excessive nesting, or a stray character).
pub fn parse_all(src: &[u8]) -> Result<Vec<Sexpr>> {
    let mut p = Parser {
        src,
        pos: 0,
        depth: 0,
    };
    let mut forms = Vec::new();
    p.skip_trivia();
    while p.pos < src.len() {
        forms.push(p.parse_value()?);
        p.skip_trivia();
    }
    Ok(forms)
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn err(&self, kind: ParseErrorKind) -> ParseError {
        ParseError {
            kind,
            offset: self.pos,
        }
    }

    fn peek(&self, ahead: usize) -> u8 {
        self.src.get(self.pos + ahead).copied().unwrap_or(0)
    }

    fn skip_trivia(&mut self) {
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            if matches!(c, b' ' | b'\t' | b'\r' | b'\n') {
                self.pos += 1;
            } else if c == b';' && self.peek(1) == b';' {
                self.pos += 2;
                while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else if c == b'(' && self.peek(1) == b';' {
                self.pos += 2;
                let mut depth = 1usize;
                while self.pos < self.src.len() && depth > 0 {
                    if self.src[self.pos] == b'(' && self.peek(1) == b';' {
                        depth += 1;
                        self.pos += 2;
                    } else if self.src[self.pos] == b';' && self.peek(1) == b')' {
                        depth -= 1;
                        self.pos += 2;
                    } else {
                        self.pos += 1;
                    }
                }
            } else if c == b'(' && self.peek(1) == b'@' {
                self.skip_annotation();
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self) -> Result<Sexpr> {
        self.skip_trivia();
        if self.pos >= self.src.len() {
            return Err(self.err(ParseErrorKind::UnexpectedEof));
        }
        match self.src[self.pos] {
            b'(' => self.parse_list(),
            b')' => Err(self.err(ParseErrorKind::UnexpectedParen)),
            b'"' => {
                let s = self.parse_string()?;
                self.require_delimiter()?;
                Ok(Sexpr::Str(s))
            }
            // A lone `;`: trivia-skipping consumes only `;;` and `(;`, and `parse_atom`
            // treats `;` as a terminator — so it would return an EMPTY atom without
            // advancing `pos`, and the parse loops would append empty atoms forever.
            b';' => Err(self.err(ParseErrorKind::UnexpectedChar)),
            _ => {
                let at = self.parse_atom();
                // Belt-and-braces: no delimiter added to `parse_atom` in future may
                // reintroduce a zero-progress loop.
                if at.is_empty() {
                    return Err(self.err(ParseErrorKind::UnexpectedChar));
                }
                // A **quoted identifier**: `$` (or an annotation's `@`) immediately followed
                // by a string is ONE token, not an atom next to a string — it is how a name
                // holds characters the bare form cannot, e.g. `$"a b"`. The distinction from
                // the malformed `(data $l"a")` is that there the atom is `$l`, not `$`, so
                // the string genuinely is a second token butted against the first.
                if (at == "$" || at == "@") && self.src.get(self.pos) == Some(&b'"') {
                    let s = self.parse_string()?;
                    self.require_delimiter()?;
                    let mut name = at;
                    name.push_str(&String::from_utf8_lossy(&s));
                    return Ok(Sexpr::Atom(name));
                }
                self.require_delimiter()?;
                Ok(Sexpr::Atom(at))
            }
        }
    }

    fn parse_list(&mut self) -> Result<Sexpr> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.err(ParseErrorKind::NestingTooDeep));
        }
        self.pos += 1; // consume '('
        let mut items = Vec::new();
        loop {
            self.skip_trivia();
            if self.pos >= self.src.len() {
                return Err(self.err(ParseErrorKind::UnterminatedList));
            }
            if self.src[self.pos] == b')' {
                self.pos += 1;
                break;
            }
            items.push(self.parse_value()?);
        }
        self.depth -= 1;
        Ok(Sexpr::List(items))
    }

    /// Skip an annotation `(@id …)` whole, treating it as trivia.
    ///
    /// The annotations proposal is not one wasmrt targets, and it says explicitly that a tool
    /// which does not understand an annotation must **ignore** it — so discarding is the
    /// correct behaviour, not a shortcut. It also has to be discarded *lexically*: an
    /// annotation's body is a raw token sequence where the usual separation rules do not
    /// apply (`(@a x-y$yz"aa"-2)` is legal), so parsing it as an ordinary list would fail.
    ///
    /// Strings **and comments** are tracked, because a `)` inside either does not close the
    /// annotation — `(@a ;; bla)` and `(@a (; ) ;))` both appear in the proposal's own tests.
    fn skip_annotation(&mut self) {
        self.pos += 2; // consume `(@`
        let mut depth = 1usize;
        while self.pos < self.src.len() && depth > 0 {
            match self.src[self.pos] {
                b'"' => {
                    self.pos += 1;
                    while self.pos < self.src.len() && self.src[self.pos] != b'"' {
                        // A `\"` escape does not end the string.
                        self.pos += if self.src[self.pos] == b'\\' { 2 } else { 1 };
                    }
                    self.pos += 1;
                }
                b';' if self.peek(1) == b';' => {
                    while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                }
                b'(' if self.peek(1) == b';' => {
                    self.pos += 2;
                    let mut c = 1usize;
                    while self.pos < self.src.len() && c > 0 {
                        if self.src[self.pos] == b'(' && self.peek(1) == b';' {
                            c += 1;
                            self.pos += 2;
                        } else if self.src[self.pos] == b';' && self.peek(1) == b')' {
                            c -= 1;
                            self.pos += 2;
                        } else {
                            self.pos += 1;
                        }
                    }
                }
                b'(' => {
                    depth += 1;
                    self.pos += 1;
                }
                b')' => {
                    depth -= 1;
                    self.pos += 1;
                }
                _ => self.pos += 1,
            }
        }
    }

    /// A token must end at whitespace, a parenthesis, a comment, or end-of-input.
    ///
    /// Without this, two tokens written flush against each other lex as two tokens —
    /// `(data "a""b")`, `(data $l"a")`, `(br_table $l$l)` — where the spec says the source is
    /// malformed. It matters because `"a""b"` silently becoming the *concatenation* `ab` is a
    /// wrong value, not a rejected one. Parentheses are exempt on purpose: `(func(nop))` is
    /// legal, and `token.wast` tests both halves of that rule.
    fn require_delimiter(&mut self) -> Result<()> {
        match self.src.get(self.pos) {
            None | Some(b' ' | b'\t' | b'\r' | b'\n' | b'(' | b')' | b';') => Ok(()),
            Some(_) => Err(self.err(ParseErrorKind::UnexpectedChar)),
        }
    }

    /// An atom runs to the next delimiter. Source is not required to be UTF-8 overall, but
    /// an atom that is not valid UTF-8 cannot be a keyword or identifier, so it is
    /// lossily converted — the assembler will reject it by name anyway.
    fn parse_atom(&mut self) -> String {
        let start = self.pos;
        while self.pos < self.src.len() {
            match self.src[self.pos] {
                b' ' | b'\t' | b'\r' | b'\n' | b'(' | b')' | b';' | b'"' => break,
                _ => self.pos += 1,
            }
        }
        String::from_utf8_lossy(&self.src[start..self.pos]).into_owned()
    }

    fn parse_string(&mut self) -> Result<Vec<u8>> {
        self.pos += 1; // consume the opening quote
        let mut buf = Vec::new();
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            self.pos += 1;
            if c == b'"' {
                return Ok(buf);
            }
            if c != b'\\' {
                buf.push(c);
                continue;
            }
            if self.pos >= self.src.len() {
                return Err(self.err(ParseErrorKind::BadEscape));
            }
            let e = self.src[self.pos];
            self.pos += 1;
            match e {
                b't' => buf.push(b'\t'),
                b'n' => buf.push(b'\n'),
                b'r' => buf.push(b'\r'),
                b'"' => buf.push(b'"'),
                b'\'' => buf.push(b'\''),
                b'\\' => buf.push(b'\\'),
                b'u' => self.parse_unicode_escape(&mut buf)?,
                _ => {
                    // `\XX` — a raw hex byte.
                    let hi = hex_val(e).ok_or_else(|| self.err(ParseErrorKind::BadEscape))?;
                    let lo = hex_val(self.src.get(self.pos).copied().unwrap_or(0))
                        .ok_or_else(|| self.err(ParseErrorKind::BadEscape))?;
                    self.pos += 1;
                    buf.push(hi * 16 + lo);
                }
            }
        }
        Err(self.err(ParseErrorKind::UnterminatedString))
    }

    fn parse_unicode_escape(&mut self, buf: &mut Vec<u8>) -> Result<()> {
        if self.src.get(self.pos) != Some(&b'{') {
            return Err(self.err(ParseErrorKind::BadEscape));
        }
        self.pos += 1;
        let mut cp: u32 = 0;
        while self.pos < self.src.len() && self.src[self.pos] != b'}' {
            let d = hex_val(self.src[self.pos]).ok_or_else(|| self.err(ParseErrorKind::BadEscape))?;
            // Overflow-CHECKED, not wrapping: `\u{100000041}` must be rejected, not silently
            // truncated mod 2^32 into a valid scalar (here `'A'`).
            cp = cp
                .checked_mul(16)
                .and_then(|v| v.checked_add(u32::from(d)))
                .ok_or_else(|| self.err(ParseErrorKind::BadEscape))?;
            self.pos += 1;
        }
        if self.pos >= self.src.len() {
            return Err(self.err(ParseErrorKind::BadEscape));
        }
        self.pos += 1; // consume '}'
        let ch = char::from_u32(cp).ok_or_else(|| self.err(ParseErrorKind::BadEscape))?;
        let mut utf8 = [0u8; 4];
        buf.extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
        Ok(())
    }
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Result<Vec<Sexpr>> {
        parse_all(src.as_bytes())
    }

    /// Tokens must be separated by whitespace or a parenthesis. The parenthesis half matters
    /// as much as the separator half: `(func(nop))` is legal and `(data "a""b")` is not, and
    /// `token.wast` tests both. Accepting `"a""b"` silently concatenated it to `ab` — a wrong
    /// value rather than a rejection.
    #[test]
    fn adjacent_tokens_need_a_separator_but_parens_are_one() {
        for bad in [
            r#"(data "a""b")"#,
            r#"(data $l"a")"#,
            r#"(data"a")"#,
            r#"(f "a"1)"#,
        ] {
            assert!(parse(bad).is_err(), "should reject `{bad}`");
        }
        for ok in [
            "(func(nop))",
            "(func (nop)nop)",
            "(func nop(nop))",
            "(func br 0(nop))",
            r#"(data "a" "b")"#,
            "(func nop);;trailing comment",
        ] {
            assert!(parse(ok).is_ok(), "should accept `{ok}`");
        }
    }

    /// `$` immediately followed by a string is ONE token — the quoted-identifier form, which
    /// is how a name carries characters the bare spelling cannot. The contrast with the
    /// rejected `(data $l"a")` above is that there the atom is `$l`, not a bare `$`.
    #[test]
    fn a_quoted_identifier_is_a_single_token() {
        let v = parse(r#"(func $"a b" nop)"#).expect("quoted identifier parses");
        let Sexpr::List(items) = &v[0] else {
            panic!("expected a list")
        };
        assert_eq!(items[1], Sexpr::Atom(String::from("$a b")));
        // Two identifiers still cannot be butted together.
        assert!(parse(r#"(br_table $"l"$l)"#).is_err());
    }

    /// An annotation is skipped whole, as the proposal requires of a tool that does not
    /// implement it. It has to be skipped *lexically* — its body is a raw token sequence
    /// where the separation rule above does not apply — and a `)` inside a string or a
    /// comment must not be mistaken for its terminator.
    #[test]
    fn annotations_are_skipped_as_trivia() {
        let v = parse(r#"(module (@custom "x") (func (@a 1) nop))"#).expect("parses");
        let Sexpr::List(items) = &v[0] else {
            panic!("expected a list")
        };
        assert_eq!(items.len(), 2, "the annotation must not become an item");
        // The awkward bodies from the proposal's own tests.
        for src in [
            r#"(module (@a x-y$yz"aa"-2))"#,
            r#"(module (@a ;; bla)
               ))"#,
            r#"(module (@a (; ) ;)))"#,
            r#"(module (@a "str)ing"))"#,
        ] {
            assert!(parse(src).is_ok(), "should skip `{src}`");
        }
    }

    #[test]
    fn parses_a_nested_module_form() {
        let forms = parse(
            "(module\n  (func (export \"add\") (param $x i32) (result i32)\n    \
             (i32.add (local.get $x) (i32.const 1))))",
        )
        .unwrap();
        assert_eq!(forms.len(), 1);
        assert_eq!(forms[0].keyword(), Some("module"));
        let module = forms[0].as_list().unwrap();
        assert_eq!(module[1].keyword(), Some("func"));
        let func = module[1].as_list().unwrap();
        assert_eq!(func[1].keyword(), Some("export"));
        assert_eq!(func[1].as_list().unwrap()[1].as_str(), Some(&b"add"[..]));
    }

    #[test]
    fn skips_line_and_block_comments() {
        let forms = parse(";; a leading line comment\n(a (; nested (; block ;) comment ;) b) ;; trailing")
            .unwrap();
        assert_eq!(forms.len(), 1);
        let list = forms[0].as_list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].as_atom(), Some("a"));
        assert_eq!(list[1].as_atom(), Some("b"));
    }

    #[test]
    fn decodes_string_escapes_to_bytes() {
        let forms = parse(r#"(module binary "\00asm\01\00\00\00")"#).unwrap();
        let list = forms[0].as_list().unwrap();
        assert_eq!(
            list[2].as_str(),
            Some(&[0x00, b'a', b's', b'm', 0x01, 0x00, 0x00, 0x00][..])
        );
    }

    #[test]
    fn decodes_named_and_unicode_escapes() {
        let forms = parse(r#"("a\tb\n\u{41}\u{20AC}")"#).unwrap();
        let s = forms[0].as_list().unwrap()[0].as_str().unwrap();
        assert_eq!(s, b"a\tb\nA\xe2\x82\xac");
    }

    #[test]
    fn rejects_an_over_large_unicode_escape() {
        // Must not wrap mod 2^32 into a valid scalar ('A').
        let e = parse(r#"("\u{100000041}")"#).unwrap_err();
        assert_eq!(e.kind, ParseErrorKind::BadEscape);
    }

    #[test]
    fn reports_an_unterminated_list() {
        assert_eq!(
            parse("(module (func").unwrap_err().kind,
            ParseErrorKind::UnterminatedList
        );
    }

    #[test]
    fn reports_an_unterminated_string() {
        assert_eq!(
            parse("(data \"abc)").unwrap_err().kind,
            ParseErrorKind::UnterminatedString
        );
    }

    #[test]
    fn a_lone_semicolon_is_an_error_not_a_hang() {
        // The regression that hung wazmrt's CLI at 10 GB RSS on 12 bytes of input.
        assert_eq!(
            parse("(module) ; x").unwrap_err().kind,
            ParseErrorKind::UnexpectedChar
        );
    }

    #[test]
    fn caps_nesting_depth() {
        let src = "(".repeat(MAX_DEPTH + 5);
        assert_eq!(
            parse(&src).unwrap_err().kind,
            ParseErrorKind::NestingTooDeep
        );
    }

    #[test]
    fn rejects_a_stray_close_paren() {
        assert_eq!(
            parse("(a) )").unwrap_err().kind,
            ParseErrorKind::UnexpectedParen
        );
    }
}
