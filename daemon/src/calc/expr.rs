//! The arithmetic grammar: integers, decimals, unary signs, parentheses, `+ - * /`, a postfix
//! `%` and `of` for percent-of.
//!
//! A plain recursive descent over a fully-lexed token list, and it insists on consuming every
//! token: anything it cannot read makes the whole expression unrecognised, which is what keeps
//! ordinary search text out of the calculator. `%` is a postfix "divide by a hundred" — `50%` is
//! `0.5` — and `of` is a multiplication, so `20% of 50` is `10`. There is no modulo and no
//! exponentiation operator (see the spec's "Out of Scope").

/// Evaluate an arithmetic expression, or `None` when the text is not one.
pub(super) fn evaluate(text: &str) -> Option<f64> {
    let tokens = lex(text)?;
    if tokens.is_empty() {
        return None;
    }

    let mut parser = Parser {
        tokens: &tokens,
        at: 0,
    };
    let value = parser.sum()?;
    if parser.at != tokens.len() {
        return None; // trailing junk: `1 2`, `(1 + 2) 3`, `5 % 2`, …
    }

    value.is_finite().then_some(value)
}

/// Scan a number at `chars[at]`: sign, digits with at most one `.`, and a complete exponent when
/// one follows (`1e3`, `2.5e-2`). Returns the value and the index just past it.
///
/// Shared with the unit reader, where every conversion starts with a number and may glue it to
/// its unit (`10km`, `-40°c`).
pub(super) fn number(chars: &[char], at: usize) -> Option<(f64, usize)> {
    let mut at = at;
    let mut text = String::new();

    if matches!(chars.get(at).copied(), Some('+') | Some('-')) {
        text.push(chars[at]);
        at += 1;
    }

    let mut digits = 0;
    while let Some(c) = chars.get(at).copied() {
        if c.is_ascii_digit() {
            digits += 1;
        } else if c != '.' || text.contains('.') {
            break;
        }
        text.push(c);
        at += 1;
    }
    if digits == 0 {
        return None;
    }

    let mut exponent = String::new();
    let mut look = at;
    if matches!(chars.get(look).copied(), Some('e') | Some('E')) {
        look += 1;
        if matches!(chars.get(look).copied(), Some('+') | Some('-')) {
            exponent.push(chars[look]);
            look += 1;
        }
        let mut exponent_digits = 0;
        while matches!(chars.get(look).copied(), Some(c) if c.is_ascii_digit()) {
            exponent.push(chars[look]);
            exponent_digits += 1;
            look += 1;
        }
        if exponent_digits == 0 {
            exponent.clear();
        } else {
            text.push('e');
            text.push_str(&exponent);
            at = look;
        }
    }

    Some((text.parse().ok()?, at))
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Token {
    Number(f64),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Of,
    Open,
    Close,
}

/// Turn the text into tokens, or `None` on the first character the grammar does not know. A word
/// is only ever the multiplication `of`; every other word — `km`, `abc`, `files` — makes the text
/// not an expression.
fn lex(text: &str) -> Option<Vec<Token>> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut at = 0;

    while at < chars.len() {
        let c = chars[at];
        match c {
            c if c.is_whitespace() => at += 1,
            c if c.is_ascii_digit() || c == '.' => {
                let (value, next) = number(&chars, at)?;
                tokens.push(Token::Number(value));
                at = next;
            }
            '+' => {
                tokens.push(Token::Plus);
                at += 1;
            }
            '-' | '−' => {
                tokens.push(Token::Minus);
                at += 1;
            }
            '*' | '×' | '⋅' => {
                tokens.push(Token::Star);
                at += 1;
            }
            '/' | '÷' => {
                tokens.push(Token::Slash);
                at += 1;
            }
            '%' => {
                tokens.push(Token::Percent);
                at += 1;
            }
            '(' => {
                tokens.push(Token::Open);
                at += 1;
            }
            ')' => {
                tokens.push(Token::Close);
                at += 1;
            }
            c if c.is_alphabetic() => {
                let mut word = String::new();
                while at < chars.len() && chars[at].is_alphabetic() {
                    word.push(chars[at]);
                    at += 1;
                }
                if word == "of" {
                    tokens.push(Token::Of);
                } else {
                    return None;
                }
            }
            _ => return None,
        }
    }

    Some(tokens)
}

struct Parser<'a> {
    tokens: &'a [Token],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<Token> {
        self.tokens.get(self.at).copied()
    }

    fn advance(&mut self) {
        self.at += 1;
    }

    /// `sum := term (('+' | '-') term)*`
    fn sum(&mut self) -> Option<f64> {
        let mut value = self.term()?;
        loop {
            match self.peek() {
                Some(Token::Plus) => {
                    self.advance();
                    value += self.term()?;
                }
                Some(Token::Minus) => {
                    self.advance();
                    value -= self.term()?;
                }
                _ => return Some(value),
            }
        }
    }

    /// `term := unary (('*' | '/' | 'of') unary)*`
    fn term(&mut self) -> Option<f64> {
        let mut value = self.unary()?;
        loop {
            match self.peek() {
                Some(Token::Star) | Some(Token::Of) => {
                    self.advance();
                    value *= self.unary()?;
                }
                Some(Token::Slash) => {
                    self.advance();
                    let divisor = self.unary()?;
                    if divisor == 0.0 {
                        return None; // `1/0` is not a value to show
                    }
                    value /= divisor;
                }
                _ => return Some(value),
            }
        }
    }

    /// `unary := ('+' | '-') unary | postfix`
    fn unary(&mut self) -> Option<f64> {
        match self.peek() {
            Some(Token::Minus) => {
                self.advance();
                Some(-self.unary()?)
            }
            Some(Token::Plus) => {
                self.advance();
                self.unary()
            }
            _ => self.postfix(),
        }
    }

    /// `postfix := primary '%'*`
    fn postfix(&mut self) -> Option<f64> {
        let mut value = self.primary()?;
        while self.peek() == Some(Token::Percent) {
            self.advance();
            value /= 100.0;
        }
        Some(value)
    }

    /// `primary := number | '(' sum ')'`
    fn primary(&mut self) -> Option<f64> {
        match self.peek()? {
            Token::Number(value) => {
                self.advance();
                Some(value)
            }
            Token::Open => {
                self.advance();
                let value = self.sum()?;
                if self.peek() != Some(Token::Close) {
                    return None;
                }
                self.advance();
                Some(value)
            }
            _ => None,
        }
    }
}
