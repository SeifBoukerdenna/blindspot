//! A calculator for the query field.
//!
//! Arithmetic only. "15% of 89" and friends are deliberately absent: parsing words is a
//! natural-language query, which CLAUDE.md lists as an explicit non-goal, and scope creep
//! is the stated failure mode for this project. Written with an explicit operator —
//! `15% * 89` — the same sum works fine.
//!
//! Hand-rolled recursive descent rather than a crate. The grammar is nine lines, the
//! whole thing is pure and allocation-free on the hot path, and pulling in a parser
//! dependency to evaluate `12 * 34` would cost more than it saves.

/// Evaluates `input` if it looks like arithmetic.
///
/// Returns `None` for anything that is not a complete, finite expression — including a
/// bare number. `2` on its own is far more likely to be the start of an app name than a
/// sum the user wants echoed back, and an operator is what separates the two.
/// As [`evaluate`] but accepts a bare number, for tools that take an expression argument —
/// `hex 255` has no operator and must still produce a value.
pub fn evaluate_value(input: &str) -> Option<f64> {
    let tokens = tokenize(input)?;
    let mut parser = Parser {
        tokens: &tokens,
        at: 0,
    };
    let value = parser.expression()?;
    (parser.at == tokens.len() && value.is_finite()).then_some(value)
}

pub fn evaluate(input: &str) -> Option<f64> {
    let tokens = tokenize(input)?;
    if !tokens
        .iter()
        .any(|t| matches!(t, Token::Op(_) | Token::Mod))
    {
        return None;
    }
    let mut parser = Parser {
        tokens: &tokens,
        at: 0,
    };
    let value = parser.expression()?;
    // Trailing junk means we understood only a prefix, which is not an answer.
    if parser.at != tokens.len() {
        return None;
    }
    value.is_finite().then_some(value)
}

/// Formats a result the way a calculator would: no trailing `.0`, no float noise.
pub fn format(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        return format!("{value:.0}");
    }
    // Ten significant digits is enough for anything typed by hand and short enough to
    // hide the usual binary-floating-point tail: 0.1 + 0.2 reads as 0.3, not 0.30000000004.
    let mut s = format!("{value:.10}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Token {
    Number(f64),
    Op(char),
    /// Modulo, spelled as a word so `%` is free to mean percent.
    Mod,
    Open,
    Close,
}

fn tokenize(input: &str) -> Option<Vec<Token>> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' => i += 1,
            '+' | '-' | '*' | '/' | '%' | '^' => {
                tokens.push(Token::Op(c));
                i += 1;
            }
            c if c.is_ascii_alphabetic() => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                if !word.eq_ignore_ascii_case("mod") {
                    // Any other word means this is a name, not a sum. Bailing here is
                    // what keeps "Modem Manager" out of the calculator.
                    return None;
                }
                tokens.push(Token::Mod);
            }
            '(' => {
                tokens.push(Token::Open);
                i += 1;
            }
            ')' => {
                tokens.push(Token::Close);
                i += 1;
            }
            // `0xff`: a hex literal, so `0xff + 1` works as arithmetic.
            '0' if chars.get(i + 1).is_some_and(|x| *x == 'x' || *x == 'X') => {
                let start = i + 2;
                let mut end = start;
                while end < chars.len() && chars[end].is_ascii_hexdigit() {
                    end += 1;
                }
                let digits: String = chars[start..end].iter().collect();
                tokens.push(Token::Number(u64::from_str_radix(&digits, 16).ok()? as f64));
                i = end;
            }
            c if c.is_ascii_digit() || c == '.' => {
                let start = i;
                // `_` is consumed *inside* the number so `1_000` is one token, not two.
                // `,` is deliberately not a separator: on a French keyboard `1,5` means
                // one and a half, and guessing which the user meant is worse than
                // declining to calculate.
                while i < chars.len()
                    && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == '_')
                {
                    i += 1;
                }
                let text: String = chars[start..i].iter().filter(|c| **c != '_').collect();
                tokens.push(Token::Number(text.parse().ok()?));
            }
            // Anything else means this is not arithmetic — an app name, most likely.
            // Bailing out entirely is what keeps "1Password" out of the calculator.
            _ => return None,
        }
    }

    (!tokens.is_empty()).then_some(tokens)
}

