use std::cmp::Ordering;
use std::collections::HashMap;

use super::{
    cell::Units,
    column::{Column, Sort},
    filter::Filter,
    groups::Groups,
    row::Row,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Notice {
    Lacks(&'static str),
    Disabled(&'static str),
    Failed(String),
    Loading,
}

impl Notice {
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Loading => "waiting for the first sample of this view".into(),
            Self::Lacks(what) => {
                format!("the server lacks {what}: its management API answered 404")
            }
            Self::Disabled(what) => {
                format!("{what} is disabled on the server: enable it in the configuration")
            }
            Self::Failed(error) => format!("could not read this view: {error}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Population {
    pub(crate) shown: u64,
    pub(crate) of: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct Table {
    pub(crate) columns: &'static [Column],
    pub(crate) rows: Vec<Row>,
    pub(crate) other: Option<Row>,
    pub(crate) total: Option<Row>,
    pub(crate) notice: Option<Notice>,
    pub(crate) population: Option<Population>,
}

impl Table {
    pub(crate) const fn new(columns: &'static [Column], rows: Vec<Row>) -> Self {
        Self {
            columns,
            rows,
            other: None,
            total: None,
            notice: None,
            population: None,
        }
    }

    pub(crate) fn noticed(columns: &'static [Column], notice: Notice) -> Self {
        let mut table = Self::new(columns, Vec::new());
        table.notice = Some(notice);
        table
    }

    pub(crate) fn totalled(mut self) -> Self {
        let roots = self
            .rows
            .iter()
            .filter(|row| row.parent.is_none())
            .collect::<Vec<_>>();
        if !roots.is_empty() {
            self.total = Some(Row::folded("TOTAL", "TOTAL", &roots, self.columns));
        }
        self
    }

    pub(crate) fn grouped(self, key: &str) -> Self {
        let Some(column) = self.columns.iter().position(|column| column.key == key) else {
            return self;
        };
        let mut order: Vec<String> = Vec::new();
        let mut members: HashMap<String, Vec<Row>> = HashMap::new();
        for row in self.rows {
            let group = match row.cells[column].render(Units::Bytes) {
                text if text.is_empty() || text == "-" => "(none)".to_owned(),
                text => text,
            };
            if !members.contains_key(&group) {
                order.push(group.clone());
            }
            members.entry(group).or_default().push(row);
        }
        let mut rows = Vec::new();
        for group in order {
            let children = members.remove(&group).unwrap_or_default();
            let label = format!("{group} ({})", children.len());
            let parent = rows.len();
            rows.push(Row::folded(
                &format!("group:{group}"),
                &label,
                &children.iter().collect::<Vec<_>>(),
                self.columns,
            ));
            rows.extend(children.into_iter().map(|child| child.under(parent, 1)));
        }
        Self { rows, ..self }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Arranged {
    pub(crate) table: Table,
    pub(crate) order: Vec<usize>,
    pub(crate) parents: Vec<bool>,
}

pub(crate) struct Arrangement<'a> {
    pub(crate) sort: Sort,
    pub(crate) filter: &'a Filter,
    pub(crate) groups: &'a Groups,
    pub(crate) lock: Option<&'a [String]>,
    pub(crate) units: Units,
}

impl Arranged {
    pub(crate) fn new(table: Table, arrangement: &Arrangement) -> Self {
        let mut children = vec![Vec::new(); table.rows.len()];
        let mut roots = Vec::new();
        for (index, row) in table.rows.iter().enumerate() {
            match row.parent {
                Some(parent) => children[parent].push(index),
                None => roots.push(index),
            }
        }
        let included = included(&table, arrangement);
        let positions = arrangement.lock.map(|keys| {
            keys.iter()
                .enumerate()
                .map(|(position, key)| (key.as_str(), position))
                .collect::<HashMap<_, _>>()
        });
        let compare = |left: &usize, right: &usize| {
            order(&table, arrangement.sort, positions.as_ref(), *left, *right)
        };
        roots.retain(|index| included[*index]);
        roots.sort_by(compare);
        let mut order = Vec::with_capacity(table.rows.len());
        let forced = !arrangement.filter.is_empty();
        for root in roots {
            order.push(root);
            let open = forced || arrangement.groups.is_open(&table.rows[root].key);
            if open {
                let mut shown = children[root]
                    .iter()
                    .copied()
                    .filter(|index| included[*index])
                    .collect::<Vec<_>>();
                shown.sort_by(compare);
                order.extend(shown);
            }
        }
        let parents = children
            .iter()
            .map(|children| !children.is_empty())
            .collect();
        Self {
            table,
            order,
            parents,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }

    pub(crate) fn row(&self, position: usize) -> Option<&Row> {
        self.order
            .get(position)
            .map(|index| &self.table.rows[*index])
    }

    pub(crate) fn is_parent(&self, position: usize) -> bool {
        self.order
            .get(position)
            .is_some_and(|index| self.parents[*index])
    }

    pub(crate) fn position(&self, key: &str) -> Option<usize> {
        self.order
            .iter()
            .position(|index| self.table.rows[*index].key == key)
    }

    pub(crate) fn keys(&self) -> Vec<String> {
        self.order
            .iter()
            .map(|index| self.table.rows[*index].key.clone())
            .collect()
    }
}

fn included(table: &Table, arrangement: &Arrangement) -> Vec<bool> {
    let mut included = table
        .rows
        .iter()
        .map(|row| {
            arrangement
                .filter
                .matches(row, table.columns, arrangement.units)
        })
        .collect::<Vec<_>>();
    for index in (0..table.rows.len()).rev() {
        if let (true, Some(parent)) = (included[index], table.rows[index].parent) {
            included[parent] = true;
        }
    }
    included
}

fn order(
    table: &Table,
    sort: Sort,
    positions: Option<&HashMap<&str, usize>>,
    left: usize,
    right: usize,
) -> Ordering {
    let (left, right) = (&table.rows[left], &table.rows[right]);
    let locked = positions.map(|positions| {
        (
            positions.get(left.key.as_str()),
            positions.get(right.key.as_str()),
        )
    });
    match locked {
        Some((Some(left), Some(right))) => return left.cmp(right),
        Some((Some(_), None)) => return Ordering::Less,
        Some((None, Some(_))) => return Ordering::Greater,
        _ => {}
    }
    let (first, second) = (&left.cells[sort.column], &right.cells[sort.column]);
    let by_value = match (first.is_missing(), second.is_missing()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) if sort.descending => second.ordering(first),
        (false, false) => first.ordering(second),
    };
    by_value.then_with(|| left.key.cmp(&right.key))
}
