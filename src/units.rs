//! Electrical units and bounded, non-executable waveform expressions.
use crate::raw::{RawPlot, Sample, Variable};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    dimensions: Option<[i32; 3]>, // V, A, s
    pub label: String,
}
impl Unit {
    pub fn parse(kind: &str) -> Self {
        let (dimensions, label) = match kind.to_ascii_lowercase().as_str() {
            "voltage" | "v" => (Some([1, 0, 0]), "V"),
            "current" | "a" => (Some([0, 1, 0]), "A"),
            "time" | "s" => (Some([0, 0, 1]), "s"),
            "frequency" | "hz" => (Some([0, 0, -1]), "Hz"),
            "resistance" | "ohm" | "ω" => (Some([1, -1, 0]), "Ω"),
            "conductance" | "siemens" => (Some([-1, 1, 0]), "S"),
            "capacitance" | "f" => (Some([-1, 1, 1]), "F"),
            "inductance" | "h" => (Some([1, -1, 1]), "H"),
            "power" | "w" => (Some([1, 1, 0]), "W"),
            "charge" | "c" => (Some([0, 1, 1]), "C"),
            "1" | "dimensionless" => (Some([0, 0, 0]), "1"),
            "v/v" => (Some([0, 0, 0]), "V/V"),
            "a/a" => (Some([0, 0, 0]), "A/A"),
            _ => (None, "?"),
        };
        // Generated composite units are canonical V/A/s exponent expressions.
        let mut result = Self {
            dimensions,
            label: label.into(),
        };
        if dimensions.is_none() && kind.starts_with("unit:") {
            let pieces: Vec<_> = kind[5..].split(',').collect();
            if pieces.len() == 3 {
                if let (Ok(v), Ok(a), Ok(s)) =
                    (pieces[0].parse(), pieces[1].parse(), pieces[2].parse())
                {
                    result = Self::dimensions([v, a, s]);
                }
            }
        }
        result
    }
    fn dimensions(d: [i32; 3]) -> Self {
        let known = match d {
            [1, 0, 0] => "V",
            [0, 1, 0] => "A",
            [0, 0, 1] => "s",
            [0, 0, -1] => "Hz",
            [1, -1, 0] => "Ω",
            [-1, 1, 0] => "S",
            [-1, 1, 1] => "F",
            [1, -1, 1] => "H",
            [1, 1, 0] => "W",
            [0, 1, 1] => "C",
            [0, 0, 0] => "1",
            _ => "",
        };
        let label = if known.is_empty() {
            let part = |positive: bool| {
                d.iter()
                    .zip(["V", "A", "s"])
                    .filter(|(n, _)| if positive { **n > 0 } else { **n < 0 })
                    .map(|(n, u)| {
                        if n.abs() == 1 {
                            u.to_string()
                        } else {
                            format!("{u}^{}", n.abs())
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("·")
            };
            let top = part(true);
            let bottom = part(false);
            if bottom.is_empty() {
                top
            } else {
                format!(
                    "{}/{}",
                    if top.is_empty() { "1" } else { &top },
                    if bottom.contains('·') {
                        format!("({bottom})")
                    } else {
                        bottom
                    }
                )
            }
        } else {
            known.into()
        };
        Self {
            dimensions: Some(d),
            label,
        }
    }
    pub fn kind(&self) -> String {
        match self.label.as_str() {
            "V" => "voltage".into(),
            "A" => "current".into(),
            "s" => "time".into(),
            "Hz" => "frequency".into(),
            "Ω" => "resistance".into(),
            "S" => "conductance".into(),
            "F" => "capacitance".into(),
            "H" => "inductance".into(),
            "W" => "power".into(),
            "C" => "charge".into(),
            "V/V" | "A/A" | "1" => self.label.clone(),
            _ => self
                .dimensions
                .map(|d| format!("unit:{},{},{}", d[0], d[1], d[2]))
                .unwrap_or_else(|| "notype".into()),
        }
    }
    fn product(&self, rhs: &Self, divide: bool) -> Self {
        let (Some(a), Some(b)) = (self.dimensions, rhs.dimensions) else {
            return Self::parse("notype");
        };
        if divide && a == b && matches!(self.label.as_str(), "V" | "A") && self.label == rhs.label {
            return Self::parse(&format!("{}/{}", self.label, rhs.label));
        }
        Self::dimensions(std::array::from_fn(|i| {
            a[i] + if divide { -b[i] } else { b[i] }
        }))
    }
}

#[derive(Clone)]
enum Expr {
    Constant(f64),
    Signal(usize),
    Unary(char, Box<Expr>),
    Binary(char, Box<Expr>, Box<Expr>),
    Abs(Box<Expr>),
    Derivative(Box<Expr>),
}
struct Parser<'a> {
    text: &'a str,
    offset: usize,
    plot: &'a RawPlot,
    depth: usize,
    aliases: &'a std::collections::BTreeMap<String, String>,
}
impl Parser<'_> {
    fn ws(&mut self) {
        while self
            .text
            .as_bytes()
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset += 1;
        }
    }
    fn expression(&mut self, min: u8) -> Result<Expr, String> {
        self.depth += 1;
        if self.depth > 64 {
            return Err("expression nesting exceeds 64".into());
        }
        self.ws();
        let mut lhs = self.atom()?;
        loop {
            self.ws();
            let Some(&op) = self.text.as_bytes().get(self.offset) else {
                break;
            };
            let precedence = match op {
                b'+' | b'-' => 1,
                b'*' | b'/' => 2,
                _ => 0,
            };
            if precedence == 0 || precedence < min {
                break;
            }
            self.offset += 1;
            let rhs = self.expression(precedence + 1)?;
            lhs = Expr::Binary(op as char, Box::new(lhs), Box::new(rhs));
        }
        self.depth -= 1;
        Ok(lhs)
    }
    fn atom(&mut self) -> Result<Expr, String> {
        self.ws();
        let rest = &self.text[self.offset..];
        if let Some(c) = rest.chars().next().filter(|c| matches!(c, '+' | '-')) {
            self.offset += 1;
            return Ok(Expr::Unary(c, Box::new(self.expression(3)?)));
        }
        if rest.starts_with('(') {
            self.offset += 1;
            let e = self.expression(1)?;
            self.ws();
            if self.text.as_bytes().get(self.offset) != Some(&b')') {
                return Err("missing closing parenthesis".into());
            }
            self.offset += 1;
            return Ok(e);
        }
        for (prefix, deriv) in [("abs(", false), ("deriv(", true)] {
            if rest.starts_with(prefix) {
                self.offset += prefix.len();
                let e = self.expression(1)?;
                self.ws();
                if self.text.as_bytes().get(self.offset) != Some(&b')') {
                    return Err("missing closing function parenthesis".into());
                }
                self.offset += 1;
                return Ok(if deriv {
                    Expr::Derivative(Box::new(e))
                } else {
                    Expr::Abs(Box::new(e))
                });
            }
        }
        for prefix in ["v(", "i("] {
            if rest.starts_with(prefix) {
                let end = rest.find(')').ok_or("missing signal closing parenthesis")?;
                let name = rest[2..end].trim();
                let candidates = if prefix == "v(" {
                    vec![format!("v({name})"), name.to_string()]
                } else {
                    vec![
                        format!("i({name})"),
                        format!("{name}#branch"),
                        format!("{name}:flow(br)"),
                    ]
                };
                let index = self.lookup(&candidates)?;
                self.offset += end + 1;
                return Ok(Expr::Signal(index));
            }
        }
        if rest.starts_with('`') {
            let end = rest[1..].find('`').ok_or("unterminated quoted signal")? + 1;
            let index = self.lookup(&[rest[1..end].into()])?;
            self.offset += end + 1;
            return Ok(Expr::Signal(index));
        }
        if rest.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
            let start = self.offset;
            let bytes = self.text.as_bytes();
            while bytes
                .get(self.offset)
                .is_some_and(|c| c.is_ascii_digit() || *c == b'.')
            {
                self.offset += 1;
            }
            if bytes
                .get(self.offset)
                .is_some_and(|c| matches!(c, b'e' | b'E'))
            {
                self.offset += 1;
                if bytes
                    .get(self.offset)
                    .is_some_and(|c| matches!(c, b'+' | b'-'))
                {
                    self.offset += 1;
                }
                while bytes.get(self.offset).is_some_and(u8::is_ascii_digit) {
                    self.offset += 1;
                }
            }
            let n: f64 = self.text[start..self.offset]
                .parse()
                .map_err(|_| "invalid numeric constant")?;
            if !n.is_finite() {
                return Err("constant must be finite".into());
            }
            return Ok(Expr::Constant(n));
        }
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '#')))
            .unwrap_or(rest.len());
        if end == 0 {
            return Err("expected number, signal, v(...), i(...), abs(...) or deriv(...)".into());
        }
        let index = self.lookup(&[rest[..end].into()])?;
        self.offset += end;
        Ok(Expr::Signal(index))
    }
    fn lookup(&self, names: &[String]) -> Result<usize, String> {
        for name in names {
            if let Some(i) = self.plot.variables.iter().position(|v| v.name == *name) {
                return Ok(i);
            }
            if let Some(native) = self.aliases.get(name) {
                if let Some(i) = self.plot.variables.iter().position(|v| v.name == *native) {
                    return Ok(i);
                }
            }
        }
        let found: Vec<_> = self
            .plot
            .variables
            .iter()
            .enumerate()
            .filter(|(_, v)| names.iter().any(|n| v.name.eq_ignore_ascii_case(n)))
            .map(|(i, _)| i)
            .collect();
        if found.len() == 1 {
            Ok(found[0])
        } else {
            Err(format!("unknown or ambiguous signal: {}", names[0]))
        }
    }
}
impl Expr {
    fn validate(&self, depth: usize, derivatives: usize) -> Result<(), String> {
        if depth > 64 || derivatives > 1 {
            return Err("expression too deep or nested derivatives unsupported".into());
        }
        match self {
            Self::Unary(_, e) | Self::Abs(e) => e.validate(depth + 1, derivatives),
            Self::Derivative(e) => e.validate(depth + 1, derivatives + 1),
            Self::Binary(_, a, b) => {
                a.validate(depth + 1, derivatives)?;
                b.validate(depth + 1, derivatives)
            }
            _ => Ok(()),
        }
    }
    fn unit(&self, plot: &RawPlot) -> Result<Unit, String> {
        Ok(match self {
            Self::Constant(_) => Unit::parse("1"),
            Self::Signal(i) => Unit::parse(&plot.variables[*i].kind),
            Self::Unary(_, e) | Self::Abs(e) => e.unit(plot)?,
            Self::Derivative(e) => {
                let axis = plot.scale().ok_or("deriv requires a sweep/time axis")?;
                e.unit(plot)?
                    .product(&Unit::parse(&plot.variables[axis].kind), true)
            }
            Self::Binary(op, a, b) => {
                let x = a.unit(plot)?;
                let y = b.unit(plot)?;
                match op {
                    '+' | '-' => {
                        if x.dimensions.is_some()
                            && y.dimensions.is_some()
                            && x.dimensions != y.dimensions
                        {
                            return Err(format!(
                                "incompatible units: {} {op} {}",
                                x.label, y.label
                            ));
                        }
                        if x.dimensions.is_none() || y.dimensions.is_none() {
                            Unit::parse("notype")
                        } else {
                            x
                        }
                    }
                    '*' => x.product(&y, false),
                    '/' => x.product(&y, true),
                    _ => unreachable!(),
                }
            }
        })
    }
    fn value(&self, plot: &RawPlot, index: usize) -> Sample {
        match self {
            Self::Constant(n) => Sample {
                real: *n,
                imaginary: 0.,
            },
            Self::Signal(i) => plot.variables[*i].values[index],
            Self::Unary(op, e) => {
                let v = e.value(plot, index);
                if *op == '-' {
                    Sample {
                        real: -v.real,
                        imaginary: -v.imaginary,
                    }
                } else {
                    v
                }
            }
            Self::Abs(e) => {
                let v = e.value(plot, index);
                Sample {
                    real: v.real.hypot(v.imaginary),
                    imaginary: 0.,
                }
            }
            Self::Derivative(e) => {
                let axis = &plot.variables[plot.scale().unwrap()].values;
                let lo = index.saturating_sub(1);
                let hi = (index + 1).min(plot.points - 1);
                let dt = axis[hi].real - axis[lo].real;
                let a = e.value(plot, lo);
                let b = e.value(plot, hi);
                Sample {
                    real: (b.real - a.real) / dt,
                    imaginary: (b.imaginary - a.imaginary) / dt,
                }
            }
            Self::Binary(op, a, b) => {
                let a = a.value(plot, index);
                let b = b.value(plot, index);
                match op {
                    '+' => Sample {
                        real: a.real + b.real,
                        imaginary: a.imaginary + b.imaginary,
                    },
                    '-' => Sample {
                        real: a.real - b.real,
                        imaginary: a.imaginary - b.imaginary,
                    },
                    '*' => Sample {
                        real: a.real * b.real - a.imaginary * b.imaginary,
                        imaginary: a.real * b.imaginary + a.imaginary * b.real,
                    },
                    '/' => {
                        let scale = b.real * b.real + b.imaginary * b.imaginary;
                        Sample {
                            real: (a.real * b.real + a.imaginary * b.imaginary) / scale,
                            imaginary: (a.imaginary * b.real - a.real * b.imaginary) / scale,
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
}
/// Evaluate a bounded expression without changing its source plot. Invalid samples
/// (e.g. division by zero) remain non-finite and the graph renders them as gaps.
pub fn calculate(plot: &RawPlot, expression: &str) -> Result<Variable, String> {
    calculate_with_aliases(plot, expression, &Default::default())
}
pub fn calculate_with_aliases(
    plot: &RawPlot,
    expression: &str,
    aliases: &std::collections::BTreeMap<String, String>,
) -> Result<Variable, String> {
    if expression.is_empty() || expression.len() > 4096 {
        return Err("expression must contain 1..4096 bytes".into());
    }
    let mut parser = Parser {
        text: expression,
        offset: 0,
        plot,
        depth: 0,
        aliases,
    };
    let expr = parser.expression(1)?;
    parser.ws();
    if parser.offset != expression.len() {
        return Err("unexpected expression suffix".into());
    }
    expr.validate(0, 0)?;
    let unit = expr.unit(plot)?;
    Ok(Variable {
        name: expression.into(),
        kind: unit.kind(),
        native_kind: "calculated".into(),
        unit: unit.label,
        values: (0..plot.points).map(|i| expr.value(plot, i)).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn electrical_ratios_and_products() {
        let v = Unit::parse("voltage");
        let a = Unit::parse("current");
        assert_eq!(v.product(&v, true).label, "V/V");
        assert_eq!(v.product(&a, true).label, "Ω");
        assert_eq!(v.product(&a, false).label, "W");
        assert_eq!(a.product(&v, true).label, "S");
        assert_eq!(
            Unit::parse(&v.product(&Unit::parse("time"), true).kind()).label,
            "V/s"
        );
    }
    #[test]
    fn calculate_signals_with_checked_dimensions() {
        let raw=crate::raw::RawFile::parse(b"Title: test\nPlotname: Transient Analysis\nFlags: real\nNo. Variables: 3\nNo. Points: 2\nVariables:\n0 time time\n1 v(out) voltage\n2 i(v1) current\nValues:\n0 0\n2\n0.001\n1 1\n4\n0.002\n").unwrap();
        let p = &raw.plots[0];
        let r = calculate(p, "v(out)/i(v1)").unwrap();
        assert_eq!(r.kind, "resistance");
        assert!((r.values[0].real - 2000.).abs() < 1e-9);
        assert_eq!(calculate(p, "v(out)/v(out)").unwrap().kind, "V/V");
        assert_eq!(calculate(p, "deriv(v(out))").unwrap().values[0].real, 2.);
        assert!(
            calculate(p, "v(out)+i(v1)")
                .unwrap_err()
                .contains("incompatible")
        );
        assert!(calculate(p, "__import__('os')").is_err());
    }
}
