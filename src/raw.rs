//! Ngspice raw files: ASCII and native-endian binary doubles, real or complex,
//! with one or more concatenated plots. No GUI dependencies.
use std::{fs::File, io::Read, path::Path};

const MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_SAMPLES: usize = 16_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    pub real: f64,
    pub imaginary: f64,
}

#[derive(Debug)]
pub struct Variable {
    pub name: String,
    pub kind: String,
    pub values: Vec<Sample>,
}

#[derive(Debug)]
pub struct RawPlot {
    pub title: String,
    pub name: String,
    pub complex: bool,
    pub points: usize,
    pub variables: Vec<Variable>,
}
impl RawPlot {
    /// Operating-point files contain signals, not a sweep axis.
    pub fn scale(&self) -> Option<usize> {
        (!self.name.to_ascii_lowercase().contains("operating point")).then_some(0)
    }
}

#[derive(Debug)]
pub struct RawFile {
    pub plots: Vec<RawPlot>,
}
impl RawFile {
    pub fn open(path: &Path) -> Result<Self, String> {
        let load = || -> Result<Self, String> {
            let file = File::open(path).map_err(|e| e.to_string())?;
            if file.metadata().map_err(|e| e.to_string())?.len() > MAX_BYTES as u64 {
                return Err("raw file exceeds the 256 MiB limit".into());
            }
            let mut bytes = Vec::new();
            file.take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            Self::parse(&bytes)
        };
        load().map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("raw file exceeds the 256 MiB limit".into());
        }
        let mut reader = Reader {
            bytes,
            offset: usize::from(bytes.starts_with(b"\xef\xbb\xbf")) * 3,
        };
        let mut plots = Vec::new();
        let mut samples = 0;
        reader.whitespace();
        while reader.offset < bytes.len() {
            let start = reader.offset;
            let plot = reader
                .plot(&mut samples)
                .map_err(|e| format!("raw plot {} (byte {start}): {e}", plots.len() + 1))?;
            plots.push(plot);
            reader.whitespace();
        }
        if plots.is_empty() {
            return Err("raw file contains no plots".into());
        }
        Ok(Self { plots })
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl Reader<'_> {
    fn whitespace(&mut self) {
        while self
            .bytes
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset += 1;
        }
    }
    fn line(&mut self) -> Result<&str, String> {
        let remaining = &self.bytes[self.offset..];
        if remaining.is_empty() {
            return Err("unexpected end of raw file".into());
        }
        let end = remaining
            .iter()
            .position(|b| *b == b'\n')
            .unwrap_or(remaining.len());
        self.offset += end + usize::from(end < remaining.len());
        std::str::from_utf8(&remaining[..end])
            .map(str::trim)
            .map_err(|_| {
                "expected a UTF-8/ASCII raw header (LTspice UTF-16 files are not supported)".into()
            })
    }
    fn word(&mut self) -> Result<&str, String> {
        self.whitespace();
        let start = self.offset;
        while self
            .bytes
            .get(self.offset)
            .is_some_and(|b| !b.is_ascii_whitespace() && *b != b',')
        {
            self.offset += 1;
        }
        if start == self.offset {
            return Err("missing ASCII raw value".into());
        }
        std::str::from_utf8(&self.bytes[start..self.offset])
            .map_err(|_| "invalid ASCII raw value".into())
    }
    fn number(&mut self) -> Result<f64, String> {
        let word = self.word()?;
        word.parse()
            .map_err(|_| format!("invalid raw number {word:?}"))
    }
    fn binary_number(&mut self) -> f64 {
        // The complete payload length is checked before reading any samples.
        let value =
            f64::from_ne_bytes(self.bytes[self.offset..self.offset + 8].try_into().unwrap());
        self.offset += 8;
        value
    }
    fn plot(&mut self, total: &mut usize) -> Result<RawPlot, String> {
        let mut title = None;
        let mut name = None;
        let mut flags = None;
        let mut count = None;
        let mut points = None;
        loop {
            let line = self.line()?;
            if line.is_empty() {
                continue;
            }
            let (key, value) = line.split_once(':').ok_or("invalid raw header line")?;
            let value = value.trim();
            match key.to_ascii_lowercase().as_str() {
                "title" => title = Some(value.to_owned()),
                "plotname" => name = Some(value.to_owned()),
                "flags" => flags = Some(value.to_ascii_lowercase()),
                "no. variables" => {
                    count = Some(
                        value
                            .parse::<usize>()
                            .map_err(|_| "invalid variable count")?,
                    )
                }
                "no. points" => {
                    points = Some(value.parse::<usize>().map_err(|_| "invalid point count")?)
                }
                "variables" => break,
                "binary" | "values" => return Err("missing Variables section".into()),
                _ => {} // Date, Command, Dimensions, Option, etc.
            }
        }
        let count = count
            .filter(|n| *n > 0 && *n <= 65_536)
            .ok_or("missing or invalid variable count (maximum 65536)")?;
        let points = points
            .filter(|n| *n > 0)
            .ok_or("missing point count or empty plot")?;
        let samples = count.checked_mul(points).ok_or("raw dimensions overflow")?;
        *total = total
            .checked_add(samples)
            .filter(|n| *n <= MAX_SAMPLES)
            .ok_or("raw file exceeds 16 million samples")?;
        let flags = flags.ok_or("missing real/complex flags")?;
        let flag_list: Vec<_> = flags.split_whitespace().collect();
        let complex = flag_list.contains(&"complex");
        if complex == flag_list.contains(&"real") {
            return Err("expected exactly one of the real/complex flags".into());
        }
        if flag_list
            .iter()
            .any(|flag| !matches!(*flag, "real" | "complex" | "padded"))
        {
            return Err(format!(
                "unsupported raw flags {flags:?}; only padded ngspice real/complex data is supported"
            ));
        }
        let mut variables = Vec::new();
        for index in 0..count {
            let fields: Vec<_> = self.line()?.split_whitespace().collect();
            if fields.len() < 3 || fields[0].parse::<usize>().ok() != Some(index) {
                return Err(format!("expected variable {index} with a name and type"));
            }
            variables.push(Variable {
                name: fields[1].into(),
                kind: fields[2].into(),
                values: Vec::new(),
            });
        }
        let section = loop {
            let line = self.line()?;
            if !line.is_empty() {
                break line.to_ascii_lowercase();
            }
        };
        let binary = match section.as_str() {
            "binary:" => true,
            "values:" => false,
            _ => return Err("expected Binary: or Values: after variables".into()),
        };
        let minimum = samples
            .checked_mul(if binary {
                if complex { 16 } else { 8 }
            } else {
                2
            })
            .ok_or("raw payload size overflow")?;
        if self.bytes.len() - self.offset < minimum {
            return Err("truncated raw sample data".into());
        }
        for variable in &mut variables {
            variable.values.reserve(points);
        }
        for index in 0..points {
            if !binary && self.word()?.parse::<usize>().ok() != Some(index) {
                return Err(format!("expected ASCII point index {index}"));
            }
            for variable in &mut variables {
                let real = if binary {
                    self.binary_number()
                } else {
                    self.number()?
                };
                let imaginary = if complex {
                    if binary {
                        self.binary_number()
                    } else {
                        self.whitespace();
                        if self.bytes.get(self.offset) != Some(&b',') {
                            return Err("complex ASCII values need a comma between real and imaginary parts".into());
                        }
                        self.offset += 1;
                        self.number()?
                    }
                } else {
                    0.
                };
                variable.values.push(Sample { real, imaginary });
            }
        }
        Ok(RawPlot {
            title: title.ok_or("missing Title")?,
            name: name.ok_or("missing Plotname")?,
            complex,
            points,
            variables,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(flags: &str, points: usize, section: &str) -> String {
        format!(
            "Title: test\nDate: today\nPlotname: Transient Analysis\nFlags: {flags}\nNo. Variables: 2\nNo. Points: {points}\nVariables:\n\t0\ttime\ttime\n\t1\tv(out)\tvoltage\n{section}:\n"
        )
    }

    #[test]
    fn ascii_and_binary_concatenated_plots_preserve_samples() {
        let mut bytes = format!("{}0 0\n2\n1 1e-6\n3\n", header("real", 2, "Values")).into_bytes();
        bytes.extend(header("real", 2, "Binary").bytes());
        for value in [0_f64, 2., 1e-6, 3.] {
            bytes.extend(value.to_ne_bytes());
        }
        let raw = RawFile::parse(&bytes).unwrap();
        assert_eq!(raw.plots.len(), 2);
        for plot in &raw.plots {
            assert_eq!(plot.variables[0].values[1].real, 1e-6);
            assert_eq!(
                plot.variables[1]
                    .values
                    .iter()
                    .map(|v| v.real)
                    .collect::<Vec<_>>(),
                [2., 3.]
            );
            assert_eq!(plot.scale(), Some(0));
        }
    }

    #[test]
    fn complex_ascii_crlf_and_binary_pairs() {
        let ascii =
            format!("{}0 10, 0\n3, -4\n", header("complex", 1, "Values")).replace('\n', "\r\n");
        let mut binary = header("complex", 1, "Binary").into_bytes();
        for v in [10_f64, 0., 3., -4.] {
            binary.extend(v.to_ne_bytes());
        }
        let a = RawFile::parse(ascii.as_bytes()).unwrap();
        let b = RawFile::parse(&binary).unwrap();
        assert_eq!(
            a.plots[0].variables[1].values,
            b.plots[0].variables[1].values
        );
        assert_eq!(
            b.plots[0].variables[1].values[0],
            Sample {
                real: 3.,
                imaginary: -4.
            }
        );
    }

    #[test]
    fn malformed_counts_indices_payloads_and_layouts_are_rejected() {
        for source in [
            String::new(),
            header("real", usize::MAX, "Binary"),
            header("real", 2, "Binary"),
            format!("{}1 0\n2\n", header("real", 1, "Values")),
            format!("{}0 0\n2\n", header("real fastaccess", 1, "Values")),
            format!("{}0 0\n2\n", header("complex", 1, "Values")),
            format!("{}0 0\nbad\n", header("real", 1, "Values")),
        ] {
            assert!(RawFile::parse(source.as_bytes()).is_err(), "{source}");
        }
        let source = format!("{}0 2\n1e-3", header("real", 1, "Values"))
            .replace("Transient Analysis", "Operating Point");
        assert_eq!(
            RawFile::parse(source.as_bytes()).unwrap().plots[0].scale(),
            None
        );
    }

    #[test]
    #[ignore = "requires ngspice on PATH"]
    fn reads_ngspice_operating_point_transient_and_ac_in_both_encodings() {
        use std::{fs, process::Command};
        let dir = std::env::temp_dir().join(format!("fschem raw test {}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let netlist = dir.join("divider.spice");
        let raw_path = dir.join("divider.raw");
        for analysis in [".op", ".tran 1n 10n", ".ac dec 2 1 100"] {
            fs::write(
                &netlist,
                format!(
                    "* divider\nV1 in 0 DC 3 AC 1\nR1 in out 10k\nR2 out 0 20k\n{analysis}\n.end\n"
                ),
            )
            .unwrap();
            for ascii in [false, true] {
                let mut command = Command::new("ngspice");
                command
                    .arg("-b")
                    .arg("-r")
                    .arg(&raw_path)
                    .arg(&netlist)
                    .env_remove("SPICE_ASCIIRAWFILE");
                if ascii {
                    command.env("SPICE_ASCIIRAWFILE", "1");
                }
                let output = command.output().unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let bytes = fs::read(&raw_path).unwrap();
                assert!(String::from_utf8_lossy(&bytes).contains(if ascii {
                    "Values:"
                } else {
                    "Binary:"
                }));
                let raw = RawFile::open(&raw_path).unwrap();
                let plot = &raw.plots[0];
                let out = plot.variables.iter().find(|v| v.name == "v(out)").unwrap();
                let ac = analysis.starts_with(".ac");
                assert_eq!(plot.complex, ac);
                for value in &out.values {
                    assert!((value.real - if ac { 2. / 3. } else { 2. }).abs() < 1e-10);
                    assert!(value.imaginary.abs() < 1e-10);
                }
                if ac {
                    assert_eq!(plot.variables[0].kind, "frequency");
                }
            }
        }
    }
}
