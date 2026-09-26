//! Формы множественного числа для строк `Ключ_one/few/many/other` (Android GLOSSARY §1.4, Windows `Plurals.cs`).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluralForm {
    One,
    Few,
    Many,
    Other,
}

impl PluralForm {
    pub fn suffix(self) -> &'static str {
        match self {
            PluralForm::One => "one",
            PluralForm::Few => "few",
            PluralForm::Many => "many",
            PluralForm::Other => "other",
        }
    }
}

/// Русский: one (1, 21), few (2–4, 22–24), many (5–20, 25…); английский: one (1) и other.
pub fn form(count: i64, russian: bool) -> PluralForm {
    let count = count.unsigned_abs();
    if !russian {
        return if count == 1 { PluralForm::One } else { PluralForm::Other };
    }
    let (n10, n100) = (count % 10, count % 100);
    if n10 == 1 && n100 != 11 {
        PluralForm::One
    } else if (2..=4).contains(&n10) && !(12..=14).contains(&n100) {
        PluralForm::Few
    } else {
        PluralForm::Many
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn russian_forms() {
        for (n, expected) in [
            (1, PluralForm::One),
            (21, PluralForm::One),
            (121, PluralForm::One),
            (11, PluralForm::Many),
            (2, PluralForm::Few),
            (24, PluralForm::Few),
            (122, PluralForm::Few),
            (12, PluralForm::Many),
            (14, PluralForm::Many),
            (0, PluralForm::Many),
            (5, PluralForm::Many),
            (111, PluralForm::Many),
        ] {
            assert_eq!(form(n, true), expected, "{n}");
        }
    }

    #[test]
    fn english_forms() {
        assert_eq!(form(1, false), PluralForm::One);
        assert_eq!(form(0, false), PluralForm::Other);
        assert_eq!(form(21, false), PluralForm::Other);
    }
}
