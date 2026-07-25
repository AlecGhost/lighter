from collections.abc import Iterable
from typing import Final, TypeAlias

from models import Priority, Task

Tag: TypeAlias = str
OWNER: Final = "Ada"


def summarize(
    task: Task, tags: Iterable[Tag]
) -> str:
    """Build a label for one project task."""
    match task.priority:
        case Priority.HIGH:
            label = "urgent"
        case Priority.LOW:
            label = "later"

    names = ", ".join(
        tag.casefold() for tag in tags if tag
    )
    prefix = f"{OWNER}: {task.title!r}"
    return f"{prefix} · {label} · {names}"


def main() -> None:
    tasks = {
        "demo": Task(
            title="Ship semantic colors",
            priority=Priority.HIGH,
        )
    }

    if selected := tasks.get("demo"):
        print(
            summarize(
                selected, ["LSP", "Tree-sitter"]
            )
        )


if __name__ == "__main__":
    main()