struct Parser<'a> {
    tokens: &'a [Token],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<Token> {
        self.tokens.get(self.at).copied()
    }

    /// `expression := term (('+' | '-') term)*`
    fn expression(&mut self) -> Option<f64> {
        let mut value = self.term()?;
        while let Some(Token::Op(op @ ('+' | '-'))) = self.peek() {
            self.at += 1;
            let rhs = self.term()?;
            value = if op == '+' { value + rhs } else { value - rhs };
        }
        Some(value)
    }

    /// `term := power (('*' | '/' | 'mod') power)*`
    fn term(&mut self) -> Option<f64> {
        let mut value = self.power()?;
        loop {
            match self.peek() {
                Some(Token::Op(op @ ('*' | '/'))) => {
                    self.at += 1;
                    let rhs = self.power()?;
                    value = if op == '*' { value * rhs } else { value / rhs };
                }
                Some(Token::Mod) => {
                    self.at += 1;
                    value %= self.power()?;
                }
                _ => break,
            }
        }
        Some(value)
    }

    /// `power := unary ('^' power)?` — right associative, so `2^3^2` is 512.
    fn power(&mut self) -> Option<f64> {
        let base = self.unary()?;
        if let Some(Token::Op('^')) = self.peek() {
            self.at += 1;
            let exponent = self.power()?;
            return Some(base.powf(exponent));
        }
        Some(base)
    }

    /// `unary := ('-' | '+')* postfix`
    fn unary(&mut self) -> Option<f64> {
        match self.peek()? {
            Token::Op('-') => {
                self.at += 1;
                Some(-self.unary()?)
            }
            Token::Op('+') => {
                self.at += 1;
                self.unary()
            }
            _ => self.postfix(),
        }
    }

    /// `postfix := primary '%'*`
    ///
    /// Percent is a suffix meaning "divide by a hundred", which binds tighter than
    /// anything else — so `15% * 89` is 13.35 and `200 * 15%` is 30.
    ///
    /// Deliberately *not* the contextual behaviour desk calculators have, where
    /// `200 + 10%` means 220 but `200 * 10%` means 20. That rule changes meaning based
    /// on the neighbouring operator, which is fine on a device with no other features
    /// and confusing in a launcher.
    fn postfix(&mut self) -> Option<f64> {
        let mut value = self.primary()?;
        while let Some(Token::Op('%')) = self.peek() {
            self.at += 1;
            value /= 100.0;
        }
        Some(value)
    }

    /// `primary := number | '(' expression ')'`
    fn primary(&mut self) -> Option<f64> {
        match self.peek()? {
            Token::Number(n) => {
                self.at += 1;
                Some(n)
            }
            Token::Open => {
                self.at += 1;
                let value = self.expression()?;
                matches!(self.peek(), Some(Token::Close)).then(|| self.at += 1)?;
                Some(value)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(input: &str) -> Option<String> {
        evaluate(input).map(format)
    }

    #[test]
    fn the_four_operations_work() {
        assert_eq!(eval("2+2").as_deref(), Some("4"));
        assert_eq!(eval("12 * 34").as_deref(), Some("408"));
        assert_eq!(eval("100 - 42").as_deref(), Some("58"));
        assert_eq!(eval("144/12").as_deref(), Some("12"));
    }

    #[test]
    fn precedence_and_parentheses_are_respected() {
        assert_eq!(eval("2+3*4").as_deref(), Some("14"));
        assert_eq!(eval("(2+3)*4").as_deref(), Some("20"));
        assert_eq!(eval("(1920-1440)/2").as_deref(), Some("240"));
        assert_eq!(eval("((3))").as_deref(), None, "no operator, no answer");
    }

    #[test]
    fn power_is_right_associative() {
        assert_eq!(eval("2^10").as_deref(), Some("1024"));
        assert_eq!(eval("2^3^2").as_deref(), Some("512"));
    }

    #[test]
    fn percent_divides_by_a_hundred() {
        assert_eq!(eval("50%").as_deref(), Some("0.5"));
        assert_eq!(eval("15% * 89").as_deref(), Some("13.35"));
        assert_eq!(eval("200 * 15%").as_deref(), Some("30"));
        assert_eq!(eval("-25%").as_deref(), Some("-0.25"));
    }

    #[test]
    fn modulo_is_spelled_out() {
        assert_eq!(eval("10 mod 3").as_deref(), Some("1"));
        assert_eq!(eval("10 MOD 3").as_deref(), Some("1"));
        assert_eq!(eval("1024 mod 256").as_deref(), Some("0"));
        assert_eq!(eval("2 + 10 mod 3").as_deref(), Some("3"), "binds like *");
        // `%` no longer means modulo, and a wrong answer would be worse than none.
        assert_eq!(eval("10 % 3"), None);
    }

    #[test]
    fn words_that_are_not_mod_are_not_arithmetic() {
        for input in ["Modem Manager", "mod", "mod 3", "3 mod", "modulo 3", "xmod"] {
            assert_eq!(eval(input), None, "{input:?} should not calculate");
        }
    }

    #[test]
    fn unary_minus_works_anywhere() {
        assert_eq!(eval("-5 + 3").as_deref(), Some("-2"));
        assert_eq!(eval("3 * -2").as_deref(), Some("-6"));
        assert_eq!(eval("--4 + 0").as_deref(), Some("4"));
    }

    #[test]
    fn a_bare_number_is_not_an_answer() {
        // Otherwise typing "1" while reaching for 1Password shows a calculator row.
        for input in ["2", "3.5", "  42  ", "(7)"] {
            assert_eq!(eval(input), None, "{input:?} should not calculate");
        }
    }

    #[test]
    fn app_names_are_never_mistaken_for_arithmetic() {
        for input in [
            "Safari",
            "1Password",
            "Final Cut Pro",
            "x^2",
            "2 apples",
            "",
        ] {
            assert_eq!(eval(input), None, "{input:?} should not calculate");
        }
    }

    #[test]
    fn incomplete_expressions_yield_nothing() {
        for input in ["2 +", "* 3", "(2+3", "2+3)", "()", "+"] {
            assert_eq!(eval(input), None, "{input:?} should not calculate");
        }
    }

    #[test]
    fn results_that_are_not_finite_are_not_shown() {
        assert_eq!(eval("1/0"), None, "infinity is not an answer");
        assert_eq!(eval("0/0"), None, "NaN is not an answer");
    }

    #[test]
    fn formatting_hides_float_noise_and_trailing_zeros() {
        assert_eq!(format(408.0), "408");
        assert_eq!(format(-2.0), "-2");
        assert_eq!(format(13.35), "13.35");
        assert_eq!(
            eval("0.1 + 0.2").as_deref(),
            Some("0.3"),
            "no 0.30000000004"
        );
        assert_eq!(eval("1/3").as_deref(), Some("0.3333333333"));
    }

    #[test]
    fn underscores_group_digits_but_commas_are_refused() {
        assert_eq!(eval("1_000 + 1").as_deref(), Some("1001"));
        assert_eq!(eval("1_000_000 / 1_000").as_deref(), Some("1000"));
        // Ambiguous: `1,5` is one and a half on a French keyboard. Declining beats guessing.
        assert_eq!(eval("1,000 * 2"), None);
    }
}
