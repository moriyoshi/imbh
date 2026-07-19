use crate::{Diagnostic, DiagnosticCode, SourceRange};

#[derive(Clone)]
pub(crate) struct Cursor<'a> {
    source: &'a str,
    position: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(source: &'a str) -> Self {
        Self {
            source,
            position: 0,
        }
    }

    pub(crate) fn skip_ws(&mut self) {
        while let Some(character) = self.remaining().chars().next() {
            if !character.is_whitespace() {
                break;
            }
            self.position += character.len_utf8();
        }
    }

    pub(crate) fn remaining(&self) -> &'a str {
        &self.source[self.position..]
    }

    pub(crate) fn consume(&mut self, token: &str) -> bool {
        self.skip_ws();
        if self.remaining().starts_with(token) {
            self.position += token.len();
            true
        } else {
            false
        }
    }
    pub(crate) fn consume_keyword(&mut self, keyword: &str) -> bool {
        self.skip_ws();
        let remaining = self.remaining();
        if !remaining.starts_with(keyword) {
            return false;
        }
        let boundary = remaining[keyword.len()..].chars().next();
        if boundary.is_some_and(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | ':' | '.' | '-')
        }) {
            return false;
        }
        self.position += keyword.len();
        true
    }

    pub(crate) fn expect(&mut self, token: &str) -> Result<(), Diagnostic> {
        if self.consume(token) {
            Ok(())
        } else {
            Err(self.error(DiagnosticCode::Syntax, format!("expected {token:?}")))
        }
    }

    pub(crate) fn identifier(&mut self) -> Result<(String, SourceRange), Diagnostic> {
        self.skip_ws();
        let start = self.position;
        while let Some(character) = self.remaining().chars().next() {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | ':' | '.' | '-') {
                self.position += character.len_utf8();
            } else {
                break;
            }
        }
        if self.position == start {
            return Err(self.error(DiagnosticCode::Syntax, "expected identifier"));
        }
        Ok((
            self.source[start..self.position].to_owned(),
            SourceRange {
                start,
                end: self.position,
            },
        ))
    }

    pub(crate) fn quoted_string(&mut self) -> Result<String, Diagnostic> {
        self.skip_ws();
        let start = self.position;
        if !self.remaining().starts_with('"') {
            return Err(self.error(DiagnosticCode::Syntax, "expected quoted string"));
        }
        self.position += 1;
        let mut output = String::new();
        loop {
            let Some(character) = self.remaining().chars().next() else {
                return Err(Diagnostic::new(
                    DiagnosticCode::Syntax,
                    start,
                    self.position,
                    "unterminated quoted string",
                ));
            };
            self.position += character.len_utf8();
            match character {
                '"' => return Ok(output),
                '\\' => {
                    let Some(escaped) = self.remaining().chars().next() else {
                        return Err(self.error(DiagnosticCode::Syntax, "unterminated escape"));
                    };
                    self.position += escaped.len_utf8();
                    output.push(match escaped {
                        '"' => '"',
                        '\\' => '\\',
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        _ => {
                            return Err(self.error(
                                DiagnosticCode::Unsupported,
                                "this string escape is outside the compatibility profile",
                            ));
                        }
                    });
                }
                value => output.push(value),
            }
        }
    }

    pub(crate) fn unsigned(&mut self) -> Result<u64, Diagnostic> {
        self.skip_ws();
        let start = self.position;
        while self
            .remaining()
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_digit)
        {
            self.position += 1;
        }
        self.source[start..self.position].parse().map_err(|_| {
            Diagnostic::new(
                DiagnosticCode::Syntax,
                start,
                self.position.max(start + 1),
                "expected unsigned integer",
            )
        })
    }

    pub(crate) fn float(&mut self) -> Result<f64, Diagnostic> {
        self.skip_ws();
        let start = self.position;
        while let Some(character) = self.remaining().chars().next() {
            if character.is_ascii_digit() || matches!(character, '.' | '-' | '+' | 'e' | 'E') {
                self.position += character.len_utf8();
            } else {
                break;
            }
        }
        self.source[start..self.position].parse().map_err(|_| {
            Diagnostic::new(
                DiagnosticCode::Syntax,
                start,
                self.position.max(start + 1),
                "expected number",
            )
        })
    }

    pub(crate) fn duration_ns(&mut self) -> Result<u64, Diagnostic> {
        self.skip_ws();
        let start = self.position;
        let mut total = 0_u64;
        let mut components = 0_usize;
        while self
            .remaining()
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_digit)
        {
            let value = self.unsigned()?;
            let multiplier = [
                ("ns", 1_u64),
                ("us", 1_000),
                ("µs", 1_000),
                ("ms", 1_000_000),
                ("s", 1_000_000_000),
                ("m", 60_000_000_000),
                ("h", 3_600_000_000_000),
                ("d", 86_400_000_000_000),
            ]
            .into_iter()
            .find_map(|(unit, multiplier)| self.consume(unit).then_some(multiplier))
            .ok_or_else(|| {
                Diagnostic::new(
                    DiagnosticCode::Syntax,
                    start,
                    self.position,
                    "expected duration unit ns, us, µs, ms, s, m, h, or d",
                )
            })?;
            let component = value.checked_mul(multiplier).ok_or_else(|| {
                Diagnostic::new(
                    DiagnosticCode::LimitExceeded,
                    start,
                    self.position,
                    "duration overflows nanoseconds",
                )
            })?;
            total = total.checked_add(component).ok_or_else(|| {
                Diagnostic::new(
                    DiagnosticCode::LimitExceeded,
                    start,
                    self.position,
                    "duration overflows nanoseconds",
                )
            })?;
            components += 1;
        }
        if components == 0 {
            return Err(Diagnostic::new(
                DiagnosticCode::Syntax,
                start,
                self.position.max(start + 1),
                "expected duration",
            ));
        }
        Ok(total)
    }

    pub(crate) fn finish(&mut self) -> Result<(), Diagnostic> {
        self.skip_ws();
        if self.position == self.source.len() {
            Ok(())
        } else {
            Err(self.error(
                DiagnosticCode::Unsupported,
                "unsupported trailing query construct",
            ))
        }
    }

    pub(crate) fn error(&self, code: DiagnosticCode, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(
            code,
            self.position,
            self.source.len().min(self.position.saturating_add(1)),
            message,
        )
    }
}
