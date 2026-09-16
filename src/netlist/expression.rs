//! Strict scalar expression tree. No simulator text substitution or evaluation.
use std::collections::BTreeMap;
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", content = "arguments", rename_all = "snake_case")]
pub enum Expr {
    Number(f64),
    Name(String),
    Voltage(String, Option<String>),
    Unary(char, Box<Expr>),
    Binary(char, Box<Expr>, Box<Expr>),
}
impl Expr {
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let text = text
            .strip_prefix("\\{")
            .and_then(|s| s.strip_suffix("\\}"))
            .unwrap_or(text);
        let text = text
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .unwrap_or(text);
        let mut parser = Parser {
            text: text.as_bytes(),
            at: 0,
        };
        let result = parser.expr(0)?;
        parser.space();
        if parser.at != parser.text.len() {
            return Err(format!("unsupported expression {text:?} at {}", parser.at));
        }
        Ok(result)
    }
    pub fn evaluate(&self, scope: &BTreeMap<String, f64>) -> Result<f64, String> {
        let value = match self {
            Self::Voltage(..) => {
                return Err("voltage references are allowed only in behavioral expressions".into());
            }
            Self::Number(v) => *v,
            Self::Name(n) => *scope
                .get(n)
                .ok_or_else(|| format!("undefined parameter {n}"))?,
            Self::Unary(op, v) => v.evaluate(scope)? * if *op == '-' { -1.0 } else { 1.0 },
            Self::Binary(op, a, b) => {
                let (a, b) = (a.evaluate(scope)?, b.evaluate(scope)?);
                match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    '/' => a / b,
                    '^' => a.powf(b),
                    _ => unreachable!(),
                }
            }
        };
        if !value.is_finite() {
            return Err("non-finite expression result".into());
        }
        Ok(value)
    }
}
struct Parser<'a> {
    text: &'a [u8],
    at: usize,
}
impl Parser<'_> {
    fn space(&mut self) {
        while self.text.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }
    fn node(&mut self) -> Result<String, String> {
        self.space();
        let start = self.at;
        while self
            .text
            .get(self.at)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            self.at += 1;
        }
        if start == self.at {
            return Err("expected literal voltage node".into());
        }
        Ok(std::str::from_utf8(&self.text[start..self.at])
            .unwrap()
            .into())
    }
    fn expr(&mut self, min: u8) -> Result<Expr, String> {
        self.space();
        let ch = *self.text.get(self.at).ok_or("expected expression")?;
        let mut lhs = if ch == b'+' || ch == b'-' {
            self.at += 1;
            Expr::Unary(ch as char, Box::new(self.expr(5)?))
        } else if ch == b'(' {
            self.at += 1;
            let v = self.expr(0)?;
            self.space();
            if self.text.get(self.at) != Some(&b')') {
                return Err("expected closing parenthesis".into());
            }
            self.at += 1;
            v
        } else if ch.is_ascii_digit() || ch == b'.' {
            let start = self.at;
            while self
                .text
                .get(self.at)
                .is_some_and(|c| c.is_ascii_digit() || *c == b'.')
            {
                self.at += 1;
            }
            if self
                .text
                .get(self.at)
                .is_some_and(|c| *c == b'e' || *c == b'E')
            {
                self.at += 1;
                if self
                    .text
                    .get(self.at)
                    .is_some_and(|c| *c == b'+' || *c == b'-')
                {
                    self.at += 1;
                }
                while self.text.get(self.at).is_some_and(u8::is_ascii_digit) {
                    self.at += 1;
                }
            }
            let number = std::str::from_utf8(&self.text[start..self.at])
                .unwrap()
                .parse::<f64>()
                .map_err(|_| "invalid number")?;
            let start = self.at;
            while self.text.get(self.at).is_some_and(u8::is_ascii_alphabetic) {
                self.at += 1;
            }
            let suffix = std::str::from_utf8(&self.text[start..self.at])
                .unwrap()
                .to_ascii_lowercase();
            let scale = match suffix.as_str() {
                "" => 1.0,
                "t" => 1e12,
                "g" => 1e9,
                "meg" => 1e6,
                "k" => 1e3,
                "m" => 1e-3,
                "u" => 1e-6,
                "n" => 1e-9,
                "p" => 1e-12,
                "f" => 1e-15,
                _ => return Err(format!("unsupported numeric suffix {suffix}")),
            };
            Expr::Number(number * scale)
        } else if ch.is_ascii_alphabetic() || ch == b'_' {
            let start = self.at;
            self.at += 1;
            while self
                .text
                .get(self.at)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            {
                self.at += 1;
            }
            let name = std::str::from_utf8(&self.text[start..self.at])
                .unwrap()
                .to_owned();
            self.space();
            if name.eq_ignore_ascii_case("v") && self.text.get(self.at) == Some(&b'(') {
                self.at += 1;
                let a = self.node()?;
                self.space();
                let b = if self.text.get(self.at) == Some(&b',') {
                    self.at += 1;
                    Some(self.node()?)
                } else {
                    None
                };
                self.space();
                if self.text.get(self.at) != Some(&b')') {
                    return Err("voltage requires one or two literal nodes".into());
                }
                self.at += 1;
                Expr::Voltage(a, b)
            } else {
                Expr::Name(name)
            }
        } else {
            return Err(format!("unsupported expression character {:?}", ch as char));
        };
        loop {
            self.space();
            let Some(&op) = self.text.get(self.at) else {
                break;
            };
            let (left, right) = match op {
                b'+' | b'-' => (1, 2),
                b'*' | b'/' => (3, 4),
                b'^' => (6, 5),
                _ => break,
            };
            if left < min {
                break;
            }
            self.at += 1;
            lhs = Expr::Binary(op as char, Box::new(lhs), Box::new(self.expr(right)?));
        }
        Ok(lhs)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arithmetic_and_scope() {
        let s = BTreeMap::from([("w".into(), 3e-6)]);
        assert_eq!(
            Expr::parse("{2*w + 1u}").unwrap().evaluate(&s).unwrap(),
            7e-6
        );
        assert_eq!(Expr::parse("-2^2").unwrap().evaluate(&s).unwrap(), -4.0);
    }
    #[test]
    fn rejects_unknowns() {
        for text in ["sin(1)", "@x", "1foo", "1;2", "a?b:c"] {
            assert!(Expr::parse(text).is_err(), "{text}");
        }
        assert!(
            Expr::parse("missing")
                .unwrap()
                .evaluate(&BTreeMap::new())
                .is_err()
        );
        assert!(
            Expr::parse("1/0")
                .unwrap()
                .evaluate(&BTreeMap::new())
                .is_err()
        );
    }
}
