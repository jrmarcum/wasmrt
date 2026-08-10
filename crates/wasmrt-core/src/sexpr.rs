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
    /// A byte that may not appear in `.wat` **source** at all (§6.2).
    ///
    /// Outside strings and comments the source character set is **printable ASCII** — a
    /// control character, `DEL`, or any non-ASCII byte is malformed, not merely unexpected.
    /// Inside a string, `stringchar` additionally requires `c ≥ U+20 ∧ c ≠ U+7F`, so a raw
    /// control byte must be written as an escape.
    IllegalCharacter,
    /// A non-ASCII byte sequence in a string literal that is not valid UTF-8. Source text is
    /// Unicode; arbitrary bytes reach a data segment through **escapes**, not raw. Also raised
    /// for a quoted identifier whose escapes do not spell a valid name.
    MalformedUtf8,
    /// `id ::= '$' idchar+` — a bare `$`, or the quoted form `$""`, names nothing.
    EmptyIdentifier,
}

/// Is this byte an `idchar` (§6.2.2)?
///
/// The set is closed and entirely ASCII, which is what lets [`Parser::parse_atom`] build its
/// `String` without a lossy conversion. Notably absent: space, `(`, `)`, `"`, `;`, `,`, `[`,
/// `]`, `{`, `}` — and every byte outside `0x21..=0x7E`.
const fn is_idchar(c: u8) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'/'
                | b':'
                | b'<'
                | b'='
                | b'>'
                | b'?'
                | b'@'
                | b'\\'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// May this byte appear in source outside a string or comment?
