import { Priority, type Task } from "./models.js";

type Tag = `#${string}`;
const OWNER = "Ada" as const;

function summarize<T extends Tag>(task: Task, tags: readonly T[]): string {
  const label = (() => {
    switch (task.priority) {
      case Priority.High:
        return "urgent";
      case Priority.Low:
        return "later";
    }
  })();
  const names = tags.map((tag) => tag.toLocaleLowerCase()).join(", ");

  return `${OWNER}: ${task.title} · ${label} · ${names}`;
}

async function main(): Promise<void> {
  const tasks = new Map<string, Task>([
    [
      "demo",
      {
        title: "Ship semantic colors",
        priority: Priority.High,
      },
    ],
  ]);
  const selected = tasks.get("demo");

  if (selected) {
    await Promise.resolve(console.log(summarize(selected, ["#LSP", "#TreeSitter"])));
  }
}

void main();
