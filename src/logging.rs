#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, clap::ValueEnum)]
#[value(rename_all = "UPPER")]
pub enum LogLevel {
    #[default]
    Error,
    Warn,
    Info,
    Debug,
}

impl LogLevel {
    pub fn includes(self, required: Self) -> bool {
        self >= required
    }
}
