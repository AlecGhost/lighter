mod model;

use std::collections::BTreeMap;

use model::{Priority, Task};

const OWNER: &str = "Ada";

fn summarize<T: AsRef<str>>(
    task: &Task,
    tags: &[T],
) -> String {
    let label = match task.priority {
        Priority::High => "urgent",
        Priority::Low => "later",
    };
    let names = tags
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join(", ");

    let prefix = format!("{OWNER}: {task}");
    format!("{prefix} · {label} · {names}")
}

fn main() {
    let tasks = BTreeMap::from([
        (
            "backlog",
            Task {
                title: String::from(
                    "Tune syntax colors",
                ),
                priority: Priority::Low,
            },
        ),
        (
            "demo",
            Task {
                title: String::from(
                    "Ship semantic colors",
                ),
                priority: Priority::High,
            },
        ),
    ]);

    if let Some(selected) = tasks.get("demo") {
        println!(
            "{}",
            summarize(
                selected,
                &["lsp", "tree-sitter"]
            )
        );
    }
}
