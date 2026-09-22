//! Checked exact decimal policy shared by JSON parsing, bridging and rendering.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decimal {
    coefficient: String,
    exponent: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecimalError {
    #[error("invalid decimal syntax")]
    Syntax,
    #[error("decimal exponent is outside the supported Int range")]
    ExponentOutOfRange,
}

impl Decimal {
    /// Parse JSON's number grammar without converting through floating point.
    pub fn parse_token(token: &str) -> Result<Self, DecimalError> {
        let (negative, unsigned) = token
            .strip_prefix('-')
            .map_or((false, token), |s| (true, s));
        let (mantissa, exponent) = unsigned
            .split_once(['e', 'E'])
            .map_or((unsigned, None), |(m, e)| (m, Some(e)));
        let (integer, fraction) = mantissa
            .split_once('.')
            .map_or((mantissa, None), |(i, f)| (i, Some(f)));
        if integer.is_empty()
            || !integer.bytes().all(|b| b.is_ascii_digit())
            || (integer.len() > 1 && integer.starts_with('0'))
            || fraction.is_some_and(|f| f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(DecimalError::Syntax);
        }
        if let Some(exponent) = exponent {
            let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(DecimalError::Syntax);
            }
        }
        let fraction = fraction.unwrap_or("");
        let mut coefficient =
            String::with_capacity(integer.len() + fraction.len() + usize::from(negative));
        if negative {
            coefficient.push('-');
        }
        coefficient.push_str(integer);
        coefficient.push_str(fraction);
        // Zero is independent of the exponent, including exponents too large
        // to parse into a machine integer. No expansion is needed.
        if integer.bytes().chain(fraction.bytes()).all(|b| b == b'0') {
            return Ok(Self {
                coefficient: "0".into(),
                exponent: 0,
            });
        }
        let exponent = exponent.map_or(Ok(0), |e| {
            e.parse::<i128>()
                .map_err(|_| DecimalError::ExponentOutOfRange)
        })?;
        let exponent = exponent
            .checked_sub(fraction.len() as i128)
            .ok_or(DecimalError::ExponentOutOfRange)?;
        Self::normalize(&coefficient, exponent)
    }

    pub fn from_parts(coefficient: &str, exponent: i64) -> Result<Self, DecimalError> {
        Self::normalize(coefficient, i128::from(exponent))
    }

    fn normalize(coefficient: &str, mut exponent: i128) -> Result<Self, DecimalError> {
        let (sign, digits) = coefficient
            .strip_prefix('-')
            .map_or(("", coefficient), |s| ("-", s));
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(DecimalError::Syntax);
        }
        let mut digits = digits.trim_start_matches('0');
        if digits.is_empty() {
            return Ok(Self {
                coefficient: "0".into(),
                exponent: 0,
            });
        }
        while digits.ends_with('0') && exponent < i128::from(i64::MAX) {
            digits = &digits[..digits.len() - 1];
            exponent += 1;
        }
        let exponent = i64::try_from(exponent).map_err(|_| DecimalError::ExponentOutOfRange)?;
        Ok(Self {
            coefficient: format!("{sign}{digits}"),
            exponent,
        })
    }

    pub fn coefficient(&self) -> &str {
        &self.coefficient
    }
    pub fn exponent(&self) -> i64 {
        self.exponent
    }
    pub fn into_parts(self) -> (String, i64) {
        (self.coefficient, self.exponent)
    }

    /// Choose fixed notation only when it is shorter than exponent notation.
    /// Calculate lengths first: extreme exponents never allocate zero padding.
    pub fn render(&self) -> String {
        if self.coefficient == "0" {
            return "0".into();
        }
        let (sign, digits) = self
            .coefficient
            .strip_prefix('-')
            .map_or(("", self.coefficient.as_str()), |s| ("-", s));
        let scientific = format!("{}e{}", self.coefficient, self.exponent);
        let point = digits.len() as i128 + i128::from(self.exponent);
        let fixed_length = sign.len() as i128
            + if point <= 0 {
                2 - point + digits.len() as i128
            } else if point < digits.len() as i128 {
                digits.len() as i128 + 1
            } else {
                point
            };
        if fixed_length >= scientific.len() as i128 {
            return scientific;
        }
        let mut fixed = String::with_capacity(fixed_length as usize);
        fixed.push_str(sign);
        if point <= 0 {
            fixed.push_str("0.");
            fixed.extend(std::iter::repeat_n('0', (-point) as usize));
            fixed.push_str(digits);
        } else if point < digits.len() as i128 {
            let (whole, fraction) = digits.split_at(point as usize);
            fixed.push_str(whole);
            fixed.push('.');
            fixed.push_str(fraction);
        } else {
            fixed.push_str(digits);
            fixed.extend(std::iter::repeat_n(
                '0',
                (point - digits.len() as i128) as usize,
            ));
        }
        fixed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_tokens_round_trip_without_float_or_exponent_expansion() {
        for (token, rendered) in [
            ("3.1400", "3.14"),
            ("1e10", "1e10"),
            ("-0.001", "-1e-3"),
            ("42", "42"),
            ("1.5e-3", "15e-4"),
            ("9007199254740993", "9007199254740993"),
            ("-0e999999999999999999999999999999999999999999999", "0"),
        ] {
            let decimal = Decimal::parse_token(token).unwrap();
            assert_eq!(decimal.render(), rendered);
            assert_eq!(Decimal::parse_token(&decimal.render()).unwrap(), decimal);
        }
    }

    #[test]
    fn normalization_preserves_representable_exponent_boundaries() {
        let upper = Decimal::from_parts("10", i64::MAX).unwrap();
        assert_eq!(upper.coefficient(), "10");
        assert_eq!(upper.exponent(), i64::MAX);
        assert_eq!(upper.render(), "10e9223372036854775807");
        let lower = Decimal::from_parts("1", i64::MIN).unwrap();
        assert_eq!(lower.render(), "1e-9223372036854775808");
        assert_eq!(
            Decimal::parse_token("1.0e-9223372036854775808").unwrap(),
            lower
        );
        assert_eq!(
            Decimal::parse_token("1.1e-9223372036854775808"),
            Err(DecimalError::ExponentOutOfRange)
        );
        assert_eq!(
            Decimal::parse_token("1e9223372036854775808"),
            Err(DecimalError::ExponentOutOfRange)
        );
        assert_eq!(Decimal::from_parts("0", i64::MIN).unwrap().render(), "0");
        assert_eq!(Decimal::parse_token(&upper.render()).unwrap(), upper);
    }

    #[test]
    fn malformed_numbers_are_rejected_at_the_shared_boundary() {
        for token in [
            "", "+1", "01", "1.", ".1", "1e", "1e+", "1e--1", "NaN", "1e1.5", "--1", " 1", "0e!",
        ] {
            assert_eq!(
                Decimal::parse_token(token),
                Err(DecimalError::Syntax),
                "{token}"
            );
        }
    }
}
