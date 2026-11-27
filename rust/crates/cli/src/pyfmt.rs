//! Formatting that matches what the Python command line printed, byte for byte: JSON as
//! `json.dumps(indent=2)` writes it, `repr` of a string, and half-even rounding.

use serde::Serialize;
use serde_json::ser::{CompactFormatter, Formatter, PrettyFormatter, Serializer};
use serde_json::Value;
use std::io::{self, Write};

// `json.dumps` escapes everything outside printable ASCII (`ensure_ascii`), DEL included, which
// serde_json writes as it is; this wraps a formatter with that difference.
struct Ascii<F>(F);

impl<F: Formatter> Formatter for Ascii<F> {
    fn begin_array<W: ?Sized + Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.0.begin_array(w)
    }
    fn end_array<W: ?Sized + Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.0.end_array(w)
    }
    fn begin_array_value<W: ?Sized + Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
        self.0.begin_array_value(w, first)
    }
    fn end_array_value<W: ?Sized + Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.0.end_array_value(w)
    }
    fn begin_object<W: ?Sized + Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.0.begin_object(w)
    }
    fn end_object<W: ?Sized + Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.0.end_object(w)
    }
    fn begin_object_key<W: ?Sized + Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
        self.0.begin_object_key(w, first)
    }
    fn begin_object_value<W: ?Sized + Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.0.begin_object_value(w)
    }
    fn end_object_value<W: ?Sized + Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.0.end_object_value(w)
    }

    fn write_string_fragment<W: ?Sized + Write>(
        &mut self,
        w: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        let mut units = [0u16; 2];
        for ch in fragment.chars() {
            if ch.is_ascii() && ch != '\u{7f}' {
                w.write_all(&[ch as u8])?;
            } else {
                for unit in ch.encode_utf16(&mut units) {
                    write!(w, "\\u{unit:04x}")?;
                }
            }
        }
        Ok(())
    }
}

/// `json.dumps(value, indent=2)`.
pub(crate) fn dumps_indent2(value: &Value) -> String {
    let mut out = Vec::new();
    let mut serializer =
        Serializer::with_formatter(&mut out, Ascii(PrettyFormatter::with_indent(b"  ")));
    if value.serialize(&mut serializer).is_err() {
        return String::new();
    }
    String::from_utf8(out).unwrap_or_default()
}

/// `json.dumps(text)` for a string.
pub(crate) fn dumps_str(text: &str) -> String {
    let mut out = Vec::new();
    let mut serializer = Serializer::with_formatter(&mut out, Ascii(CompactFormatter));
    if text.serialize(&mut serializer).is_err() {
        return String::new();
    }
    String::from_utf8(out).unwrap_or_default()
}

/// `repr(text)` for a Python `str`.
pub(crate) fn repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `round(x)`: ties go to the even neighbour.
pub(crate) fn round_half_even(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// `f"{x}"` for a float: a whole number keeps its `.0`.
pub(crate) fn float_str(x: f64) -> String {
    format!("{x:?}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_should_escape_non_ascii_and_delete_as_json_dumps_does() {
        // arrange
        let value = json!({"a": "é\u{7f}😀"});

        // act
        let text = dumps_indent2(&value);

        // assert
        assert_eq!("{\n  \"a\": \"\\u00e9\\u007f\\ud83d\\ude00\"\n}", text);
    }

    #[test]
    fn test_should_indent_nested_values_with_two_spaces_and_keep_empty_containers_compact() {
        // arrange
        let value = json!({"errors": [], "stats": {"n": 1}, "files": [{"x": null}]});

        // act
        let text = dumps_indent2(&value);

        // assert
        let expected = "{\n  \"errors\": [],\n  \"stats\": {\n    \"n\": 1\n  },\n  \"files\": [\n    {\n      \"x\": null\n    }\n  ]\n}";
        assert_eq!(expected, text);
    }

    #[test]
    fn test_should_dump_a_string_with_escapes_like_json_dumps() {
        assert_eq!("\"na\\u00efve \\\"x\\\"\"", dumps_str("na\u{ef}ve \"x\""));
    }

    #[test]
    fn test_should_quote_a_string_with_an_apostrophe_in_double_quotes() {
        assert_eq!("\"it's\"", repr_str("it's"));
        assert_eq!("'plain'", repr_str("plain"));
        assert_eq!("'a\\\\b\\n'", repr_str("a\\b\n"));
        assert_eq!("'it\\'s \"x\"'", repr_str("it's \"x\""));
    }

    #[test]
    fn test_should_round_ties_to_even_like_python() {
        assert_eq!(2, round_half_even(2.5));
        assert_eq!(4, round_half_even(3.5));
        assert_eq!(0, round_half_even(0.5));
    }

    #[test]
    fn test_should_keep_the_point_zero_on_a_whole_float() {
        assert_eq!("150.0", float_str(150.0));
        assert_eq!("0.5", float_str(0.5));
    }
}
