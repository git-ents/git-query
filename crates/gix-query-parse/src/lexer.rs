//! Hand-written lexer.
//!
//! Operates on bytes throughout, including inside quoted strings: every
//! multi-byte UTF-8 sequence's continuation and lead bytes are `>= 0x80`, so
//! they can never be mistaken for one of this grammar's ASCII control bytes
//! (`\`, `"`, the digits, punctuation). Copying such bytes through verbatim
//! is therefore safe without decoding them, and `String::from_utf8` at the
//! end never fails because every byte run copied came from a valid `&str`
//! slice or from single-byte ASCII escapes.

use std::sync::Arc;

use gix_query_ir::Span;

use crate::error::ParseError;
use crate::token::{Token, TokenKind};

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    errors: Vec<ParseError>,
}

/// Lex `src` into a token stream, always ending in one [`TokenKind::Eof`].
///
/// Never fails outright: a lexical error is recorded and the lexer resyncs
/// by skipping the offending byte (or, for an unterminated string, running
/// to the end of input), so a source file with several unrelated typos
/// reports several diagnostics in one pass.
pub(crate) fn tokenize(src: &str) -> (Vec<Token>, Vec<ParseError>) {
    let mut lexer = Lexer {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        errors: Vec::new(),
    };
    let mut tokens = Vec::new();
    loop {
        let tok = lexer.next_token();
        let done = matches!(tok.kind, TokenKind::Eof);
        tokens.push(tok);
        if done {
            break;
        }
    }
    (tokens, lexer.errors)
}

