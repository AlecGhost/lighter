use std::fmt;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Priority {
    Low,
    High,
}

#[derive(Debug)]
pub struct Task {
    pub title: String,
    pub priority: Priority,
}

impl fmt::Display for Task {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} [{:?}]", self.title, self.priority)
    }
}
