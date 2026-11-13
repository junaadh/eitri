//! Compact, verified interpolation plans stored in the function literal pool.

use crate::ValueType;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatPart<'a> {
    Literal(&'a str),
    Value(ValueType),
}

pub struct FormatParts<'a> {
    rest: &'a str,
}

pub fn format_parts(plan: &str) -> FormatParts<'_> {
    FormatParts { rest: plan }
}

impl<'a> Iterator for FormatParts<'a> {
    type Item = Result<FormatPart<'a>, ()>;

    fn next(&mut self) -> Option<Self::Item> {
        let (&tag, _) = self.rest.as_bytes().split_first()?;
        let Some(rest) = self.rest.get(1..) else {
            self.rest = "";
            return Some(Err(()));
        };
        self.rest = rest;
        if tag == b'L' {
            let result = (|| {
                let digits = self.rest.get(..8).ok_or(())?;
                let length = usize::from_str_radix(digits, 16).map_err(|_| ())?;
                let text = self
                    .rest
                    .get(8..8usize.checked_add(length).ok_or(())?)
                    .ok_or(())?;
                self.rest = self.rest.get(8 + length..).ok_or(())?;
                Ok(FormatPart::Literal(text))
            })();
            if result.is_err() {
                self.rest = "";
            }
            return Some(result);
        }
        Some(match tag {
            b's' => Ok(FormatPart::Value(ValueType::String)),
            b'i' => Ok(FormatPart::Value(ValueType::I64)),
            b'b' => Ok(FormatPart::Value(ValueType::I8)),
            b'u' => Ok(FormatPart::Value(ValueType::U8)),
            b'U' => Ok(FormatPart::Value(ValueType::U64)),
            b'f' => Ok(FormatPart::Value(ValueType::F32)),
            b'F' => Ok(FormatPart::Value(ValueType::F64)),
            b't' => Ok(FormatPart::Value(ValueType::Bool)),
            b'c' => Ok(FormatPart::Value(ValueType::Char)),
            _ => {
                self.rest = "";
                Err(())
            }
        })
    }
}