impl<'a> Lexer<'a> {
    fn cur(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn span_from(&self, start: usize) -> Span {
        Span::new(start as u32, self.pos as u32)
    }

    fn single(&mut self, start: usize, kind: TokenKind) -> Token {
        self.pos = start + 1;
        Token {
            kind,
            span: self.span_from(start),
        }
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.cur() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.pos += 1,
                // `%` runs to end of line, or end of input.
                Some(b'%') => {
                    while !matches!(self.cur(), None | Some(b'\n')) {
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn next_token(&mut self) -> Token {
        loop {
            self.skip_trivia();
            let start = self.pos;
            let Some(b) = self.cur() else {
                return Token {
                    kind: TokenKind::Eof,
                    span: Span::new(start as u32, start as u32),
                };
            };
            match b {
                b'(' => return self.single(start, TokenKind::LParen),
                b')' => return self.single(start, TokenKind::RParen),
                b',' => return self.single(start, TokenKind::Comma),
                b'.' => return self.single(start, TokenKind::Dot),
                b':' => {
                    self.pos += 1;
                    if self.cur() == Some(b'-') {
                        self.pos += 1;
                        return Token {
                            kind: TokenKind::ColonDash,
                            span: self.span_from(start),
                        };
                    }
                    self.errors.push(ParseError::UnexpectedChar {
                        found: ':',
                        span: self.span_from(start),
                    });
                    continue;
                }
                b'!' => {
                    self.pos += 1;
                    if self.cur() == Some(b'=') {
                        self.pos += 1;
                        return Token {
                            kind: TokenKind::Ne,
                            span: self.span_from(start),
                        };
                    }
                    return Token {
                        kind: TokenKind::Bang,
                        span: self.span_from(start),
                    };
                }
                b'=' => return self.single(start, TokenKind::Eq),
                b'<' => {
                    self.pos += 1;
                    if self.cur() == Some(b'=') {
                        self.pos += 1;
                        return Token {
                            kind: TokenKind::Le,
                            span: self.span_from(start),
                        };
                    }
                    return Token {
                        kind: TokenKind::Lt,
                        span: self.span_from(start),
                    };
                }
                b'>' => {
                    self.pos += 1;
                    if self.cur() == Some(b'=') {
                        self.pos += 1;
                        return Token {
                            kind: TokenKind::Ge,
                            span: self.span_from(start),
                        };
                    }
                    return Token {
                        kind: TokenKind::Gt,
                        span: self.span_from(start),
                    };
                }
                b'"' => return self.lex_string(start),
                b'-' => {
                    if let Some(tok) = self.lex_negative_int(start) {
                        return tok;
                    }
                    continue;
                }
                b'_' => return self.lex_underscore(start),
                b if b.is_ascii_uppercase() => return self.lex_var(start),
                b if b.is_ascii_lowercase() || b.is_ascii_digit() => return self.lex_bare(start),
                0 => {
                    self.errors.push(ParseError::UnexpectedChar {
                        found: '\0',
                        span: Span::new(start as u32, start as u32 + 1),
                    });
                    self.pos += 1;
                    continue;
                }
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap_or('\u{fffd}');
                    let len = ch.len_utf8();
                    self.errors.push(ParseError::UnexpectedChar {
                        found: ch,
                        span: Span::new(start as u32, (start + len) as u32),
                    });
                    self.pos += len;
                    continue;
                }
            }
        }
    }

    fn word_tail(&mut self) {
        while matches!(self.cur(), Some(b) if b.is_ascii_alphanumeric() || b == b'_') {
            self.pos += 1;
        }
    }

    fn lex_var(&mut self, start: usize) -> Token {
        self.pos += 1;
        self.word_tail();
        let text = &self.src[start..self.pos];
        Token {
            kind: TokenKind::Var(Arc::from(text)),
            span: self.span_from(start),
        }
    }

    fn lex_underscore(&mut self, start: usize) -> Token {
        self.pos += 1;
        let word_start = self.pos;
        self.word_tail();
        if self.pos == word_start {
            return Token {
                kind: TokenKind::Anon,
                span: self.span_from(start),
            };
        }
        let text = &self.src[start..self.pos];
        Token {
            kind: TokenKind::Var(Arc::from(text)),
            span: self.span_from(start),
        }
    }

    fn lex_bare(&mut self, start: usize) -> Token {
        self.pos += 1;
        self.word_tail();
        let text = &self.src[start..self.pos];
        if text.bytes().all(|b| b.is_ascii_digit()) {
            self.int_token(start, text)
        } else {
            Token {
                kind: TokenKind::Sym(Arc::from(text)),
                span: self.span_from(start),
            }
        }
    }

    fn int_token(&mut self, start: usize, text: &str) -> Token {
        match text.parse::<i64>() {
            Ok(n) => Token {
                kind: TokenKind::Int(n),
                span: self.span_from(start),
            },
            Err(_) => {
                self.errors.push(ParseError::IntOverflow {
                    text: text.to_owned(),
                    span: self.span_from(start),
                });
                // A placeholder keeps the token stream shaped like a term,
                // so the parser can carry on past the bad literal.
                Token {
                    kind: TokenKind::Int(0),
                    span: self.span_from(start),
                }
            }
        }
    }

    /// `-` followed by one or more digits. Returns `None` when what follows
    /// `-` cannot be an integer, having already recorded why; the caller
    /// resyncs by looping back into `next_token`.
    fn lex_negative_int(&mut self, start: usize) -> Option<Token> {
        self.pos += 1;
        let digits_start = self.pos;
        while matches!(self.cur(), Some(b) if b.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == digits_start {
            self.errors.push(ParseError::UnexpectedChar {
                found: '-',
                span: self.span_from(start),
            });
            return None;
        }
        if matches!(self.cur(), Some(b) if b.is_ascii_alphabetic() || b == b'_') {
            self.word_tail();
            let text = self.src[start..self.pos].to_owned();
            self.errors.push(ParseError::UnexpectedToken {
                expected: "an integer".to_owned(),
                found: format!("'{text}'"),
                span: self.span_from(start),
            });
            return None;
        }
        let text = self.src[start..self.pos].to_owned();
        Some(self.int_token(start, &text))
    }

    fn lex_string(&mut self, start: usize) -> Token {
        self.pos += 1;
        let mut content = String::new();
        loop {
            match self.cur() {
                None => {
                    self.errors.push(ParseError::UnterminatedString {
                        span: self.span_from(start),
                    });
                    break;
                }
                Some(b'"') => {
                    self.pos += 1;
                    break;
                }
                Some(b'\\') => self.lex_escape(&mut content),
                Some(0) => {
                    self.errors.push(ParseError::NulInString {
                        span: Span::new(self.pos as u32, self.pos as u32 + 1),
                    });
                    self.pos += 1;
                }
                Some(_) => {
                    let ch = self.src[self.pos..].chars().next().expect("byte at pos starts a char");
                    content.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        Token {
            kind: TokenKind::Str(Arc::from(content.as_str())),
            span: self.span_from(start),
        }
    }

    fn lex_escape(&mut self, content: &mut String) {
        let esc_start = self.pos;
        self.pos += 1;
        match self.cur() {
            Some(b'\\') => {
                content.push('\\');
                self.pos += 1;
            }
            Some(b'"') => {
                content.push('"');
                self.pos += 1;
            }
            Some(b'n') => {
                content.push('\n');
                self.pos += 1;
            }
            Some(b't') => {
                content.push('\t');
                self.pos += 1;
            }
            Some(b'0') => {
                self.errors.push(ParseError::NulInString {
                    span: self.span_from(esc_start),
                });
                self.pos += 1;
            }
            Some(_) => {
                let ch = self.src[self.pos..].chars().next().expect("byte at pos starts a char");
                self.errors.push(ParseError::InvalidEscape {
                    found: ch,
                    span: Span::new(esc_start as u32, (self.pos + ch.len_utf8()) as u32),
                });
                self.pos += ch.len_utf8();
            }
            // End of input mid-escape. Leave it unreported here: the outer
            // `lex_string` loop immediately sees the same `None` and reports
            // one `UnterminatedString`, not two.
            None => {}
        }
    }
}
