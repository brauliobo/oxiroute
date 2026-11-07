#[allow(clippy::wildcard_imports)]
use super::*;

/// Capability routing compiled against the generation's pools: header value to pool.
#[derive(Debug)]
pub(crate) struct PoolSelectPlan {
    header: HeaderName,
    rules: Box<[(f64, Arc<UpstreamPlan>)]>,
    on_invalid: HttpSelectInvalid,
    spill: HttpSelectSpill,
    pub(crate) forward: bool,
}

/// The selector header holds something other than one non-negative number and the route rejects it.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct InvalidSelectorValue;

impl PoolSelectPlan {
    /// `pools` are the rule pools in rule order.
    pub(crate) fn compile(select: &HttpPoolSelect, pools: Vec<Arc<UpstreamPlan>>) -> Self {
        Self {
            header: HeaderName::from_bytes(select.header.as_bytes())
                .expect("validated selector header"),
            rules: select
                .rules
                .iter()
                .zip(pools)
                .map(|(rule, pool)| (f64::from(rule.at_most), pool))
                .collect(),
            on_invalid: select.on_invalid,
            spill: select.spill,
            forward: select.forward,
        }
    }

    pub(crate) const fn header(&self) -> &HeaderName {
        &self.header
    }

    /// The pool serving a request with these headers; `default` is the route pool.
    pub(crate) fn choose<'a>(
        &'a self,
        headers: &HeaderMap,
        default: &'a Arc<UpstreamPlan>,
    ) -> Result<&'a Arc<UpstreamPlan>, InvalidSelectorValue> {
        let first = match self.first_rule(headers) {
            Ok(first) => first,
            Err(InvalidSelectorValue) if self.on_invalid == HttpSelectInvalid::Default => {
                self.rules.len()
            }
            Err(invalid) => return Err(invalid),
        };
        let mut candidates = self.rules[first..]
            .iter()
            .map(|(_, pool)| pool)
            .chain(std::iter::once(default));
        let chosen = candidates.next().expect("the route pool closes the list");
        if self.spill == HttpSelectSpill::None || first == self.rules.len() {
            return Ok(chosen);
        }
        Ok(std::iter::once(chosen)
            .chain(candidates)
            .find(|pool| pool.selector().has_selectable())
            .unwrap_or(chosen))
    }

    /// Index of the first rule covering the header value; `rules.len()` when the header is absent or
    /// above every bound.
    fn first_rule(&self, headers: &HeaderMap) -> Result<usize, InvalidSelectorValue> {
        let mut values = headers.get_all(&self.header).iter();
        let Some(value) = values.next() else {
            return Ok(self.rules.len());
        };
        let number = value
            .to_str()
            .ok()
            .filter(|_| values.next().is_none())
            .and_then(|text| text.trim().parse::<f64>().ok())
            .filter(|number| number.is_finite() && *number >= 0.0)
            .ok_or(InvalidSelectorValue)?;
        Ok(self
            .rules
            .iter()
            .position(|(at_most, _)| number <= *at_most)
            .unwrap_or(self.rules.len()))
    }
}