///
/// Printable ASCII, plus the four whitespace forms §6.2.3 allows (`space` and
/// `format ::= U+09 | U+0A | U+0D`). `annotations.wast` is an exact statement of this set: of the
/// 33 control bytes it enumerates, precisely `\09`, `\0a`, `\0d` and `\20` are *not* asserted
/// malformed.
///
/// Wider than [`is_idchar`] on purpose: `,`, `;`, `[`, `]`, `{` and `}` begin *reserved* tokens,
/// which are legal source that simply means nothing — an annotation body may contain them, and the
/// proposal's own test module does.
const fn is_source_char(c: u8) -> bool {
    c.is_ascii_graphic() || matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

/// Length in bytes of the UTF-8 sequence a lead byte introduces, or `None` if it is not a legal
/// lead byte (a continuation byte, or one of the values UTF-8 never uses).
const fn utf8_len(lead: u8) -> Option<usize> {
    match lead {
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        // 0x80..=0xbf are continuations; 0xc0/0xc1 are overlong two-byte forms; 0xf5.. is past
        // the last code point. `from_utf8` would reject all of them, but rejecting here gives
        // the honest error rather than an out-of-range slice.
        _ => None,
    }
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
    p.skip_trivia()?;
    while p.pos < src.len() {
        forms.push(p.parse_value()?);
        p.skip_trivia()?;
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

    fn skip_trivia(&mut self) -> Result<()> {
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
                self.skip_annotation()?;
            } else {
                break;
            }
        }
        Ok(())
    }

    fn parse_value(&mut self) -> Result<Sexpr> {
        self.skip_trivia()?;
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
                let at = self.parse_atom()?;
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
                    // `id ::= '$' idchar+` — the quoted spelling still has to name something.
                    if s.is_empty() {
                        return Err(self.err(ParseErrorKind::EmptyIdentifier));
                    }
                    // An identifier is a **name**, so it must be valid UTF-8. This was
                    // `from_utf8_lossy`, the same silent rewrite `parse_atom` carried: `$"\ef"`
                    // became `$\u{FFFD}`, so a malformed identifier was accepted *and* renamed,
                    // and two different bad escapes collided on one name.
                    let Ok(text) = core::str::from_utf8(&s) else {
                        return Err(self.err(ParseErrorKind::MalformedUtf8));
                    };
                    let mut name = at;
                    name.push_str(text);
                    return Ok(Sexpr::Atom(name));
                }
                self.require_delimiter()?;
                // A bare `$` names nothing — `(func $)` and `(func $ "a")` are both malformed.
                if at == "$" {
                    return Err(self.err(ParseErrorKind::EmptyIdentifier));
                }
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
            self.skip_trivia()?;
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
    fn skip_annotation(&mut self) -> Result<()> {
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
                // Skipping an annotation is not the same as not reading it. The proposal says an
                // unrecognized annotation must be **ignored**, not that its bytes stop being
                // source: `(@a \00)` is malformed, and the source character set is the reason.
                c if !is_source_char(c) => {
                    return Err(self.err(ParseErrorKind::IllegalCharacter));
                }
                _ => self.pos += 1,
            }
        }
        Ok(())
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
    /// Scan one atom (§6.2.2 — a keyword, `id`, number, or `key=value`).
    ///
    /// Every byte must be an `idchar`. It used to consume anything that was not a delimiter and
    /// then run `from_utf8_lossy`, which was wrong twice over: it accepted control characters and
    /// non-ASCII that the source character set forbids, and the lossy conversion **silently
    /// rewrote** what it accepted — `$a\xffb` and `$a\xfeb` both became `$a\u{FFFD}b`, so two
    /// distinct identifiers collided on one name. Restricting to `idchar` makes the slice ASCII by
    /// construction, so the conversion cannot lose anything.
    fn parse_atom(&mut self) -> Result<String> {
        let start = self.pos;
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            if matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b'(' | b')' | b';' | b'"') {
                break;
            }
            if !is_idchar(c) {
                // A printable non-`idchar` (`,`, `[`, `{`, …) merely ends the atom — it begins a
                // *reserved* token, which `require_delimiter` will reject in context. A byte
                // outside the source character set is malformed wherever it appears.
                if !is_source_char(c) {
                    return Err(self.err(ParseErrorKind::IllegalCharacter));
                }
                break;
            }
            self.pos += 1;
        }
        // ASCII by construction, so this cannot fail; `unwrap_or_default` keeps the promise
        // without a panic path in a parser that must stay total on hostile input.
        Ok(String::from_utf8(self.src[start..self.pos].to_vec()).unwrap_or_default())
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
                // §6.3.3 `stringchar ::= c:char  (if c ≥ U+20 ∧ c ≠ U+7F ∧ c ≠ '"' ∧ c ≠ '\')`.
                // A control byte or DEL must be written as an escape; raw, it is malformed.
                if c < 0x20 || c == 0x7f {
                    self.pos -= 1;
                    return Err(self.err(ParseErrorKind::IllegalCharacter));
                }
                // Source text is Unicode, so a byte ≥ 0x80 is only legal as part of a valid UTF-8
                // sequence — `"héllo"` is fine, a lone `\xff` is not. Arbitrary bytes reach a data
                // segment through escapes (`"\ff"`), which is the branch below.
                if c >= 0x80 {
                    let len = utf8_len(c).ok_or_else(|| {
                        self.pos -= 1;
                        self.err(ParseErrorKind::MalformedUtf8)
                    })?;
                    let start = self.pos - 1;
                    let end = start + len;
                    let seq = self.src.get(start..end).filter(|s| {
                        core::str::from_utf8(s).is_ok()
                    });
                    let Some(seq) = seq else {
                        self.pos = start;
                        return Err(self.err(ParseErrorKind::MalformedUtf8));
                    };
                    buf.extend_from_slice(seq);
                    self.pos = end;
                    continue;
                }
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

#[cfg(test)]
mod source_charset_tests {
    use super::*;

    fn err(src: &[u8]) -> ParseErrorKind {
        parse_all(src).expect_err("should be malformed").kind
    }

    /// §6.2 — the source character set outside strings and comments is printable ASCII plus the
    /// four whitespace forms. These were all **accepted** before, and none of them involves an
    /// annotation: the defect was generic, the file that caught it was not.
    #[test]
    fn a_control_character_in_an_atom_is_malformed() {
        assert_eq!(err(b"(module (func $a\x01b))"), ParseErrorKind::IllegalCharacter);
        assert_eq!(err(b"(module (func $a\x7fb))"), ParseErrorKind::IllegalCharacter);
    }

    /// Non-ASCII is not an `idchar`, however well-formed its UTF-8 — `(@a Heiße)` is the suite's
    /// case, and it is an *illegal character*, not a UTF-8 problem.
    #[test]
    fn a_non_ascii_atom_is_malformed() {
        assert_eq!(err("(module (func $Heiße))".as_bytes()), ParseErrorKind::IllegalCharacter);
    }

