use anyhow::{Result, ensure};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Input {
    Letter(char),
    Backspace,
    Escape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Redraw,
    Selected(usize),
    Cancelled,
}

pub(crate) struct Hints {
    labels: Vec<String>,
    prefix: String,
    length: usize,
}

impl Hints {
    pub fn new(count: usize) -> Result<Self> {
        let mut length = 1;
        let mut capacity = 26usize;
        while capacity < count {
            capacity = capacity
                .checked_mul(26)
                .ok_or_else(|| anyhow::anyhow!("too many windows for label generation"))?;
            length += 1;
        }
        ensure!(count <= 100_000, "too many windows to display");
        let labels = (0..count)
            .map(|mut index| {
                let mut letters = vec![b'a'; length];
                for ch in letters.iter_mut().rev() {
                    *ch += (index % 26) as u8;
                    index /= 26;
                }
                letters.into_iter().map(char::from).collect()
            })
            .collect();
        Ok(Self {
            labels,
            prefix: String::new(),
            length,
        })
    }

    pub fn label(&self, index: usize) -> &str {
        &self.labels[index]
    }
    pub fn prefix(&self) -> &str {
        &self.prefix
    }
    pub fn matches(&self, index: usize) -> bool {
        self.labels[index].starts_with(&self.prefix)
    }

    pub fn input(&mut self, input: Input) -> Action {
        match input {
            Input::Escape => return Action::Cancelled,
            Input::Backspace => {
                self.prefix.pop();
            }
            Input::Letter(c) if c.is_ascii_alphabetic() && self.prefix.len() < self.length => {
                self.prefix.push(c.to_ascii_lowercase());
            }
            Input::Letter(_) => {}
        }
        if self.prefix.len() == self.length
            && let Some(index) = self.labels.iter().position(|label| label == &self.prefix)
        {
            return Action::Selected(index);
        }
        Action::Redraw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    #[test]
    fn boundary_counts_are_unique_and_equal_length() {
        for (n, len) in [(0, 1), (1, 1), (26, 1), (27, 2), (676, 2), (677, 3)] {
            let h = Hints::new(n).unwrap();
            assert_eq!(h.length, len);
            assert_eq!(h.labels.iter().collect::<HashSet<_>>().len(), n);
            assert!(h.labels.iter().all(|s| s.len() == len));
        }
    }
    #[test]
    fn unique_prefix_does_not_select_and_invalid_input_is_editable() {
        let mut h = Hints::new(27).unwrap();
        assert_eq!(h.input(Input::Letter('b')), Action::Redraw);
        assert_eq!(h.input(Input::Letter('z')), Action::Redraw);
        assert!((0..27).all(|i| !h.matches(i)));
        assert_eq!(h.input(Input::Letter('a')), Action::Redraw);
        assert_eq!(h.prefix(), "bz");
        h.input(Input::Backspace);
        assert_eq!(h.input(Input::Letter('A')), Action::Selected(26));
    }
    #[test]
    fn cancel_and_empty_backspace() {
        let mut h = Hints::new(1).unwrap();
        assert_eq!(h.input(Input::Backspace), Action::Redraw);
        assert_eq!(h.input(Input::Escape), Action::Cancelled);
        assert_eq!(h.input(Input::Letter('A')), Action::Selected(0));
    }
}
