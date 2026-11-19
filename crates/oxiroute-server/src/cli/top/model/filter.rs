use super::{cell::Units, column::Column, row::Row};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Term {
    negated: bool,
    column: Option<String>,
    text: String,
}

impl Term {
    fn parse(word: &str) -> Self {
        let (negated, word) = word
            .strip_prefix('!')
            .map_or((false, word), |rest| (true, rest));
        match word.split_once(':') {
            Some((column, text)) if !column.is_empty() => Self {
                negated,
                column: Some(column.to_ascii_lowercase()),
                text: text.to_ascii_lowercase(),
            },
            _ => Self {
                negated,
                column: None,
                text: word.to_ascii_lowercase(),
            },
        }
    }

    fn holds(&self, row: &Row, columns: &[Column], units: Units) -> bool {
        let found = match &self.column {
            None => row.search.contains(&self.text),
            Some(name) => columns
                .iter()
                .position(|column| column.named(name))
                .is_some_and(|index| {
                    row.cells[index]
                        .render(units)
                        .to_ascii_lowercase()
                        .contains(&self.text)
                }),
        };
        found != self.negated
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Filter {
    source: String,
    terms: Vec<Term>,
}

impl Filter {
    pub(crate) fn parse(source: &str) -> Self {
        Self {
            source: source.to_owned(),
            terms: source.split_whitespace().map(Term::parse).collect(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    pub(crate) fn matches(&self, row: &Row, columns: &[Column], units: Units) -> bool {
        self.terms
            .iter()
            .all(|term| term.holds(row, columns, units))
    }

    pub(crate) fn prefix(&self, field: &str) -> Option<&str> {
        let tag = format!("{field}:");
        self.source
            .split_whitespace()
            .find_map(|word| word.strip_prefix(tag.as_str()))
            .filter(|prefix| !prefix.is_empty())
    }
}