    /// The whitespace exceptions must keep working — this is the half a tightening breaks.
    #[test]
    fn tab_newline_and_carriage_return_stay_legal() {
        parse_all(b"(module\t(func)\r\n(func))").unwrap();
        parse_all(b"(module (@a \t\r\n x) (func))").unwrap();
    }

    /// §6.3.3 `stringchar` requires `c >= U+20 && c != U+7F`: a control byte must be **escaped**.
    /// The escape must still work, or this rule would break every `(data "\00…")` in the corpus.
    #[test]
    fn a_raw_control_byte_in_a_string_is_malformed_but_the_escape_is_not() {
        assert_eq!(err(b"(module (data \"\x01\"))"), ParseErrorKind::IllegalCharacter);
        assert_eq!(err(b"(module (data \"\x7f\"))"), ParseErrorKind::IllegalCharacter);
        let ok = parse_all(br#"(module (data "\01\7f\ff"))"#).unwrap();
        assert!(!ok.is_empty(), "escaped control bytes must still assemble");
    }

    /// Raw non-ASCII in a string is legal when it is valid UTF-8 and malformed when it is not.
    /// Arbitrary bytes reach a data segment through escapes, which the test above pins.
    #[test]
    fn a_string_takes_valid_utf8_raw_and_rejects_the_rest() {
        parse_all("(module (data \"héllo ☃\"))".as_bytes()).unwrap();
        assert_eq!(err(b"(module (data \"\xff\"))"), ParseErrorKind::MalformedUtf8);
        // A truncated sequence: a valid lead byte with no continuation.
        assert_eq!(err(b"(module (data \"\xe0\"))"), ParseErrorKind::MalformedUtf8);
        // A bare continuation byte.
        assert_eq!(err(b"(module (data \"\x80\"))"), ParseErrorKind::MalformedUtf8);
    }

    /// `id ::= '$' idchar+`. All three spellings of "nothing" are malformed.
    #[test]
    fn an_empty_identifier_is_malformed() {
        assert_eq!(err(b"(module (func $))"), ParseErrorKind::EmptyIdentifier);
        assert_eq!(err(b"(module (func $\"\"))"), ParseErrorKind::EmptyIdentifier);
        assert_eq!(err(b"(module (func $ \"a\"))"), ParseErrorKind::EmptyIdentifier);
    }

    /// A quoted identifier is a **name**, so it must be valid UTF-8. This was `from_utf8_lossy`,
    /// which accepted it *and renamed it* — `$"\ef"` became `$\u{FFFD}`, so two different bad
    /// escapes produced the same identifier. Silent rewriting, not just over-acceptance.
    #[test]
    fn a_quoted_identifier_must_be_valid_utf8() {
        assert_eq!(err(br#"(module (func $"\ef"))"#), ParseErrorKind::MalformedUtf8);
        // The legitimate use of the quoted form still works.
        parse_all("(module (func $\"a b\") (func $\"héllo\"))".as_bytes()).unwrap();
    }

    /// The rule must reach **inside** an annotation. Skipping an annotation means ignoring what it
    /// says, not exempting its bytes from being source — which is where 44 of these live.
    #[test]
    fn the_charset_rule_applies_inside_a_skipped_annotation() {
        assert_eq!(err(b"(module (@a \x00))"), ParseErrorKind::IllegalCharacter);
        assert_eq!(err(b"(module (@a \x7f))"), ParseErrorKind::IllegalCharacter);
        assert_eq!(err(b"(module (@a \xff))"), ParseErrorKind::IllegalCharacter);
        // ...but a well-formed annotation is still skipped whole, including its odd tokens.
        parse_all(br#"(module (@a , ; ] [ }} }x{ ({) ,{{};}] ;) (func))"#).unwrap();
    }

    /// Comments are deliberately NOT tightened: `linechar ::= c:char (if c != U+0A)` admits
    /// anything but a newline, so a control byte in a comment is legal and must stay accepted.
    /// Recorded because it was the one probe of four that turned out not to be a defect.
    #[test]
    fn a_control_character_in_a_comment_stays_legal() {
        parse_all(b"(module (func) ;; \x01 note\n)").unwrap();
        parse_all(b"(module (func) (; \x01 note ;))").unwrap();
    }
}
